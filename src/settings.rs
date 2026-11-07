//! User settings: a flat `key=value` file in the data dir, validated on every write.

use std::path::{Path, PathBuf};

pub const ENGINES: &[(&str, &str, &str)] = &[
    ("duckduckgo", "DuckDuckGo", "https://duckduckgo.com/?q="),
    ("google", "Google", "https://www.google.com/search?q="),
    ("brave", "Brave Search", "https://search.brave.com/search?q="),
    ("startpage", "Startpage", "https://www.startpage.com/do/search?q="),
    ("ecosia", "Ecosia", "https://www.ecosia.org/search?q="),
    ("bing", "Bing", "https://www.bing.com/search?q="),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub theme: String,       // "dark" | "light"
    pub engine: String,      // key of ENGINES
    pub tabs_top: bool,      // horizontal tab bar instead of the sidebar
    pub sidebar_right: bool,
    pub sidebar_width: u32,  // logical px, 200..=360
    pub compact: bool,       // tab UI hidden until the cursor reaches its edge
    pub gap: u32,            // space around the page card, 0..=16
    pub radius: u32,         // page card corner radius, 0..=20
    pub start_essentials: bool,
    pub start_clock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "dark".into(),
            engine: "duckduckgo".into(),
            tabs_top: false,
            sidebar_right: false,
            sidebar_width: 240,
            compact: false,
            gap: 8,
            radius: 8,
            start_essentials: true,
            start_clock: true,
        }
    }
}

impl Settings {
    fn path(dir: &Path) -> PathBuf {
        dir.join("settings.ini")
    }

    /// Unknown keys (such as the retired `accent`) and invalid values are ignored,
    /// so an old or hand-edited file can't break startup.
    pub fn load(dir: &Path) -> Self {
        let mut s = Settings::default();
        if let Ok(text) = std::fs::read_to_string(Self::path(dir)) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    s.set(k.trim(), v.trim());
                }
            }
        }
        s
    }

    pub fn save(&self, dir: &Path) {
        let text = format!(
            "theme={}\nengine={}\ntabs_top={}\nsidebar_right={}\nsidebar_width={}\ncompact={}\ngap={}\nradius={}\nstart_essentials={}\nstart_clock={}\n",
            self.theme, self.engine, self.tabs_top, self.sidebar_right, self.sidebar_width,
            self.compact, self.gap, self.radius, self.start_essentials, self.start_clock
        );
        let _ = std::fs::write(Self::path(dir), text);
    }

    /// Apply one setting; returns false (and changes nothing) for unknown keys or invalid values.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let num = |lo: u32, hi: u32| value.parse::<u32>().ok().map(|n| n.clamp(lo, hi));
        let flag = || match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        match key {
            "theme" if value == "dark" || value == "light" => self.theme = value.into(),
            "engine" if ENGINES.iter().any(|e| e.0 == value) => self.engine = value.into(),
            "tabs_top" => match flag() { Some(b) => self.tabs_top = b, None => return false },
            "sidebar_right" => match flag() { Some(b) => self.sidebar_right = b, None => return false },
            "compact" => match flag() { Some(b) => self.compact = b, None => return false },
            "start_essentials" => match flag() { Some(b) => self.start_essentials = b, None => return false },
            "start_clock" => match flag() { Some(b) => self.start_clock = b, None => return false },
            "sidebar_width" => match num(200, 360) { Some(n) => self.sidebar_width = n, None => return false },
            "gap" => match num(0, 16) { Some(n) => self.gap = n, None => return false },
            "radius" => match num(0, 20) { Some(n) => self.radius = n, None => return false },
            _ => return false,
        }
        true
    }

    pub fn search_prefix(&self) -> &'static str {
        ENGINES.iter().find(|e| e.0 == self.engine).map_or(ENGINES[0].2, |e| e.2)
    }

    pub fn engine_name(&self) -> &'static str {
        ENGINES.iter().find(|e| e.0 == self.engine).map_or(ENGINES[0].1, |e| e.1)
    }

    /// Window and tab-UI background; the page card floats on it. Must match --frame in the pages.
    pub fn frame_rgb(&self) -> (u8, u8, u8) {
        if self.theme == "light" { (232, 232, 234) } else { (24, 24, 27) }
    }

    /// Background of the built-in pages (new tab, settings); must match --card in their CSS.
    pub fn card_rgba(&self) -> (u8, u8, u8, u8) {
        if self.theme == "light" { (250, 250, 251, 255) } else { (32, 32, 36, 255) }
    }

    /// Object literal for the pages; every value here is from a fixed set or a number.
    pub fn to_js(&self) -> String {
        format!(
            "{{theme:'{}',engine:'{}',engineName:'{}',tabsTop:{},sidebarRight:{},sidebarWidth:{},compact:{},gap:{},radius:{},startEssentials:{},startClock:{}}}",
            self.theme, self.engine, self.engine_name(), self.tabs_top, self.sidebar_right,
            self.sidebar_width, self.compact, self.gap, self.radius, self.start_essentials, self.start_clock
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_validates() {
        let mut s = Settings::default();
        assert!(s.set("theme", "light"));
        assert!(!s.set("theme", "neon"));
        assert_eq!(s.theme, "light");
        assert!(s.set("sidebar_width", "9999"));
        assert_eq!(s.sidebar_width, 360);
        assert!(!s.set("gap", "abc"));
        assert!(!s.set("engine", "evil"));
        assert!(!s.set("accent", "sky"), "accent is retired");
        assert!(!s.set("nope", "1"));
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join("velocity-settings-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Settings::default();
        s.set("tabs_top", "true");
        s.set("compact", "true");
        s.set("radius", "4");
        s.save(&dir);
        assert_eq!(Settings::load(&dir), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
