# Velocity design

Neutral, quiet browser chrome. Greys only: no accent colour, no gradients, no decorative backgrounds. The page is the only colourful thing on screen.

## Colour

Dark is the default; light is a full alternative. Every neutral is a ground or a white/black wash at a fixed opacity.

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

## Motion

None. Hovers and state changes switch instantly.
