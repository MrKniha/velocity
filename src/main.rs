// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod settings;

use std::path::{Path, PathBuf};

use settings::{Settings, ENGINES};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
    window::{Icon, ResizeDirection, Window, WindowBuilder},
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::{
        Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE},
        Gdi::{CreateRoundRectRgn, SetWindowRgn},
    },
    UI::WindowsAndMessaging::{SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    WebViewExtWindows,
};

const APP: &str = "Velocity";
/// Marker for the built-in new-tab page (pages/newtab.html); never sent to the network.
const NEW_TAB: &str = "velocity:newtab";
const DEFAULT_ESSENTIALS: &[&str] = &["https://github.com", "https://www.youtube.com", "https://www.wikipedia.org"];
/// Width of the hover strip that reveals the sidebar in compact mode.
const PEEK_STRIP: f64 = 6.0;

/// Runs in every page: forwards browser shortcuts, since keys pressed in a page never reach the sidebar.
const PAGE_KEYS_JS: &str = r#"
addEventListener('keydown', e => {
  const k = !e.ctrlKey ? null : e.altKey ? ({c: 'compact'})[e.key]
    : ({t: 'new', w: 'close', l: 'focus', ',': 'settings', Tab: e.shiftKey ? 'prev' : 'next'})[e.key];
  if (k) { e.preventDefault(); e.stopPropagation(); window.ipc.postMessage('key:' + k); }
}, true);
"#;

enum UserEvent {
    Ui(String),
    Setting(String),
    PageKey(String),
    PageNav(u32, String),
    NewTab(String),
    UrlChanged(u32, String),
    TitleChanged(u32, String),
}

struct Tab {
    id: u32,
    title: String,
    url: String, // empty while the tab shows the new-tab page
    view: WebView,
    unseen: bool, // navigated while in the background; marked until visited
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
    settings: Settings,
    settings_view: Option<WebView>,
    peeking: bool,
}

impl Browser {
    /// Data the built-in pages read at load: settings, essentials and the option lists.
    fn page_init_js(&self) -> String {
        let essentials: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let engines: Vec<String> = ENGINES.iter().map(|(k, name, _)| format!("['{k}','{name}']")).collect();
        format!(
            "window.VELOCITY={};window.ESSENTIALS=[{}];window.ENGINES=[{}];",
            self.settings.to_js(),
            essentials.join(","),
            engines.join(",")
        )
    }

