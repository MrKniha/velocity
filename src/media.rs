//! New tab background media and Wallpaper Engine import.
//!
//! The chosen file is copied into `<data>/background/bg.<ext>` and that one folder is mapped to a virtual
//! https host in the webviews that show it, so WebView2 streams it (video included) without loading it into memory.

use std::fs;
use std::path::{Path, PathBuf};

use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_3, COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use wry::{WebView, WebViewExtWindows};

use crate::settings::MEDIA_EXTS;

pub const BG_HOST: &str = "bg.velocity.example";

/// Wallpaper Engine's Steam app id; its workshop items live in `steamapps/workshop/content/431960/<id>`.
const WE_APP: &str = "431960";

pub fn we_host(library: usize) -> String {
    format!("we{library}.velocity.example")
}

pub fn bg_dir(data: &Path) -> PathBuf {
    data.join("background")
}

/// Copy `src` into the background folder as `bg.<ext>`, replacing the previous background.
/// Returns the new file name, or None for unsupported types or failed copies.
pub fn install(data: &Path, src: &Path) -> Option<String> {
    let ext = src.extension()?.to_str()?.to_ascii_lowercase();
    if !MEDIA_EXTS.contains(&ext.as_str()) {
        return None;
    }
    let dir = bg_dir(data);
    fs::create_dir_all(&dir).ok()?;
    let name = format!("bg.{ext}");
    let tmp = dir.join("incoming.tmp");
    fs::copy(src, &tmp).ok()?; // copy first, so a failed copy keeps the old background
    clear(data);
    fs::rename(&tmp, dir.join(&name)).ok()?;
    Some(name)
}

pub fn clear(data: &Path) {
    if let Ok(entries) = fs::read_dir(bg_dir(data)) {
        for e in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("bg.")) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Map `folder` to `https://<host>/` in this webview; pages may show files from it as img/video.
// shortcut: DENY_CORS lets any page in this webview embed (not read) these files, so a site opened later in a
// former new-tab webview could tell a wallpaper exists; upgrade by serving the new tab from its own mapped origin with DENY.
pub fn map(view: &WebView, host: &str, folder: &Path) {
    if let Ok(wv) = view.webview().cast::<ICoreWebView2_3>() {
        let _ = fs::create_dir_all(folder);
        unsafe {
            let _ = wv.SetVirtualHostNameToFolderMapping(
                &HSTRING::from(host),
                &HSTRING::from(folder.as_os_str()),
                COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
            );
        }
    }
}

pub struct Wallpaper {
    pub library: usize,
    pub id: String,
    pub title: String,
    pub kind: String,
    /// File (relative to the item folder) Velocity can show: the video itself, or the preview image
    /// for scene/web wallpapers, which only Wallpaper Engine can render.
    pub media: String,
    pub preview: String,
}

/// Workshop folders of Wallpaper Engine in every Steam library on this machine.
pub fn workshop_dirs() -> Vec<PathBuf> {
    let steam = steam_path().unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)\Steam"));
    let mut libs = vec![steam.clone()];
    if let Ok(vdf) = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) {
        libs.extend(vdf_paths(&vdf).into_iter().map(PathBuf::from));
    }
    libs.dedup();
    let mut dirs: Vec<PathBuf> = libs
        .into_iter()
        .map(|l| l.join("steamapps").join("workshop").join("content").join(WE_APP))
        .filter(|d| d.is_dir())
        .collect();
    dirs.dedup();
    dirs
}

