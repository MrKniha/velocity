//! Downloads: the engine's network layer saves them (`engine::follow_downloads`); Velocity follows their
//! progress and keeps a list per profile in `downloads.tsv`:
//! `<unix seconds>\t<state>\t<bytes>\t<url>\t<file path>`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LIMIT: usize = 200;

static NEXT: AtomicU32 = AtomicU32::new(1);

/// Whether new downloads ask where to save the file.
pub fn set_ask(ask: bool) {
    net::download::set_ask(ask); // the engine's network layer asks before it saves
}

/// What the engine reports about a download, sent to the event loop.
pub enum Update {
    Started { id: u32, tab: u32, url: String, path: String, total: i64 },
    /// Waiting for the user to pick where to save it; answered with `Engine::answer_download`.
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

/// An id for a download in Velocity's list.
pub fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
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
