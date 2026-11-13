// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod downloads;
mod engine;
mod history;
mod import;
mod icon;
mod media;
mod picker;
mod profiles;
mod sandbox;
mod settings;

use std::path::{Path, PathBuf};

use settings::{Settings, ENGINES, PRESETS};
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
    UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_BOTTOM, HWND_TOP,
        LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_LAYERED,
    },
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
    : ({t: 'new', w: 'close', l: 'focus', j: 'downloads', ',': 'settings', Tab: e.shiftKey ? 'prev' : 'next'})[e.key];
  if (k) { e.preventDefault(); e.stopPropagation(); window.ipc.postMessage('key:' + k); }
}, true);
"#;

enum UserEvent {
    Ui(String),
    Setting(String),
    PageKey(String),
    PageNav(u32, String),
    PagePos(String),
    PageSuggest(u32, String),
    Download(downloads::Update),
    Fullscreen(u32, bool),
    Playing(u32, bool),
    BgInstalled(Option<media::Installed>, String),
    NewTab(String),
    UrlChanged(u32, String),
    TitleChanged(u32, String),
    /// The engine has work: pump it.
    Engine,
    /// A page on the engine opened popups (`window.open`, `target=_blank`): give each a tab.
    EnginePopup,
}

struct Tab {
    id: u32,
    title: String,
    url: String, // empty while the tab shows the new-tab page
    /// The new tab page (Velocity UI, WebView2). Web pages are on the engine instead: `None`.
    view: Option<WebView>,
    unseen: bool, // navigated while in the background; marked until visited
    pending: Option<String>, // restored from the last session: the page loads only when the tab is opened
}

struct Browser {
    window: Window,
    ui: WebView,
    ctx: WebContext,
    base: PathBuf, // the app's data folder; extensions are shared by all profiles
    dir: PathBuf,  // this profile's folder
    profile: String,
    history: history::History,
    downloads: downloads::Downloads,
    downloads_unseen: bool, // a download finished since the list was last opened
    close_after: Vec<(u32, u32, String)>, // (download, tab, url): tabs opened only for a download
    proxy: EventLoopProxy<UserEvent>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    essentials: Vec<String>,
    settings: Settings,
    settings_view: Option<WebView>,
    settings_tab: Option<u32>, // id of the tab the settings panel was opened on
    peeking: bool,
    minimized: bool,
    fullscreen: bool,                         // a page (video) is showing full screen
    playing: std::collections::HashSet<u32>, // tabs playing audio or video
    top_peek: bool, // sidebar layout: the window-controls strip above the page is shown
    we_dirs: Vec<PathBuf>,
    wallpapers: Vec<media::Wallpaper>,
    arrange_next: bool, // the next new tab opens in arrange mode
    settings_page: &'static str, // the page the settings panel opens on next, or "" for the last one
    /// Velocity's engine, which shows every web page; `None` with VELOCITY_WEBVIEW2=1 (WebView2 does).
    engine: Option<engine::Engine>,
}

impl Browser {
    /// Data the built-in pages read at load: settings, essentials, the option lists and the theme helper.
    /// Init scripts stay on a webview across navigations, so this only runs on our own (non-http) pages:
    /// a site the user later opens in the same tab must not see their Essentials.
    fn page_init_js(&self) -> String {
        let essentials: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let engines: Vec<String> = ENGINES.iter().map(|(k, name, _)| format!("['{k}','{name}']")).collect();
        let presets: Vec<String> = PRESETS
            .iter()
            .map(|(k, name, f, c, t, a)| format!("['{k}','{name}','{f}','{c}','{t}','{a}']"))
            .collect();
        format!(
            "if(!/^https?:$/.test(location.protocol)){{{THEME_JS}{SUGGEST_JS}window.VELOCITY={};window.ESSENTIALS=[{}];window.ENGINES=[{}];window.PRESETS=[{}];window.FRAME={};window.ARRANGE={};window.START_PAGE='{}';}}",
            self.settings.to_js(),
            essentials.join(","),
            engines.join(","),
            presets.join(","),
            self.frame_js(),
            self.arrange_next,
            self.settings_page
        )
    }

    fn open_tab(&mut self, url: &str) {
        self.add_tab(url, None);
    }

    /// `restored`: the title of a tab from the last session, which waits with an empty page until it's opened.
    fn add_tab(&mut self, url: &str, restored: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        if url != NEW_TAB && self.engine.is_some() && !needs_webview2(url) {
            let pending = restored.is_some().then(|| url.to_string());
            let title = restored.unwrap_or_else(|| "New tab".into());
            if pending.is_none() {
                let zoom = self.zoom();
                self.engine.as_mut().expect("engine").load(id, url, zoom);
            }
            let waiting = pending.is_some();
            self.tabs.push(Tab { id, title, url: url.to_string(), view: None, unseen: false, pending });
            if !waiting {
                self.activate(self.tabs.len() - 1);
            }
            return;
        }
        let new_tab = url == NEW_TAB;
        let view = self.make_view(id, if restored.is_some() { "about:blank" } else { url });
        let shown_url = if new_tab { String::new() } else { url.to_string() };
        if let Some(title) = restored {
            let _ = view.set_visible(false);
            self.tabs.push(Tab { id, title, url: shown_url, view: Some(view), unseen: false, pending: Some(url.to_string()) });
            return;
        }
        self.tabs.push(Tab { id, title: "New tab".into(), url: shown_url, view: Some(view), unseen: false, pending: None });
        self.activate(self.tabs.len() - 1);
        if let (true, Some(view)) = (new_tab, self.current()) {
            let _ = view.focus(); // type into the centred search bar straight away
        }
    }

