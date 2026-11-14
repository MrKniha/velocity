//! Encrypted media (EME with PlayReady) for pages on the engine.
//!
//! A page's content process is sandboxed and has no window, so the PlayReady CDM and the player
//! that decrypts and shows protected video live here, in the browser process: the Windows CDM
//! (`IMFContentDecryptionModule`) and Media Foundation's Media Engine, drawing into a swap chain
//! that DirectComposition shows over the page. Content processes talk to this service over a
//! named pipe (`drm_wire`): EME calls on a MediaKeys connection, and on a player connection the
//! demuxed, still encrypted samples of a MediaSource plus play/pause/seek.
//!
//! What each Media Foundation step needs was found with a standalone spike
//! (velocity-engine/experiments/playready/README.md): the PMP host app, one ITA per stream, the
//! protected media types, our own protection manager, windowless swap chain mode.

#![allow(unsafe_op_in_unsafe_fn, non_snake_case)] // Media Foundation calls throughout; COM method names.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use drm_wire::{Encryption, FromMain, ToMain};
use windows::core::{implement, IInspectable, IUnknown, Interface, BSTR, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Media::Protection::MediaProtectionPMPServer;
use windows::Win32::Foundation::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::*;
use windows::Win32::UI::Shell::PropertiesSystem::*;
use windows_core::{ComObject, IUnknown_Vtbl};

/// COM objects of the free-threaded Media Foundation classes, moved between our threads.
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

type Shared<T> = Arc<Mutex<T>>;

/// MediaKeys connections' CDMs, by the token a player connection names them with.
static CDMS: LazyLock<Mutex<HashMap<u64, Agile<IMFContentDecryptionModule>>>> = LazyLock::new(Default::default);

/// Starts the service; content processes find it through the environment (they inherit it).
pub fn start(data: PathBuf) {
    let name = format!(r"\\.\pipe\velocity-drm-{}-{:x}", std::process::id(), random());
    let Some(security) = pipe_security() else {
        eprintln!("DRM service not started: no security descriptor");
        return;
    };
    // SAFETY: before any thread that reads the environment is started.
    unsafe { std::env::set_var(drm_wire::PIPE_ENV, &name) };
    std::thread::Builder::new().name("drm-pipe".into()).spawn(move || accept(name, security, data)).unwrap();
}

fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.finish()
}

// ---------- Showing the video ----------
//
// The engine draws every page into one child window. Each protected player's swap chain is a
// DirectComposition visual on that window, at the video element's place, shown while its
// webview is the one on screen.
// shortcut: the video is drawn over the page (the page's own controls and subtitles over the
// video are hidden while it plays); upgrade by composing the page above it with alpha.

#[derive(Default)]
struct View {
    hwnd: isize,
    active: Option<String>,
    display: Option<Agile<Display>>,
    videos: Vec<Agile<Video>>,
}

struct Display {
    device: windows::Win32::Graphics::DirectComposition::IDCompositionDevice,
    _target: windows::Win32::Graphics::DirectComposition::IDCompositionTarget,
    root: windows::Win32::Graphics::DirectComposition::IDCompositionVisual,
}

/// One player's picture.
#[derive(Clone)]
struct Video {
    id: u64,
    webview: String,
    visual: windows::Win32::Graphics::DirectComposition::IDCompositionVisual,
    attached: Shared<bool>,
    visible: Shared<bool>,
}

static VIEW: LazyLock<Mutex<View>> = LazyLock::new(Default::default);

/// The engine's page window (its `HWND`).
pub fn set_engine_window(hwnd: isize) {
    VIEW.lock().unwrap().hwnd = hwnd;
}

/// The webview on screen (`None`: no page of the engine is).
pub fn set_active_webview(webview: Option<String>) {
    let mut view = VIEW.lock().unwrap();
    view.active = webview;
    let active = view.active.clone();
    let videos: Vec<Video> = view.videos.iter().map(|v| v.0.clone()).collect();
    if let Some(display) = view.display.as_ref() {
        for video in videos {
            let show = *video.visible.lock().unwrap() && active.as_deref() == Some(&video.webview);
            attach(&display.0.root, &video, show);
        }
        unsafe {
            let _ = display.0.device.Commit();
        }
    }
}

fn attach(root: &windows::Win32::Graphics::DirectComposition::IDCompositionVisual, video: &Video, show: bool) {
    let mut attached = video.attached.lock().unwrap();
    unsafe {
        if show && !*attached {
            let _ = root.AddVisual(&video.visual, true, None);
        } else if !show && *attached {
            let _ = root.RemoveVisual(&video.visual);
        }
    }
    *attached = show;
}

unsafe fn display(hwnd: isize) -> windows::core::Result<Display> {
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::DirectComposition::*;
    let mut device = None;
    D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)?;
    let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice = device.ok_or(E_FAIL)?.cast()?;
    let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
    let target = device.CreateTargetForHwnd(HWND(hwnd as _), true)?;
    let root = device.CreateVisual()?;
    target.SetRoot(&root)?;
    device.Commit()?;
    Ok(Display { device, _target: target, root })
}

/// Puts player `video`'s picture at (x, y, width, height), device pixels of the page window.
fn place(engine: &IMFMediaEngine, video: &mut Option<Video>, id: u64, webview: &str, rect: (f64, f64, f64, f64), visible: bool) {
    let mut view = VIEW.lock().unwrap();
    if view.display.is_none() && view.hwnd != 0 {
        match unsafe { display(view.hwnd) } {
            Ok(d) => view.display = Some(Agile(d)),
            Err(e) => eprintln!("DRM display: {e}"),
        }
    }
    let Some((device, root)) = view.display.as_ref().map(|d| (d.0.device.clone(), d.0.root.clone())) else { return };
    unsafe {
        if video.is_none() {
            // The swap chain exists once the engine knows the video's size.
            let Ok(handle) = engine.cast::<IMFMediaEngineEx>().and_then(|e| e.GetVideoSwapchainHandle()) else { return };
            if handle.is_invalid() {
                return;
            }
            let Ok(surface) = device.CreateSurfaceFromHandle(handle) else { return };
            let Ok(visual) = device.CreateVisual() else { return };
            let _ = visual.SetContent(&surface);
            let v = Video { id, webview: webview.to_string(), visual, attached: Default::default(), visible: Default::default() };
            view.videos.push(Agile(v.clone()));
            *video = Some(v);
        }
        let v = video.as_ref().unwrap();
        let (x, y, w, h) = rect;
        let (w, h) = (w.round().max(1.0) as i32, h.round().max(1.0) as i32);
        if let Ok(ex) = engine.cast::<IMFMediaEngineEx>() {
            let _ = ex.UpdateVideoStream(None, Some(&RECT { left: 0, top: 0, right: w, bottom: h }), None);
        }
        let _ = v.visual.SetOffsetX2(x.round() as f32);
        let _ = v.visual.SetOffsetY2(y.round() as f32);
        *v.visible.lock().unwrap() = visible;
        let show = visible && view.active.as_deref() == Some(webview);
        if trace() {
            eprintln!("DRM video {webview} at {x:.0},{y:.0} {w}x{h}, visible {visible}, on screen: {:?}, shown {show}", view.active);
        }
        attach(&root, v, show);
        let _ = device.Commit();
    }
}

