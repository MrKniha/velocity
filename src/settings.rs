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

/// Colour presets: (key, name, frame, card, text, accent). Graphite is the default and fully neutral
/// (accent = text). The named ones use the official published palettes (values checked against each
/// project's palette page on 2026-11-07): frame = the darker/"mantle" ground, card = the base ground.
pub const PRESETS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("graphite", "Graphite", "#18181b", "#202024", "#ececee", "#ececee"),
    ("black", "Black", "#000000", "#0c0c0c", "#f2f2f2", "#f2f2f2"),
    ("mocha", "Catppuccin Mocha", "#181825", "#1e1e2e", "#cdd6f4", "#cba6f7"),
    ("macchiato", "Catppuccin Macchiato", "#1e2030", "#24273a", "#cad3f5", "#c6a0f6"),
    ("tokyonight", "Tokyo Night", "#16161e", "#1a1b26", "#c0caf5", "#7aa2f7"),
    ("nord", "Nord", "#2e3440", "#3b4252", "#eceff4", "#88c0d0"),
    ("dracula", "Dracula", "#282a36", "#353747", "#f8f8f2", "#bd93f9"),
    ("gruvbox", "Gruvbox Dark", "#1d2021", "#282828", "#ebdbb2", "#fe8019"),
    ("everforest", "Everforest", "#232a2e", "#2d353b", "#d3c6aa", "#a7c080"),
    ("solarized", "Solarized Dark", "#002b36", "#073642", "#93a1a1", "#268bd2"),
    ("paper", "Paper", "#e8e8ea", "#fafafb", "#18181b", "#18181b"),
    ("latte", "Catppuccin Latte", "#e6e9ef", "#eff1f5", "#4c4f69", "#8839ef"),
    ("gruvboxlight", "Gruvbox Light", "#ebdbb2", "#fbf1c7", "#3c3836", "#af3a03"),
    ("solarizedlight", "Solarized Light", "#eee8d5", "#fdf6e3", "#586e75", "#268bd2"),
];

pub const BG_FITS: &[&str] = &["cover", "contain"];
/// New tab content position on a 3x3 grid: vertical (t/c/b) + horizontal (l/c/r).
/// "free" = the clock and the search block each sit where the user dragged them.
pub const NT_POSITIONS: &[&str] = &["tl", "tc", "tr", "cl", "cc", "cr", "bl", "bc", "br", "free"];
/// New tab element style: "frosted" blur, or "liquid" glass with refraction and a specular rim.
pub const GLASS: &[&str] = &["none", "frosted", "liquid"];
pub const NT_ORDERS: &[&str] = &["clock", "search"];
pub const EFFECTS: &[&str] = &["none", "snow", "rain", "stars"];
pub const CLOCK_STYLES: &[&str] = &["digital", "analog"];
pub const CLOCK_FORMATS: &[&str] = &["auto", "12", "24"];
pub const CLOCK_WEIGHTS: &[&str] = &["thin", "regular", "bold"];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub frame: String,       // window / tab UI background, "#rrggbb"
    pub card: String,        // built-in pages background
    pub text: String,        // text and icons; washes are derived from it
    pub accent: String,      // selection, focus and toggles
    pub opacity: u32,        // whole-window opacity in %, 40..=100
    pub font_size: u32,      // tab UI text size in px, 12..=16
    pub engine: String,      // key of ENGINES
    pub tabs_top: bool,      // horizontal tab bar instead of the sidebar
    pub sidebar_right: bool,
    pub sidebar_width: u32,  // logical px, 200..=360
    pub compact: bool,       // tab UI hidden until the cursor reaches its edge
    pub gap: u32,            // space around the page card, 0..=16
    pub radius: u32,         // page card corner radius, 0..=20
    pub start_essentials: bool,
    pub start_clock: bool,
    pub clock_style: String,  // CLOCK_STYLES
    pub clock_format: String, // CLOCK_FORMATS: follow the system, or force 12/24 hours
    pub clock_weight: String, // CLOCK_WEIGHTS (digital only)
    pub clock_size: u32,      // digital font size in px, 32..=120 (analog scales from it)
    pub clock_seconds: bool,
    pub clock_date: bool,
    pub bg: String,           // file name inside the data dir's `background` folder, or empty
    pub bg_ver: u32,          // bumped on every change so the page never shows a cached old file
    pub bg_dim: u32,          // darkening over the background in %, 0..=80
    pub bg_blur: u32,         // blur in px, 0..=20
    pub bg_fit: String,       // BG_FITS
    pub nt_pos: String,       // NT_POSITIONS
    pub nt_order: String,     // NT_ORDERS: what comes first, clock or search
    pub nt_search: bool,      // show the search field on the new tab page
    pub nt_width: u32,        // search field width in px, 320..=760
    pub effect: String,       // EFFECTS
    pub effect_amount: u32,   // particle count, 10..=300
    pub clock_x: u32,         // free position of the clock centre, % of the page, 0..=100
    pub clock_y: u32,
    pub search_x: u32,        // free position of the search block centre
    pub search_y: u32,
    pub ui_scale: u32,        // size of all of Velocity's own interface in %, 80..=150
    pub page_zoom: u32,       // default zoom for websites in %, 50..=200
    pub glass: String,        // GLASS
}