    /// A WebView2 for tab `id`: the new tab page (`NEW_TAB`), or a site the engine can't show yet.
    fn make_view(&mut self, id: u32, url: &str) -> WebView {
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
            .with_extensions_path(self.base.join("extensions"))
            .with_devtools(true)
            .with_initialization_script(PAGE_KEYS_JS)
            .with_initialization_script_for_main_only(if new_tab { init } else { String::new() }, true)
            // Pages can post anything: only shortcuts and "navigate this same tab" are accepted from them.
            .with_ipc_handler(move |req| {
                let body = req.body();
                if let Some(k) = body.strip_prefix("key:") {
                    let _ = p1.send_event(UserEvent::PageKey(k.to_string()));
                } else if let Some(q) = body.strip_prefix("nav:") {
                    let _ = p1.send_event(UserEvent::PageNav(id, q.to_string()));
                } else if let Some(p) = body.strip_prefix("ntpos:") {
                    // Dragging on the new tab page; worst case a site rearranges the user's new tab layout.
                    let _ = p1.send_event(UserEvent::PagePos(p.to_string()));
                } else if let Some(q) = body.strip_prefix("suggest:") {
                    let _ = p1.send_event(UserEvent::PageSuggest(id, q.to_string()));
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
        round_corners(host_hwnd(&view), &self.window, &self.settings, bounds);
        let p5 = self.proxy.clone();
        downloads::watch(&view, id, move |u| {
            let _ = p5.send_event(UserEvent::Download(u));
        });
        watch_media(&view, id, self.proxy.clone());
        if new_tab {
            media::map(&view, media::BG_HOST, &media::bg_dir(&self.dir));
        } else {
            let _ = view.zoom(self.settings.page_zoom as f64 / 100.0);
        }
        view
    }

    fn zoom(&self) -> f32 {
        self.settings.page_zoom as f32 / 100.0
    }

    /// Load `url` in tab `index`. Web pages go to the engine; a new tab page leaving for a site
    /// gives its WebView2 up. The caller shows the tab again if it is the active one.
    fn navigate(&mut self, index: usize, url: &str) {
        let id = self.tabs[index].id;
        let zoom = self.zoom();
        match self.engine.as_mut() {
            Some(engine) if !needs_webview2(url) && (engine.has(id) || url.starts_with("http")) => {
                engine.load(id, url, zoom);
                self.tabs[index].view = None;
            }
            engine => {
                if let Some(engine) = engine {
                    engine.close(id); // a site the engine can't show yet moves to WebView2
                }
                match &self.tabs[index].view {
                    Some(view) => {
                        let _ = view.load_url(url);
                    }
                    None => {
                        let view = self.make_view(id, url);
                        let _ = view.set_visible(false); // the caller shows the tab
                        self.tabs[index].view = Some(view);
                    }
                }
            }
        }
    }

    /// Whether the shown tab is a web page on the engine.
    fn on_engine(&self) -> Option<&engine::Engine> {
        let id = self.tabs.get(self.active)?.id;
        self.engine.as_ref().filter(|e| e.has(id))
    }

    /// The window a tab is drawn in: its WebView2 host, or the engine's window.
    fn tab_hwnd(&self, index: usize) -> Option<HWND> {
        match (&self.tabs.get(index)?.view, &self.engine) {
            (Some(view), _) => host_hwnd(view),
            (None, Some(engine)) => Some(HWND(engine.hwnd() as _)),
            (None, None) => None,
        }
    }

    /// Track a tab's URL; leaving the new-tab page restores the white default sites expect.
    fn set_url(&mut self, index: usize, url: String) {
        let background = index != self.active;
        let tab = &mut self.tabs[index];
        if url.starts_with("http") {
            if let (true, Some(view)) = (tab.url.is_empty(), &tab.view) {
                let _ = view.set_background_color((255, 255, 255, 255));
                let _ = view.zoom(self.settings.page_zoom as f64 / 100.0);
            }
            if self.settings.history {
                self.history.record(&url, "");
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
        let closed = self.tabs.remove(index); // dropping the WebView destroys it
        if let Some(engine) = self.engine.as_mut() {
            engine.close(closed.id);
        }
        if self.settings_tab == Some(closed.id) {
            self.close_settings(); // the panel belonged to that tab
        }
        if self.tabs.is_empty() {
            self.open_tab(NEW_TAB);
            return;
        }
        self.activate(active_after_close(index, self.active, self.tabs.len()));
    }

    fn activate(&mut self, index: usize) {
        if self.fullscreen {
            // The full-screen page is going away (closed or switched from): give the window back.
            self.fullscreen = false;
            if let Some(engine) = &self.engine {
                engine.exit_fullscreen(); // the page leaves full screen too
            }
            self.window.set_fullscreen(None);
            self.active = index.min(self.tabs.len() - 1);
            self.layout();
        }
        self.active = index;
        self.tabs[index].unseen = false;
        if let Some(url) = self.tabs[index].pending.take() {
            self.navigate(index, &url);
        }
        // Show the new page before hiding the old one, so the empty frame never flashes in between.
        let id = self.tabs[index].id;
        if let Some(engine) = self.engine.as_mut() {
            engine.activate(Some(id)); // hidden when this tab shows the new tab page
        }
        if let Some(view) = &self.tabs[index].view {
            let _ = view.set_visible(true);
            memory_target(view, false);
        }
        // Always in front: with window glass the tab UI spans the window underneath, and anything that raised it
        // (suggestions, compact peek) would otherwise cover a tab that was hidden at the time.
        bring_to_top(self.tab_hwnd(index));
        for (_, t) in self.tabs.iter().enumerate().filter(|(i, _)| *i != index) {
            if let Some(view) = &t.view {
                let _ = view.set_visible(false);
                memory_target(view, true); // background tabs give memory back to Windows
            }
        }
        self.show_settings_if_owner();
        self.sync_ui();
        self.update_opacity();
    }

    /// The settings panel belongs to the tab it was opened on: it shows only while that tab is active.
    fn show_settings_if_owner(&self) {
        if let Some(view) = &self.settings_view {
            let mine = self.settings_tab == Some(self.tabs[self.active].id);
            let _ = view.set_visible(mine);
            if mine {
                bring_to_top(host_hwnd(view)); // tabs opened since then sit above it
            }
        }
    }

    fn close_settings(&mut self) {
        self.settings_view = None;
        self.settings_tab = None;
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The shown tab's new tab page, if it shows one.
    fn current(&self) -> Option<&WebView> {
        self.tabs.get(self.active)?.view.as_ref()
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
        self.essentials_changed();
    }

    /// Save the Essentials and show the new list everywhere it is drawn.
    fn essentials_changed(&self) {
        save_essentials(&self.dir, &self.essentials);
        self.sync_ui();
        let list: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let js = format!("renderEssentials([{}])", list.join(","));
        for view in self.tabs.iter().filter(|t| t.url.is_empty()).filter_map(|t| t.view.as_ref()).chain(self.settings_view.as_ref()) {
            let _ = view.evaluate_script(&js);
        }
    }

    /// Open tabs for the next start: `active\t<index>`, then `<url>\t<title>` per tab (empty url = new tab page).
    fn save_session(&self) {
        let mut text = format!("active\t{}\n", self.active);
        for t in &self.tabs {
            let url = t.pending.as_deref().unwrap_or(&t.url);
            text.push_str(&format!("{}\t{}\n", url.replace(['\t', '\n'], ""), t.title.replace(['\t', '\n'], " ")));
        }
        let _ = std::fs::write(self.dir.join("session.tsv"), text);
    }

    /// Reopen the last session's web pages (empty new tabs aren't kept); returns whether any came back.
    fn restore_session(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(self.dir.join("session.tsv")) else { return false };
        let (pages, active) = parse_session(&text);
        for (url, title) in &pages {
            self.add_tab(url, Some(title.clone()));
        }
        if self.tabs.is_empty() {
            return false;
        }
        self.activate(active.min(self.tabs.len() - 1));
        true
    }

    /// Drag and drop in the tab list: move tab `id` to position `to`.
    fn move_tab(&mut self, id: u32, to: usize) {
        let Some(from) = self.index_of(id) else { return };
        let active = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to.min(self.tabs.len()), tab);
        self.active = self.index_of(active).unwrap_or(0);
        self.sync_ui();
    }

    /// Push the full state to the sidebar; small enough that diffing isn't worth it.
    fn sync_ui(&self) {
        self.save_session();
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
        let profile = if self.profile == profiles::DEFAULT { String::new() } else { format!(" ({})", self.profile) };
        self.window.set_title(&format!("{} - {APP}{profile}", cur.title));
        let _ = self.ui.evaluate_script(&format!(
            "render([{}],{},[{}],{},{})",
            tabs.join(","),
            cur.id,
            essentials.join(","),
            self.settings.to_js(),
            format!("{{n:{},a:{}}}", js_str(&self.profile), js_str(&profiles::avatar(&self.base, &self.profile)))
        ));
        self.push_frame();
    }

    fn sidebar_shown(&self) -> bool {
        !self.settings.compact || self.peeking
    }

    fn sidebar_bounds(&self) -> Rect {
        let (w, h) = logical_size(&self.window);
        rect(layout_rects(w, h, &self.settings, self.top_peek).0)
    }

    fn content_bounds(&self) -> Rect {
        let (w, h) = logical_size(&self.window);
        rect(layout_rects(w, h, &self.settings, self.top_peek).1)
    }

    fn layout(&self) {
        if self.fullscreen {
            let (w, h) = logical_size(&self.window);
            let _ = self.ui.set_visible(false);
            let full = rect((0.0, 0.0, w, h));
            if let Some(view) = self.current() {
                let _ = view.set_bounds(full);
            }
            self.place_engine(full, 0);
            let hwnd = self.tab_hwnd(self.active);
            if self.on_engine().is_none() {
                clear_region(hwnd);
            }
            bring_to_top(hwnd);
            return;
        }
        let _ = self.ui.set_bounds(self.sidebar_bounds());
        let _ = self.ui.set_visible(self.sidebar_shown());
        if self.peeking {
            bring_to_top(host_hwnd(&self.ui)); // in compact mode the sidebar overlays the page
        } else {
            send_to_bottom(host_hwnd(&self.ui)); // under every page again (it spans the window with window glass)
        }
        let b = self.content_bounds();
        for view in self.tabs.iter().filter_map(|t| t.view.as_ref()).chain(self.settings_view.as_ref()) {
            let _ = view.set_bounds(b);
            round_corners(host_hwnd(view), &self.window, &self.settings, b);
        }
        self.place_engine(b, (self.settings.radius as f64 * 2.0 * self.window.scale_factor()) as i32);
        self.push_frame();
    }

    /// The engine's window covers `bounds` (logical), like a tab's WebView would.
    fn place_engine(&self, bounds: Rect, corner: i32) {
        let Some(engine) = &self.engine else { return };
        let scale = self.window.scale_factor();
        let position = bounds.position.to_physical::<i32>(scale);
        let size = bounds.size.to_physical::<u32>(scale);
        engine.set_bounds(position.x, position.y, size.width, size.height, corner);
    }

    fn set_peek(&mut self, peek: bool) {
        if self.settings.compact && self.peeking != peek {
            self.peeking = peek;
            self.layout();
        }
    }

    /// The Downloads page of the settings panel.
    fn open_downloads(&mut self) {
        self.downloads_unseen = false;
        self.show_settings_page("downloads");
        self.refresh_downloads();
    }

    fn download_update(&mut self, u: downloads::Update) {
        use downloads::Update;
        match u {
            Update::Ask { id, tab, url, path, total } => {
                let suggested = Path::new(&path);
                let mut dialog = rfd::FileDialog::new().set_title("Save as");
                if let Some(dir) = suggested.parent() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(name) = suggested.file_name().and_then(|n| n.to_str()) {
                    dialog = dialog.set_file_name(name);
                }
                let choice = dialog.save_file();
                if downloads::answer(id, choice.as_deref()) {
                    let path = choice.map(|p| p.to_string_lossy().into_owned()).unwrap_or(path);
                    return self.download_update(Update::Started { id, tab, url, path, total });
                }
                return self.close_download_tab(tab, &url); // cancelled
            }
            // A tab that only existed to start this download (a link opening a new window) has nothing to show.
            // It closes once the download is done: closing it now would also stop its progress reports.
            Update::Started { id, tab, ref url, .. } => {
                if self.index_of(tab).is_some_and(|i| self.tabs[i].url == *url && self.tabs[i].title == "New tab") {
                    self.close_after.push((id, tab, url.clone()));
                }
            }
            Update::Finished { id, .. } => {
                if let Some(at) = self.close_after.iter().position(|c| c.0 == id) {
                    let (_, tab, url) = self.close_after.remove(at);
                    self.close_download_tab(tab, &url);
                }
                self.downloads_unseen = true;
            }
            Update::Progress { .. } => {}
        }
        if self.downloads.apply(u) {
            self.refresh_downloads();
        }
    }

    fn close_download_tab(&mut self, tab: u32, url: &str) {
        let still_empty = self.index_of(tab).filter(|&i| self.tabs[i].url == url && self.tabs[i].title == "New tab");
        if let Some(i) = still_empty.filter(|_| self.tabs.len() > 1) {
            self.close_tab(i);
        }
    }

    /// Progress ring on the downloads button, and the list if the Downloads page is open.
    fn refresh_downloads(&self) {
        let (running, pct) = self.downloads.progress();
        let _ = self.ui.evaluate_script(&format!("downloadsBadge({running},{pct},{})", self.downloads_unseen));
        let items: Vec<String> = self
            .downloads
            .items
            .iter()
            .map(|i| {
                let name = Path::new(&i.path).file_name().and_then(|n| n.to_str()).unwrap_or(&i.path);
                let state = match i.state {
                    downloads::State::Running => "running",
                    downloads::State::Done => "done",
                    downloads::State::Failed => "failed",
                };
                format!(
                    "{{id:{},n:{},u:{},s:'{state}',r:{},t:{},ts:{}}}",
                    i.id, js_str(name), js_str(&i.url), i.received, i.total, i.ts
                )
            })
            .collect();
        self.settings_js(&format!("window.renderDownloads&&renderDownloads([{}])", items.join(",")));
    }

    /// Commands from the Downloads page; only files from our own list are ever opened.
    fn download_command(&mut self, cmd: &str, id: &str) {
        use std::os::windows::process::CommandExt;
        let Some(item) = id.parse().ok().and_then(|id| self.downloads.get(id)).cloned() else {
            return self.refresh_downloads();
        };
        let done = item.state == downloads::State::Done && Path::new(&item.path).is_file();
        match cmd {
            "open" if done => drop(std::process::Command::new("explorer").arg(&item.path).spawn()),
            "show" if done => drop(std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", item.path)).spawn()),
            "cancel" => downloads::cancel(item.id),
            "remove" => self.downloads.remove(item.id),
            "retry" if item.url.starts_with("http") => self.open_tab(&item.url),
            _ => {}
        }
        self.refresh_downloads();
    }

    /// Show the settings panel on this tab at `page` (opening it if needed).
    fn show_settings_page(&mut self, page: &'static str) {
        if self.settings_view.is_some() {
            self.settings_tab = Some(self.tabs[self.active].id);
            self.show_settings_if_owner();
            self.settings_js(&format!("go('{page}')"));
        } else {
            self.settings_page = page;
            self.toggle_settings();
            self.settings_page = "";
        }
    }

    fn toggle_settings(&mut self) {
        let here = self.tabs[self.active].id;
        if self.settings_view.is_some() {
            if self.settings_tab == Some(here) {
                return self.close_settings(); // dropping the WebView closes the panel
            }
            // Open on another tab: move the panel to this one instead of opening a second.
            self.settings_tab = Some(here);
            return self.show_settings_if_owner();
        }
        self.settings_tab = Some(here);
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
        round_corners(host_hwnd(&view), &self.window, &self.settings, bounds);
        media::map(&view, media::BG_HOST, &media::bg_dir(&self.dir));
        media::map(&view, profiles::AVATAR_HOST, &profiles::avatars_dir(&self.base));
        let _ = view.focus();
        self.settings_view = Some(view);
    }

    /// Find Wallpaper Engine wallpapers and list them in the settings page (previews via mapped hosts).
    fn scan_wallpapers(&mut self) {
        self.we_dirs = media::workshop_dirs();
        self.wallpapers = media::scan(&self.we_dirs);
        let Some(view) = &self.settings_view else { return };
        for (i, dir) in self.we_dirs.iter().enumerate() {
            media::map(view, &media::we_host(i), dir);
        }
        let items: Vec<String> = self
            .wallpapers
            .iter()
            .map(|w| {
                format!(
                    "{{lib:{},id:{},title:{},kind:{},preview:{},entry:{}}}",
                    w.library, js_str(&w.id), js_str(&w.title), js_str(&w.kind), js_str(&w.preview), js_str(&w.entry)
                )
            })
            .collect();
        let _ = view.evaluate_script(&format!("renderWallpapers([{}],{})", items.join(","), self.we_dirs.len()));
    }

    /// Install a new background file and show it everywhere. Copying runs on a worker thread, so a large
    /// Wallpaper Engine video doesn't freeze the window; `we` records which wallpaper it came from.
    fn set_background(&mut self, src: Option<PathBuf>, we: Option<(PathBuf, media::Wallpaper)>) {
        if src.is_none() && we.is_none() {
            media::clear(&self.dir);
            let none = media::Installed { bg: String::new(), entry: String::new(), thumb: String::new() };
            return self.background_installed(Some(none), "");
        }
        if let Some(v) = &self.settings_view {
            let _ = v.evaluate_script("bgBusy(true)");
        }
        let (dir, proxy) = (self.dir.clone(), self.proxy.clone());
        let tag = we.as_ref().map(|(_, w)| format!("{}:{}", w.library, w.id)).unwrap_or_default();
        std::thread::spawn(move || {
            let done = match (src, we) {
                (Some(path), _) => media::install_file(&dir, &path),
                (None, Some((item, w))) => media::install_wallpaper(&dir, &item, &w),
                (None, None) => None,
            };
            let _ = proxy.send_event(UserEvent::BgInstalled(done, tag));
        });
    }

    fn background_installed(&mut self, done: Option<media::Installed>, we: &str) {
        if let Some(v) = &self.settings_view {
            let _ = v.evaluate_script(&format!("bgBusy(false,{})", done.is_none()));
        }
        let Some(done) = done else { return };
        self.settings.bg = done.bg;
        self.settings.set("bg_entry", &done.entry);
        self.settings.set("bg_thumb", &done.thumb);
        self.settings.set("bg_we", we);
        self.settings.bg_ver += 1;
        self.settings.save(&self.dir);
        self.apply_settings();
    }

    /// Where the page card and the tab UI sit in the window, for pages that align the wallpaper to it.
    fn frame_js(&self) -> String {
        let (w, h) = logical_size(&self.window);
        let (ui, card) = layout_rects(w, h, &self.settings, self.top_peek);
        format!("{{uiX:{},uiY:{},cardX:{},cardY:{},w:{},h:{}}}", ui.0, ui.1, card.0, card.1, w, h)
    }

    fn push_frame(&self) {
        let js = format!("window.velocityFrame&&velocityFrame({})", self.frame_js());
        let _ = self.ui.evaluate_script(&js);
        for view in self.tabs.iter().filter(|t| t.url.is_empty()).filter_map(|t| t.view.as_ref()) {
            let _ = view.evaluate_script(&js);
        }
    }

    fn handle_setting(&mut self, msg: &str) {
        if msg == "close" {
            return self.close_settings();
        }
        if let Some(cmd) = msg.strip_prefix("ess:") {
            if edit_essentials(&mut self.essentials, cmd) {
                self.essentials_changed();
            }
            return;
        }
        match msg {
            "bg:pick" => {
                let exts: Vec<&str> = settings::MEDIA_EXTS.to_vec();
                let file = rfd::FileDialog::new()
                    .set_title("Choose a new tab background")
                    .add_filter("Images, GIFs and videos", &exts)
                    .pick_file();
                if file.is_some() {
                    self.set_background(file, None);
                }
                return;
            }
            "bg:clear" => return self.set_background(None, None),
            "arrange" => {
                // Close settings and put the new tab page into arrange mode (opening one if needed).
                self.close_settings();
                if self.tabs[self.active].url.is_empty() {
                    if let Some(view) = self.current() {
                        let _ = view.evaluate_script("arrange(true)");
                    }
                } else {
                    self.arrange_next = true;
                    self.open_tab(NEW_TAB);
                    self.arrange_next = false;
                }
                return;
            }
            "we:scan" => return self.scan_wallpapers(),
            "hist:clear" => {
                self.history.clear();
                return self.show_history("");
            }
            "prof:list" => return self.show_profiles(),
            "imp:scan" => return self.show_import(),
            // Windows only lets the user choose the default browser, in its own Settings page.
            "default" => return drop(std::process::Command::new("explorer").arg("ms-settings:defaultapps?registeredAppUser=Velocity").spawn()),
            "dl:list" => return self.refresh_downloads(),
            "dl:clear" => {
                self.downloads.clear();
                return self.refresh_downloads();
            }
            _ => {}
        }
        if let Some((cmd, id)) = msg.strip_prefix("dl:").and_then(|m| m.split_once(':')) {
            return self.download_command(cmd, id);
        }
        if let Some((id, what)) = msg.strip_prefix("imp:run:").and_then(|m| m.split_once(':')) {
            return self.run_import(id, what);
        }
        if let Some(q) = msg.strip_prefix("hist:list:") {
            return self.show_history(q);
        }
        if let Some(rest) = msg.strip_prefix("hist:del:") {
            let (url, q) = rest.split_once('\n').unwrap_or((rest, ""));
            self.history.remove(url);
            return self.show_history(q);
        }
        if let Some(url) = msg.strip_prefix("hist:open:") {
            if url.starts_with("https://") || url.starts_with("http://") {
                self.open_tab(url);
            }
            return;
        }
        if let Some((cmd, name)) = msg.strip_prefix("prof:").and_then(|m| m.split_once(':')) {
            let known = profiles::list(&self.base).into_iter().find(|n| n == name);
            match (cmd, known) {
                ("new", _) => match profiles::create(&self.base, name) {
                    Some(n) => profiles::launch(&n, None),
                    None => self.settings_js("profileError('That name is taken or not allowed.')"),
                },
                ("open", Some(n)) => profiles::launch(&n, None),
                ("pic", Some(n)) => {
                    let file = rfd::FileDialog::new()
                        .set_title(format!("Choose a picture for {n}"))
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
                        .pick_file();
                    if let Some(f) = file {
                        if !profiles::set_avatar(&self.base, &n, &f) {
                            self.settings_js("profileError('That picture couldn\\'t be used. Pick an image under 10 MB.')");
                        }
                    }
                    self.sync_ui(); // the profile chip in the tab bar
                }
                ("nopic", Some(n)) => {
                    profiles::remove_avatar(&self.base, &n);
                    self.sync_ui();
                }
                ("ren", _) => {
                    // "ren:<old>:<new>"; names never contain ':'.
                    let (old, new) = name.split_once(':').unwrap_or((name, ""));
                    if old == self.profile || !profiles::rename(&self.base, old, new) {
                        self.settings_js("profileError('Couldn\\'t rename it: the name is taken or not allowed, or that profile is open in a window. Close its window first.')");
                    }
                }
                ("del", Some(n)) if n != self.profile => {
                    if !profiles::delete(&self.base, &n) {
                        self.settings_js("profileError('Close the window of that profile first, then delete it.')");
                    }
                }
                _ => {}
            }
            return self.show_profiles();
        }
        if let Some(pick) = msg.strip_prefix("bg:we:") {
            // Only wallpapers from our own scan can be chosen, by library index and folder id.
            let (lib, id) = pick.split_once(':').unwrap_or(("", ""));
            let found = self.wallpapers.iter().position(|w| lib.parse() == Ok(w.library) && w.id == id);
            if let Some(i) = found {
                let w = self.wallpapers[i].clone();
                let item = self.we_dirs[w.library].join(&w.id);
                self.set_background(None, Some((item, w)));
            }
            return;
        }
        if msg == "reset" {
            let ver = self.settings.bg_ver + 1;
            media::clear(&self.dir);
            self.settings = Settings::default();
            self.settings.bg_ver = ver;
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

    fn settings_js(&self, js: &str) {
        if let Some(v) = &self.settings_view {
            let _ = v.evaluate_script(js);
        }
    }

    /// The History page: entries matching `q`, newest first.
    fn show_history(&self, q: &str) {
        let items: Vec<String> = self
            .history
            .search(q, 300)
            .iter()
            .map(|e| format!("{{u:{},t:{},ts:{}}}", js_str(&e.url), js_str(&e.title), e.ts))
            .collect();
        self.settings_js(&format!("renderHistory([{}],{})", items.join(","), self.history.entries.len()));
    }

    /// The Import page: other browsers found for this Windows user and what can be brought over from each.
    fn show_import(&self) {
        let items: Vec<String> = import::find()
            .iter()
            .map(|b| format!("{{id:'{}',name:{},h:{},b:{},c:{}}}", b.id, js_str(b.name), b.has_history(), b.has_bookmarks(), b.has_cookies()))
            .collect();
        self.settings_js(&format!("renderImport([{}])", items.join(",")));
    }

    /// Import from browser `id`; `what` holds h (history), b (bookmarks bar to Essentials), c (Firefox cookies).
    fn run_import(&mut self, id: &str, what: &str) {
        let Some(b) = import::find().into_iter().find(|b| b.id == id) else { return };
        let mut done = Vec::new();
        let mut failed = Vec::new();
        if what.contains('h') {
            match import::history(&b, 5000) {
                Ok(v) => done.push(format!("{} pages of history", self.history.import(v))),
                Err(e) => failed.push(format!("history ({e})")),
            }
        }
        if what.contains('b') {
            match import::bookmarks_bar(&b) {
                Ok(urls) => {
                    let before = self.essentials.len();
                    for u in urls.iter().take(24) {
                        if self.essentials.len() >= 12 {
                            break; // Essentials are quick tiles, not a bookmark library
                        }
                        edit_essentials(&mut self.essentials, &format!("add:{u}"));
                    }
                    let added = self.essentials.len() - before;
                    if added > 0 {
                        self.essentials_changed();
                    }
                    done.push(format!("{added} Essentials from the bookmarks bar"));
                }
                Err(e) => failed.push(format!("bookmarks ({e})")),
            }
        }
        if what.contains('c') && b.has_cookies() {
            match import::firefox_cookies(&b) {
                Ok(c) => done.push(format!("{} cookies (sites you were logged in to)", import::add_cookies(&self.ui, &c))),
                Err(e) => failed.push(format!("cookies ({e})")),
            }
        }
        let mut text = if done.is_empty() { String::new() } else { format!("Imported {}.", done.join(", ")) };
        if !failed.is_empty() {
            text = format!("{text} Couldn't import {}.", failed.join(", ")).trim().to_string();
        }
        self.settings_js(&format!("importDone({},{})", js_str(id), js_str(&text)));
    }

    fn show_profiles(&self) {
        self.settings_js(&format!("renderProfiles({},{})", profiles_js(&self.base), js_str(&self.profile)));
    }

    /// Address bar suggestions from history, as a call to the page's `suggestions(query, items)`.
    fn suggestions_js(&self, q: &str) -> String {
        let items: Vec<String> = self
            .history
            .suggest(q, 6)
            .iter()
            .map(|e| format!("{{u:{},t:{}}}", js_str(&e.url), js_str(&e.title)))
            .collect();
        format!("window.suggestions&&suggestions({},[{}])", js_str(q), items.join(","))
    }

    /// Re-apply everything that depends on settings, live.
    fn apply_settings(&mut self) {
        downloads::set_ask(self.settings.ask_download);
        let (r, g, b) = self.settings.frame_rgb();
        self.window.set_background_color(Some((r, g, b, 255)));
        let _ = self.ui.set_background_color((r, g, b, 255));
        self.update_opacity();
        let card = self.settings.card_rgba();
        for t in self.tabs.iter().filter(|t| t.url.is_empty()).filter_map(|t| t.view.as_ref()).chain(self.settings_view.as_ref()) {
            let _ = t.set_background_color(card);
        }
        let zoom = self.settings.page_zoom as f64 / 100.0;
        for view in self.tabs.iter().filter(|t| !t.url.is_empty()).filter_map(|t| t.view.as_ref()) {
            let _ = view.zoom(zoom);
        }
        if let Some(engine) = &self.engine {
            engine.set_zoom(zoom as f32);
        }
        if !self.settings.compact {
            self.peeking = false;
        }
        self.layout();
        self.sync_ui();
        let js = format!("applySettings({})", self.settings.to_js());
        for view in self.tabs.iter().filter(|t| t.url.is_empty()).filter_map(|t| t.view.as_ref()) {
            let _ = view.evaluate_script(&js);
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
            "downloads" => self.open_downloads(),
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
            ("go", _) => {
                let (i, url) = (self.active, to_url(arg, self.settings.search_prefix()));
                self.navigate(i, &url);
                Ok(self.activate(i))
            }
            ("back", _) => match self.on_engine() {
                Some(engine) => Ok(engine.back()),
                None => self.current().map_or(Ok(()), |v| v.evaluate_script("history.back()")),
            },
            ("forward", _) => match self.on_engine() {
                Some(engine) => Ok(engine.forward()),
                None => self.current().map_or(Ok(()), |v| v.evaluate_script("history.forward()")),
            },
            ("reload", _) => match self.on_engine() {
                Some(engine) => Ok(engine.reload()),
                None => self.current().map_or(Ok(()), |v| v.reload()),
            },
            // shortcut: no developer tools for pages on the engine yet; upgrade with Servo's devtools server.
            ("devtools", _) => Ok(if let Some(v) = self.current() { v.open_devtools() }),
            ("select", Some(i)) => Ok(self.activate(i)),
            ("close", Some(i)) => Ok(self.close_tab(i)),
            ("pin", Some(i)) => Ok(self.toggle_essential(&self.tabs[i].url.clone())),
            ("unpin", _) => Ok(self.toggle_essential(arg)),
            ("essential", _) => Ok(arg.parse().map(|i| self.open_essential(i)).unwrap_or(())),
            ("key", _) => Ok(self.shortcut(arg)),
            ("unpeek", _) => Ok(self.set_peek(false)),
            ("profiles", _) => Ok(self.show_settings_page("profiles")),
            ("move", _) => {
                // "move:<id>:<position>"
                let (id, to) = arg.split_once(':').unwrap_or(("", ""));
                if let (Ok(id), Ok(to)) = (id.parse(), to.parse()) {
                    self.move_tab(id, to);
                }
                Ok(())
            }
            ("downloads", _) => Ok(self.open_downloads()),
            ("toppeek", _) => Ok(self.set_top_peek(arg == "1")),
            ("suggest", _) => self.ui.evaluate_script(&self.suggestions_js(arg)),
            ("sugg", _) => Ok(self.suggest_space(arg.parse().unwrap_or(0.0))),
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

    /// With tabs on top the tab UI is only as tall as the bar, so while the address bar shows suggestions
    /// it grows by `extra` CSS px over the page; 0 puts it back.
    fn suggest_space(&self, extra: f64) {
        if !self.settings.tabs_top {
            return;
        }
        self.layout();
        if extra > 0.0 {
            let (w, h) = logical_size(&self.window);
            let scale = self.settings.ui_scale as f64 / 100.0;
            let bar = TOP_BAR * scale;
            let _ = self.ui.set_bounds(rect((0.0, 0.0, w, (bar + extra * scale).min(h))));
            bring_to_top(host_hwnd(&self.ui));
        } else {
            bring_to_top(self.tab_hwnd(self.active)); // with window glass the tab UI lies under the page
            self.show_settings_if_owner();
        }
    }

    /// The strip with the window controls above the page (sidebar layout): the page slides down to make room.
    fn set_top_peek(&mut self, on: bool) {
        let on = on && !self.settings.tabs_top && !self.settings.compact;
        if self.top_peek == on {
            return;
        }
        self.top_peek = on;
        self.layout();
        let _ = self.ui.evaluate_script(&format!("topPeek({on})"));
        // The tab UI spans the window under the page while the strip shows; keep the page in front.
        bring_to_top(self.tab_hwnd(self.active));
        self.show_settings_if_owner();
    }

    /// Window opacity, except while the shown tab plays a video or sound or is full screen: then it's solid.
    fn update_opacity(&self) {
        let busy = self.fullscreen || self.tabs.get(self.active).is_some_and(|t| self.playing.contains(&t.id));
        set_opacity(&self.window, if busy { 100 } else { self.settings.opacity });
    }

    /// A page asked for full screen (a video's full-screen button) or left it: the whole window becomes that page.
    fn set_fullscreen(&mut self, id: u32, on: bool) {
        if id != self.tabs[self.active].id || self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        self.window.set_fullscreen(on.then_some(tao::window::Fullscreen::Borderless(None)));
        self.layout();
        self.update_opacity();
    }

    /// Hiding the page while minimised also pauses it: videos and effects stop, which saves power.
    fn set_minimized(&mut self, min: bool) {
        if self.minimized == min {
            return;
        }
        self.minimized = min;
        if let Some(view) = &self.tabs[self.active].view {
            let _ = view.set_visible(!min);
        }
        let id = self.tabs[self.active].id;
        if let Some(engine) = self.engine.as_mut() {
            engine.activate((!min).then_some(id));
        }
        if min {
            if let Some(v) = &self.settings_view {
                let _ = v.set_visible(false);
            }
        } else {
            self.show_settings_if_owner();
        }
    }

    /// Tabs for the popups pages on the engine opened; the last one is shown.
    fn adopt_popups(&mut self) {
        let Some(engine) = self.engine.as_mut() else { return };
        let mut next = self.next_id;
        let ids = engine.adopt_popups(|| {
            next += 1;
            next - 1
        });
        self.next_id = next;
        for id in &ids {
            self.tabs.push(Tab { id: *id, title: "New tab".into(), url: String::new(), view: None, unseen: false, pending: None });
        }
        if !ids.is_empty() {
            self.activate(self.tabs.len() - 1);
        }
    }

    /// Before the window closes: the engine saves logins and site data when it is dropped.
    fn shut_down(&mut self) {
        self.save_session();
        self.engine = None;
    }

    /// Compact mode: the cursor reaching the tab UI's edge of the window reveals it.
    fn cursor_moved(&mut self, x_physical: f64, y_physical: f64) {
        let (x, y) = (x_physical / self.window.scale_factor(), y_physical / self.window.scale_factor());
        let s = &self.settings;
        // Sidebar layout: the cursor on the frame above the page opens the window-controls strip.
        if !s.compact && !s.tabs_top {
            let strip = TOP_STRIP * s.ui_scale as f64 / 100.0;
            // Open at the frame above the page; close once the cursor is back below the strip (in the gaps
            // around the page), in case it left without passing over the strip itself.
            if y <= s.gap.max(PEEK_STRIP as u32) as f64 {
                return self.set_top_peek(true);
            }
            if self.top_peek && y > strip {
                return self.set_top_peek(false);
            }
        }
        if !s.compact || self.peeking {
            return;
        }
        let (w, _) = logical_size(&self.window);
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
/// `top_strip`: sidebar layout with the window-controls strip shown above the page.
fn layout_rects(w: f64, h: f64, s: &Settings, top_strip: bool) -> (Xywh, Xywh) {
    let gap = s.gap as f64;
    let scale = s.ui_scale as f64 / 100.0; // the tab UI is drawn zoomed, so its box grows with it
    let (ui, card) = if s.tabs_top {
        let bar = TOP_BAR * scale;
        let top = if s.compact { gap.max(PEEK_STRIP) } else { bar };
        ((0.0, 0.0, w, bar), (gap, top, (w - 2.0 * gap).max(1.0), (h - top - gap).max(1.0)))
    } else {
        let sw = (s.sidebar_width as f64 * scale).min(w * 0.6);
        let side = if s.compact { gap.max(PEEK_STRIP) } else { sw };
        let (left, right) = if s.sidebar_right { (gap, side) } else { (side, gap) };
        let ui_x = if s.sidebar_right { w - sw } else { 0.0 };
        // Outside compact mode the frame above the page is at least PEEK_STRIP tall, so the cursor can reach it.
        let top = if top_strip && !s.compact { TOP_STRIP * scale } else if s.compact { gap } else { gap.max(PEEK_STRIP) };
        ((ui_x, 0.0, sw, h), (left, top, (w - left - right).max(1.0), (h - top - gap).max(1.0)))
    };
    // The strip is drawn by the tab UI, which then spans the window under the page like window glass.
    let top_strip = top_strip && !s.tabs_top && !s.compact;
    // Window glass: the tab UI's webview spans the whole window under the page, so the wallpaper also shows
    // in the gaps around the page. Not in compact mode, where the tab UI is raised over the page to peek.
    if window_glass(s) || top_strip { ((0.0, 0.0, w, h), card) } else { (ui, card) }
}

/// Height of the window-controls strip above the page in the sidebar layout.
const TOP_STRIP: f64 = 36.0;

fn window_glass(s: &Settings) -> bool {
    s.bg_window && !s.bg.is_empty() && !s.compact
}

fn rect((x, y, w, h): Xywh) -> Rect {
    Rect { position: LogicalPosition::new(x, y).into(), size: LogicalSize::new(w, h).into() }
}

/// Which tab is active after closing `closed`: keep the same tab, or take the neighbour that slid into its place.
fn active_after_close(closed: usize, active: usize, remaining: usize) -> usize {
    if closed < active || active >= remaining { active.saturating_sub(1) } else { active }
}

/// An Essential typed by the user: "github.com" -> "https://github.com"; anything that isn't an
/// http(s) address with a host (search text, javascript:, file:) is rejected.
fn essential_url(input: &str) -> Option<String> {
    let url = to_url(input, "\u{0}"); // search text gets the NUL prefix and is rejected below
    let ok = (url.starts_with("https://") || url.starts_with("http://")) && !host(&url).is_empty();
    ok.then_some(url).filter(|u| !u.contains(char::is_whitespace))
}

/// Apply one Essentials edit from the settings page; returns whether the list changed.
/// Commands: `add:<address>`, `remove:<i>`, `edit:<i>:<address>`, `up:<i>`, `down:<i>`.
fn edit_essentials(list: &mut Vec<String>, cmd: &str) -> bool {
    let (op, rest) = cmd.split_once(':').unwrap_or((cmd, ""));
    let (idx, arg) = rest.split_once(':').unwrap_or((rest, ""));
    let i = idx.parse::<usize>().ok().filter(|&i| i < list.len());
    let taken = |list: &Vec<String>, url: &str, skip: Option<usize>| {
        list.iter().enumerate().any(|(j, e)| Some(j) != skip && host(e) == host(url))
    };
    match (op, i) {
        ("add", _) => match essential_url(rest) {
            Some(url) if !taken(list, &url, None) => list.push(url),
            _ => return false,
        },
        ("remove", Some(i)) => {
            list.remove(i);
        }
        ("edit", Some(i)) => match essential_url(arg) {
            Some(url) if !taken(list, &url, Some(i)) => list[i] = url,
            _ => return false,
        },
        ("up", Some(i)) if i > 0 => list.swap(i, i - 1),
        ("down", Some(i)) if i + 1 < list.len() => list.swap(i, i + 1),
        _ => return false,
    }
    true
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

fn bring_to_top(hwnd: Option<HWND>) {
    restack(hwnd, HWND_TOP);
}

fn send_to_bottom(hwnd: Option<HWND>) {
    restack(hwnd, HWND_BOTTOM);
}

fn restack(hwnd: Option<HWND>, after: HWND) {
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

/// Clip the page's host window to a rounded rect so it reads as a floating card.
// shortcut: window regions are not anti-aliased, so the corners are slightly jagged; upgrade with DirectComposition hosting.
fn round_corners(hwnd: Option<HWND>, window: &Window, settings: &Settings, bounds: Rect) {
    let Some(hwnd) = hwnd else { return };
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

/// Full screen and media events of a tab, which wry doesn't pass on.
fn watch_media(view: &WebView, id: u32, proxy: EventLoopProxy<UserEvent>) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8;
    use webview2_com::{ContainsFullScreenElementChangedEventHandler, IsDocumentPlayingAudioChangedEventHandler};
    use windows::core::{Interface, BOOL};
    let wv = view.webview();
    let p = proxy.clone();
    let full = ContainsFullScreenElementChangedEventHandler::create(Box::new(move |w, _| {
        if let Some(w) = w {
            let mut on = BOOL(0);
            unsafe { w.ContainsFullScreenElement(&mut on)? };
            let _ = p.send_event(UserEvent::Fullscreen(id, on.as_bool()));
        }
        Ok(())
    }));
    unsafe {
        let _ = wv.add_ContainsFullScreenElementChanged(&full, &mut 0);
    }
    let Ok(wv8) = wv.cast::<ICoreWebView2_8>() else { return };
    let audio = IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |w, _| {
        if let Some(w) = w.and_then(|w| w.cast::<ICoreWebView2_8>().ok()) {
            let mut on = BOOL(0);
            unsafe { w.IsDocumentPlayingAudio(&mut on)? };
            let _ = proxy.send_event(UserEvent::Playing(id, on.as_bool()));
        }
        Ok(())
    }));
    unsafe {
        let _ = wv8.add_IsDocumentPlayingAudioChanged(&audio, &mut 0);
    }
}

/// The web pages of a saved session and which of them was open (counted among those pages).
fn parse_session(text: &str) -> (Vec<(String, String)>, usize) {
    let (mut pages, mut active_line, mut active) = (Vec::new(), usize::MAX, 0);
    for (i, line) in text.lines().enumerate() {
        let (a, b) = line.split_once('\t').unwrap_or((line, ""));
        if a == "active" {
            active_line = b.parse::<usize>().map_or(usize::MAX, |n| n + 1); // tab lines start after this one
        } else if a.starts_with("https://") || a.starts_with("http://") {
            if i <= active_line {
                active = pages.len(); // the open tab, or the last page before it if it was a new tab page
            }
            pages.push((a.to_string(), if b.is_empty() { a.to_string() } else { b.to_string() }));
        }
    }
    (pages, active)
}

/// Ask WebView2 to keep a hidden tab small (it trims caches and memory) or let a shown one use what it needs.
fn memory_target(view: &WebView, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows::core::Interface;
    if let Ok(wv) = view.webview().cast::<ICoreWebView2_19>() {
        let level = if low { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
        unsafe {
            let _ = wv.SetMemoryUsageTargetLevel(level);
        }
    }
}

fn clear_region(hwnd: Option<HWND>) {
    if let Some(hwnd) = hwnd {
        unsafe {
            SetWindowRgn(hwnd, None, true);
        }
    }
}

/// Whole-window opacity (page included). At 100% the layered style is removed so normal drawing costs nothing extra.
fn set_opacity(window: &Window, percent: u32) {
    let hwnd = HWND(window.hwnd() as _);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if percent >= 100 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED.0 as isize));
        } else {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
            let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), (percent * 255 / 100) as u8, LWA_ALPHA);
        }
    }
}

/// Shared by every built-in page: turns the four user colours into the full set of CSS variables
/// (washes are the text or accent colour at fixed opacities, so any palette stays coherent).
const THEME_JS: &str = r#"window.velocityTheme = s => {
  const hex = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
  const lum = c => (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255;
  const f = hex(s.frame), c = hex(s.card), t = hex(s.text), a = hex(s.accent);
  const mix = (x, y, k) => 'rgb(' + x.map((v, i) => Math.round(v * k + y[i] * (1 - k))).join(' ') + ')';
  const wash = (x, o) => 'rgb(' + x.join(' ') + ' / ' + o + ')';
  const neutral = a.join() === t.join();
  const v = {
    frame: s.frame, card: s.card, text: s.text, accent: s.accent,
    muted: mix(t, f, .62), 'muted-card': mix(t, c, .62),
    hair: wash(t, .1), hover: wash(t, .06), field: wash(t, .07), active: wash(a, neutral ? .11 : .2),
    'on-accent': lum(a) > .55 ? '#111111' : '#ffffff', 'font-size': s.fontSize + 'px'
  };
  const root = document.documentElement;
  for (const k in v) root.style.setProperty('--' + k, v[k]);
  root.dataset.dark = lum(f) < .5;
  root.style.zoom = s.uiScale / 100;
};
"#;

/// History suggestions under a search field, shared by the address bar and the new tab search.
/// `velocitySuggest(input, list, submit)`: asks Rust with `suggest:<text>` while typing; Rust answers by
/// calling `suggestions(text, items)`. Arrow keys walk the list (filling the field), Enter opens.
const SUGGEST_JS: &str = r#"window.velocitySuggest = (input, box, submit) => {
  let items = [], sel = -1, typed = '';
  const draw = () => {
    box.replaceChildren(...items.map((it, i) => {
      const li = document.createElement('li'), t = document.createElement('b'), u = document.createElement('span');
      li.role = 'option'; li.ariaSelected = i === sel; li.id = 'sg' + i;
      t.textContent = it.t || it.u;
      u.textContent = it.u.replace(/^https?:\/\/(www\.)?/, '').replace(/\/$/, '');
      li.append(t, u);
      li.onmousedown = e => { e.preventDefault(); input.value = it.u; close(); submit(); };
      return li;
    }));
    box.hidden = !items.length;
    input.setAttribute('aria-expanded', items.length > 0);
    if (sel >= 0) input.setAttribute('aria-activedescendant', 'sg' + sel); else input.removeAttribute('aria-activedescendant');
    box.dispatchEvent(new Event('change'));
  };
  const close = () => { if (items.length) { items = []; sel = -1; draw(); } };
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  box.setAttribute('role', 'listbox');
  window.suggestions = (q, list) => {
    if (q !== input.value.trim() || document.activeElement !== input) return; // an older answer
    items = list; sel = -1; draw();
  };
  input.addEventListener('input', () => {
    typed = input.value;
    const v = input.value.trim();
    v ? window.ipc.postMessage('suggest:' + v) : close();
  });
  input.addEventListener('keydown', e => {
    if (!items.length) return;
    const n = items.length;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      sel = e.key === 'ArrowDown' ? (sel + 1 >= n ? -1 : sel + 1) : (sel - 1 < -1 ? n - 1 : sel - 1);
      input.value = sel < 0 ? typed : items[sel].u;
      draw();
    } else if (e.key === 'Escape') {
      e.stopPropagation(); input.value = typed; close();
    } else if (e.key === 'Enter') close();
  });
  input.addEventListener('blur', close);
  return close;
};
"#;

/// Dark window border/shadow. (tao's with_theme(Dark) starts a borderless window minimized.)
fn dark_frame(window: &Window) {
    unsafe {
        let dark: i32 = 1;
        let hwnd = HWND(window.hwnd() as _);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
    }
}

/// Every profile with its picture and last use, for the pages: `[{n:name,a:"file?v=...",t:unix seconds}]`.
fn profiles_js(base: &Path) -> String {
    let items: Vec<String> = profiles::list(base)
        .iter()
        .map(|n| format!("{{n:{},a:{},t:{}}}", js_str(n), js_str(&profiles::avatar(base, n)), profiles::last_used(base, n)))
        .collect();
    format!("[{}]", items.join(","))
}

/// Extensions shipped next to velocity.exe (the download bundles uBlock Origin Lite in `extensions\`) are
/// copied into the data folder the first time; one that is already there is left alone, user changes included.
fn seed_extensions(bundled: &Path, data: &Path) {
    let Ok(entries) = std::fs::read_dir(bundled) else { return };
    for e in entries.flatten().filter(|e| e.path().join("manifest.json").is_file()) {
        let to = data.join(e.file_name());
        if !to.exists() && media::copy_dir(&e.path(), &to, &mut u64::MAX.clone()).is_err() {
            let _ = std::fs::remove_dir_all(&to); // half a copy would load as a broken extension
        }
    }
}

/// `velocity.exe <address>`, as Windows starts the default browser for a link: only web addresses, never files.
/// Sites that need what Velocity's engine doesn't have yet. Its Media Source Extensions play WebM
/// (VP8, VP9, Opus) and MP4 (H.264, AAC): YouTube and Twitch play. DRM (Widevine, PlayReady,
/// FairPlay, which Servo can't license) it can't have: Netflix, Spotify, and Vimeo, which serves
/// its videos with DRM only. Kick and Dailymotion aren't verified yet. They open in WebView2.
// shortcut: a fixed list; upgrade by testing each site on the engine (the DRM sites stay).
const WEBVIEW2_SITES: &[&str] = &[
    "netflix.com", "spotify.com", "disneyplus.com",
    "primevideo.com", "max.com", "hbomax.com", "vimeo.com", "dailymotion.com", "kick.com", "skyshowtime.com",
    "voyo.sk", "voyo.cz", "music.apple.com", "tv.apple.com", "soundcloud.com", "deezer.com", "tidal.com",
];

fn needs_webview2(url: &str) -> bool {
    let host = host(url).trim_start_matches("www.").to_ascii_lowercase();
    WEBVIEW2_SITES.iter().any(|site| host == *site || host.ends_with(&format!(".{site}")))
}

fn start_url() -> Option<String> {
    std::env::args().skip(1).find(|a| a.starts_with("https://") || a.starts_with("http://"))
}

fn main() -> wry::Result<()> {
    // Velocity's engine starts web content as copies of this exe: `velocity.exe --content-process <token>`.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--content-process") {
        let token = args.next().expect("--content-process needs a token");
        sandbox::use_low_integrity_caches();
        // Never run web content unrestricted: exit if the lockdown fails.
        if let Err(e) = sandbox::lock_down() {
            eprintln!("content process lockdown failed: {e}");
            std::process::exit(1);
        }
        // Layout runs here, in the content process: Stylo's prefs must be set here too.
        engine::set_style_prefs();
        servo::run_content_process(token);
        return Ok(());
    }
    let base = data_dir();
    std::fs::create_dir_all(base.join("extensions")).ok();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        seed_extensions(&exe_dir.join("extensions"), &base.join("extensions"));
    }
    // Started without --profile: with more than one profile, ask who's using Velocity first.
    let profile = match profiles::from_args(&base) {
        Some(p) => p,
        None if profiles::list(&base).len() > 1 => return picker::run(&base, start_url()),
        // One profile: that one, which isn't Default any more once Default was renamed or deleted.
        None => profiles::list(&base).remove(0),
    };
    let dir = profiles::dir(&base, &profile);
    let settings = Settings::load(&dir);
    let history = history::History::load(&dir);
    let downloads = downloads::Downloads::load(&dir);
    downloads::set_ask(settings.ask_download);
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
    dark_frame(&window);

    // One shared profile = one WebView2 process tree; all webviews must then use the same extension settings.
    let mut ctx = WebContext::new(Some(dir.join("profile")));
    let proxy = event_loop.create_proxy();
    let ui_proxy = proxy.clone();
    let ui = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("pages/sidebar.html"))
        .with_initialization_script(THEME_JS)
        .with_initialization_script(SUGGEST_JS)
        .with_background_color((r, g, b, 255))
        .with_browser_extensions_enabled(true)
        .with_ipc_handler(move |req| {
            let _ = ui_proxy.send_event(UserEvent::Ui(req.body().clone()));
        })
        .build_as_child(&window)?;
    media::map(&ui, media::BG_HOST, &media::bg_dir(&dir)); // window glass shows the wallpaper in the tab UI too
    media::map(&ui, profiles::AVATAR_HOST, &profiles::avatars_dir(&base));

    let essentials = load_essentials(&dir);
    let engine = std::env::var_os("VELOCITY_WEBVIEW2")
        .is_none()
        .then(|| engine::Engine::new(&window, &event_loop, proxy.clone(), &dir.join("engine"), &base.join("extensions").join("uBOLite")));
    let mut b = Browser {
        window, ui, ctx, base, dir, profile, history, downloads, downloads_unseen: false, close_after: Vec::new(), proxy, tabs: Vec::new(), active: 0, next_id: 1, essentials, settings,
        settings_view: None, settings_tab: None, peeking: false, minimized: false, fullscreen: false, playing: Default::default(), top_peek: false, we_dirs: Vec::new(), wallpapers: Vec::new(), arrange_next: false, settings_page: "",
        engine,
    };
    b.update_opacity();
    b.layout();
    let restored = b.restore_session();
    match start_url() {
        Some(url) => b.open_tab(&url),
        None if !restored => b.open_tab(NEW_TAB),
        None => {}
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let engine_window = b.engine.as_ref().map(|e| e.window_id());
        match event {
            Event::UserEvent(UserEvent::Engine) => {
                if let Some(engine) = &b.engine {
                    engine.spin();
                }
            }
            Event::UserEvent(UserEvent::EnginePopup) => b.adopt_popups(),
            Event::RedrawRequested(id) if Some(id) == engine_window => {
                if let Some(engine) = &b.engine {
                    engine.paint();
                }
            }
            Event::WindowEvent { window_id, event, .. } if Some(window_id) == engine_window => {
                let key = b.engine.as_ref().and_then(|e| e.handle(&event));
                if let Some(key) = key {
                    b.shortcut(key);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                b.shut_down();
                *control_flow = ControlFlow::Exit
            }
            // Minimising reports a 0x0 size: laying the pages out at that size would squash them to 1 px
            // (the new tab's effects then respawn in a corner), so they are hidden until the window comes back.
            Event::WindowEvent { event: WindowEvent::Resized(size), .. } => {
                let min = size.width == 0 || size.height == 0 || b.window.is_minimized();
                b.set_minimized(min);
                if !min {
                    b.layout();
                }
            }
            Event::WindowEvent { event: WindowEvent::CursorMoved { position, .. }, .. } => b.cursor_moved(position.x, position.y),
            Event::UserEvent(UserEvent::Ui(msg)) => {
                if !b.handle_ui(&msg) {
                    b.shut_down();
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::Setting(msg)) => b.handle_setting(&msg),
            Event::UserEvent(UserEvent::PageKey(k)) => b.shortcut(&k),
            Event::UserEvent(UserEvent::BgInstalled(name, we)) => b.background_installed(name, &we),
            Event::UserEvent(UserEvent::PagePos(p)) => {
                // "clock:<x>:<y>" or "search:<x>:<y>" in % of the page; only numbers pass Settings::set.
                let mut parts = p.splitn(3, ':');
                if let (Some(which @ ("clock" | "search")), Some(x), Some(y)) = (parts.next(), parts.next(), parts.next()) {
                    let ok = b.settings.set(&format!("{which}_x"), x) & b.settings.set(&format!("{which}_y"), y);
                    if ok && b.settings.set("nt_pos", "free") {
                        b.settings.save(&b.dir);
                        b.apply_settings();
                    }
                }
            }
            Event::UserEvent(UserEvent::PageNav(id, q)) => {
                if let Some(i) = b.index_of(id) {
                    let url = to_url(&q, b.settings.search_prefix());
                    b.navigate(i, &url);
                    if i == b.active {
                        b.activate(i);
                    }
                }
            }
            Event::UserEvent(UserEvent::NewTab(url)) => b.open_tab(&url),
            Event::UserEvent(UserEvent::UrlChanged(id, url)) => {
                if let Some(i) = b.index_of(id) {
                    // A page on the engine went (by a link) to a site it can't show yet: WebView2 takes over.
                    if needs_webview2(&url) && b.engine.as_ref().is_some_and(|e| e.has(id)) {
                        b.navigate(i, &url);
                        if i == b.active {
                            b.activate(i);
                        }
                    }
                    b.set_url(i, url);
                }
            }
            Event::UserEvent(UserEvent::Download(u)) => b.download_update(u),
            Event::UserEvent(UserEvent::Fullscreen(id, on)) => b.set_fullscreen(id, on),
            Event::UserEvent(UserEvent::Playing(id, on)) => {
                if on { b.playing.insert(id) } else { b.playing.remove(&id) };
                b.update_opacity();
            }
            Event::UserEvent(UserEvent::PageSuggest(id, q)) => {
                // Only our new tab page gets history; once the tab shows a site, the site could read the reply.
                if let Some(view) = b.index_of(id).map(|i| &b.tabs[i]).filter(|t| t.url.is_empty()).and_then(|t| t.view.as_ref()) {
                    let _ = view.evaluate_script(&b.suggestions_js(&q));
                }
            }
            Event::UserEvent(UserEvent::TitleChanged(id, title)) => {
                if let Some(i) = b.index_of(id) {
                    if b.settings.history && b.tabs[i].url.starts_with("http") {
                        b.history.record(&b.tabs[i].url.clone(), &title);
                    }
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
        assert_eq!(layout_rects(1000.0, 600.0, &s, false), ((0.0, 0.0, 240.0, 600.0), (240.0, 8.0, 752.0, 584.0)));
        s.sidebar_right = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).0 .0, 760.0, "sidebar hugs the right edge");
        s.tabs_top = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false), ((0.0, 0.0, 1000.0, TOP_BAR), (8.0, TOP_BAR, 984.0, 600.0 - TOP_BAR - 8.0)));
        s.compact = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).1 .1, 8.0, "compact leaves only the gap on top");
        s.compact = false;
        s.ui_scale = 150;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).0 .3, TOP_BAR * 1.5, "top bar grows with the interface size");
        s.tabs_top = false;
        s.sidebar_right = false;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).0 .2, 360.0, "sidebar too");
        s.bg_window = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).0 .2, 360.0, "window glass needs a background");
        s.bg = "bg.jpg".into();
        let (ui, card) = layout_rects(1000.0, 600.0, &s, false);
        assert_eq!((ui, card.0), ((0.0, 0.0, 1000.0, 600.0), 360.0), "glass: tab UI under the whole window, page unchanged");
        s.compact = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).0 .2, 360.0, "no window glass in compact mode");
        let s = Settings::default();
        let (ui, card) = layout_rects(1000.0, 600.0, &s, true);
        assert_eq!(ui, (0.0, 0.0, 1000.0, 600.0), "the strip is drawn by the tab UI under the page");
        assert_eq!((card.1, card.3), (TOP_STRIP, 600.0 - TOP_STRIP - 8.0), "the page slides down by the strip");
        let mut s = s;
        s.gap = 0;
        assert_eq!(layout_rects(1000.0, 600.0, &s, false).1 .1, PEEK_STRIP, "always room above the page to reach the strip");
        s.tabs_top = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s, true).0 .3, TOP_BAR, "no strip with tabs on top");
    }

    #[test]
    fn editing_essentials() {
        let mut l: Vec<String> = vec!["https://a.com".into(), "https://b.com".into()];
        assert!(edit_essentials(&mut l, "add:c.com"));
        assert_eq!(l[2], "https://c.com");
        assert!(!edit_essentials(&mut l, "add:a.com"), "no duplicate sites");
        assert!(!edit_essentials(&mut l, "add:rust lang"), "search text is not an address");
        assert!(!edit_essentials(&mut l, "add:javascript:alert(1)"));
        assert!(edit_essentials(&mut l, "edit:0:https://x.org/start?y=1"));
        assert_eq!(l[0], "https://x.org/start?y=1");
        assert!(!edit_essentials(&mut l, "edit:0:b.com"), "edit can't collide with another tile");
        assert!(edit_essentials(&mut l, "down:0") && l[1] == "https://x.org/start?y=1");
        assert!(!edit_essentials(&mut l, "down:2"), "last can't move down");
        assert!(edit_essentials(&mut l, "remove:1") && l.len() == 2);
        assert!(!edit_essentials(&mut l, "remove:9"));
    }

    #[test]
    fn seeding_extensions() {
        let root = std::env::temp_dir().join("velocity-seed-test");
        let _ = std::fs::remove_dir_all(&root);
        let (bundled, data) = (root.join("app"), root.join("data"));
        std::fs::create_dir_all(bundled.join("ubo/_locales")).unwrap();
        std::fs::write(bundled.join("ubo/manifest.json"), "{}").unwrap();
        std::fs::write(bundled.join("ubo/_locales/x.json"), "1").unwrap();
        std::fs::create_dir_all(bundled.join("not-an-extension")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        seed_extensions(&bundled, &data);
        assert!(data.join("ubo/_locales/x.json").is_file(), "copied with its folders");
        assert!(!data.join("not-an-extension").exists(), "only folders with a manifest");
        std::fs::write(data.join("ubo/manifest.json"), "user").unwrap();
        seed_extensions(&bundled, &data);
        assert_eq!(std::fs::read_to_string(data.join("ubo/manifest.json")).unwrap(), "user", "existing ones are kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sessions() {
        let text = "active\t2\nhttps://a.com/\tA\n\tNew tab\nhttps://b.com/\t\nfile:///x\tX\n";
        let (pages, active) = parse_session(text);
        assert_eq!(pages, [("https://a.com/".to_string(), "A".to_string()), ("https://b.com/".into(), "https://b.com/".into())]);
        assert_eq!(active, 1, "b.com was open; the new tab page before it isn't restored");
        assert_eq!(parse_session("active\t1\nhttps://a.com/\tA\n\tNew tab\n").1, 0, "an open new tab page falls back to the page before");
        assert!(parse_session("").0.is_empty());
    }

    #[test]
    fn webview2_sites() {
        assert!(!needs_webview2("https://www.youtube.com/watch?v=x"), "YouTube plays on the engine");
        assert!(!needs_webview2("https://www.twitch.tv/x"), "Twitch plays on the engine");
        assert!(needs_webview2("https://vimeo.com/1"));
        assert!(needs_webview2("https://open.spotify.com/track/1"));
        assert!(!needs_webview2("https://en.wikipedia.org/wiki/YouTube"));
        assert!(!needs_webview2("https://notyoutube.com/"));
        assert!(!needs_webview2("velocity:newtab"));
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.a.com:8080/x?y"), "www.a.com:8080");
        assert_eq!(host("https://a.com"), "a.com");
        assert_eq!(host("https://a.com#top"), "a.com");
        assert_eq!(host("about:blank"), "");
    }
}
