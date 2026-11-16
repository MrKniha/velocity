//! Web pages through Velocity's own engine (Servo, from `velocity-engine`) instead of WebView2.
//! Every http(s) page of a tab is a Servo WebView. They all draw into one child window over the
//! content area, which shows the active tab's page; the shell copies that page's offscreen frame
//! into it. The tab UI (sidebar or top bar) is a page on the engine too, in a child window of its
//! own (`open_ui`); the new tab page and settings stay in WebView2.
//!
//! Servo runs on Velocity's event loop: it wakes the loop with `UserEvent::Engine` and is pumped
//! there. Web content runs in separate, locked-down Low-integrity processes (`sandbox.rs`).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use euclid::{Point2D, Rect, Scale, Size2D};
use servo::{
    Code, ConsoleLogLevel, CreateNewWebViewRequest, Cursor, DevicePoint, EventLoopWaker, InputEvent, Key, KeyState,
    KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey,
    MouseLeftViewportEvent, OffscreenRenderingContext, Opts, Preferences, RenderingContext, Servo, ServoBuilder,
    UserContentManager, UserScript, WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate,
    WebViewId, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};

/// Where the tab UI page is served from (`load_web_resource`; never reaches the network).
const UI_HOST: &str = "ui.velocity.invalid";

/// Runs in the tab UI page: `window.ipc.postMessage` as in WebView2 (wry). Messages go out as
/// console messages starting with \u{1}, which only the UI's WebView is listened to for; "ready"
/// once the page's own script has run, so Rust's calls into it can start.
const UI_IPC_JS: &str = r#"window.ipc = { postMessage: m => console.log('\u0001' + m) };
document.addEventListener('DOMContentLoaded', () => {
  // Under a see-through UI (window glass) WebView2 showed the frame colour; the engine shows white.
  const s = document.createElement('style');
  s.textContent = 'html { background: var(--frame) !important; }';
  document.head.append(s);
  // The engine draws inline SVG from its attributes only, not from CSS: the icons' stroke and fill
  // become attributes (the colour as computed now), again whenever the UI or its theme changes.
  const icons = () => {
    for (const svg of document.querySelectorAll('svg')) {
      const c = getComputedStyle(svg);
      for (const [k, v] of [['fill', 'none'], ['stroke', c.color], ['stroke-width', c.strokeWidth || '1.6'],
        ['stroke-linecap', 'round'], ['stroke-linejoin', 'round']]) {
        if (svg.getAttribute(k) !== v) svg.setAttribute(k, v);
      }
    }
  };
  let queued = false;
  const queue = () => { if (!queued) { queued = true; setTimeout(() => { queued = false; icons(); }); } };
  new MutationObserver(queue).observe(document.documentElement, { childList: true, subtree: true, attributeFilter: ['style', 'data-dark', 'class'] });
  // Hover changes an icon's colour too.
  document.addEventListener('mouseover', queue);
  document.addEventListener('mouseout', queue);
  icons();
  console.log('\u0001\u0001ready');
});"#;

/// Runs in every page on the engine: links the stylesheet that hides the boxes blocked ads leave
/// (cosmetic filters, which the network layer's content blocker serves from uBOL's data). First
/// with the host's filters, then again with the page's ids and classes as they appear.
// shortcut: rescans at DOMContentLoaded, load and 3 s later, not on every DOM change; upgrade with
// a throttled MutationObserver if late-built pages keep their empty boxes.
const COSMETIC_JS: &str = r#"(() => {
  if (!/^https?:$/.test(location.protocol)) return;
  const sent = new Set();
  let first = true;
  const link = () => {
    const tokens = [];
    for (const el of document.querySelectorAll('[id],[class]')) {
      for (const t of [el.id && '#' + el.id, ...Array.from(el.classList, c => '.' + c)]) {
        if (t && !sent.has(t)) { sent.add(t); tokens.push(t); }
      }
    }
    if (!first && !tokens.length) return;
    const l = document.createElement('link');
    l.rel = 'stylesheet';
    l.href = 'https://cosmetic.velocity.invalid/css?h=' + encodeURIComponent(location.hostname) +
      '&t=' + encodeURIComponent(tokens.join(',')) + (first ? '' : '&p=1');
    first = false;
    (document.head || document.documentElement).appendChild(l);
  };
  link();
  document.addEventListener('DOMContentLoaded', link);
  addEventListener('load', () => { link(); setTimeout(link, 3000); });
})();"#;
use tao::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton as TaoButton, MouseScrollDelta, WindowEvent},
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    keyboard::{Key as TaoKey, KeyCode, KeyLocation, ModifiersState},
    platform::windows::WindowBuilderExtWindows,
    window::{CursorIcon, Window, WindowBuilder, WindowId},
};
use url::Url;

use crate::UserEvent;

/// A tab's page: the WebView and the offscreen framebuffer it draws into.
struct Page {
    webview: WebView,
    context: Rc<OffscreenRenderingContext>,
}

