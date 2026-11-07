// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod history;
mod icon;
mod media;
mod profiles;
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
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOP,
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
    : ({t: 'new', w: 'close', l: 'focus', ',': 'settings', Tab: e.shiftKey ? 'prev' : 'next'})[e.key];
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
    BgInstalled(Option<media::Installed>, String),
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
    base: PathBuf, // the app's data folder; extensions are shared by all profiles
    dir: PathBuf,  // this profile's folder
    profile: String,
    history: history::History,
    proxy: EventLoopProxy<UserEvent>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    essentials: Vec<String>,
    settings: Settings,
    settings_view: Option<WebView>,
    settings_tab: Option<u32>, // id of the tab the settings panel was opened on
    peeking: bool,
    we_dirs: Vec<PathBuf>,
    wallpapers: Vec<media::Wallpaper>,
    arrange_next: bool, // the next new tab opens in arrange mode
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
            "if(!/^https?:$/.test(location.protocol)){{{THEME_JS}{SUGGEST_JS}window.VELOCITY={};window.ESSENTIALS=[{}];window.ENGINES=[{}];window.PRESETS=[{}];window.FRAME={};window.ARRANGE={};}}",
            self.settings.to_js(),
            essentials.join(","),
            engines.join(","),
            presets.join(","),
            self.frame_js(),
            self.arrange_next
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
        round_corners(&view, &self.window, &self.settings, bounds);
        if new_tab {
            media::map(&view, media::BG_HOST, &media::bg_dir(&self.dir));
        } else {
            let _ = view.zoom(self.settings.page_zoom as f64 / 100.0);
        }
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
                let _ = tab.view.zoom(self.settings.page_zoom as f64 / 100.0);
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
        self.active = index;
        self.tabs[index].unseen = false;
        // Show the new page before hiding the old one, so the empty frame never flashes in between.
        let _ = self.tabs[index].view.set_visible(true);
        for (_, t) in self.tabs.iter().enumerate().filter(|(i, _)| *i != index) {
            let _ = t.view.set_visible(false);
        }
        self.show_settings_if_owner();
        self.sync_ui();
    }

    /// The settings panel belongs to the tab it was opened on: it shows only while that tab is active.
    fn show_settings_if_owner(&self) {
        if let Some(view) = &self.settings_view {
            let mine = self.settings_tab == Some(self.tabs[self.active].id);
            let _ = view.set_visible(mine);
            if mine {
                bring_to_top(view); // tabs opened since then sit above it
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
        self.essentials_changed();
    }

    /// Save the Essentials and show the new list everywhere it is drawn.
    fn essentials_changed(&self) {
        save_essentials(&self.dir, &self.essentials);
        self.sync_ui();
        let list: Vec<String> = self.essentials.iter().map(|e| js_str(e)).collect();
        let js = format!("renderEssentials([{}])", list.join(","));
        for view in self.tabs.iter().filter(|t| t.url.is_empty()).map(|t| &t.view).chain(self.settings_view.as_ref()) {
            let _ = view.evaluate_script(&js);
        }
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
        let profile = if self.profile == profiles::DEFAULT { String::new() } else { format!(" ({})", self.profile) };
        self.window.set_title(&format!("{} - {APP}{profile}", cur.title));
        let _ = self.ui.evaluate_script(&format!(
            "render([{}],{},[{}],{},{})",
            tabs.join(","),
            cur.id,
            essentials.join(","),
            self.settings.to_js(),
            js_str(&self.profile)
        ));
        self.push_frame();
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
        self.push_frame();
    }

    fn set_peek(&mut self, peek: bool) {
        if self.settings.compact && self.peeking != peek {
            self.peeking = peek;
            self.layout();
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
        round_corners(&view, &self.window, &self.settings, bounds);
        media::map(&view, media::BG_HOST, &media::bg_dir(&self.dir));
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
        let (ui, card) = layout_rects(w, h, &self.settings);
        format!("{{uiX:{},uiY:{},cardX:{},cardY:{},w:{},h:{}}}", ui.0, ui.1, card.0, card.1, w, h)
    }

    fn push_frame(&self) {
        let js = format!("window.velocityFrame&&velocityFrame({})", self.frame_js());
        let _ = self.ui.evaluate_script(&js);
        for t in self.tabs.iter().filter(|t| t.url.is_empty()) {
            let _ = t.view.evaluate_script(&js);
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
                    let _ = self.current().evaluate_script("arrange(true)");
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
            _ => {}
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
                    Some(n) => profiles::launch(&n),
                    None => self.settings_js("profileError('That name is taken or not allowed.')"),
                },
                ("open", Some(n)) => profiles::launch(&n),
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

    fn show_profiles(&self) {
        let names: Vec<String> = profiles::list(&self.base).iter().map(|n| js_str(n)).collect();
        self.settings_js(&format!("renderProfiles([{}],{})", names.join(","), js_str(&self.profile)));
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
        let (r, g, b) = self.settings.frame_rgb();
        self.window.set_background_color(Some((r, g, b, 255)));
        let _ = self.ui.set_background_color((r, g, b, 255));
        set_opacity(&self.window, self.settings.opacity);
        let card = self.settings.card_rgba();
        for t in self.tabs.iter().filter(|t| t.url.is_empty()).map(|t| &t.view).chain(self.settings_view.as_ref()) {
            let _ = t.set_background_color(card);
        }
        let zoom = self.settings.page_zoom as f64 / 100.0;
        for t in self.tabs.iter().filter(|t| !t.url.is_empty()) {
            let _ = t.view.zoom(zoom);
        }
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
            bring_to_top(&self.ui);
        } else {
            bring_to_top(self.current()); // with window glass the tab UI lies under the page
            self.show_settings_if_owner();
        }
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
        ((ui_x, 0.0, sw, h), (left, gap, (w - left - right).max(1.0), (h - 2.0 * gap).max(1.0)))
    };
    // Window glass: the tab UI's webview spans the whole window under the page, so the wallpaper also shows
    // in the gaps around the page. Not in compact mode, where the tab UI is raised over the page to peek.
    if window_glass(s) { ((0.0, 0.0, w, h), card) } else { (ui, card) }
}

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

fn main() -> wry::Result<()> {
    let base = data_dir();
    std::fs::create_dir_all(base.join("extensions")).ok();
    let profile = profiles::from_args(&base);
    let dir = profiles::dir(&base, &profile);
    let settings = Settings::load(&dir);
    let history = history::History::load(&dir);
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
        .with_initialization_script(THEME_JS)
        .with_initialization_script(SUGGEST_JS)
        .with_background_color((r, g, b, 255))
        .with_browser_extensions_enabled(true)
        .with_ipc_handler(move |req| {
            let _ = ui_proxy.send_event(UserEvent::Ui(req.body().clone()));
        })
        .build_as_child(&window)?;
    media::map(&ui, media::BG_HOST, &media::bg_dir(&dir)); // window glass shows the wallpaper in the tab UI too

    let essentials = load_essentials(&dir);
    let mut b = Browser {
        window, ui, ctx, base, dir, profile, history, proxy, tabs: Vec::new(), active: 0, next_id: 1, essentials, settings,
        settings_view: None, settings_tab: None, peeking: false, we_dirs: Vec::new(), wallpapers: Vec::new(), arrange_next: false,
    };
    set_opacity(&b.window, b.settings.opacity);
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
                    let _ = b.tabs[i].view.load_url(&to_url(&q, b.settings.search_prefix()));
                }
            }
            Event::UserEvent(UserEvent::NewTab(url)) => b.open_tab(&url),
            Event::UserEvent(UserEvent::UrlChanged(id, url)) => {
                if let Some(i) = b.index_of(id) {
                    b.set_url(i, url);
                }
            }
            Event::UserEvent(UserEvent::PageSuggest(id, q)) => {
                // Only our new tab page gets history; once the tab shows a site, the site could read the reply.
                if let Some(t) = b.index_of(id).map(|i| &b.tabs[i]).filter(|t| t.url.is_empty()) {
                    let _ = t.view.evaluate_script(&b.suggestions_js(&q));
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
        assert_eq!(layout_rects(1000.0, 600.0, &s), ((0.0, 0.0, 240.0, 600.0), (240.0, 8.0, 752.0, 584.0)));
        s.sidebar_right = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .0, 760.0, "sidebar hugs the right edge");
        s.tabs_top = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s), ((0.0, 0.0, 1000.0, TOP_BAR), (8.0, TOP_BAR, 984.0, 600.0 - TOP_BAR - 8.0)));
        s.compact = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).1 .1, 8.0, "compact leaves only the gap on top");
        s.compact = false;
        s.ui_scale = 150;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .3, TOP_BAR * 1.5, "top bar grows with the interface size");
        s.tabs_top = false;
        s.sidebar_right = false;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .2, 360.0, "sidebar too");
        s.bg_window = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .2, 360.0, "window glass needs a background");
        s.bg = "bg.jpg".into();
        let (ui, card) = layout_rects(1000.0, 600.0, &s);
        assert_eq!((ui, card.0), ((0.0, 0.0, 1000.0, 600.0), 360.0), "glass: tab UI under the whole window, page unchanged");
        s.compact = true;
        assert_eq!(layout_rects(1000.0, 600.0, &s).0 .2, 360.0, "no window glass in compact mode");
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
    fn hosts() {
        assert_eq!(host("https://www.a.com:8080/x?y"), "www.a.com:8080");
        assert_eq!(host("https://a.com"), "a.com");
        assert_eq!(host("https://a.com#top"), "a.com");
        assert_eq!(host("about:blank"), "");
    }
}