    fn open_tab(&mut self, url: &str) {
        let id = self.next_id;
        self.next_id += 1;
        let (p1, p2, p3, p4) = (self.proxy.clone(), self.proxy.clone(), self.proxy.clone(), self.proxy.clone());
        let new_tab = url == NEW_TAB;
        let init = self.page_init_js();
        let bounds = self.content_bounds();
        let builder = WebViewBuilder::new_with_web_context(&mut self.ctx);
        let builder = if new_tab {
            builder.with_html(include_str!("pages/newtab.html")).with_background_color(self.settings.card_rgba())
        } else {
            builder.with_url(url)
        };
        let view = builder
            .with_bounds(bounds)
            .with_browser_extensions_enabled(true)
            .with_extensions_path(self.dir.join("extensions"))
            .with_devtools(true)
            .with_initialization_script(PAGE_KEYS_JS)
            // Only the new-tab page needs this, but it holds nothing a site couldn't read elsewhere.
            .with_initialization_script_for_main_only(if new_tab { init } else { String::new() }, true)
            // Pages can post anything: only shortcuts and "navigate this same tab" are accepted from them.
            .with_ipc_handler(move |req| {
                let body = req.body();
                if let Some(k) = body.strip_prefix("key:") {
                    let _ = p1.send_event(UserEvent::PageKey(k.to_string()));
                } else if let Some(q) = body.strip_prefix("nav:") {
                    let _ = p1.send_event(UserEvent::PageNav(id, q.to_string()));
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
        round_corners(&view, &self.window, &self.settings, bounds);
        let shown_url = if new_tab { String::new() } else { url.to_string() };
        self.tabs.push(Tab { id, title: "New tab".into(), url: shown_url, view, unseen: false });
        self.activate(self.tabs.len() - 1);
        if new_tab {
            let _ = self.current().focus(); // type into the centred search bar straight away
        }
    }

    /// Track a tab's URL; leaving the new-tab page restores the white default sites expect.
    fn set_url(&mut self, index: usize, url: String) {
        let background = index != self.active;
        let tab = &mut self.tabs[index];
        if url.starts_with("http") {
            if tab.url.is_empty() {
                let _ = tab.view.set_background_color((255, 255, 255, 255));
            }
            tab.url = url;
            tab.unseen |= background;
        }
        self.sync_ui();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index); // dropping the WebView destroys it
        if self.tabs.is_empty() {
            self.open_tab(NEW_TAB);
            return;
        }
        self.activate(active_after_close(index, self.active, self.tabs.len()));
    }

    fn activate(&mut self, index: usize) {
        self.active = index;
        self.tabs[index].unseen = false;
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
            .map(|t| {
                format!(
                    "{{\"id\":{},\"title\":{},\"url\":{},\"unseen\":{}}}",
                    t.id, js_str(&t.title), js_str(&t.url), t.unseen
                )
            })
            .collect();
        let essentials: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let cur = &self.tabs[self.active];
        self.window.set_title(&format!("{} - {APP}", cur.title));
        let _ = self.ui.evaluate_script(&format!(
            "render([{}],{},[{}],{})",
            tabs.join(","),
            cur.id,
            essentials.join(","),
            self.settings.to_js()
        ));
    }

    fn sidebar_shown(&self) -> bool {
        !self.settings.compact || self.peeking
    }

    fn sidebar_bounds(&self) -> Rect {
        let (w, h) = logical_size(&self.window);
        rect(layout_rects(w, h, &self.settings).0)
    }

    fn content_bounds(&self) -> Rect {
        let (w, h) = logical_size(&self.window);
        rect(layout_rects(w, h, &self.settings).1)
    }

    fn layout(&self) {
        let _ = self.ui.set_bounds(self.sidebar_bounds());
        let _ = self.ui.set_visible(self.sidebar_shown());
        if self.peeking {
            bring_to_top(&self.ui); // in compact mode the sidebar overlays the page
        }
        let b = self.content_bounds();
        for view in self.tabs.iter().map(|t| &t.view).chain(self.settings_view.as_ref()) {
            let _ = view.set_bounds(b);
            round_corners(view, &self.window, &self.settings, b);
        }
    }

    fn set_peek(&mut self, peek: bool) {
        if self.settings.compact && self.peeking != peek {
            self.peeking = peek;
            self.layout();
        }
    }

    fn toggle_settings(&mut self) {
        if self.settings_view.take().is_some() {
            return; // dropping the WebView closes the panel
        }
        let proxy = self.proxy.clone();
        let bounds = self.content_bounds();
        let init = self.page_init_js();
        let view = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_html(include_str!("pages/settings.html"))
            .with_background_color(self.settings.card_rgba())
            .with_bounds(bounds)
            .with_browser_extensions_enabled(true)
            .with_initialization_script(init)
            // This view only ever shows our own page, which is what makes its messages trustworthy.
            .with_navigation_handler(|url| !url.starts_with("http"))
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::Setting(req.body().clone()));
            })
            .build_as_child(&self.window)
            .expect("create settings webview");
        round_corners(&view, &self.window, &self.settings, bounds);
        let _ = view.focus();
        self.settings_view = Some(view);
    }

    fn handle_setting(&mut self, msg: &str) {
        if msg == "close" {
            self.settings_view = None;
            return;
        }
        if msg == "reset" {
            self.settings = Settings::default();
        } else if let Some((k, v)) = msg.strip_prefix("set:").and_then(|kv| kv.split_once('=')) {
            if !self.settings.set(k, v) {
                return;
            }
        } else {
            return;
        }
        self.settings.save(&self.dir);
        self.apply_settings();
    }

    /// Re-apply everything that depends on settings, live.
    fn apply_settings(&mut self) {
        let (r, g, b) = self.settings.frame_rgb();
        self.window.set_background_color(Some((r, g, b, 255)));
        let _ = self.ui.set_background_color((r, g, b, 255));
        if !self.settings.compact {
            self.peeking = false;
        }
        self.layout();
        self.sync_ui();
        let js = format!("applySettings({})", self.settings.to_js());
        for t in self.tabs.iter().filter(|t| t.url.is_empty()) {
            let _ = t.view.evaluate_script(&js);
        }
        if let Some(v) = &self.settings_view {
            let _ = v.evaluate_script(&js);
        }
    }

    fn shortcut(&mut self, key: &str) {
        let n = self.tabs.len();
        match key {
            "new" => self.open_tab(NEW_TAB),
            "close" => self.close_tab(self.active),
            "next" => self.activate((self.active + 1) % n),
            "prev" => self.activate((self.active + n - 1) % n),
            "settings" => self.toggle_settings(),
            "compact" => {
                let next = (!self.settings.compact).to_string();
                self.handle_setting(&format!("set:compact={next}"));
            }
            "focus" => {
                self.set_peek(true);
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
            ("go", _) => self.current().load_url(&to_url(arg, self.settings.search_prefix())),
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
            ("unpeek", _) => Ok(self.set_peek(false)),
            ("win", _) => match arg {
                "min" => Ok(self.window.set_minimized(true)),
                "max" => Ok(self.window.set_maximized(!self.window.is_maximized())),
                "close" => return false,
                _ => Ok(()),
            },
            ("drag", _) => Ok(drop(self.window.drag_window())),
            ("resize", _) => {
                let s = &self.settings;
                let dir = if s.tabs_top { ResizeDirection::North } else if s.sidebar_right { ResizeDirection::East } else { ResizeDirection::West };
                Ok(drop(self.window.drag_resize_window(dir)))
            }
            _ => Ok(()),
        };
        true
    }

    /// Compact mode: the cursor reaching the tab UI's edge of the window reveals it.
    fn cursor_moved(&mut self, x_physical: f64, y_physical: f64) {
        let s = &self.settings;
        if !s.compact || self.peeking {
            return;
        }
        let (w, _) = logical_size(&self.window);
        let (x, y) = (x_physical / self.window.scale_factor(), y_physical / self.window.scale_factor());
        let edge = s.gap as f64 + PEEK_STRIP;
        let hit = if s.tabs_top { y <= edge } else if s.sidebar_right { x >= w - edge } else { x <= edge };
        if hit {
            self.set_peek(true);
        }
    }
}