fn remove_video(id: u64) {
    let mut view = VIEW.lock().unwrap();
    let Some(i) = view.videos.iter().position(|v| v.0.id == id) else { return };
    let video = view.videos.remove(i);
    if let Some(display) = view.display.as_ref() {
        attach(&display.0.root, &video.0, false);
        unsafe {
            let _ = display.0.device.Commit();
        }
    }
}

// ---------- The pipe ----------

/// Only this user's processes, including its Low integrity content processes, may connect.
fn pipe_security() -> Option<Agile<windows::Win32::Security::PSECURITY_DESCRIPTOR>> {
    use windows::Win32::Security::Authorization::*;
    use windows::Win32::Security::*;
    use windows::Win32::System::Threading::*;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut size = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buf = vec![0u8; size as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as _), size, &mut size).ok()?;
        let _ = CloseHandle(token);
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})S:(ML;;NW;;;LW)", sid.to_string().ok()?);
        let _ = LocalFree(Some(HLOCAL(sid.0 as _)));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None).ok()?;
        Some(Agile(sd))
    }
}

fn accept(name: String, security: Agile<windows::Win32::Security::PSECURITY_DESCRIPTOR>, data: PathBuf) {
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;
    let half: Shared<HashMap<u64, (bool, File)>> = Default::default();
    let data = Arc::new(data);
    loop {
        let pipe = unsafe {
            let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: security.0 .0, bInheritHandle: false.into() };
            CreateNamedPipeW(
                &HSTRING::from(name.as_str()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                Some(&sa),
            )
        };
        if pipe.is_invalid() {
            eprintln!("DRM pipe: {:?}", windows::core::Error::from_thread());
            return;
        }
        let connected = unsafe { ConnectNamedPipe(pipe, None) };
        if connected.is_err() && windows::core::Error::from_thread().code() != ERROR_PIPE_CONNECTED.to_hresult() {
            unsafe { let _ = CloseHandle(pipe); }
            continue;
        }
        let mut file = unsafe { File::from_raw_handle(pipe.0 as _) };
        let (half, data) = (half.clone(), data.clone());
        // The hello can't block the accept loop.
        std::thread::spawn(move || {
            let Ok(frame) = drm_wire::read_frame(&mut file) else { return };
            let Some((id, up)) = drm_wire::parse_hello(&frame) else { return };
            let mut halves = half.lock().unwrap();
            match halves.remove(&id) {
                Some((other_up, other)) if other_up != up => {
                    drop(halves);
                    let (up, down) = if up { (file, other) } else { (other, file) };
                    serve(up, down, &data);
                },
                _ => {
                    halves.insert(id, (up, file));
                },
            }
        });
    }
}

/// Writes messages to the content process (from any thread).
#[derive(Clone)]
struct Out(Shared<File>);
impl Out {
    fn send(&self, message: FromMain) {
        let _ = drm_wire::write_frame(&mut *self.0.lock().unwrap(), &message.encode());
    }
}

fn serve(mut up: File, down: File, data: &std::path::Path) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
    let out = Out(Arc::new(Mutex::new(down)));
    let Ok(frame) = drm_wire::read_frame(&mut up) else { return };
    match ToMain::decode(&frame) {
        Some(ToMain::CreateKeys { key_system }) => keys_connection(up, out, &key_system, data),
        Some(ToMain::OpenPlayer { keys, webview }) => player_connection(up, out, keys, webview),
        _ => {},
    }
}

// ---------- Property stores ----------
const fn key(g: u128, pid: u32) -> PROPERTYKEY {
    PROPERTYKEY { fmtid: GUID::from_u128(g), pid }
}
const INITDATATYPES: PROPERTYKEY = key(0x497d231b_4eb9_4df0_b474_b9afeb0adf38, 3);
const DISTINCTIVEID: PROPERTYKEY = key(0x7dc9c4a5_12be_497e_8bff_9b60b2dc5845, 4);
const PERSISTEDSTATE: PROPERTYKEY = key(0x5d4df6ae_9af1_4e3d_955b_0e4bd22fedf0, 5);
const AUDIOCAPABILITIES: PROPERTYKEY = key(0x980fbb84_297d_4ea7_895f_bcf28a462881, 6);
const VIDEOCAPABILITIES: PROPERTYKEY = key(0xb172f83d_30dd_4c10_8006_ed53da4d3bdb, 7);
const ROBUSTNESS: PROPERTYKEY = key(0x9d3d2b9e_7023_4944_a8f5_ecca52a46990, 3);
const CONTENTTYPE: PROPERTYKEY = key(0x289fb1fc_d9c4_4cc7_b2be_972b0e9b283a, 4);
const CDM_STOREPATH: PROPERTYKEY = key(0x77d993b9_ba61_4bb7_92c6_18c86a189c06, 2);
const ORIGIN_ID: PROPERTYKEY = key(0x1218a3e2_cfb0_4c98_90e5_5f5818d4b67e, 2);
const GUID_OBJECT_STREAM: GUID = GUID::from_u128(0x3e73735c_e6c0_481d_8260_ee5db1343b5f);
const GUID_CLASS_NAME: GUID = GUID::from_u128(0x77631a31_e5e7_4785_bf17_20f57b224802);
const CLSID_EME_STORE_ACTIVATE: GUID = GUID::from_u128(0x2df7b51e_797b_4d06_be71_d14a52cf8421);