/// The tab UI: a page on the engine in its own child window (drawn straight into it).
struct Ui {
    window: Window,
    context: Rc<WindowRenderingContext>,
    webview: OnceCell<WebView>,
    html: &'static str,
    /// Hosts the page loads local files from (the wallpaper, profile pictures) and their folders.
    files: Vec<(String, PathBuf)>,
    /// Scripts for the page while it loads; run once it is ready (None from then on).
    pending: RefCell<Option<Vec<String>>>,
    cursor: Cell<DevicePoint>,
}

impl Ui {
    fn is(&self, webview: &WebView) -> bool {
        self.webview.get().is_some_and(|w| w.id() == webview.id())
    }

    /// The body for `url` if the UI serves it: Some(None) for a missing or outside file.
    fn file(&self, url: &Url) -> Option<Option<Vec<u8>>> {
        if url.host_str()? == UI_HOST {
            return Some(Some(self.html.as_bytes().to_vec()));
        }
        Some(local_file(&self.files, url)?.and_then(|p| std::fs::read(p).ok()))
    }
}

/// The file `url` names in `files` (hosts and their folders): None if the host isn't one of them,
/// Some(None) if the path leads outside its folder.
fn local_file(files: &[(String, PathBuf)], url: &Url) -> Option<Option<PathBuf>> {
    let host = url.host_str()?;
    let (_, dir) = files.iter().find(|(h, _)| h == host)?;
    // Through a file: URL, so %20 decodes and ".." is resolved; an escaped "\..\\" survives that,
    // so no ".." may be left either.
    let path = Url::from_directory_path(dir)
        .ok()
        .and_then(|base| base.join(url.path().trim_start_matches('/')).ok())
        .and_then(|u| u.to_file_path().ok())
        .filter(|p| p.starts_with(dir) && !p.components().any(|c| c == std::path::Component::ParentDir));
    // shortcut: whole files in memory (`Ui::file`); fine for pictures, upgrade to streaming if videos are served.
    Some(path)
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "text/html; charset=utf-8",
    }
}

/// What the delegate (called by Servo) shares with the engine.
struct Inner {
    window: Window,
    context: Rc<WindowRenderingContext>,
    proxy: EventLoopProxy<UserEvent>,
    /// Which tab each WebView belongs to.
    tabs: RefCell<HashMap<WebViewId, u32>>,
    /// Popups (`window.open`, `target=_blank`) built by Servo, waiting for a tab.
    popups: RefCell<Vec<Page>>,
    delegate: RefCell<Option<Rc<dyn WebViewDelegate>>>,
    /// Scripts every page runs (the cosmetic filters' stylesheet link).
    user_content: Rc<UserContentManager>,
    ui: OnceCell<Ui>,
}

pub struct Engine {
    servo: Servo,
    /// Downloads running in the engine: its id to the id in Velocity's downloads list.
    downloads: Arc<Mutex<HashMap<u64, u32>>>,
    inner: Rc<Inner>,
    pages: HashMap<u32, Page>,
    active: Option<u32>,
    cursor: Cell<DevicePoint>,
    modifiers: Cell<Modifiers>,
}

impl Engine {
    /// The engine, with its window as a child of `parent`; `data` keeps cookies and site storage.
    /// `ad_rules`: uBlock Origin Lite's folder, whose rules block ads and trackers on every page.
    pub fn new(parent: &Window, target: &EventLoopWindowTarget<UserEvent>, proxy: EventLoopProxy<UserEvent>, data: &Path, ad_rules: &Path) -> Engine {
        use tao::platform::windows::WindowExtWindows;
        use tao::rwh_06::{HasDisplayHandle, HasWindowHandle};

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        // Loads in the background; the network layer blocks nothing until it is ready.
        net::content_blocker::load(ad_rules.to_path_buf());
        let downloads = Arc::new(Mutex::new(HashMap::new()));
        follow_downloads(proxy.clone(), downloads.clone());
        let window = WindowBuilder::new()
            .with_parent_window(parent.hwnd())
            .with_decorations(false)
            .with_resizable(false) // no resize frame: the client area is the whole window
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(800, 600))
            .build(target)
            .expect("engine window");
        crate::drm::set_engine_window(window.hwnd() as isize); // protected video is drawn on it
        clip_siblings(window.hwnd() as isize);
        let display = window.display_handle().expect("display handle");
        let handle = window.window_handle().expect("window handle");
        let context = Rc::new(WindowRenderingContext::new(display, handle, window.inner_size()).expect("GL context for pages"));
        let _ = context.make_current();

