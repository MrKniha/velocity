# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

(Desktop browser for Windows. The UI is HTML/CSS rendered in WebView2 inside a native Rust window, so web design rules apply to every surface.)

## Users

People choosing a desktop web browser for daily use who want it to feel fast and light and to look and behave the way they set it up. Velocity is meant to be published to the public, in the same space as Zen Browser and Helium.

## Product Purpose

A fast, low-memory desktop browser with a minimal interface that can be configured in detail. Success: it starts and switches tabs quickly, stays light on RAM, and lets the user shape its look and behaviour without touching files.

## Positioning

Speed and configurability together: a lightweight shell (small Rust binary on the system WebView2 runtime, one shared process tree, built-in support for uBlock Origin Lite) that still exposes real settings for layout, theme and behaviour.

## Operating Context

- Windows 11, WebView2 runtime (Chromium-based engine shipped with Windows).
- Used all day as the primary browser: many tabs, frequent switching, searching from the new tab page.
- Extensions are loaded unpacked from `%LOCALAPPDATA%\velocity\extensions`; uBlock Origin Lite is the tested one.

## Capabilities and Constraints

- Vertical tab sidebar, Essentials (pinned sites), built-in new tab page, settings panel, compact mode (sidebar hidden, revealed at the window edge).
- Settings persist in `%LOCALAPPDATA%\velocity\settings.ini`: theme (dark/light), accent, search engine, sidebar side and width, compact mode, page card spacing and corner radius, new-tab clock and Essentials.
- Built-in pages are self-contained HTML strings: no network fonts or images may be loaded by browser chrome (privacy and speed).
- Undecided: workspaces, tab sleeping, history, downloads UI, sync.

## Brand Commitments

- Name: Velocity. Everything else (logo, palette, type, look) is open; the previous gradient-squircle logo is rejected as generic.

## Evidence on Hand

- No users, testimonials, benchmarks or press yet. Do not fabricate performance numbers or adoption claims.

## Product Principles

1. Speed is felt, not claimed: the chrome must never be the slow part.
2. Configurable without clutter: options live in settings, not on screen.
3. Private by default: browser chrome makes no third-party requests.
4. The page is the content; the chrome recedes.
