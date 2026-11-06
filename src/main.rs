// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
    window::{ResizeDirection, Window, WindowBuilder},
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::{
        Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE},
        Gdi::{CreateRoundRectRgn, SetWindowRgn},
    },
    UI::Controls::MARGINS,
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    WebViewExtWindows,
};

const SIDEBAR_WIDTH: f64 = 240.0;
/// Space around the page card; also where the window's own resize borders are hit.
const GAP: f64 = 8.0;
const CARD_RADIUS: f64 = 10.0;
const HOME: &str = "https://duckduckgo.com";
const DEFAULT_ESSENTIALS: &[&str] = &["https://github.com", "https://www.youtube.com", "https://www.wikipedia.org"];

/// Runs in every page: forwards browser shortcuts, since keys pressed in a page never reach the sidebar.
const PAGE_KEYS_JS: &str = r#"
addEventListener('keydown', e => {
  const k = e.ctrlKey && !e.altKey ? ({t: 'new', w: 'close', l: 'focus', Tab: e.shiftKey ? 'prev' : 'next'})[e.key] : null;
  if (k) { e.preventDefault(); e.stopPropagation(); window.ipc.postMessage('key:' + k); }
}, true);
"#;

enum UserEvent {
    Ui(String),
    PageKey(String),
    NewTab(String),
    UrlChanged(u32, String),
    TitleChanged(u32, String),
}

struct Tab {
    id: u32,
    title: String,
    url: String,
    view: WebView,
}

struct Browser {
    window: Window,
    ui: WebView,
    ctx: WebContext,
    dir: PathBuf,
    proxy: EventLoopProxy<UserEvent>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    essentials: Vec<String>,
}

impl Browser {
    fn open_tab(&mut self, url: &str) {
        let id = self.next_id;
        self.next_id += 1;
        let (p1, p2, p3, p4) = (self.proxy.clone(), self.proxy.clone(), self.proxy.clone(), self.proxy.clone());
        let view = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_url(url)
            .with_bounds(content_bounds(&self.window))
            .with_browser_extensions_enabled(true)
            .with_extensions_path(self.dir.join("extensions"))
            .with_devtools(true)
            .with_initialization_script(PAGE_KEYS_JS)
            // Pages can post anything, so only the "key:" shortcuts are accepted from them.
            .with_ipc_handler(move |req| {
                if let Some(k) = req.body().strip_prefix("key:") {
                    let _ = p1.send_event(UserEvent::PageKey(k.to_string()));
                }
            })
            .with_on_page_load_handler(move |ev, url| {
                if let PageLoadEvent::Started = ev {
                    let _ = p2.send_event(UserEvent::UrlChanged(id, url));
                }
            })
            .with_document_title_changed_handler(move |title| {
                let _ = p3.send_event(UserEvent::TitleChanged(id, title));
            })
            // shortcut: popups open as plain tabs, so window.opener is lost (breaks some OAuth popups); upgrade via NewWindowResponse::Create.
            .with_new_window_req_handler(move |url, _| {
                let _ = p4.send_event(UserEvent::NewTab(url));
                NewWindowResponse::Deny
            })
            .build_as_child(&self.window)
            .expect("create tab webview");
        round_corners(&view, &self.window);
        self.tabs.push(Tab { id, title: "New tab".into(), url: url.into(), view });
        self.activate(self.tabs.len() - 1);
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index); // dropping the WebView destroys it
        if self.tabs.is_empty() {
            self.open_tab(HOME);
            return;
        }
        self.activate(active_after_close(index, self.active, self.tabs.len()));
    }

    fn activate(&mut self, index: usize) {
        self.active = index;
        for (i, t) in self.tabs.iter().enumerate() {
            let _ = t.view.set_visible(i == index);
        }
        self.sync_ui();
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn current(&self) -> &WebView {
        &self.tabs[self.active].view
    }

    /// Essentials switch to an open tab on the same site instead of opening duplicates.
    fn open_essential(&mut self, index: usize) {
        let Some(url) = self.essentials.get(index).cloned() else { return };
        match self.tabs.iter().position(|t| host(&t.url) == host(&url)) {
            Some(i) => self.activate(i),
            None => self.open_tab(&url),
        }
    }

    fn toggle_essential(&mut self, url: &str) {
        match self.essentials.iter().position(|e| host(e) == host(url)) {
            Some(i) => {
                self.essentials.remove(i);
            }
            None if host(url).is_empty() => return,
            None => self.essentials.push(url.to_string()),
        }
        save_essentials(&self.dir, &self.essentials);
        self.sync_ui();
    }

    /// Push the full state to the sidebar; small enough that diffing isn't worth it.
    fn sync_ui(&self) {
        let tabs: Vec<String> = self
            .tabs
            .iter()
            .map(|t| format!("{{\"id\":{},\"title\":{},\"url\":{}}}", t.id, js_str(&t.title), js_str(&t.url)))
            .collect();
        let essentials: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let cur = &self.tabs[self.active];
        self.window.set_title(&format!("{} - Zenith", cur.title));
        let _ = self.ui.evaluate_script(&format!(
            "render([{}],{},[{}])",
            tabs.join(","),
            cur.id,
            essentials.join(",")
        ));
    }

    fn resize(&self) {
        let _ = self.ui.set_bounds(sidebar_bounds(&self.window));
        let b = content_bounds(&self.window);
        for t in &self.tabs {
            let _ = t.view.set_bounds(b);
            round_corners(&t.view, &self.window);
        }
    }

    fn shortcut(&mut self, key: &str) {
        let n = self.tabs.len();
        match key {
            "new" => self.open_tab(HOME),
            "close" => self.close_tab(self.active),
            "next" => self.activate((self.active + 1) % n),
            "prev" => self.activate((self.active + n - 1) % n),
            "focus" => {
                let _ = self.ui.focus();
                let _ = self.ui.evaluate_script("focusUrl()");
            }
            _ => {}
        }
    }

    /// Returns false when the window should close.
    fn handle_ui(&mut self, msg: &str) -> bool {
        let (cmd, arg) = msg.split_once(':').unwrap_or((msg, ""));
        let tab = arg.parse::<u32>().ok().and_then(|id| self.index_of(id));
        let _ = match (cmd, tab) {
            ("go", _) => self.current().load_url(&to_url(arg)),
            ("back", _) => self.current().evaluate_script("history.back()"),
            ("forward", _) => self.current().evaluate_script("history.forward()"),
            ("reload", _) => self.current().reload(),
            ("devtools", _) => Ok(self.current().open_devtools()),
            ("select", Some(i)) => Ok(self.activate(i)),
            ("close", Some(i)) => Ok(self.close_tab(i)),
            ("pin", Some(i)) => Ok(self.toggle_essential(&self.tabs[i].url.clone())),
            ("unpin", _) => Ok(self.toggle_essential(arg)),
            ("essential", _) => Ok(arg.parse().map(|i| self.open_essential(i)).unwrap_or(())),
            ("key", _) => Ok(self.shortcut(arg)),
            ("win", _) => match arg {
                "min" => Ok(self.window.set_minimized(true)),
                "max" => Ok(self.window.set_maximized(!self.window.is_maximized())),
                "close" => return false,
                _ => Ok(()),
            },
            ("drag", _) => Ok(drop(self.window.drag_window())),
            ("resize", _) => Ok(drop(self.window.drag_resize_window(ResizeDirection::West))),
            _ => Ok(()),
        };
        true
    }
}