/// A background file name we wrote ourselves: "bg.<ext>" with a known media extension.
fn bg_file(v: &str) -> Option<String> {
    let ext = v.strip_prefix("bg.")?;
    MEDIA_EXTS.contains(&ext).then(|| v.to_string())
}

/// Extensions the new tab can show: images (GIF animates) and videos.
pub const MEDIA_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif", "mp4", "webm"];

impl Default for Settings {
    fn default() -> Self {
        let (_, _, frame, card, text, accent) = PRESETS[0];
        Settings {
            frame: frame.into(),
            card: card.into(),
            text: text.into(),
            accent: accent.into(),
            opacity: 100,
            font_size: 13,
            engine: "duckduckgo".into(),
            tabs_top: false,
            sidebar_right: false,
            sidebar_width: 240,
            compact: false,
            gap: 8,
            radius: 8,
            start_essentials: true,
            start_clock: true,
            clock_style: "digital".into(),
            clock_format: "auto".into(),
            clock_weight: "thin".into(),
            clock_size: 56,
            clock_seconds: false,
            clock_date: true,
            bg: String::new(),
            bg_ver: 0,
            bg_dim: 25,
            bg_blur: 0,
            bg_fit: "cover".into(),
            nt_pos: "cc".into(),
            nt_order: "clock".into(),
            nt_search: true,
            nt_width: 560,
            effect: "none".into(),
            effect_amount: 120,
            clock_x: 50,
            clock_y: 34,
            search_x: 50,
            search_y: 56,
            ui_scale: 100,
            page_zoom: 100,
            glass: "frosted".into(),
        }
    }
}

/// "#rrggbb" (any case) -> lowercase "#rrggbb"; anything else is rejected.
fn hex_color(v: &str) -> Option<String> {
    let h = v.strip_prefix('#')?;
    (h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("#{}", h.to_ascii_lowercase()))
}

fn rgb(hex: &str) -> (u8, u8, u8) {
    let c = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    (c(1), c(3), c(5))
}

impl Settings {
    fn path(dir: &Path) -> PathBuf {
        dir.join("settings.ini")
    }