        // Off by default in servo 0.8.0, but real sites throw without them (see velocity-engine's compat report).
        let preferences = Preferences {
            dom_intersection_observer_enabled: true,
            dom_adoptedstylesheet_enabled: true,
            // Memory: Servo keeps its HTTP cache in RAM (bodies too), 5000 entries by default;
            // 500 already held ~20 MB after one YouTube page.
            network_http_cache_size: 500,
            // Firefox's own format: Servo's adds "Servo/0.8.0", which some sites' firewalls
            // refuse (PlayReady's license server answers 403).
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:153.0) Gecko/20100101 Firefox/153.0".into(),
            // ...and SpiderMonkey let a page's heap grow 150-300% before collecting it.
            js_mem_gc_high_frequency_heap_growth_max: 150,
            js_mem_gc_high_frequency_heap_growth_min: 120,
            js_mem_gc_low_frequency_heap_growth: 120,
            ..Default::default()
        };
        set_style_prefs();
        let _ = std::fs::create_dir_all(data);
        let opts = Opts { multiprocess: true, config_dir: Some(data.to_path_buf()), ..Default::default() };
        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(preferences)
            .event_loop_waker(Box::new(Waker(proxy.clone())))
            .build();
        servo.setup_logging();

        let user_content = Rc::new(UserContentManager::new(&servo));
        user_content.add_script(Rc::new(UserScript::new(COSMETIC_JS.into(), None)));
        let inner = Rc::new(Inner {
            window,
            context,
            proxy,
            user_content,
            tabs: RefCell::new(HashMap::new()),
            popups: RefCell::new(Vec::new()),
            delegate: RefCell::new(None),
            ui: OnceCell::new(),
        });
        let delegate: Rc<dyn WebViewDelegate> = Rc::new(Delegate(Rc::downgrade(&inner)));
        *inner.delegate.borrow_mut() = Some(delegate);
        Engine { servo, downloads, inner, pages: HashMap::new(), active: None, cursor: Cell::new(DevicePoint::default()), modifiers: Cell::new(Modifiers::empty()) }
    }

    pub fn window_id(&self) -> WindowId {
        self.inner.window.id()
    }

    /// Show the tab UI page `html` in a child window of `parent`, with `scripts` run before its own;
    /// `files`: hosts it loads local files from, and their folders.
    pub fn open_ui(&self, parent: &Window, target: &EventLoopWindowTarget<UserEvent>, html: &'static str, scripts: &[&str], files: Vec<(String, PathBuf)>) {
        use tao::platform::windows::WindowExtWindows;
        use tao::rwh_06::{HasDisplayHandle, HasWindowHandle};
        let window = WindowBuilder::new()
            .with_parent_window(parent.hwnd())
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(300, 600))
            .build(target)
            .expect("UI window");
        clip_siblings(window.hwnd() as isize);
        let display = window.display_handle().expect("display handle");
        let handle = window.window_handle().expect("window handle");
        let context = Rc::new(WindowRenderingContext::new(display, handle, window.inner_size()).expect("GL context for the UI"));
        let user_content = Rc::new(UserContentManager::new(&self.servo));
        for js in std::iter::once(UI_IPC_JS).chain(scripts.iter().copied()) {
            user_content.add_script(Rc::new(UserScript::new(js.into(), None)));
        }
        let scale = Scale::new(window.scale_factor() as f32);
        let theme = match window.theme() {
            tao::window::Theme::Dark => servo::Theme::Dark,
            _ => servo::Theme::Light,
        };
        let ui = Ui { window, context: context.clone(), webview: OnceCell::new(), html, files, pending: RefCell::new(Some(Vec::new())), cursor: Cell::new(DevicePoint::default()) };
        let ui = match self.inner.ui.set(ui) {
            Ok(()) => self.inner.ui.get().unwrap(),
            Err(_) => return,
        };
        let delegate = self.inner.delegate.borrow().clone().expect("delegate set");
        let webview = WebViewBuilder::new(&self.servo, context)
            .url(Url::parse(&format!("https://{UI_HOST}/")).unwrap())
            .hidpi_scale_factor(scale)
            .delegate(delegate)
            .user_content_manager(user_content)
            .build();
        webview.notify_theme_change(theme);
        webview.show();
        let _ = ui.webview.set(webview);
    }

    pub fn ui_window_id(&self) -> Option<WindowId> {
        self.inner.ui.get().map(|ui| ui.window.id())
    }

    pub fn ui_hwnd(&self) -> Option<isize> {
        use tao::platform::windows::WindowExtWindows;
        self.inner.ui.get().map(|ui| ui.window.hwnd() as isize)
    }

    /// Run `js` in the tab UI page (once it has loaded).
    pub fn ui_js(&self, js: &str) {
        let Some(ui) = self.inner.ui.get() else { return };
        if let Some(pending) = ui.pending.borrow_mut().as_mut() {
            pending.push(js.into());
            return;
        }
        if let Some(webview) = ui.webview.get() {
            webview.evaluate_javascript(js, |_| {});
        }
    }

    /// Place the tab UI over `x`, `y`, `width` x `height` (physical pixels of the parent's client area).
    pub fn ui_bounds(&self, x: i32, y: i32, width: u32, height: u32) {
        let Some(ui) = self.inner.ui.get() else { return };
        let size = PhysicalSize::new(width.max(1), height.max(1));
        // The webview resizes its context itself (only when the context's size differs).
        if place(&ui.window, x, y, size, 0) {
            if let Some(webview) = ui.webview.get() {
                webview.resize(size);
            }
        }
        ui.window.request_redraw();
    }

    pub fn ui_visible(&self, on: bool) {
        if let Some(ui) = self.inner.ui.get() {
            ui.window.set_visible(on);
        }
    }

    /// Keys go to the tab UI.
    pub fn ui_focus(&self) {
        let Some(ui) = self.inner.ui.get() else { return };
        ui.window.set_focus();
        if let Some(webview) = ui.webview.get() {
            webview.set_focused(true);
        }
    }

    pub fn paint_ui(&self) {
        let Some(ui) = self.inner.ui.get() else { return };
        let Some(webview) = ui.webview.get() else { return };
        let _ = ui.context.make_current();
        webview.paint();
        ui.context.present();
    }

    /// Input for the tab UI's window; returns a Velocity shortcut as `handle` does.
    pub fn handle_ui(&self, event: &WindowEvent) -> Option<&'static str> {
        let ui = self.inner.ui.get()?;
        let webview = ui.webview.get()?.clone();
        self.input(webview, &ui.cursor, || self.ui_focus(), event)
    }

    /// Let Servo handle its messages; called whenever it wakes the event loop.
    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    pub fn has(&self, tab: u32) -> bool {
        self.pages.contains_key(&tab)
    }

    /// Show `url` in tab `tab`, creating its page on first use.
    pub fn load(&mut self, tab: u32, url: &str, zoom: f32) {
        let Ok(url) = Url::parse(url) else { return };
        if let Some(page) = self.pages.get(&tab) {
            page.webview.load(url);
            return;
        }
        let servo = &self.servo;
        let page = self.inner.page(|context| WebViewBuilder::new(servo, context).url(url));
        page.webview.set_page_zoom(zoom);
        self.add(tab, page);
    }

    /// Popups Servo opened since the last call; `tab_for` gives each its new tab id.
    pub fn adopt_popups(&mut self, mut tab_for: impl FnMut() -> u32) -> Vec<u32> {
        let popups: Vec<Page> = self.inner.popups.borrow_mut().drain(..).collect();
        popups
            .into_iter()
            .map(|page| {
                let id = tab_for();
                self.add(id, page);
                id
            })
            .collect()
    }

    fn add(&mut self, tab: u32, page: Page) {
        self.inner.tabs.borrow_mut().insert(page.webview.id(), tab);
        page.webview.hide();
        self.pages.insert(tab, page);
    }

    pub fn close(&mut self, tab: u32) {
        if let Some(page) = self.pages.remove(&tab) {
            self.inner.tabs.borrow_mut().remove(&page.webview.id());
        }
        if self.active == Some(tab) {
            self.active = None;
            let _ = self.inner.window.set_visible(false);
        }
    }

    /// Show tab `tab`'s page, or hide the engine window when that tab has none (its new tab page).
    pub fn activate(&mut self, tab: Option<u32>) {
        let tab = tab.filter(|t| self.pages.contains_key(t));
        for (id, page) in &self.pages {
            if Some(*id) == tab { page.webview.show() } else { page.webview.hide() }
        }
        self.active = tab;
        crate::drm::set_active_webview(tab.and_then(|t| self.pages.get(&t)).map(|p| p.webview.id().to_string()));
        self.inner.window.set_visible(tab.is_some());
        if tab.is_some() {
            self.inner.window.request_redraw();
        }
    }

    pub fn active_webview(&self) -> Option<&WebView> {
        self.active.and_then(|t| self.pages.get(&t)).map(|p| &p.webview)
    }

    pub fn back(&self) {
        let _ = self.active_webview().map(|w| w.go_back(1));
    }

    pub fn forward(&self) {
        let _ = self.active_webview().map(|w| w.go_forward(1));
    }

    pub fn reload(&self) {
        let _ = self.active_webview().map(|w| w.reload());
    }

    pub fn set_zoom(&self, zoom: f32) {
        for page in self.pages.values() {
            page.webview.set_page_zoom(zoom);
        }
    }

    /// Put cookies imported from another browser into the engine's cookie jar (the user stays
    /// logged in to those sites on pages the engine shows). Returns how many it took.
    pub fn import_cookies(&self, cookies: &[crate::import::Cookie]) -> usize {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let mut added = 0;
        for c in cookies {
            let Some(cookie) = engine_cookie(c, now) else { continue };
            // The URL the cookie is set from: its host, and https for a Secure cookie.
            let scheme = if c.secure { "https" } else { "http" };
            let path = if c.path.starts_with('/') { c.path.as_str() } else { "/" };
            let Ok(url) = Url::parse(&format!("{scheme}://{}{path}", c.host.trim_start_matches('.'))) else { continue };
            self.servo.site_data_manager().set_cookie_for_url(url, cookie, None);
            added += 1;
        }
        added
    }

    /// Answer the "save as" question for download `id` (Velocity's id): whether it was the
    /// engine's. `choice`: the file, or None to cancel.
    pub fn answer_download(&self, id: u32, choice: Option<std::path::PathBuf>) -> bool {
        let mut downloads = self.downloads.lock().unwrap();
        let Some((&engine_id, _)) = downloads.iter().find(|(_, v)| **v == id) else { return false };
        if choice.is_none() {
            downloads.remove(&engine_id);
        }
        net::download::answer(engine_id, choice);
        true
    }

    /// Cancel download `id` (Velocity's id) if the engine runs it.
    pub fn cancel_download(&self, id: u32) {
        let downloads = self.downloads.lock().unwrap();
        if let Some((&engine_id, _)) = downloads.iter().find(|(_, v)| **v == id) {
            net::download::cancel(engine_id);
        }
    }

    pub fn exit_fullscreen(&self) {
        let _ = self.active_webview().map(|w| w.exit_fullscreen());
    }

    /// Keys go to the page (the engine window has keyboard focus).
    pub fn focus(&self) {
        self.inner.window.set_focus();
        let _ = self.active_webview().map(|w| w.set_focused(true));
    }

    /// Place the page over the content area (physical pixels of the parent's client area), with
    /// corners rounded to `corner` (the ellipse diameter, 0 for square), as tabs' WebViews are.
    pub fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32, corner: i32) {
        let size = PhysicalSize::new(width.max(1), height.max(1));
        if place(&self.inner.window, x, y, size, corner) {
            // Resizing rebinds the window's surface to the context current now: this one, not the UI's.
            let _ = self.inner.context.make_current();
            self.inner.context.resize(size);
            for page in self.pages.values() {
                page.webview.resize(size);
            }
        }
        self.inner.window.request_redraw();
    }

    pub fn hwnd(&self) -> isize {
        use tao::platform::windows::WindowExtWindows;
        self.inner.window.hwnd()
    }

    /// Copy the active page into the window.
    pub fn paint(&self) {
        let Some(page) = self.active.and_then(|t| self.pages.get(&t)) else { return };
        let size = self.inner.window.inner_size();
        let _ = self.inner.context.make_current();
        page.webview.paint();
        self.inner.context.prepare_for_rendering();
        if let Some(blit) = page.context.render_to_parent_callback() {
            let gl = self.inner.context.glow_gl_api();
            blit(&gl, Rect::new(Point2D::new(0, 0), Size2D::new(size.width as i32, size.height as i32)));
        }
        self.inner.context.present();
    }

    /// Input for the engine window. Returns a Velocity shortcut ("new", "close", ...) instead of
    /// passing the key to the page, as `PAGE_KEYS_JS` does for WebView2 pages.
    pub fn handle(&self, event: &WindowEvent) -> Option<&'static str> {
        let webview = self.active_webview()?.clone();
        self.input(webview, &self.cursor, || self.focus(), event)
    }

    /// `event` from a window of the engine, for `webview`; `focus` moves the keys to that window.
    fn input(&self, webview: WebView, cursor: &Cell<DevicePoint>, focus: impl Fn(), event: &WindowEvent) -> Option<&'static str> {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = DevicePoint::new(position.x as f32, position.y as f32);
                cursor.set(point);
                webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
            },
            WindowEvent::CursorLeft { .. } => {
                webview.notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
            },
            WindowEvent::MouseInput { state, button, .. } => {
                let action = if *state == ElementState::Pressed { MouseButtonAction::Down } else { MouseButtonAction::Up };
                if action == MouseButtonAction::Down {
                    focus(); // a click moves the keys to the page, as in any browser
                }
                let point = cursor.get();
                webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(action, mouse_button(*button), point.into())));
            },
            WindowEvent::MouseWheel { delta, .. } => {
                // tao counts wheel-up as positive; DOM wheel events count scrolling down as positive.
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => ((-dx * 76.0) as f64, (-dy * 76.0) as f64, WheelMode::DeltaLine),
                    MouseScrollDelta::PixelDelta(d) => (-d.x, -d.y, WheelMode::DeltaPixel),
                    _ => return None,
                };
                webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(WheelDelta { x, y, z: 0.0, mode }, cursor.get().into())));
            },
            WindowEvent::ModifiersChanged(state) => self.modifiers.set(modifiers(*state)),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    if let Some(shortcut) = shortcut(&event.logical_key, event.physical_key, self.modifiers.get()) {
                        match shortcut {
                            "back" => self.back(),
                            "forward" => self.forward(),
                            "reload" => self.reload(),
                            other => return Some(other),
                        }
                        return None;
                    }
                }
                webview.notify_input_event(InputEvent::Keyboard(keyboard_event(event, self.modifiers.get())));
            },
            WindowEvent::Focused(focused) => webview.set_focused(*focused),
            _ => {},
        }
        None
    }
}