/// Which tab is active after closing `closed`: keep the same tab, or take the neighbour that slid into its place.
fn active_after_close(closed: usize, active: usize, remaining: usize) -> usize {
    if closed < active || active >= remaining { active.saturating_sub(1) } else { active }
}

/// "https://www.a.com:8080/x" -> "www.a.com:8080"; empty for URLs without a host.
fn host(url: &str) -> &str {
    url.split_once("://").map_or("", |(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("zenith")
}

fn load_essentials(dir: &PathBuf) -> Vec<String> {
    match std::fs::read_to_string(dir.join("essentials.txt")) {
        Ok(s) => s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        Err(_) => DEFAULT_ESSENTIALS.iter().map(|s| s.to_string()).collect(),
    }
}

fn save_essentials(dir: &PathBuf, essentials: &[String]) {
    let _ = std::fs::write(dir.join("essentials.txt"), essentials.join("\n"));
}

/// Turn address-bar input into a URL: keep URLs, add https:// to hosts, search everything else.
fn to_url(input: &str) -> String {
    let s = input.trim();
    if s.contains("://") || s.starts_with("about:") {
        s.to_string()
    } else if !s.contains(' ') && (s.contains('.') || s.starts_with("localhost")) {
        format!("https://{s}")
    } else {
        let q: String = s.bytes().map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        }).collect();
        format!("https://duckduckgo.com/?q={q}")
    }
}

fn logical_size(window: &Window) -> (f64, f64) {
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    (s.width, s.height)
}

fn sidebar_bounds(window: &Window) -> Rect {
    let (_, h) = logical_size(window);
    Rect { position: LogicalPosition::new(0.0, 0.0).into(), size: LogicalSize::new(SIDEBAR_WIDTH, h).into() }
}