fn pv(vt: VARENUM, set: impl FnOnce(&mut PROPVARIANT_0_0_0)) -> PROPVARIANT {
    let mut p = PROPVARIANT::default();
    unsafe {
        let inner = &mut *p.Anonymous.Anonymous;
        inner.vt = vt;
        set(&mut inner.Anonymous);
    }
    p
}
fn pv_str(s: &str) -> PROPVARIANT {
    pv(VT_BSTR, |a| a.bstrVal = ManuallyDrop::new(BSTR::from(s)))
}
fn pv_u32(v: u32) -> PROPVARIANT {
    pv(VT_UI4, |a| a.ulVal = v)
}
fn pv_i64(v: i64) -> PROPVARIANT {
    pv(VT_I8, |a| a.hVal = v)
}
/// A vector of BSTRs; its memory is the property store's after SetValue copies it.
fn pv_strs(v: &[&str]) -> PROPVARIANT {
    unsafe {
        let items = CoTaskMemAlloc(v.len() * size_of::<BSTR>()) as *mut BSTR;
        for (i, s) in v.iter().enumerate() {
            items.add(i).write(BSTR::from(*s));
        }
        pv(VARENUM(VT_VECTOR.0 | VT_BSTR.0), |a| a.cabstr = CABSTR { cElems: v.len() as u32, pElems: items })
    }
}
fn pv_stores(v: Vec<IPropertyStore>) -> PROPVARIANT {
    unsafe {
        let n = v.len();
        let items = CoTaskMemAlloc(n * size_of::<PROPVARIANT>()) as *mut PROPVARIANT;
        for (i, s) in v.into_iter().enumerate() {
            let u: IUnknown = s.cast().unwrap();
            items.add(i).write(pv(VT_UNKNOWN, |a| a.punkVal = ManuallyDrop::new(Some(u))));
        }
        pv(VARENUM(VT_VECTOR.0 | VT_VARIANT.0), |a| a.capropvar = CAPROPVARIANT { cElems: n as u32, pElems: items })
    }
}
fn store(values: Vec<(PROPERTYKEY, PROPVARIANT)>) -> windows::core::Result<IPropertyStore> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        PSCreateMemoryPropertyStore(&IPropertyStore::IID, &mut raw)?;
        let s = IPropertyStore::from_raw(raw);
        for (k, v) in values {
            s.SetValue(&k, &v)?;
            let _ = PropVariantClear(&mut { v });
        }
        Ok(s)
    }
}

// ---------- MediaKeys ----------

/// What the CDM uses to create its objects in the protected media path (as Firefox's
/// MFPMPHostWrapper): without it, GenerateRequest fails with DRM_E_LOGICERR.
#[implement(IMFPMPHostApp)]
struct HostApp(IMFPMPHost);
impl IMFPMPHostApp_Impl for HostApp_Impl {
    fn LockProcess(&self) -> windows::core::Result<()> {
        unsafe { self.0.LockProcess() }
    }
    fn UnlockProcess(&self) -> windows::core::Result<()> {
        unsafe { self.0.UnlockProcess() }
    }
    fn ActivateClassById(&self, id: &PCWSTR, stream: windows::core::Ref<IStream>, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.unwrap();
            attrs.SetString(&GUID_CLASS_NAME, *id)?;
            if let Some(stream) = stream.as_ref() {
                let mut stat = STATSTG::default();
                stream.Stat(&mut stat, STATFLAG(STATFLAG_NOOPEN.0 | STATFLAG_NONAME.0))?;
                let mut blob = vec![0u8; stat.cbSize as usize];
                if !blob.is_empty() {
                    let mut read = 0u32;
                    stream.Read(blob.as_mut_ptr() as _, blob.len() as u32, Some(&mut read)).ok()?;
                    attrs.SetBlob(&GUID_OBJECT_STREAM, &blob[..read as usize])?;
                }
            }
            let out = CreateStreamOnHGlobal(Default::default(), true)?;
            MFSerializeAttributesToStream(&attrs, 0, &out)?;
            out.Seek(0, STREAM_SEEK_SET, None)?;
            let activate: IMFActivate = self.0.CreateObjectByCLSID(&CLSID_EME_STORE_ACTIVATE, &out)?;
            (Interface::vtable(&activate).ActivateObject)(Interface::as_raw(&activate), riid, ppv).ok()
        }
    }
}

#[implement(IMFContentDecryptionModuleSessionCallbacks)]
struct SessionCallbacks {
    session: u32,
    out: Out,
    mf: Shared<Option<Agile<IMFContentDecryptionModuleSession>>>,
}
impl IMFContentDecryptionModuleSessionCallbacks_Impl for SessionCallbacks_Impl {
    fn KeyMessage(&self, kind: MF_MEDIAKEYSESSION_MESSAGETYPE, message: *const u8, size: u32, _url: &PCWSTR) -> windows::core::Result<()> {
        let data = unsafe { std::slice::from_raw_parts(message, size as usize) }.to_vec();
        // EME: the session ID is known before its first message.
        if let Some(session) = self.mf.lock().unwrap().as_ref() {
            if let Ok(id) = unsafe { session.0.GetSessionId() } {
                let id = unsafe { id.to_string() }.unwrap_or_default();
                self.out.send(FromMain::SessionId { session: self.session, id });
            }
        }
        self.out.send(FromMain::Message { session: self.session, kind: kind.0 as u8, data });
        Ok(())
    }
    fn KeyStatusChanged(&self) -> windows::core::Result<()> {
        let Some(session) = self.mf.lock().unwrap().as_ref().map(|s| s.0.clone()) else { return Ok(()) };
        let mut statuses = Vec::new();
        unsafe {
            let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
            session.GetKeyStatuses(&mut list, &mut count)?;
            for i in 0..count as usize {
                let s: &MFMediaKeyStatus = &*list.add(i);
                if s.cbKeyId == 16 {
                    let guid = std::ptr::read_unaligned(s.pbKeyId as *const GUID);
                    statuses.push((kid_from_guid(&guid), s.eMediaKeyStatus.0 as u8));
                }
                CoTaskMemFree(Some(s.pbKeyId as _));
            }
            CoTaskMemFree(Some(list as _));
        }
        self.out.send(FromMain::KeyStatuses { session: self.session, statuses });
        Ok(())
    }
}

/// A key ID as in the file (network order) from the CDM's GUID (little-endian fields).
fn kid_from_guid(g: &GUID) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[0..4].copy_from_slice(&g.data1.to_be_bytes());
    k[4..6].copy_from_slice(&g.data2.to_be_bytes());
    k[6..8].copy_from_slice(&g.data3.to_be_bytes());
    k[8..].copy_from_slice(&g.data4);
    k
}

/// The reverse, for the samples' MFSampleExtension_Content_KeyID (as Chromium's GetGUIDFromString).
fn guid_from_kid(k: &[u8; 16]) -> GUID {
    GUID::from_values(
        u32::from_be_bytes(k[0..4].try_into().unwrap()),
        u16::from_be_bytes([k[4], k[5]]),
        u16::from_be_bytes([k[6], k[7]]),
        k[8..16].try_into().unwrap(),
    )
}

/// VELOCITY_DRM_TRACE=1: what the service does, on stderr.
fn trace() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("VELOCITY_DRM_TRACE").is_some());
    *ON
}

pub fn supported_key_system(key_system: &str) -> bool {
    matches!(key_system, "com.microsoft.playready" | "com.microsoft.playready.recommendation")
}

