# Velocity

Velocity is a desktop web browser for Windows. It is written in Rust and renders every page, including its own tabs and settings, with a patched build of [Servo](https://servo.org). It does not use Chromium, WebView2 or any other system web runtime.

The goal is a browser that starts fast, stays light on memory, and can be set up in detail from its settings page.

> [!WARNING]
> Velocity is early software. Servo does not render every site correctly yet, and settings, data formats and features can change between versions.

## Features

- Vertical tabs in a sidebar, with Essentials (pinned sites) and a compact mode that hides the sidebar until the mouse reaches the window edge.
- A built-in new tab page with a clock, search and an optional background image or video. Wallpaper Engine backgrounds can be imported.
- Ad and tracker blocking with the rulesets of [uBlock Origin Lite](https://github.com/uBlockOrigin/uBOL-home). The engine applies them in its network layer, so there is no extension runtime.
- Web pages run in separate content processes, each locked down by Velocity's own sandbox before it receives a page.
- Profiles, each in its own window with its own settings, history, cookies and site data.
- History, downloads, and import of history, bookmarks and Firefox cookies from other browsers installed on the same PC.
- Encrypted media (EME with PlayReady through Windows Media Foundation), so services such as Netflix can play.
- Updates from this repository's GitHub Releases. The installer is checked against its published SHA-256 before it runs.

## Building

Velocity builds on Windows 10 or 11 (x64) only.

You need:

- [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Visual Studio C++ build tools
- Python with libclang: `pip install libclang`
- GStreamer for audio and video: `winget install gstreamerproject.gstreamer`
- [velocity-engine](https://github.com/MrKniha/velocity-engine) checked out next to this repository

```powershell
git clone https://github.com/MrKniha/velocity.git
git clone https://github.com/MrKniha/velocity-engine.git
cd velocity
powershell -File scripts\build.ps1
```

The build writes `target\release\velocity.exe` and copies the DLLs the engine needs (ANGLE and GStreamer) next to it. The first build compiles Servo and takes a while; a rebuild after engine changes takes about five minutes.

`scripts\package.ps1` builds the portable zip and, if [Inno Setup](https://jrsoftware.org/isinfo.php) is installed, the installer. It needs a copy of uBlock Origin Lite, by default from `%LOCALAPPDATA%\velocity\extensions\uBOLite` (pass `-Ubo <folder>` to use another).

## Repository layout

| Path | Contents |
|---|---|
| `src/` | The browser: window, tab UI, settings, profiles, downloads, sandbox, media and updates |
| `src/pages/` | Velocity's own pages (new tab, settings, sidebar, profile picker) as HTML |
| `bench/` | Memory, speed and real-site benchmarks, and their recorded results |
| `scripts/` | Build and packaging scripts |
| `installer/` | Inno Setup script for the Windows installer |

The engine, with the patched Servo crates and a list of every patch, lives in [velocity-engine](https://github.com/MrKniha/velocity-engine).

## Benchmarks

The scripts in `bench/` measure the whole process tree (browser, content and GPU helper processes) on a hidden desktop, so a run does not take over the screen. `bench/memory.ps1` uses local fixture pages and repeats a fixed set of scenarios; `bench/realsites.ps1` loads 15 live sites, one per launch and then all as tabs. Results of earlier runs are in `bench/results/`.

## Contributing

Bug reports and pull requests are welcome. For a rendering bug, please include the URL and a screenshot. A change to memory use or speed should come with before and after numbers from the bench scripts.

## License

Velocity is licensed under the [Mozilla Public License 2.0](LICENSE).

Release builds include third-party software under its own license: Servo (MPL-2.0), uBlock Origin Lite (GPL-3.0), GStreamer and FFmpeg (LGPL-2.1 or later), and ANGLE (BSD-3-Clause). `scripts\package.ps1` writes these notices into every package.