/// Move child window `window` over `x`, `y` (physical pixels of the parent's client area) at client
/// size `size`, with corners rounded to `corner` (the ellipse diameter, 0 for square). Returns
/// whether its size changed.
fn place(window: &Window, x: i32, y: i32, size: PhysicalSize<u32>, corner: i32) -> bool {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::{ClientToScreen, CreateRectRgn, CreateRoundRectRgn, SetWindowRgn},
        UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER},
    };
    let resized = window.inner_size() != size;
    let hwnd = HWND(window.hwnd() as _);
    unsafe {
        // tao keeps an invisible resize frame around an undecorated window, so its client area
        // (where the page is drawn) is inset from the window: grow the window by that frame...
        let (mut window, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
        let _ = GetWindowRect(hwnd, &mut window);
        let _ = GetClientRect(hwnd, &mut client);
        let _ = ClientToScreen(hwnd, &mut origin);
        let (left, top) = (origin.x - window.left, origin.y - window.top);
        let right = window.right - (origin.x + client.right);
        let bottom = window.bottom - (origin.y + client.bottom);
        let (w, h) = (size.width as i32, size.height as i32);
        let _ = SetWindowPos(hwnd, None, x - left, y - top, w + left + right, h + top + bottom, SWP_NOZORDER | SWP_NOACTIVATE);
        // ...and show only the client area, rounded like a tab's card.
        let region = if corner > 0 {
            CreateRoundRectRgn(left, top, left + w + 1, top + h + 1, corner, corner)
        } else {
            CreateRectRgn(left, top, left + w, top + h)
        };
        SetWindowRgn(hwnd, Some(region), true); // the system owns the region from here on
    }
    resized
}