/// Every subscribed wallpaper that has something Velocity can show.
pub fn scan(dirs: &[PathBuf]) -> Vec<Wallpaper> {
    let mut out = Vec::new();
    for (library, dir) in dirs.iter().enumerate() {
        let Ok(items) = fs::read_dir(dir) else { continue };
        for item in items.flatten() {
            let Ok(json) = fs::read_to_string(item.path().join("project.json")) else { continue };
            let id = item.file_name().to_string_lossy().into_owned();
            let kind = json_str(&json, "type").unwrap_or_default().to_ascii_lowercase();
            let file = json_str(&json, "file").unwrap_or_default();
            let preview = json_str(&json, "preview").unwrap_or_default();
            let usable = |f: &str| {
                let ext = Path::new(f).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                !f.is_empty() && !f.contains("..") && MEDIA_EXTS.contains(&ext.as_str()) && item.path().join(f).is_file()
            };
            let media = if kind == "video" && usable(&file) { file } else if usable(&preview) { preview.clone() } else { continue };
            let title = json_str(&json, "title").unwrap_or_else(|| id.clone());
            out.push(Wallpaper { library, id, title, kind, media, preview: if usable(&preview) { preview } else { String::new() } });
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

fn steam_path() -> Option<PathBuf> {
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    let key: Vec<u16> = "Software\\Valve\\Steam\0".encode_utf16().collect();
    let value: Vec<u16> = "SteamPath\0".encode_utf16().collect();
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if ok.is_err() {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
    Some(PathBuf::from(s.replace('/', "\\")))
}

/// `"path"  "D:\\SteamLibrary"` lines of Steam's libraryfolders.vdf.
fn vdf_paths(vdf: &str) -> Vec<String> {
    vdf.lines()
        .filter(|l| l.trim_start().starts_with("\"path\""))
        .filter_map(|l| l.rsplit('"').nth(1).map(|p| p.replace("\\\\", "\\")))
        .collect()
}

/// The string value of `"key": "..."` in a flat JSON object (enough for project.json; no full parser needed).
fn json_str(json: &str, key: &str) -> Option<String> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = json[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('?'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_json() {
        let j = r#"{ "file" : "scene.pkg", "preview": "preview.gif", "title": "Neon \"City\" \u00e9", "type": "Scene" }"#;
        assert_eq!(json_str(j, "file").as_deref(), Some("scene.pkg"));
        assert_eq!(json_str(j, "title").as_deref(), Some("Neon \"City\" é"));
        assert_eq!(json_str(j, "type").as_deref(), Some("Scene"));
        assert_eq!(json_str(j, "missing"), None);
    }

    #[test]
    fn library_folders() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}";
        assert_eq!(vdf_paths(vdf), vec![r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]);
    }

    #[test]
    fn scan_workshop() {
        let dir = std::env::temp_dir().join("velocity-we-test");
        let _ = fs::remove_dir_all(&dir);
        let item = |id: &str, json: &str, files: &[&str]| {
            let d = dir.join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("project.json"), json).unwrap();
            for f in files {
                fs::write(d.join(f), b"x").unwrap();
            }
        };
        item("1", r#"{"type":"video","file":"clip.mp4","preview":"preview.jpg","title":"Beach"}"#, &["clip.mp4", "preview.jpg"]);
        item("2", r#"{"type":"scene","file":"scene.pkg","preview":"preview.gif","title":"aurora"}"#, &["scene.pkg", "preview.gif"]);
        item("3", r#"{"type":"web","file":"index.html","title":"No preview"}"#, &["index.html"]);
        item("4", r#"{"type":"video","file":"../../evil.mp4","preview":"p.png","title":"Escape"}"#, &["p.png"]);
        let w = scan(&[dir.clone()]);
        let got: Vec<(&str, &str)> = w.iter().map(|w| (w.title.as_str(), w.media.as_str())).collect();
        assert_eq!(got, vec![("aurora", "preview.gif"), ("Beach", "clip.mp4"), ("Escape", "p.png")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_and_replaces() {
        let data = std::env::temp_dir().join("velocity-media-test");
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data).unwrap();
        let exe = data.join("x.exe");
        fs::write(&exe, b"no").unwrap();
        assert_eq!(install(&data, &exe), None, "executables are never installed");
        let gif = data.join("a.GIF");
        fs::write(&gif, b"GIF89a").unwrap();
        assert_eq!(install(&data, &gif).as_deref(), Some("bg.gif"));
        let png = data.join("b.png");
        fs::write(&png, b"png").unwrap();
        assert_eq!(install(&data, &png).as_deref(), Some("bg.png"));
        assert!(!bg_dir(&data).join("bg.gif").exists(), "the previous background is removed");
        let _ = fs::remove_dir_all(&data);
    }
}