    /// Unknown keys and invalid values are ignored, so an old or hand-edited file can't break startup.
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
            "frame={}\ncard={}\ntext={}\naccent={}\nopacity={}\nfont_size={}\nengine={}\ntabs_top={}\nsidebar_right={}\nsidebar_width={}\ncompact={}\ngap={}\nradius={}\nstart_essentials={}\nstart_clock={}\nclock_style={}\nclock_format={}\nclock_weight={}\nclock_size={}\nclock_seconds={}\nclock_date={}\nbg={}\nbg_ver={}\nbg_dim={}\nbg_blur={}\nbg_fit={}\nnt_pos={}\nnt_order={}\nnt_search={}\nnt_width={}\neffect={}\neffect_amount={}\nclock_x={}\nclock_y={}\nsearch_x={}\nsearch_y={}\nui_scale={}\npage_zoom={}\nglass={}\n",
            self.frame, self.card, self.text, self.accent, self.opacity, self.font_size, self.engine, self.tabs_top,
            self.sidebar_right, self.sidebar_width, self.compact, self.gap, self.radius, self.start_essentials, self.start_clock,
            self.clock_style, self.clock_format, self.clock_weight, self.clock_size, self.clock_seconds, self.clock_date,
            self.bg, self.bg_ver, self.bg_dim, self.bg_blur, self.bg_fit, self.nt_pos, self.nt_order, self.nt_search,
            self.nt_width, self.effect, self.effect_amount, self.clock_x, self.clock_y, self.search_x, self.search_y,
            self.ui_scale, self.page_zoom, self.glass
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
        macro_rules! put {
            ($field:expr, $v:expr) => {
                match $v {
                    Some(v) => $field = v,
                    None => return false,
                }
            };
        }
        match key {
            "preset" => match PRESETS.iter().find(|p| p.0 == value) {
                Some(&(_, _, f, c, t, a)) => {
                    (self.frame, self.card, self.text, self.accent) = (f.into(), c.into(), t.into(), a.into())
                }
                None => return false,
            },
            // Files from before colour settings stored only a theme.
            "theme" => return self.set("preset", if value == "light" { "paper" } else { "graphite" }),
            "frame" => put!(self.frame, hex_color(value)),
            "card" => put!(self.card, hex_color(value)),
            "text" => put!(self.text, hex_color(value)),
            "accent" => put!(self.accent, hex_color(value)),
            "opacity" => put!(self.opacity, num(40, 100)),
            "font_size" => put!(self.font_size, num(12, 16)),
            "engine" if ENGINES.iter().any(|e| e.0 == value) => self.engine = value.into(),
            "tabs_top" => put!(self.tabs_top, flag()),
            "sidebar_right" => put!(self.sidebar_right, flag()),
            "compact" => put!(self.compact, flag()),
            "start_essentials" => put!(self.start_essentials, flag()),
            "start_clock" => put!(self.start_clock, flag()),
            "clock_style" if CLOCK_STYLES.contains(&value) => self.clock_style = value.into(),
            "clock_format" if CLOCK_FORMATS.contains(&value) => self.clock_format = value.into(),
            "clock_weight" if CLOCK_WEIGHTS.contains(&value) => self.clock_weight = value.into(),
            "clock_size" => put!(self.clock_size, num(32, 120)),
            "clock_seconds" => put!(self.clock_seconds, flag()),
            "clock_date" => put!(self.clock_date, flag()),
            // Only names we generate ourselves, so the page can never be pointed at another file.
            "bg" => put!(self.bg, if value.is_empty() { Some(String::new()) } else { bg_file(value) }),
            "bg_ver" => put!(self.bg_ver, value.parse().ok()),
            "bg_dim" => put!(self.bg_dim, num(0, 80)),
            "bg_blur" => put!(self.bg_blur, num(0, 20)),
            "bg_fit" if BG_FITS.contains(&value) => self.bg_fit = value.into(),
            "nt_pos" if NT_POSITIONS.contains(&value) => self.nt_pos = value.into(),
            "nt_order" if NT_ORDERS.contains(&value) => self.nt_order = value.into(),
            "nt_search" => put!(self.nt_search, flag()),
            "nt_width" => put!(self.nt_width, num(320, 760)),
            "effect" if EFFECTS.contains(&value) => self.effect = value.into(),
            "effect_amount" => put!(self.effect_amount, num(10, 300)),
            "clock_x" => put!(self.clock_x, num(0, 100)),
            "clock_y" => put!(self.clock_y, num(0, 100)),
            "search_x" => put!(self.search_x, num(0, 100)),
            "search_y" => put!(self.search_y, num(0, 100)),
            "ui_scale" => put!(self.ui_scale, num(80, 150)),
            "page_zoom" => put!(self.page_zoom, num(50, 200)),
            "glass" if GLASS.contains(&value) => self.glass = value.into(),
            "sidebar_width" => put!(self.sidebar_width, num(200, 360)),
            "gap" => put!(self.gap, num(0, 16)),
            "radius" => put!(self.radius, num(0, 20)),
            _ => return false,
        }
        true
    }

    /// The preset whose four colours match exactly, if any (the settings page marks it).
    pub fn preset(&self) -> &'static str {
        PRESETS
            .iter()
            .find(|p| (p.2, p.3, p.4, p.5) == (&*self.frame, &*self.card, &*self.text, &*self.accent))
            .map_or("custom", |p| p.0)
    }

    pub fn search_prefix(&self) -> &'static str {
        ENGINES.iter().find(|e| e.0 == self.engine).map_or(ENGINES[0].2, |e| e.2)
    }

    pub fn engine_name(&self) -> &'static str {
        ENGINES.iter().find(|e| e.0 == self.engine).map_or(ENGINES[0].1, |e| e.1)
    }

    /// Window and tab-UI background; the page card floats on it.
    pub fn frame_rgb(&self) -> (u8, u8, u8) {
        rgb(&self.frame)
    }

    /// Background of the built-in pages (new tab, settings) before they paint.
    pub fn card_rgba(&self) -> (u8, u8, u8, u8) {
        let (r, g, b) = rgb(&self.card);
        (r, g, b, 255)
    }

    /// Object literal for the pages; every value is a validated hex colour, a key from a fixed set or a number.
    pub fn to_js(&self) -> String {
        format!(
            "{{frame:'{}',card:'{}',text:'{}',accent:'{}',preset:'{}',opacity:{},fontSize:{},engine:'{}',engineName:'{}',tabsTop:{},sidebarRight:{},sidebarWidth:{},compact:{},gap:{},radius:{},startEssentials:{},startClock:{},clockStyle:'{}',clockFormat:'{}',clockWeight:'{}',clockSize:{},clockSeconds:{},clockDate:{},bg:'{}',bgVer:{},bgDim:{},bgBlur:{},bgFit:'{}',ntPos:'{}',ntOrder:'{}',ntSearch:{},ntWidth:{},effect:'{}',effectAmount:{},clockX:{},clockY:{},searchX:{},searchY:{},uiScale:{},pageZoom:{},glass:'{}'}}",
            self.frame, self.card, self.text, self.accent, self.preset(), self.opacity, self.font_size, self.engine,
            self.engine_name(), self.tabs_top, self.sidebar_right, self.sidebar_width, self.compact, self.gap,
            self.radius, self.start_essentials, self.start_clock, self.clock_style, self.clock_format,
            self.clock_weight, self.clock_size, self.clock_seconds, self.clock_date, self.bg, self.bg_ver,
            self.bg_dim, self.bg_blur, self.bg_fit, self.nt_pos, self.nt_order, self.nt_search, self.nt_width,
            self.effect, self.effect_amount, self.clock_x, self.clock_y, self.search_x, self.search_y,
            self.ui_scale, self.page_zoom, self.glass
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_validates() {
        let mut s = Settings::default();
        assert!(s.set("frame", "#ABCDEF"));
        assert_eq!(s.frame, "#abcdef");
        assert!(!s.set("frame", "red"));
        assert!(!s.set("text", "#12345"));
        assert!(!s.set("accent", "#12345g"));
        assert!(!s.set("text", "#123456';alert(1)//"), "only plain hex reaches the pages");
        assert!(s.set("opacity", "10"));
        assert_eq!(s.opacity, 40);
        assert!(s.set("sidebar_width", "9999"));
        assert_eq!(s.sidebar_width, 360);
        assert!(!s.set("gap", "abc"));
        assert!(!s.set("engine", "evil"));
        assert!(!s.set("preset", "nope"));
        assert!(s.set("bg", "bg.gif") && s.bg == "bg.gif");
        assert!(!s.set("bg", "..\\..\\secret.txt"), "only our own bg.<media> names");
        assert!(!s.set("bg", "bg.exe"));
        assert!(s.set("bg", "") && s.bg.is_empty());
        assert!(s.set("nt_pos", "br") && !s.set("nt_pos", "xx"));
        assert!(s.set("effect", "snow") && !s.set("effect", "fire"));
        assert!(s.set("nt_pos", "free") && s.set("clock_x", "150") && s.clock_x == 100);
        assert!(s.set("ui_scale", "10") && s.ui_scale == 80);
        assert!(s.set("glass", "liquid") && !s.set("glass", "wood"));
        assert!(!s.set("nope", "1"));
    }

    #[test]
    fn presets() {
        let mut s = Settings::default();
        assert_eq!(s.preset(), "graphite");
        assert!(s.set("preset", "mocha"));
        assert_eq!((s.preset(), s.frame.as_str()), ("mocha", "#181825"));
        assert!(s.set("clock_style", "analog") && !s.set("clock_style", "sundial"));
        assert!(s.set("clock_size", "500") && s.clock_size == 120);
        s.set("accent", "#ff0000");
        assert_eq!(s.preset(), "custom");
        assert!(s.set("theme", "light"), "old theme key still loads");
        assert_eq!(s.preset(), "paper");
        assert_eq!(s.frame_rgb(), (0xe8, 0xe8, 0xea));
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join("velocity-settings-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Settings::default();
        s.set("preset", "nord");
        s.set("clock_format", "24");
        s.set("clock_seconds", "true");
        s.set("opacity", "85");
        s.set("tabs_top", "true");
        s.set("font_size", "15");
        s.save(&dir);
        assert_eq!(Settings::load(&dir), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