/// Drawing in a child window must not cover the sibling windows over it (the page over the UI).
fn clip_siblings(hwnd: isize) {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_STYLE, WS_CLIPSIBLINGS}};
    unsafe {
        let hwnd = HWND(hwnd as _);
        SetWindowLongPtrW(hwnd, GWL_STYLE, GetWindowLongPtrW(hwnd, GWL_STYLE) | WS_CLIPSIBLINGS.0 as isize);
    }
}

/// Stylo parses `mask-*` only with this pref (the patched servo-layout paints the masks), and
/// `zoom` (the tab UI's size setting; sites use it as in Chrome) only with the other. Stylo runs in
/// the content processes, so each sets them too.
pub fn set_style_prefs() {
    stylo_static_prefs::set_pref!("layout.css.masking.enabled", true);
    stylo_static_prefs::set_pref!("layout.css.zoom.enabled", true);
}

impl Inner {
    fn page(&self, builder: impl FnOnce(Rc<dyn RenderingContext>) -> WebViewBuilder) -> Page {
        let size = self.window.inner_size();
        let context = Rc::new(self.context.offscreen_context(size));
        let delegate = self.delegate.borrow().clone().expect("delegate set");
        let webview = builder(context.clone())
            .hidpi_scale_factor(Scale::new(self.window.scale_factor() as f32))
            .delegate(delegate)
            .user_content_manager(self.user_content.clone())
            .build();
        // Pages follow the Windows light or dark setting, as they do in WebView2.
        webview.notify_theme_change(match self.window.theme() {
            tao::window::Theme::Dark => servo::Theme::Dark,
            _ => servo::Theme::Light,
        });
        Page { webview, context }
    }

