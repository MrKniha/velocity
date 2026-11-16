//! What a page on the engine asks the browser to show: `<select>` lists, the right-click menu,
//! `alert()` / `confirm()` / `prompt()`, and file pickers. Servo leaves all of them to the
//! embedder; without this they never opened. Native Windows menus and dialogs.

use std::path::PathBuf;

use servo::{
    ContextMenuAction, ContextMenuItem, EmbedderControl, SelectElementOptionOrOptgroup, SimpleDialog, WebView,
};
use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ClientToScreen,
    UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, TrackPopupMenu, HMENU, MF_CHECKED, MF_DISABLED, MF_GRAYED,
        MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_TOPALIGN,
    },
};
use windows_core::HSTRING;

/// Show `control` for `webview`, whose page is drawn in window `hwnd` (its client area).
pub fn show(hwnd: isize, webview: &WebView, control: EmbedderControl) {
    let title = webview.url().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    match control {
        EmbedderControl::SelectElement(mut select) => {
            let rect = select.position();
            let selected = select.selected_options();
            let mut items = Vec::new();
            for entry in select.options() {
                match entry {
                    SelectElementOptionOrOptgroup::Option(o) => items.push(Item::choice(&o.label, o.id, !o.is_disabled, selected.contains(&o.id))),
                    SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                        items.push(Item::label(label));
                        for o in options {
                            items.push(Item::choice(&format!("    {}", o.label), o.id, !o.is_disabled, selected.contains(&o.id)));
                        }
                    },
                }
            }
            // shortcut: <select multiple> picks one option per opening; upgrade with a list that stays open.
            if let Some(id) = popup(hwnd, rect.min.x, rect.max.y, &items) {
                select.select(vec![id]);
            }
            select.submit();
        },
        EmbedderControl::ContextMenu(menu) => {
            let rect = menu.position();
            let actions: Vec<ContextMenuAction> = menu
                .items()
                .iter()
                .filter_map(|i| match i {
                    ContextMenuItem::Item { action, .. } => Some(*action),
                    ContextMenuItem::Separator => None,
                })
                .collect();
            let items: Vec<Item> = menu
                .items()
                .iter()
                .map(|i| match i {
                    ContextMenuItem::Item { label, action, enabled } => {
                        Item::choice(label, actions.iter().position(|a| a == action).unwrap_or(0), *enabled, false)
                    },
                    ContextMenuItem::Separator => Item::Separator,
                })
                .collect();
            match popup(hwnd, rect.min.x, rect.min.y, &items) {
                Some(i) => menu.select(actions[i]),
                None => menu.dismiss(),
            }
        },
        EmbedderControl::SimpleDialog(dialog) => {
            let message = dialog.message().to_string();
            let ask = |buttons| {
                rfd::MessageDialog::new().set_title(&title).set_description(&message).set_buttons(buttons).show()
            };
            match dialog {
                SimpleDialog::Alert(d) => {
                    ask(rfd::MessageButtons::Ok);
                    d.confirm();
                },
                SimpleDialog::Confirm(d) => match ask(rfd::MessageButtons::OkCancel) {
                    rfd::MessageDialogResult::Ok => d.confirm(),
                    _ => d.dismiss(),
                },
                // shortcut: no text field in a native message box, so prompt() answers with its
                // default value on OK; upgrade with a small input dialog if a site needs typing.
                SimpleDialog::Prompt(d) => match ask(rfd::MessageButtons::OkCancel) {
                    rfd::MessageDialogResult::Ok => d.confirm(),
                    _ => d.dismiss(),
                },
            }
        },
        EmbedderControl::FilePicker(mut picker) => {
            let extensions: Vec<String> = picker.filter_patterns().iter().map(|p| p.0.trim_start_matches('.').to_string()).collect();
            let mut dialog = rfd::FileDialog::new().set_title(&title);
            if !extensions.is_empty() {
                dialog = dialog.add_filter("Allowed files", &extensions);
            }
            let paths: Option<Vec<PathBuf>> =
                if picker.allow_select_multiple() { dialog.pick_files() } else { dialog.pick_file().map(|p| vec![p]) };
            match paths {
                Some(paths) => {
                    picker.select(&paths);
                    picker.submit();
                },
                None => picker.dismiss(),
            }
        },
        // A colour picker answers "no change" when dropped; the input method needs nothing shown
        // (keys already arrive as text).
        EmbedderControl::ColorPicker(_) | EmbedderControl::InputMethod(_) => {},
    }
}

enum Item {
    Choice { label: String, id: usize, enabled: bool, checked: bool },
    Label(String),
    Separator,
}

impl Item {
    fn choice(label: &str, id: usize, enabled: bool, checked: bool) -> Item {
        Item::Choice { label: label.to_string(), id, enabled, checked }
    }

    fn label(label: &str) -> Item {
        Item::Label(label.to_string())
    }
}

/// A native menu at `x`, `y` (the window's client pixels); the chosen item's id.
fn popup(hwnd: isize, x: i32, y: i32, items: &[Item]) -> Option<usize> {
    unsafe {
        let menu: HMENU = CreatePopupMenu().ok()?;
        for item in items {
            let _ = match item {
                // Menu ids start at 1: 0 means nothing was chosen.
                Item::Choice { label, id, enabled, checked } => {
                    let mut flags = MF_STRING;
                    if !enabled {
                        flags |= MF_GRAYED;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    AppendMenuW(menu, flags, id + 1, &HSTRING::from(label.replace('&', "&&")))
                },
                Item::Label(label) => AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, &HSTRING::from(label.replace('&', "&&"))),
                Item::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, None),
            };
        }
        let hwnd = HWND(hwnd as _);
        let mut at = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut at);
        let chosen = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY, at.x, at.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        (chosen.0 > 0).then(|| chosen.0 as usize - 1)
    }
}
