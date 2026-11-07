# Velocity design

Neutral, quiet browser chrome. Greys only: no accent colour, no gradients, no decorative backgrounds. The page is the only colourful thing on screen.

## Colour

The user owns the palette. Settings store four colours (`frame`, `card`, `text`, `accent`, validated `#rrggbb`) and fourteen presets fill them in: Graphite (default), Black and Paper of our own, plus the official palettes of Catppuccin (Mocha, Macchiato, Latte), Tokyo Night, Nord, Dracula, Gruvbox (dark, light), Everforest and Solarized (dark, light), mapped as frame = darker ground, card = base, text, accent. The settings page groups them into Dark and Light by frame luminance and draws each as a miniature window. `THEME_JS` in `main.rs` derives every other token from those four, the same way for all built-in pages: muted = text mixed 62% into the ground, washes = text at fixed opacities, active = accent at 11% (20% when the accent differs from the text). The table below is Graphite and Paper. Window opacity (40–100%) and tab UI text size (12–16px) are settings too; opacity uses a layered window, and is switched off entirely at 100%.

| Token | Dark | Light | Use |
|---|---|---|---|
| `--frame` | `#18181b` | `#e8e8ea` | Window, sidebar / top bar. Must equal `Settings::frame_rgb` in `src/settings.rs`. |
| `--card` | `#202024` | `#fafafb` | Built-in pages (new tab, settings). Must equal `Settings::card_rgba`. |
| `--text` | `#ececee` | `#18181b` | Primary text, active tab |
| `--muted` | `#a1a1aa` | `#52525b` | Secondary text, idle icons |
| `--hover` | white 5% | black 4.5% | Hover wash |
| `--active` | white 10% | `#ffffff` | Selected tab / tile |
| `--field` | white 6% | black 5% | Address bar, tiles |
| `--hair` | white 8% | black 8% | Dividers, focused field ring |

The only non-neutral colour in the chrome is the platform close-button hover red (`#c42b1c`). Favicons keep their own colours; in dark theme they get a 0.8px white halo so dark icons (GitHub) stay visible.

## Type

System UI face (`Segoe UI Variable`), by product constraint: chrome makes no network requests. 13px in the tab UI, 13px body in settings with 20px title, 56px light clock on the new tab page. Rank by weight and colour, not size.

## Shape

6px icon buttons, 8px fields, tabs and segmented controls, 10–12px tiles and settings groups. The page card radius and the gap around it are user settings (defaults 8 and 8). Flat: no shadows except the window's own.

## Layout

Two tab layouts, switched in settings (`tabs_top`):

- Sidebar (default): window controls, address bar, Essentials grid (4 columns), "New tab", vertical tab list, footer with logo, compact toggle and settings. Width and side are settings.
- Top bar (80px, `TOP_BAR` in `main.rs`): tab row with "+" right after the last tab and window controls on the right; toolbar row with navigation, address bar, Essentials icons, compact and settings.

Compact mode hides either layout; the cursor reaching that window edge reveals it.

## Logo

A white dot with two fading trails (35% and 20% white) on a `#141416` tile with 14/64 corners: a point in motion. Monochrome. `assets/logo.svg` and `src/icon.rs` draw the same shapes.

## New tab

Background: an image, GIF or video the user picks (or a Wallpaper Engine wallpaper: videos as-is, scene/web ones by their preview image) is copied to `<data>/background/bg.<ext>` and shown full-bleed with user-set darkening, blur and fill/fit. With a background on, text gets a soft shadow and the search field and tiles turn into dark frosted glass. The content block (clock, search, Essentials) sits on a 3x3 position grid, with clock-first or search-first order and a width setting, or in Free mode the clock and the search block each sit at their own % position, dragged in place with the new tab's Arrange button. Glass styles for the search field, tiles and analog face: Plain, Frosted (blur over a background) and Liquid (an SVG displacement map bends the backdrop, plus a bright inner rim and soft drop). Interface size zooms all of Velocity's own pages (the tab UI box grows with it); Website zoom is the default zoom for sites.

## Motion

Chrome: none; hovers and state changes switch instantly. New tab only: optional snow, rain or stars (snow follows pure-snow.js, MIT), all off under reduced motion.

## Settings page

Left category navigation (Appearance, New tab, Clock, Effects, Essentials, Tabs and window, Search, Shortcuts) with one category shown at a time; it collapses to a horizontal strip under 720px.