    fn tab_of(&self, webview: &WebView) -> Option<u32> {
        self.tabs.borrow().get(&webview.id()).copied()
    }
}

/// Servo's callbacks, turned into Velocity events. Holds the engine weakly: pages hold it.
struct Delegate(std::rc::Weak<Inner>);

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        if let Some(inner) = self.0.upgrade() {
            match inner.ui.get().filter(|ui| ui.is(&webview)) {
                Some(ui) => ui.window.request_redraw(),
                None => inner.window.request_redraw(),
            }
        }
    }

    /// The tab UI's page and the local files it shows; every other load goes on as usual.
    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        let Some(inner) = self.0.upgrade() else { return };
        let Some(ui) = inner.ui.get().filter(|ui| ui.is(&webview)) else { return };
        let url = load.request().url.as_url().clone();
        let Some(body) = ui.file(&url) else { return };
        let mut response = WebResourceResponse::new(url.clone());
        match &body {
            Some(_) => {
                let mut headers = http::HeaderMap::new();
                headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static(mime(url.path())));
                response = response.headers(headers);
            },
            None => response = response.status_code(http::StatusCode::NOT_FOUND),
        }
        let mut load = load.intercept(response);
        if let Some(body) = body {
            load.send_body_data(body);
        }
        load.finish();
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        let Some(inner) = self.0.upgrade() else { return };
        if let Some(tab) = inner.tab_of(&webview) {
            let _ = inner.proxy.send_event(UserEvent::UrlChanged(tab, url.to_string()));
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        let Some(inner) = self.0.upgrade() else { return };
        if let (Some(tab), Some(title)) = (inner.tab_of(&webview), title.filter(|t| !t.is_empty())) {
            let _ = inner.proxy.send_event(UserEvent::TitleChanged(tab, title));
        }
    }

    /// `window.open` and `target=_blank` open as tabs, keeping `window.opener` (OAuth popups need it).
    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        let Some(inner) = self.0.upgrade() else { return };
        let page = inner.page(|context| request.builder(context));
        inner.popups.borrow_mut().push(page);
        let _ = inner.proxy.send_event(UserEvent::EnginePopup);
    }

    fn notify_closed(&self, webview: WebView) {
        let Some(inner) = self.0.upgrade() else { return };
        if let Some(tab) = inner.tab_of(&webview) {
            let _ = inner.proxy.send_event(UserEvent::Ui(format!("close:{tab}")));
        }
    }

    fn notify_fullscreen_state_changed(&self, webview: WebView, on: bool) {
        let Some(inner) = self.0.upgrade() else { return };
        if let Some(tab) = inner.tab_of(&webview) {
            let _ = inner.proxy.send_event(UserEvent::Fullscreen(tab, on));
        }
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        eprintln!("page crashed: {reason}");
        let Some(inner) = self.0.upgrade() else { return };
        if let Some(tab) = inner.tab_of(&webview) {
            let _ = inner.proxy.send_event(UserEvent::Crashed(tab));
        }
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        let Some(inner) = self.0.upgrade() else { return };
        // shortcut: the common shapes only; others show the arrow. Upgrade with a full table.
        let icon = match cursor {
            Cursor::Pointer => CursorIcon::Hand,
            Cursor::Text | Cursor::VerticalText => CursorIcon::Text,
            Cursor::Wait => CursorIcon::Wait,
            Cursor::Progress => CursorIcon::Progress,
            Cursor::NotAllowed | Cursor::NoDrop => CursorIcon::NotAllowed,
            Cursor::Move | Cursor::AllScroll => CursorIcon::Move,
            Cursor::Crosshair => CursorIcon::Crosshair,
            _ => CursorIcon::Default,
        };
        let window = inner.ui.get().filter(|ui| ui.is(&webview)).map_or(&inner.window, |ui| &ui.window);
        window.set_cursor_visible(!matches!(cursor, Cursor::None));
        window.set_cursor_icon(icon);
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        // The tab UI's messages to Rust (UI_IPC_JS); a web page's console never gets here.
        if let (Some(inner), Some(msg)) = (self.0.upgrade(), message.strip_prefix('\u{1}')) {
            if let Some(ui) = inner.ui.get().filter(|ui| ui.is(&webview)) {
                if msg == "\u{1}ready" {
                    let pending = ui.pending.borrow_mut().take().unwrap_or_default();
                    for js in pending {
                        webview.evaluate_javascript(js, |_| {});
                    }
                } else {
                    let _ = inner.proxy.send_event(UserEvent::Ui(msg.to_string()));
                }
                return;
            }
        }
        if matches!(level, ConsoleLogLevel::Error) && std::env::var_os("VELOCITY_TRACE").is_some() {
            eprintln!("console.error: {message}");
        }
    }
}