/// Height of the horizontal tab bar (tab row + toolbar row) when tabs are on top.
const TOP_BAR: f64 = 80.0;

type Xywh = (f64, f64, f64, f64);

/// Where the tab UI and the page card go, in logical px, for a `w` x `h` window.
/// The tab UI's side of the window holds the UI itself, or in compact mode a strip that reveals it.
fn layout_rects(w: f64, h: f64, s: &Settings) -> (Xywh, Xywh) {
    let gap = s.gap as f64;
    if s.tabs_top {
        let top = if s.compact { gap.max(PEEK_STRIP) } else { TOP_BAR };
        ((0.0, 0.0, w, TOP_BAR), (gap, top, (w - 2.0 * gap).max(1.0), (h - top - gap).max(1.0)))
    } else {
        let sw = s.sidebar_width as f64;
        let side = if s.compact { gap.max(PEEK_STRIP) } else { sw };
        let (left, right) = if s.sidebar_right { (gap, side) } else { (side, gap) };
        let ui_x = if s.sidebar_right { w - sw } else { 0.0 };
        ((ui_x, 0.0, sw, h), (left, gap, (w - left - right).max(1.0), (h - 2.0 * gap).max(1.0)))
    }
}

fn rect((x, y, w, h): Xywh) -> Rect {
    Rect { position: LogicalPosition::new(x, y).into(), size: LogicalSize::new(w, h).into() }
}

/// Which tab is active after closing `closed`: keep the same tab, or take the neighbour that slid into its place.
fn active_after_close(closed: usize, active: usize, remaining: usize) -> usize {
    if closed < active || active >= remaining { active.saturating_sub(1) } else { active }
}