fn content_bounds(window: &Window) -> Rect {
    let (w, h) = logical_size(window);
    Rect {
        position: LogicalPosition::new(SIDEBAR_WIDTH, GAP).into(),
        size: LogicalSize::new((w - SIDEBAR_WIDTH - GAP).max(1.0), (h - 2.0 * GAP).max(1.0)).into(),
    }
}

/// Clip the page's host window to a rounded rect so it reads as a floating card.
// shortcut: window regions are not anti-aliased, so the corners are slightly jagged; upgrade with DirectComposition hosting.
fn round_corners(view: &WebView, window: &Window) {
    let scale = window.scale_factor();
    let size = content_bounds(window).size.to_physical::<i32>(scale);
    let r = (CARD_RADIUS * 2.0 * scale) as i32;
    unsafe {
        let mut hwnd = HWND::default();
        if view.controller().ParentWindow(&mut hwnd).is_ok() {
            let rgn = CreateRoundRectRgn(0, 0, size.width + 1, size.height + 1, r, r);
            SetWindowRgn(hwnd, Some(rgn), true); // the system owns rgn from here on
        }
    }
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Zenith")
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_transparent(true)
        .build(&event_loop)
        .expect("window");
    // Blurred desktop shows through the sidebar and the gaps around the page; no-op where unsupported.
    // The system backdrop only paints where the DWM frame reaches, so extend it over the whole borderless window.
    unsafe {
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let hwnd = HWND(window.hwnd() as _);
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        // Dark acrylic; the light one washes out the sidebar text. (tao's with_theme(Dark) starts this window minimized.)
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
    }
    if let Err(e) = window_vibrancy::apply_acrylic(&window, Some((20, 18, 24, 90))) {
        eprintln!("acrylic unavailable: {e}");
    }

    let dir = data_dir();
    std::fs::create_dir_all(dir.join("extensions")).ok();
    // One shared profile = one WebView2 process tree; all webviews must then use the same extension settings.
    let mut ctx = WebContext::new(Some(dir.join("profile")));

    let proxy = event_loop.create_proxy();
    let ui_proxy = proxy.clone();
    let ui = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("ui.html"))
        .with_bounds(sidebar_bounds(&window))
        .with_transparent(true)
        .with_browser_extensions_enabled(true)
        .with_ipc_handler(move |req| {
            let _ = ui_proxy.send_event(UserEvent::Ui(req.body().clone()));
        })
        .build_as_child(&window)?;

    let essentials = load_essentials(&dir);
    let mut b = Browser { window, ui, ctx, dir, proxy, tabs: Vec::new(), active: 0, next_id: 1, essentials };
    b.open_tab(HOME);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => b.resize(),
            Event::UserEvent(UserEvent::Ui(msg)) => {
                if !b.handle_ui(&msg) {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::PageKey(k)) => b.shortcut(&k),
            Event::UserEvent(UserEvent::NewTab(url)) => b.open_tab(&url),
            Event::UserEvent(UserEvent::UrlChanged(id, url)) => {
                if let Some(i) = b.index_of(id) {
                    b.tabs[i].url = url;
                    b.sync_ui();
                }
            }
            Event::UserEvent(UserEvent::TitleChanged(id, title)) => {
                if let Some(i) = b.index_of(id) {
                    b.tabs[i].title = title;
                    b.sync_ui();
                }
            }
            _ => {}
        }
    });
}

/// Minimal JS string literal escape for passing page data into the UI.
fn js_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '<' => out.push_str("\\u003c"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_bar_input() {
        assert_eq!(to_url("https://a.com/x"), "https://a.com/x");
        assert_eq!(to_url("example.com"), "https://example.com");
        assert_eq!(to_url("localhost:3000"), "https://localhost:3000");
        assert_eq!(to_url("rust lang"), "https://duckduckgo.com/?q=rust+lang");
        assert_eq!(to_url("a&b"), "https://duckduckgo.com/?q=a%26b");
        assert_eq!(js_str("a\"b</script>"), "\"a\\\"b\\u003c/script>\"");
    }

    #[test]
    fn closing_tabs() {
        assert_eq!(active_after_close(1, 1, 2), 1); // middle active closed: right neighbour
        assert_eq!(active_after_close(2, 2, 2), 1); // last active closed: left neighbour
        assert_eq!(active_after_close(0, 2, 2), 1); // tab before active closed: index shifts
        assert_eq!(active_after_close(2, 0, 2), 0); // tab after active closed: unchanged
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.a.com:8080/x?y"), "www.a.com:8080");
        assert_eq!(host("https://a.com"), "a.com");
        assert_eq!(host("https://a.com#top"), "a.com");
        assert_eq!(host("about:blank"), "");
    }
}