/// An imported cookie as Servo's cookie type, or None if it has expired. A host starting with a dot
/// is a domain cookie (for its subdomains too); otherwise the cookie is for that host only.
fn engine_cookie(c: &crate::import::Cookie, now: f64) -> Option<cookie::Cookie<'static>> {
    use cookie::SameSite;
    if c.expires <= now {
        return None;
    }
    let mut builder = cookie::Cookie::build((c.name.clone(), c.value.clone()))
        .path(if c.path.is_empty() { "/".to_string() } else { c.path.clone() })
        .secure(c.secure)
        .http_only(c.http_only)
        .max_age(cookie::time::Duration::seconds((c.expires - now) as i64));
    // Firefox's 0 is "not set": SameSite=None would also need Secure, or the cookie is refused.
    builder = match c.same_site {
        2 => builder.same_site(SameSite::Strict),
        1 => builder.same_site(SameSite::Lax),
        _ => builder,
    };
    if let Some(domain) = c.host.strip_prefix('.') {
        builder = builder.domain(domain.to_string());
    }
    Some(builder.build())
}

/// Downloads on the engine (saved by its network layer) go into Velocity's downloads list, as
/// WebView2's do. Called on the network layer's threads.
fn follow_downloads(proxy: EventLoopProxy<UserEvent>, ids: Arc<Mutex<HashMap<u64, u32>>>) {
    use crate::downloads::Update;
    use net::download::DownloadEvent;
    let size = |total: Option<u64>| total.map_or(-1, |t| t as i64);
    net::download::set_observer(move |event| {
        let mut ids = ids.lock().unwrap();
        let update = match event {
            DownloadEvent::Started { id, url, path, total } => {
                let ours = crate::downloads::next_id();
                ids.insert(id, ours);
                // No tab: the engine's download page stays in the tab that asked for the file.
                Update::Started { id: ours, tab: 0, url, path: path.display().to_string(), total: size(total) }
            }
            DownloadEvent::Ask { id, url, path, total } => {
                let ours = crate::downloads::next_id();
                ids.insert(id, ours);
                Update::Ask { id: ours, tab: 0, url, path: path.display().to_string(), total: size(total) }
            }
            DownloadEvent::Progress { id, received, total } => {
                let Some(&ours) = ids.get(&id) else { return };
                Update::Progress { id: ours, received: received as i64, total: size(total) }
            }
            DownloadEvent::Finished { id, result } => {
                let Some(ours) = ids.remove(&id) else { return };
                let path = result.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                Update::Finished { id: ours, ok: result.is_ok(), path }
            }
        };
        let _ = proxy.send_event(UserEvent::Download(update));
    });
}

#[derive(Clone)]
struct Waker(EventLoopProxy<UserEvent>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.0.send_event(UserEvent::Engine);
    }
}