/// "https://www.a.com:8080/x" -> "www.a.com:8080"; empty for URLs without a host.
fn host(url: &str) -> &str {
    url.split_once("://").map_or("", |(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
}

/// %LOCALAPPDATA%\velocity, carrying over the profile from the app's earlier name.
fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("velocity");
    let old = base.join("zenith");
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

fn load_essentials(dir: &Path) -> Vec<String> {
    match std::fs::read_to_string(dir.join("essentials.txt")) {
        Ok(s) => s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        Err(_) => DEFAULT_ESSENTIALS.iter().map(|s| s.to_string()).collect(),
    }
}

fn save_essentials(dir: &Path, essentials: &[String]) {
    let _ = std::fs::write(dir.join("essentials.txt"), essentials.join("\n"));
}

/// Turn address-bar input into a URL: keep URLs, add https:// to hosts, search everything else.
fn to_url(input: &str, search_prefix: &str) -> String {
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
        format!("{search_prefix}{q}")
    }
}

fn logical_size(window: &Window) -> (f64, f64) {
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    (s.width, s.height)
}

/// The child HWND wry hosts a webview in.
fn host_hwnd(view: &WebView) -> Option<HWND> {
    let mut hwnd = HWND::default();
    unsafe { view.controller().ParentWindow(&mut hwnd).ok().map(|_| hwnd) }
}

fn bring_to_top(view: &WebView) {
    if let Some(hwnd) = host_hwnd(view) {
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

/// Clip the page's host window to a rounded rect so it reads as a floating card.
// shortcut: window regions are not anti-aliased, so the corners are slightly jagged; upgrade with DirectComposition hosting.
fn round_corners(view: &WebView, window: &Window, settings: &Settings, bounds: Rect) {
    let Some(hwnd) = host_hwnd(view) else { return };
    let scale = window.scale_factor();
    let size = bounds.size.to_physical::<i32>(scale);
    let d = (settings.radius as f64 * 2.0 * scale) as i32;
    unsafe {
        if d == 0 {
            SetWindowRgn(hwnd, None, true);
        } else {
            let rgn = CreateRoundRectRgn(0, 0, size.width + 1, size.height + 1, d, d);
            SetWindowRgn(hwnd, Some(rgn), true); // the system owns rgn from here on
        }
    }
}

fn main() -> wry::Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(dir.join("extensions")).ok();
    let settings = Settings::load(&dir);
    let (r, g, b) = settings.frame_rgb();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let logo = || Icon::from_rgba(icon::logo_rgba(64), 64, 64).ok();
    let window = WindowBuilder::new()
        .with_title(APP)
        .with_window_icon(logo())
        .with_taskbar_icon(logo())
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 800.0))
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_background_color((r, g, b, 255))
        .build(&event_loop)
        .expect("window");
    // Dark window border/shadow. (tao's with_theme(Dark) starts this borderless window minimized.)
    unsafe {
        let dark: i32 = 1;
        let hwnd = HWND(window.hwnd() as _);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
    }

    // One shared profile = one WebView2 process tree; all webviews must then use the same extension settings.
    let mut ctx = WebContext::new(Some(dir.join("profile")));
    let proxy = event_loop.create_proxy();
    let ui_proxy = proxy.clone();
    let ui = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("pages/sidebar.html"))
        .with_background_color((r, g, b, 255))
        .with_browser_extensions_enabled(true)
        .with_ipc_handler(move |req| {
            let _ = ui_proxy.send_event(UserEvent::Ui(req.body().clone()));
        })
        .build_as_child(&window)?;

    let essentials = load_essentials(&dir);
    let mut b = Browser {
        window, ui, ctx, dir, proxy, tabs: Vec::new(), active: 0, next_id: 1, essentials, settings,
        settings_view: None, peeking: false,
    };
    b.layout();
    b.open_tab(NEW_TAB);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => b.layout(),
            Event::WindowEvent { event: WindowEvent::CursorMoved { position, .. }, .. } => b.cursor_moved(position.x, position.y),
            Event::UserEvent(UserEvent::Ui(msg)) => {
                if !b.handle_ui(&msg) {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::Setting(msg)) => b.handle_setting(&msg),
            Event::UserEvent(UserEvent::PageKey(k)) => b.shortcut(&k),
            Event::UserEvent(UserEvent::PageNav(id, q)) => {
                if let Some(i) = b.index_of(id) {
                    let _ = b.tabs[i].view.load_url(&to_url(&q, b.settings.search_prefix()));
                }
            }
            Event::UserEvent(UserEvent::NewTab(url)) => b.open_tab(&url),
            Event::UserEvent(UserEvent::UrlChanged(id, url)) => {
                if let Some(i) = b.index_of(id) {
                    b.set_url(i, url);
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

    const DDG: &str = "https://duckduckgo.com/?q=";

    #[test]
    fn address_bar_input() {
        assert_eq!(to_url("https://a.com/x", DDG), "https://a.com/x");
        assert_eq!(to_url("example.com", DDG), "https://example.com");
        assert_eq!(to_url("localhost:3000", DDG), "https://localhost:3000");
        assert_eq!(to_url("rust lang", DDG), "https://duckduckgo.com/?q=rust+lang");
        assert_eq!(to_url("a&b", "https://www.google.com/search?q="), "https://www.google.com/search?q=a%26b");
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
    fn layouts() {
        let mut s = Settings::default(); // sidebar left 240, gap 8
        assert_eq!(layout_rects(1000.0, 600.0, &s), ((0.0, 0.0, 240.0, 600.0), (240.0, 8.0, 752.0, 584.0)));
        s.sidebar_right = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .0, 760.0, "sidebar hugs the right edge");
        s.tabs_top = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s), ((0.0, 0.0, 1000.0, TOP_BAR), (8.0, TOP_BAR, 984.0, 600.0 - TOP_BAR - 8.0)));
        s.compact = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).1 .1, 8.0, "compact leaves only the gap on top");
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.a.com:8080/x?y"), "www.a.com:8080");
        assert_eq!(host("https://a.com"), "a.com");
        assert_eq!(host("https://a.com#top"), "a.com");
        assert_eq!(host("about:blank"), "");
    }
}
