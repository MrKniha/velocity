//! Downloads: Velocity takes them over from WebView2 (whose own bubble doesn't show reliably in our child
//! webviews), follows their progress and keeps a list per profile in `downloads.tsv`:
//! `<unix seconds>\t<state>\t<bytes>\t<url>\t<file path>`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Deferral, ICoreWebView2DownloadOperation, ICoreWebView2DownloadStartingEventArgs, ICoreWebView2_4,
    COREWEBVIEW2_DOWNLOAD_STATE,
    COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED, COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS,
};
use webview2_com::{take_pwstr, BytesReceivedChangedEventHandler, DownloadStartingEventHandler, StateChangedEventHandler};
use windows::core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

const LIMIT: usize = 200;

static NEXT: AtomicU32 = AtomicU32::new(1);

thread_local! {
    // Running downloads, so they can be cancelled. WebView2 objects stay on the UI thread.
    static RUNNING: RefCell<HashMap<u32, ICoreWebView2DownloadOperation>> = RefCell::new(HashMap::new());
    // Downloads held back until the user picks where to save them ("Ask where to save each file").
    static WAITING: RefCell<HashMap<u32, (ICoreWebView2DownloadStartingEventArgs, ICoreWebView2Deferral)>> = RefCell::new(HashMap::new());
    static ASK: Cell<bool> = const { Cell::new(true) };
}

/// Whether new downloads ask where to save the file.
pub fn set_ask(ask: bool) {
    ASK.with(|a| a.set(ask));
}

/// Answer a download that is waiting for a place to save: `Some(path)` saves it there, `None` cancels it.
/// Returns whether it goes ahead.
pub fn answer(id: u32, choice: Option<&Path>) -> bool {
    let Some((args, deferral)) = WAITING.with(|w| w.borrow_mut().remove(&id)) else { return false };
    let go = unsafe {
        match choice {
            Some(path) => args.SetResultFilePath(&HSTRING::from(path.as_os_str())).is_ok(),
            None => {
                let _ = args.SetCancel(true);
                false
            }
        }
    };
    if !go {
        RUNNING.with(|r| r.borrow_mut().remove(&id));
    }
    unsafe {
        let _ = deferral.Complete();
    }
    go
}

/// What WebView2 reports about a download, sent to the event loop.
pub enum Update {
    Started { id: u32, tab: u32, url: String, path: String, total: i64 },
    /// Waiting for the user to pick where to save it; answer with `answer`.
    Ask { id: u32, tab: u32, url: String, path: String, total: i64 },
    Progress { id: u32, received: i64, total: i64 },
    Finished { id: u32, ok: bool, path: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    Done,
    Failed, // interrupted or cancelled
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub ts: u64,
    pub url: String,
    pub path: String,
    pub received: i64,
    pub total: i64, // -1 when the server doesn't say
    pub state: State,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Take over the downloads of one tab.
pub fn watch(view: &WebView, tab: u32, send: impl Fn(Update) + Clone + 'static) {
    let Ok(wv) = view.webview().cast::<ICoreWebView2_4>() else { return };
    let handler = DownloadStartingEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        unsafe {
            args.SetHandled(true)?; // our list replaces WebView2's own download bubble
            let op = args.DownloadOperation()?;
            let mut p = PWSTR::null();
            args.ResultFilePath(&mut p)?;
            let path = take_pwstr(p);
            let mut u = PWSTR::null();
            op.Uri(&mut u)?;
            let url = take_pwstr(u);
            let mut total = -1i64;
            let _ = op.TotalBytesToReceive(&mut total);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);

            let progress = send.clone();
            op.add_BytesReceivedChanged(
                &BytesReceivedChangedEventHandler::create(Box::new(move |op, _| {
                    if let Some(op) = op {
                        let (mut received, mut total) = (0i64, -1i64);
                        op.BytesReceived(&mut received)?;
                        let _ = op.TotalBytesToReceive(&mut total);
                        progress(Update::Progress { id, received, total });
                    }
                    Ok(())
                })),
                &mut 0,
            )?;
            let finished = send.clone();
            op.add_StateChanged(
                &StateChangedEventHandler::create(Box::new(move |op, _| {
                    let Some(op) = op else { return Ok(()) };
                    let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
                    op.State(&mut state)?;
                    if state != COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS {
                        let mut p = PWSTR::null();
                        let path = if op.ResultFilePath(&mut p).is_ok() { take_pwstr(p) } else { String::new() };
                        RUNNING.with(|r| r.borrow_mut().remove(&id));
                        finished(Update::Finished { id, ok: state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED, path });
                    }
                    Ok(())
                })),
                &mut 0,
            )?;
            RUNNING.with(|r| r.borrow_mut().insert(id, op));
            if ASK.with(|a| a.get()) {
                // The dialog can't open inside this callback: hold the download and ask from the event loop.
                let deferral = args.GetDeferral()?;
                WAITING.with(|w| w.borrow_mut().insert(id, (args.clone(), deferral)));
                send(Update::Ask { id, tab, url, path, total });
            } else {
                send(Update::Started { id, tab, url, path, total });
            }
        }
        Ok(())
    }));
    unsafe {
        let _ = wv.add_DownloadStarting(&handler, &mut 0);
    }
}