unsafe fn create_cdm(data: &std::path::Path) -> windows::core::Result<IMFContentDecryptionModule> {
    let ks = HSTRING::from("com.microsoft.playready.recommendation");
    let engine: IMFMediaEngineClassFactory4 = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
    let mut raw = std::ptr::null_mut();
    engine.CreateContentDecryptionModuleFactory(&ks, &IMFContentDecryptionModuleFactory::IID, &mut raw)?;
    let factory = IMFContentDecryptionModuleFactory::from_raw(raw);
    // Software DRM (robustness 2000), the codecs Velocity's MSE hands out encrypted.
    let video = ["avc1.640028", "avc1.4d401f"]
        .iter()
        .map(|c| store(vec![(CONTENTTYPE, pv_str(&format!(r#"video/mp4; codecs="{c}""#))), (ROBUSTNESS, pv_str("2000"))]))
        .collect::<windows::core::Result<Vec<_>>>()?;
    let audio = ["mp4a.40.2", "ec-3"]
        .iter()
        .map(|c| store(vec![(CONTENTTYPE, pv_str(&format!(r#"audio/mp4; codecs="{c}""#))), (ROBUSTNESS, pv_str("2000"))]))
        .collect::<windows::core::Result<Vec<_>>>()?;
    let config = store(vec![
        (INITDATATYPES, pv_strs(&["cenc"])),
        (DISTINCTIVEID, pv_u32(MF_MEDIAKEYS_REQUIREMENT_REQUIRED.0 as u32)),
        (PERSISTEDSTATE, pv_u32(MF_MEDIAKEYS_REQUIREMENT_REQUIRED.0 as u32)),
        (VIDEOCAPABILITIES, pv_stores(video)),
        (AUDIOCAPABILITIES, pv_stores(audio)),
    ])?;
    let access = factory.CreateContentDecryptionModuleAccess(&ks, &[Some(config)])?;
    let dir = data.join("drm");
    let _ = std::fs::create_dir_all(&dir);
    let cdm = access.CreateContentDecryptionModule(&store(vec![
        (ORIGIN_ID, pv_str("velocity")),
        (CDM_STOREPATH, pv_str(&dir.to_string_lossy())),
    ])?)?;
    let pmp: IMFPMPHost = cdm.cast::<IMFGetService>()?.GetService(&MF_CONTENTDECRYPTIONMODULE_SERVICE)?;
    cdm.SetPMPHostApp(&IMFPMPHostApp::from(HostApp(pmp)))?;
    Ok(cdm)
}

fn keys_connection(mut up: File, out: Out, key_system: &str, data: &std::path::Path) {
    if !supported_key_system(key_system) {
        return out.send(FromMain::Failed { message: format!("{key_system} is not supported") });
    }
    let cdm = match unsafe { create_cdm(data) } {
        Ok(cdm) => cdm,
        Err(e) => return out.send(FromMain::Failed { message: format!("PlayReady: {e}") }),
    };
    let token = random();
    CDMS.lock().unwrap().insert(token, Agile(cdm.clone()));
    out.send(FromMain::KeysReady { token });
    let mut sessions: HashMap<u32, Shared<Option<Agile<IMFContentDecryptionModuleSession>>>> = HashMap::new();
    let reply = |req: u32, r: windows::core::Result<()>| {
        out.send(FromMain::Reply { req, ok: r.is_ok(), message: r.err().map(|e| e.to_string()).unwrap_or_default() })
    };
    while let Ok(frame) = drm_wire::read_frame(&mut up) {
        let Some(message) = ToMain::decode(&frame) else { break };
        let session_of = |id: u32| sessions.get(&id).and_then(|s| s.lock().unwrap().as_ref().map(|s| s.0.clone()));
        match message {
            ToMain::CreateSession { session, persistent } => {
                let slot: Shared<Option<Agile<IMFContentDecryptionModuleSession>>> = Default::default();
                let callbacks: IMFContentDecryptionModuleSessionCallbacks = SessionCallbacks { session, out: out.clone(), mf: slot.clone() }.into();
                let kind = if persistent { MF_MEDIAKEYSESSION_TYPE_PERSISTENT_LICENSE } else { MF_MEDIAKEYSESSION_TYPE_TEMPORARY };
                if let Ok(s) = unsafe { cdm.CreateSession(kind, &callbacks) } {
                    *slot.lock().unwrap() = Some(Agile(s));
                }
                sessions.insert(session, slot);
            },
            ToMain::GenerateRequest { req, session, init_data_type, init_data } => {
                let r = match session_of(session) {
                    Some(s) => unsafe { s.GenerateRequest(&HSTRING::from(init_data_type), &init_data) },
                    None => Err(E_INVALIDARG.into()),
                };
                reply(req, r);
            },
            ToMain::Update { req, session, response } => {
                let r = match session_of(session) {
                    Some(s) => unsafe { s.Update(&response) },
                    None => Err(E_INVALIDARG.into()),
                };
                reply(req, r);
            },
            ToMain::Close { req, session } => {
                let r = session_of(session).map_or(Ok(()), |s| unsafe { s.Close() });
                sessions.remove(&session);
                reply(req, r);
            },
            ToMain::Remove { req, session } => {
                let r = session_of(session).map_or(Err(E_INVALIDARG.into()), |s| unsafe { s.Remove() });
                reply(req, r);
            },
            ToMain::SetServerCertificate { req, certificate } => reply(req, unsafe { cdm.SetServerCertificate(&certificate) }),
            _ => break,
        }
    }
    CDMS.lock().unwrap().remove(&token);
    for (_, s) in sessions {
        if let Some(s) = s.lock().unwrap().take() {
            let _ = unsafe { s.0.Close() };
        }
    }
}

// ---------- The media source the engine plays ----------

struct Track {
    video: bool,
    codec: String,
    codec_data: Vec<u8>,
    width: u32,
    height: u32,
    rate: u32,
    channels: u32,
}

struct Sample {
    pts: i64,
    duration: i64,
    key: bool,
    data: Vec<u8>,
    encryption: Option<Encryption>,
}

/// SPS and PPS from an avcC box, as Annex B.
fn avc_parameter_sets(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 5;
    let Some(&n) = avcc.get(i) else { return out };
    i += 1;
    let mut sets = |count: usize, i: &mut usize| {
        for _ in 0..count {
            let Some(len) = avcc.get(*i..*i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else { return };
            let Some(set) = avcc.get(*i + 2..*i + 2 + len) else { return };
            out.extend([0, 0, 0, 1]);
            out.extend(set);
            *i += 2 + len;
        }
    };
    sets((n & 0x1f) as usize, &mut i);
    let pps = avcc.get(i).copied().unwrap_or(0) as usize;
    i += 1;
    sets(pps, &mut i);
    out
}

unsafe fn media_type(track: &Track, protected: bool) -> windows::core::Result<IMFMediaType> {
    let mt = MFCreateMediaType()?;
    if track.video {
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, (track.width as u64) << 32 | track.height as u64)?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    } else if track.codec == "ec-3" || track.codec == "ac-3" {
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        mt.SetGUID(&MF_MT_SUBTYPE, if track.codec == "ec-3" { &MFAudioFormat_Dolby_DDPlus } else { &MFAudioFormat_Dolby_AC3 })?;
        mt.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, track.channels)?;
        mt.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, track.rate)?;
    } else {
        // HEAACWAVEINFO: WAVEFORMATEX, payload type 0 (raw AAC), profile 0xFE, then the AudioSpecificConfig.
        let mut w = Vec::new();
        w.extend(0x1610u16.to_le_bytes());
        w.extend((track.channels as u16).to_le_bytes());
        w.extend(track.rate.to_le_bytes());
        w.extend((track.rate * track.channels * 2).to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(((12 + track.codec_data.len()) as u16).to_le_bytes());
        w.extend(0u16.to_le_bytes());
        w.extend(0xFEu16.to_le_bytes());
        w.extend([0u8; 8]);
        w.extend(&track.codec_data);
        MFInitMediaTypeFromWaveFormatEx(&mt, w.as_ptr() as *const windows::Win32::Media::Audio::WAVEFORMATEX, w.len() as u32)?;
    }
    if protected {
        let subtype = mt.GetGUID(&MF_MT_SUBTYPE)?;
        return MFWrapMediaType(&mt, &MFMediaType_Protected, &subtype);
    }
    Ok(mt)
}

unsafe fn mf_sample(s: &Sample, parameter_sets: Option<&[u8]>) -> windows::core::Result<IMFSample> {
    // H.264 from MP4: 4-byte NAL lengths become start codes (same size); SPS and PPS go before
    // key frames, which makes the first subsample's clear part that much longer.
    let mut data = Vec::with_capacity(s.data.len() + 64);
    let mut extra = 0u32;
    match parameter_sets {
        Some(sets) => {
            if s.key {
                data.extend(sets);
                extra = sets.len() as u32;
            }
            let mut i = 0;
            while i + 4 <= s.data.len() {
                let n = u32::from_be_bytes(s.data[i..i + 4].try_into().unwrap()) as usize;
                data.extend([0, 0, 0, 1]);
                data.extend(&s.data[i + 4..(i + 4 + n).min(s.data.len())]);
                i += 4 + n;
            }
        },
        None => data.extend(&s.data),
    }
    let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
    let mut ptr = std::ptr::null_mut();
    buffer.Lock(&mut ptr, None, None)?;
    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
    buffer.Unlock()?;
    buffer.SetCurrentLength(data.len() as u32)?;
    let sample = MFCreateSample()?;
    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(s.pts)?;
    sample.SetSampleDuration(s.duration)?;
    if s.key {
        sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
    }
    if let Some(e) = &s.encryption {
        let scheme = if e.cbcs { MF_SAMPLE_ENCRYPTION_PROTECTION_SCHEME_AES_CBC } else { MF_SAMPLE_ENCRYPTION_PROTECTION_SCHEME_AES_CTR };
        sample.SetUINT32(&MFSampleExtension_Encryption_ProtectionScheme, scheme.0 as u32)?;
        if e.cbcs && e.skip_blocks > 0 {
            sample.SetUINT32(&MFSampleExtension_Encryption_CryptByteBlock, e.crypt_blocks)?;
            sample.SetUINT32(&MFSampleExtension_Encryption_SkipByteBlock, e.skip_blocks)?;
        }
        sample.SetGUID(&MFSampleExtension_Content_KeyID, &guid_from_kid(&e.kid))?;
        // An 8-byte IV when the low half of a 16-byte one is zero (some hardware needs it).
        let iv = if e.iv.len() == 16 && e.iv[8..].iter().all(|b| *b == 0) { &e.iv[..8] } else { &e.iv[..] };
        sample.SetBlob(&MFSampleExtension_Encryption_SampleID, iv)?;
        let mut map = Vec::new();
        let subsamples = if e.subsamples.is_empty() { vec![(0, s.data.len() as u32)] } else { e.subsamples.clone() };
        for (i, (clear, encrypted)) in subsamples.iter().enumerate() {
            map.extend((clear + if i == 0 { extra } else { 0 }).to_le_bytes());
            map.extend(encrypted.to_le_bytes());
        }
        sample.SetBlob(&MFSampleExtension_Encryption_SubSample_Mapping, &map)?;
    }
    Ok(sample)
}

#[derive(Default)]
struct StreamState {
    queue: VecDeque<Sample>,
    /// Sample requests not yet answered, with their tokens.
    tokens: VecDeque<Option<IUnknown>>,
    ended: bool,
    eos_sent: bool,
}

#[implement(IMFMediaStream)]
struct Stream {
    queue: IMFMediaEventQueue,
    source: Mutex<Option<IMFMediaSource>>,
    sd: IMFStreamDescriptor,
    parameter_sets: Option<Vec<u8>>,
    state: Mutex<StreamState>,
}

impl Stream_Impl {
    fn q(&self) -> &IMFMediaEventQueue {
        &self.queue
    }

    /// Answer requests with queued samples; at the end, end of stream.
    fn deliver(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.tokens.is_empty() && !state.queue.is_empty() {
            let s = state.queue.pop_front().unwrap();
            let token = state.tokens.pop_front().unwrap();
            unsafe {
                match mf_sample(&s, self.parameter_sets.as_deref()) {
                    Ok(sample) => {
                        if let Some(t) = &token {
                            let _ = sample.SetUnknown(&MFSampleExtension_Token, t);
                        }
                        let _ = self.queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample);
                    },
                    Err(e) => eprintln!("DRM sample: {e}"),
                }
            }
        }
        if state.queue.is_empty() && state.ended && !state.eos_sent && !state.tokens.is_empty() {
            state.eos_sent = true;
            unsafe {
                let _ = self.queue.QueueEventParamVar(MEEndOfStream.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default());
            }
        }
    }
}

macro_rules! event_generator {
    ($t:ident) => {
        impl IMFMediaEventGenerator_Impl for $t {
            fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> windows::core::Result<IMFMediaEvent> {
                unsafe { self.q().GetEvent(flags.0 as u32) }
            }
            fn BeginGetEvent(&self, cb: windows::core::Ref<IMFAsyncCallback>, state: windows::core::Ref<IUnknown>) -> windows::core::Result<()> {
                unsafe { self.q().BeginGetEvent(cb.as_ref(), state.as_ref()) }
            }
            fn EndGetEvent(&self, r: windows::core::Ref<IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
                unsafe { self.q().EndGetEvent(r.as_ref()) }
            }
            fn QueueEvent(&self, met: u32, ext: *const GUID, hr: HRESULT, value: *const PROPVARIANT) -> windows::core::Result<()> {
                unsafe { self.q().QueueEventParamVar(met, ext, hr, value) }
            }
        }
    };
}
event_generator!(Stream_Impl);
event_generator!(Source_Impl);

impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        self.source.lock().unwrap().clone().ok_or(MF_E_SHUTDOWN.into())
    }
    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        Ok(self.sd.clone())
    }
    fn RequestSample(&self, token: windows::core::Ref<IUnknown>) -> windows::core::Result<()> {
        if trace() {
            let s = self.state.lock().unwrap();
            eprintln!("DRM RequestSample: {} queued, {} waiting", s.queue.len(), s.tokens.len());
        }
        self.state.lock().unwrap().tokens.push_back(token.as_ref().cloned());
        self.deliver();
        Ok(())
    }
}

#[implement(IMFMediaSource, IMFTrustedInput, IMFGetService)]
struct Source {
    queue: IMFMediaEventQueue,
    pd: IMFPresentationDescriptor,
    streams: Mutex<Vec<ComObject<Stream>>>,
    started: AtomicBool,
    cdm: IMFContentDecryptionModule,
    trusted: Mutex<Option<IMFTrustedInput>>,
    /// One ITA per stream, handed out again on later calls; a new one per call fails with
    /// MF_E_INCOMPATIBLE_SAMPLE_PROTECTION.
    authorities: Mutex<HashMap<u32, IUnknown>>,
}

impl Source_Impl {
    fn q(&self) -> &IMFMediaEventQueue {
        &self.queue
    }
}

impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        Ok((MFMEDIASOURCE_CAN_PAUSE.0 | MFMEDIASOURCE_CAN_SEEK.0) as u32)
    }
    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        unsafe { self.pd.Clone() }
    }
    fn Start(&self, _pd: windows::core::Ref<IMFPresentationDescriptor>, _format: *const GUID, pos: *const PROPVARIANT) -> windows::core::Result<()> {
        if trace() {
            eprintln!("DRM source Start");
        }
        unsafe {
            let start = match pos.as_ref() {
                Some(p) if p.Anonymous.Anonymous.vt == VT_I8 => Some(p.Anonymous.Anonymous.Anonymous.hVal),
                _ => None,
            };
            let first = !self.started.swap(true, Ordering::SeqCst);
            let seek = !first && start.is_some();
            let value = start.map_or(PROPVARIANT::default(), pv_i64);
            for stream in self.streams.lock().unwrap().iter() {
                if first {
                    self.queue.QueueEventParamUnk(MENewStream.0 as u32, &GUID::zeroed(), S_OK, &stream.to_interface::<IMFMediaStream>())?;
                } else {
                    self.queue.QueueEventParamUnk(MEUpdatedStream.0 as u32, &GUID::zeroed(), S_OK, &stream.to_interface::<IMFMediaStream>())?;
                }
                if seek {
                    // The requests before the seek are void; samples from the new position follow.
                    let mut state = stream.state.lock().unwrap();
                    state.tokens.clear();
                    state.eos_sent = false;
                }
                let met = if seek { MEStreamSeeked } else { MEStreamStarted };
                stream.queue.QueueEventParamVar(met.0 as u32, &GUID::zeroed(), S_OK, &value)?;
            }
            let met = if seek { MESourceSeeked } else { MESourceStarted };
            self.queue.QueueEventParamVar(met.0 as u32, &GUID::zeroed(), S_OK, &value)
        }
    }
    fn Stop(&self) -> windows::core::Result<()> {
        unsafe {
            for s in self.streams.lock().unwrap().iter() {
                s.state.lock().unwrap().tokens.clear();
                s.queue.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default())?;
            }
            self.queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default())
        }
    }
    fn Pause(&self) -> windows::core::Result<()> {
        unsafe {
            for s in self.streams.lock().unwrap().iter() {
                s.queue.QueueEventParamVar(MEStreamPaused.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default())?;
            }
            self.queue.QueueEventParamVar(MESourcePaused.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default())
        }
    }
    fn Shutdown(&self) -> windows::core::Result<()> {
        unsafe {
            for s in self.streams.lock().unwrap().iter() {
                *s.source.lock().unwrap() = None; // breaks the stream <-> source cycle
                let _ = s.queue.Shutdown();
            }
            self.queue.Shutdown()
        }
    }
}

impl IMFTrustedInput_Impl for Source_Impl {
    fn GetInputTrustAuthority(&self, stream: u32, riid: *const GUID) -> windows::core::Result<IUnknown> {
        unsafe {
            let mut authorities = self.authorities.lock().unwrap();
            if let Some(ita) = authorities.get(&stream) {
                let mut out = std::ptr::null_mut();
                ita.query(riid, &mut out).ok()?;
                return Ok(IUnknown::from_raw(out));
            }
            let mut trusted = self.trusted.lock().unwrap();
            if trusted.is_none() {
                *trusted = Some(self.cdm.CreateTrustedInput(&[])?);
            }
            let ita = trusted.as_ref().unwrap().GetInputTrustAuthority(stream, riid)?;
            authorities.insert(stream, ita.clone());
            Ok(ita)
        }
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(&self, _s: *const GUID, _r: *const GUID, _p: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

unsafe fn make_source(tracks: &[Track], cdm: IMFContentDecryptionModule) -> windows::core::Result<ComObject<Source>> {
    let mut descriptors = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let mt = media_type(track, true)?;
        let sd = MFCreateStreamDescriptor(i as u32, &[Some(mt.clone())])?;
        sd.GetMediaTypeHandler()?.SetCurrentMediaType(&mt)?;
        sd.SetUINT32(&MF_SD_PROTECTED, 1)?;
        descriptors.push(Some(sd));
    }
    let pd = MFCreatePresentationDescriptor(Some(&descriptors))?;
    for i in 0..descriptors.len() {
        pd.SelectStream(i as u32)?;
    }
    let source = ComObject::new(Source {
        queue: MFCreateEventQueue()?,
        pd,
        streams: Mutex::new(Vec::new()),
        started: Default::default(),
        cdm,
        trusted: Mutex::new(None),
        authorities: Default::default(),
    });
    let as_source: IMFMediaSource = source.to_interface();
    for (track, sd) in tracks.iter().zip(descriptors) {
        let parameter_sets = track.video.then(|| avc_parameter_sets(&track.codec_data));
        let stream = ComObject::new(Stream {
            queue: MFCreateEventQueue()?,
            source: Mutex::new(Some(as_source.clone())),
            sd: sd.unwrap(),
            parameter_sets,
            state: Default::default(),
        });
        source.streams.lock().unwrap().push(stream);
    }
    Ok(source)
}

// ---------- The Media Engine ----------

#[implement(IMFMediaEngineExtension)]
struct Extension(IMFMediaSource);
impl IMFMediaEngineExtension_Impl for Extension_Impl {
    fn CanPlayType(&self, _audio: windows::core::BOOL, _mime: &BSTR) -> windows::core::Result<MF_MEDIA_ENGINE_CANPLAY> {
        Ok(MF_MEDIA_ENGINE_CANPLAY_PROBABLY)
    }
    fn BeginCreateObject(&self, _url: &BSTR, _bs: windows::core::Ref<IMFByteStream>, kind: MF_OBJECT_TYPE, _cookie: windows::core::OutRef<IUnknown>, cb: windows::core::Ref<IMFAsyncCallback>, state: windows::core::Ref<IUnknown>) -> windows::core::Result<()> {
        if kind != MF_OBJECT_MEDIASOURCE {
            return Err(MF_E_UNEXPECTED.into());
        }
        unsafe {
            let result = MFCreateAsyncResult(&self.0, cb.as_ref(), state.as_ref())?;
            result.SetStatus(S_OK)?;
            cb.as_ref().ok_or(E_POINTER)?.Invoke(&result)
        }
    }
    fn CancelObjectCreation(&self, _c: windows::core::Ref<IUnknown>) -> windows::core::Result<()> {
        Err(MF_E_UNEXPECTED.into())
    }
    fn EndCreateObject(&self, r: windows::core::Ref<IMFAsyncResult>) -> windows::core::Result<IUnknown> {
        unsafe {
            let r = r.as_ref().ok_or(E_POINTER)?;
            r.GetStatus()?;
            r.GetObject()
        }
    }
}

/// Windows.Media.Protection.IMediaProtectionManager: the Media Engine reads the CDM's PMP
/// server from its Properties. Declared from IUnknown with IInspectable's three slots first, so
/// the vtable has the right layout.
#[windows_core::interface("45694947-c741-434b-a79e-474c12d93d2f")]
unsafe trait IMediaProtectionManager: IUnknown {
    unsafe fn GetIids(&self, count: *mut u32, iids: *mut *mut GUID) -> HRESULT;
    unsafe fn GetRuntimeClassName(&self, name: *mut *mut core::ffi::c_void) -> HRESULT;
    unsafe fn GetTrustLevel(&self, level: *mut i32) -> HRESULT;
    unsafe fn add_ServiceRequested(&self, handler: *mut core::ffi::c_void, cookie: *mut i64) -> HRESULT;
    unsafe fn remove_ServiceRequested(&self, cookie: i64) -> HRESULT;
    unsafe fn add_RebootNeeded(&self, handler: *mut core::ffi::c_void, cookie: *mut i64) -> HRESULT;
    unsafe fn remove_RebootNeeded(&self, cookie: i64) -> HRESULT;
    unsafe fn add_ComponentLoadFailed(&self, handler: *mut core::ffi::c_void, cookie: *mut i64) -> HRESULT;
    unsafe fn remove_ComponentLoadFailed(&self, cookie: i64) -> HRESULT;
    unsafe fn get_Properties(&self, props: *mut *mut core::ffi::c_void) -> HRESULT;
}

/// The content protection manager (as Chromium's and Firefox's): hands content enablers to the CDM.
#[implement(IMFContentProtectionManager, IMediaProtectionManager)]
struct ProtectionManager {
    props: windows::Foundation::Collections::IPropertySet,
    cdm: IMFContentDecryptionModule,
}
impl IMFContentProtectionManager_Impl for ProtectionManager_Impl {
    fn BeginEnableContent(&self, activate: windows::core::Ref<IMFActivate>, _topo: windows::core::Ref<IMFTopology>, cb: windows::core::Ref<IMFAsyncCallback>, state: windows::core::Ref<IUnknown>) -> windows::core::Result<()> {
        unsafe {
            let result = MFCreateAsyncResult(None, cb.as_ref(), state.as_ref())?;
            let activate = activate.as_ref().ok_or(E_POINTER)?;
            let mut raw = std::ptr::null_mut();
            (Interface::vtable(activate).ActivateObject)(Interface::as_raw(activate), &IUnknown::IID, &mut raw).ok()?;
            let enabler: IMFContentEnabler = IUnknown::from_raw(raw).cast()?;
            self.cdm.SetContentEnabler(&enabler, &result)
        }
    }
    fn EndEnableContent(&self, r: windows::core::Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        if let Some(r) = r.as_ref() {
            if let Err(e) = unsafe { r.GetStatus() } {
                eprintln!("DRM content enabling failed: {e}");
            }
        }
        Ok(())
    }
}
impl IMediaProtectionManager_Impl for ProtectionManager_Impl {
    unsafe fn GetIids(&self, count: *mut u32, iids: *mut *mut GUID) -> HRESULT {
        *count = 0;
        *iids = std::ptr::null_mut();
        S_OK
    }
    unsafe fn GetRuntimeClassName(&self, name: *mut *mut core::ffi::c_void) -> HRESULT {
        *name = std::ptr::null_mut();
        S_OK
    }
    unsafe fn GetTrustLevel(&self, level: *mut i32) -> HRESULT {
        *level = 0;
        S_OK
    }
    unsafe fn add_ServiceRequested(&self, _h: *mut core::ffi::c_void, _c: *mut i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn remove_ServiceRequested(&self, _c: i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn add_RebootNeeded(&self, _h: *mut core::ffi::c_void, _c: *mut i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn remove_RebootNeeded(&self, _c: i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn add_ComponentLoadFailed(&self, _h: *mut core::ffi::c_void, _c: *mut i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn remove_ComponentLoadFailed(&self, _c: i64) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn get_Properties(&self, out: *mut *mut core::ffi::c_void) -> HRESULT {
        *out = self.props.clone().into_raw();
        S_OK
    }
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    out: Out,
    engine: Shared<Option<Agile<IMFMediaEngine>>>,
}
impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, p1: usize, p2: u32) -> windows::core::Result<()> {
        if trace() && event != MF_MEDIA_ENGINE_EVENT_TIMEUPDATE.0 as u32 && event != MF_MEDIA_ENGINE_EVENT_PROGRESS.0 as u32 {
            eprintln!("DRM engine event {event} {p1} 0x{p2:08x}");
        }
        let engine = self.engine.lock().unwrap().as_ref().map(|e| e.0.clone());
        let time = || engine.as_ref().map_or(0.0, |e| unsafe { e.GetCurrentTime() });
        let message = match MF_MEDIA_ENGINE_EVENT(event as i32) {
            MF_MEDIA_ENGINE_EVENT_TIMEUPDATE => FromMain::Time { time: time() },
            MF_MEDIA_ENGINE_EVENT_PLAYING => FromMain::Playing,
            MF_MEDIA_ENGINE_EVENT_PAUSE => FromMain::Paused,
            MF_MEDIA_ENGINE_EVENT_ENDED => FromMain::Ended,
            MF_MEDIA_ENGINE_EVENT_WAITING => FromMain::Waiting,
            MF_MEDIA_ENGINE_EVENT_SEEKED => FromMain::Seeked,
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => {
                if let Some(e) = &engine {
                    let (mut w, mut h) = (0, 0);
                    if unsafe { e.GetNativeVideoSize(Some(&mut w), Some(&mut h)) }.is_ok() {
                        self.out.send(FromMain::Size { width: w, height: h });
                    }
                }
                return Ok(());
            },
            MF_MEDIA_ENGINE_EVENT_ERROR => FromMain::Error { message: format!("media engine error {p1}, 0x{p2:08x}") },
            _ => return Ok(()),
        };
        self.out.send(message);
        Ok(())
    }
}

/// The DXGI device the engine decodes and presents with (windowless swap chain mode).
unsafe fn dxgi_manager() -> windows::core::Result<IMFDXGIDeviceManager> {
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    let mut device = None;
    D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)?;
    let device = device.ok_or(E_FAIL)?;
    device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
    let (mut token, mut manager) = (0u32, None);
    MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
    let manager = manager.ok_or(E_FAIL)?;
    manager.ResetDevice(&device, token)?;
    Ok(manager)
}

struct Player {
    engine: IMFMediaEngine,
    source: ComObject<Source>,
}

unsafe fn create_player(tracks: &[Track], cdm: IMFContentDecryptionModule, out: &Out) -> windows::core::Result<Player> {
    let source = make_source(tracks, cdm.clone())?;
    let pmp: MediaProtectionPMPServer = cdm.cast::<IMFGetService>()?.GetService(&MF_CONTENTDECRYPTIONMODULE_SERVICE)?;
    let props = windows::Foundation::Collections::PropertySet::new()?;
    props.Insert(&HSTRING::from("Windows.Media.Protection.MediaProtectionPMPServer"), &pmp.cast::<IInspectable>()?)?;
    let protection: IMFContentProtectionManager = ProtectionManager { props: props.cast()?, cdm }.into();

    let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
    let mut attrs = None;
    MFCreateAttributes(&mut attrs, 6)?;
    let attrs = attrs.ok_or(E_FAIL)?;
    let engine_slot: Shared<Option<Agile<IMFMediaEngine>>> = Default::default();
    let notify: IMFMediaEngineNotify = Notify { out: out.clone(), engine: engine_slot.clone() }.into();
    attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
    attrs.SetUINT32(&MF_MEDIA_ENGINE_CONTENT_PROTECTION_FLAGS, MF_MEDIA_ENGINE_ENABLE_PROTECTED_CONTENT.0 as u32)?;
    attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &dxgi_manager()?)?;
    let extension: IMFMediaEngineExtension = Extension(source.to_interface()).into();
    attrs.SetUnknown(&MF_MEDIA_ENGINE_EXTENSION, &extension)?;
    let engine = factory.CreateInstance(0, &attrs)?;
    *engine_slot.lock().unwrap() = Some(Agile(engine.clone()));
    // In this order: the protection manager resets the swap chain mode, and both precede SetSource.
    engine.cast::<IMFMediaEngineProtectedContent>()?.SetContentProtectionManager(&protection)?;
    engine.cast::<IMFMediaEngineEx>()?.EnableWindowlessSwapchainMode(true)?;
    engine.SetSource(&BSTR::from("velocity-drm://player"))?;
    Ok(Player { engine, source })
}

fn player_connection(mut up: File, out: Out, keys: u64, webview: String) {
    let Some(cdm) = CDMS.lock().unwrap().get(&keys).map(|c| c.0.clone()) else {
        return out.send(FromMain::Error { message: "no MediaKeys".into() });
    };
    let mut tracks = Vec::new();
    let mut player: Option<Player> = None;
    let id = random();
    let mut video: Option<Video> = None;
    while let Ok(frame) = drm_wire::read_frame(&mut up) {
        let Some(message) = ToMain::decode(&frame) else { break };
        if trace() && !matches!(message, ToMain::Sample { .. } | ToMain::Rect { .. }) {
            eprintln!("DRM player <- {message:?}");
        }
        if let ToMain::AddTrack { video, codec, codec_data, width, height, rate, channels, .. } = message {
            tracks.push(Track { video, codec, codec_data, width, height, rate, channels });
            continue;
        }
        // The first other message: every track is known.
        if player.is_none() {
            match unsafe { create_player(&tracks, cdm.clone(), &out) } {
                Ok(p) => player = Some(p),
                Err(e) => {
                    out.send(FromMain::Error { message: format!("protected player: {e}") });
                    break;
                },
            }
        }
        let p = player.as_ref().unwrap();
        unsafe {
            match message {
                ToMain::Sample { track, pts, duration, key, data, encryption } => {
                    if let Some(stream) = p.source.streams.lock().unwrap().get(track as usize) {
                        stream.state.lock().unwrap().queue.push_back(Sample { pts, duration, key, data, encryption });
                        stream.deliver();
                    }
                },
                ToMain::EndOfStream => {
                    for s in p.source.streams.lock().unwrap().iter() {
                        s.state.lock().unwrap().ended = true;
                        s.deliver();
                    }
                },
                ToMain::Flush => {
                    for s in p.source.streams.lock().unwrap().iter() {
                        let mut state = s.state.lock().unwrap();
                        state.queue.clear();
                        state.ended = false;
                    }
                },
                ToMain::Play => {
                    let _ = p.engine.Play();
                },
                ToMain::Pause => {
                    let _ = p.engine.Pause();
                },
                ToMain::Seek { time } => {
                    let _ = p.engine.SetCurrentTime(time);
                },
                ToMain::Rate { rate } => {
                    let _ = p.engine.SetPlaybackRate(rate);
                },
                ToMain::Volume { volume } => {
                    let _ = p.engine.SetVolume(volume);
                },
                ToMain::Muted { muted } => {
                    let _ = p.engine.SetMuted(muted);
                },
                ToMain::Rect { x, y, width, height, visible } => {
                    place(&p.engine, &mut video, id, &webview, (x, y, width, height), visible);
                },
                _ => {},
            }
        }
    }
    remove_video(id);
    if let Some(p) = player {
        unsafe {
            let _ = p.engine.Shutdown();
            let _ = p.source.to_interface::<IMFMediaSource>().Shutdown();
        }
    }
}