/// The shortcuts `PAGE_KEYS_JS` forwards from WebView2 pages, plus the navigation keys WebView2
/// handles itself.
fn shortcut(key: &TaoKey, code: KeyCode, m: Modifiers) -> Option<&'static str> {
    let ctrl = m.contains(Modifiers::CONTROL);
    let alt = m.contains(Modifiers::ALT);
    let shift = m.contains(Modifiers::SHIFT);
    match (key, ctrl, alt) {
        (TaoKey::Tab, true, false) => Some(if shift { "prev" } else { "next" }),
        (TaoKey::ArrowLeft, false, true) => Some("back"),
        (TaoKey::ArrowRight, false, true) => Some("forward"),
        (TaoKey::F5, _, _) => Some("reload"),
        // By physical key, so Ctrl+T works on any keyboard layout.
        (_, true, true) if code == KeyCode::KeyC => Some("compact"),
        (_, true, false) => match code {
            KeyCode::KeyT => Some("new"),
            KeyCode::KeyW => Some("close"),
            KeyCode::KeyL => Some("focus"),
            KeyCode::KeyJ => Some("downloads"),
            KeyCode::Comma => Some("settings"),
            KeyCode::KeyR => Some("reload"),
            _ => None,
        },
        _ => None,
    }
}

/// tao names keys and key codes as the W3C does ("Enter", "ArrowLeft", "KeyA"), which is what
/// keyboard-types parses, so a key converts through its name instead of a translation table.
fn keyboard_event(event: &tao::event::KeyEvent, modifiers: Modifiers) -> KeyboardEvent {
    let key = match &event.logical_key {
        TaoKey::Character(text) => Key::Character(text.to_string()),
        TaoKey::Space => Key::Character(" ".into()), // a character in the W3C list
        named => format!("{named:?}").parse().map_or(Key::Named(NamedKey::Unidentified), Key::Named),
    };
    let code = format!("{:?}", event.physical_key).parse().unwrap_or(Code::Unidentified);
    let location = match event.location {
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
        _ => Location::Standard,
    };
    let state = if event.state == ElementState::Pressed { KeyState::Down } else { KeyState::Up };
    KeyboardEvent::new_without_event(state, key, code, location, modifiers, event.repeat, false)
}

fn modifiers(state: ModifiersState) -> Modifiers {
    let mut m = Modifiers::empty();
    m.set(Modifiers::SHIFT, state.shift_key());
    m.set(Modifiers::CONTROL, state.control_key());
    m.set(Modifiers::ALT, state.alt_key());
    m.set(Modifiers::META, state.super_key());
    m
}

fn mouse_button(button: TaoButton) -> MouseButton {
    match button {
        TaoButton::Left => MouseButton::Primary,
        TaoButton::Right => MouseButton::Secondary,
        TaoButton::Middle => MouseButton::Auxiliary,
        _ => MouseButton::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_cookies() {
        let c = |host: &str, expires: f64, same_site: i64| crate::import::Cookie {
            name: "sid".into(),
            value: "1".into(),
            host: host.into(),
            path: "/".into(),
            expires,
            secure: true,
            http_only: true,
            same_site,
        };
        let cookie = engine_cookie(&c(".example.com", 2000.0, 1), 1000.0).expect("valid");
        assert_eq!(cookie.domain(), Some("example.com"));
        assert_eq!(cookie.same_site(), Some(cookie::SameSite::Lax));
        assert_eq!(cookie.max_age(), Some(cookie::time::Duration::seconds(1000)));
        assert!(cookie.http_only() == Some(true) && cookie.secure() == Some(true));
        let host_only = engine_cookie(&c("www.example.com", 2000.0, 0), 1000.0).unwrap();
        assert_eq!(host_only.domain(), None, "host-only");
        assert_eq!(host_only.same_site(), None, "Firefox's 0: not set");
        assert!(engine_cookie(&c(".example.com", 500.0, 0), 1000.0).is_none(), "expired");
    }

    #[test]
    fn ui_files() {
        let dir = std::env::temp_dir().join("velocity-bg");
        let files = vec![("bg.velocity.example".to_string(), dir.clone())];
        let get = |u: &str| local_file(&files, &Url::parse(u).unwrap());
        assert_eq!(get("https://bg.velocity.example/my%20bg.png?v=3"), Some(Some(dir.join("my bg.png"))));
        assert_eq!(get("https://other.example/bg.png"), None, "not a UI host");
        assert!(get("https://bg.velocity.example/%2e%2e/%2e%2e/x").unwrap().is_none_or(|p| p.starts_with(&dir) && !p.to_string_lossy().contains("..")), "no way out of the folder");
        assert!(get("https://bg.velocity.example/..%5c..%5cx").unwrap().is_none(), "nor with backslashes");
    }

    #[test]
    fn shortcuts() {
        let ctrl = Modifiers::CONTROL;
        assert_eq!(shortcut(&TaoKey::Character("t"), KeyCode::KeyT, ctrl), Some("new"));
        // Slovak layout: the key labelled T still is KeyT.
        assert_eq!(shortcut(&TaoKey::Character("ť"), KeyCode::KeyT, ctrl), Some("new"));
        assert_eq!(shortcut(&TaoKey::Tab, KeyCode::Tab, ctrl | Modifiers::SHIFT), Some("prev"));
        assert_eq!(shortcut(&TaoKey::ArrowLeft, KeyCode::ArrowLeft, Modifiers::ALT), Some("back"));
        assert_eq!(shortcut(&TaoKey::Character("t"), KeyCode::KeyT, Modifiers::empty()), None, "typing t");
        assert_eq!(shortcut(&TaoKey::Character("c"), KeyCode::KeyC, ctrl), None, "copy goes to the page");
    }
}