/// An id for a download that didn't come from WebView2 (the engine's), from the same counter.
pub fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub fn cancel(id: u32) {
    if let Some(op) = RUNNING.with(|r| r.borrow().get(&id).cloned()) {
        unsafe {
            let _ = op.Cancel();
        }
    }
}

pub struct Downloads {
    path: PathBuf,
    /// Newest first.
    pub items: Vec<Item>,
}

impl Downloads {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("downloads.tsv");
        let items = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.splitn(5, '\t').collect();
                let [ts, state, bytes, url, file] = f[..] else { return None };
                let bytes = bytes.parse().unwrap_or(0);
                Some(Item {
                    id: NEXT.fetch_add(1, Ordering::Relaxed),
                    ts: ts.parse().ok()?,
                    url: url.to_string(),
                    path: file.to_string(),
                    received: bytes,
                    total: bytes,
                    // A download still running when Velocity closed didn't finish.
                    state: if state == "done" { State::Done } else { State::Failed },
                })
            })
            .collect();
        Downloads { path, items }
    }

    pub fn save(&self) {
        let text: String = self
            .items
            .iter()
            .filter(|i| i.state != State::Running)
            .map(|i| {
                let state = if i.state == State::Done { "done" } else { "failed" };
                let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
                format!("{}\t{state}\t{}\t{}\t{}\n", i.ts, i.received.max(0), clean(&i.url), clean(&i.path))
            })
            .collect();
        let _ = fs::write(&self.path, text);
    }

    /// Apply one update; returns whether the list should be redrawn now (progress is throttled to 1 % steps).
    pub fn apply(&mut self, u: Update) -> bool {
        match u {
            Update::Started { id, url, path, total, .. } => {
                self.items.insert(0, Item { id, ts: now(), url, path, received: 0, total, state: State::Running });
                self.items.truncate(LIMIT);
                true
            }
            Update::Ask { .. } => false,
            Update::Progress { id, received, total } => {
                let Some(i) = self.items.iter_mut().find(|i| i.id == id) else { return false };
                let pct = |r: i64, t: i64| if t > 0 { r * 100 / t } else { r >> 20 }; // unknown size: every MB
                let changed = pct(i.received, i.total) != pct(received, total);
                (i.received, i.total) = (received, total);
                changed
            }
            Update::Finished { id, ok, path } => {
                let Some(i) = self.items.iter_mut().find(|i| i.id == id) else { return false };
                i.state = if ok { State::Done } else { State::Failed };
                if ok && i.total > 0 {
                    i.received = i.total;
                }
                if !path.is_empty() {
                    i.path = path;
                }
                self.save();
                true
            }
        }
    }

    pub fn remove(&mut self, id: u32) {
        self.items.retain(|i| i.id != id || i.state == State::Running);
        self.save();
    }

    pub fn clear(&mut self) {
        self.items.retain(|i| i.state == State::Running);
        self.save();
    }

    /// Running downloads and their overall progress in % (-1 when a size is unknown).
    pub fn progress(&self) -> (usize, i64) {
        let running: Vec<&Item> = self.items.iter().filter(|i| i.state == State::Running).collect();
        let (r, t) = running.iter().fold((0i64, 0i64), |(r, t), i| (r + i.received, t + i.total));
        let known = running.iter().all(|i| i.total > 0);
        (running.len(), if known && t > 0 { r * 100 / t } else { -1 })
    }

    pub fn get(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_lifecycle() {
        let dir = std::env::temp_dir().join("velocity-downloads-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut d = Downloads::load(&dir);
        let start = |id, total| Update::Started { id, tab: 1, url: "https://a.com/f.zip".into(), path: "C:\\f.zip".into(), total };
        assert!(d.apply(start(901, 1000)));
        assert!(!d.apply(Update::Progress { id: 901, received: 5, total: 1000 }), "under 1 % changes nothing");
        assert!(d.apply(Update::Progress { id: 901, received: 500, total: 1000 }));
        assert_eq!(d.progress(), (1, 50));
        assert!(d.apply(start(902, -1)));
        assert_eq!(d.progress(), (2, -1), "an unknown size makes the total unknown");
        d.apply(Update::Finished { id: 901, ok: true, path: "C:\\f (1).zip".into() });
        d.apply(Update::Finished { id: 902, ok: false, path: String::new() });
        assert_eq!(d.progress().0, 0);

        let d2 = Downloads::load(&dir);
        assert_eq!(d2.items.len(), 2);
        assert_eq!(d2.items[1].path, "C:\\f (1).zip", "the final file name is kept");
        assert_eq!(d2.items[1].state, State::Done);
        assert_eq!(d2.items[0].state, State::Failed);

        let mut d2 = d2;
        let first = d2.items[0].id;
        d2.remove(first);
        assert_eq!(Downloads::load(&dir).items.len(), 1);
        d2.clear();
        assert!(Downloads::load(&dir).items.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
