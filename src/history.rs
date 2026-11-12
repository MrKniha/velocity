//! Browsing history of one profile: `history.tsv` lines of `<unix seconds>\t<url>\t<title>`.
//!
//! Visits are appended (cheap on every page load); on start the file is compacted to one line per
//! URL, newest first, capped at `LIMIT` entries.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LIMIT: usize = 5000;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub ts: u64,
    pub url: String,
    pub title: String,
}

pub struct History {
    path: PathBuf,
    /// Newest first, one entry per URL.
    pub entries: Vec<Entry>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Tabs and line breaks would break the file format; titles are also kept to a sane length.
fn clean(s: &str, max: usize) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect::<String>().trim().to_string()
}

impl History {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("history.tsv");
        let mut entries: Vec<Entry> = Vec::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let mut f = line.splitn(3, '\t');
                if let (Some(ts), Some(url), title) = (f.next(), f.next(), f.next().unwrap_or("")) {
                    if let Ok(ts) = ts.parse() {
                        entries.push(Entry { ts, url: url.to_string(), title: title.to_string() });
                    }
                }
            }
        }
        // Newest visit per URL wins; the file is append-only, so later lines are newer.
        entries.reverse();
        let mut seen = std::collections::HashSet::new();
        entries.retain(|e| seen.insert(e.url.clone()));
        entries.truncate(LIMIT);
        let h = History { path, entries };
        h.save();
        h
    }

    fn save(&self) {
        let text: String = self.entries.iter().rev().map(|e| format!("{}\t{}\t{}\n", e.ts, e.url, e.title)).collect();
        let _ = fs::write(&self.path, text);
    }

    /// Record a visit to an http(s) page.
    pub fn record(&mut self, url: &str, title: &str) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        let e = Entry { ts: now(), url: clean(url, 2048), title: clean(title, 300) };
        if self.entries.first().is_some_and(|x| x.url == e.url && x.title == e.title) {
            return; // same page reporting the same title again
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{}\t{}\t{}", e.ts, e.url, e.title);
        }
        self.entries.retain(|x| x.url != e.url);
        self.entries.insert(0, e);
        self.entries.truncate(LIMIT);
    }

    pub fn remove(&mut self, url: &str) {
        self.entries.retain(|e| e.url != url);
        self.save();
    }

    /// Add visits from another browser (any order); the newest visit per address wins. Returns how many were new.
    pub fn import(&mut self, visits: Vec<(u64, String, String)>) -> usize {
        let before = self.entries.len();
        for (ts, url, title) in visits {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            let e = Entry { ts, url: clean(&url, 2048), title: clean(&title, 300) };
            match self.entries.iter_mut().find(|x| x.url == e.url) {
                Some(x) if x.ts < e.ts => *x = e,
                Some(_) => {}
                None => self.entries.push(e),
            }
        }
        self.entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        self.entries.truncate(LIMIT);
        self.save();
        self.entries.len().saturating_sub(before)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    /// Entries matching every word of `q` in the URL or title, newest first.
    pub fn search(&self, q: &str, limit: usize) -> Vec<&Entry> {
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        self.entries
            .iter()
            .filter(|e| {
                let hay = format!("{} {}", e.url, e.title).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .take(limit)
            .collect()
    }

    /// Address bar suggestions: pages whose address starts with what was typed come first
    /// ("git" finds github.com before a page that merely mentions git), then other matches.
    pub fn suggest(&self, q: &str, limit: usize) -> Vec<&Entry> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let bare = |u: &str| {
            let u = u.split_once("://").map_or(u, |(_, r)| r);
            u.strip_prefix("www.").unwrap_or(u).to_lowercase()
        };
        let mut hits = self.search(&q, 200);
        hits.sort_by_key(|e| !bare(&e.url).starts_with(&q)); // stable: keeps newest first within each group
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_compact_search() {
        let dir = std::env::temp_dir().join("velocity-history-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut h = History::load(&dir);
        h.record("https://www.rust-lang.org/", "Rust Programming Language");
        h.record("https://github.com/rust-lang/rust", "rust-lang/rust · GitHub");
        h.record("about:blank", "ignored");
        h.record("https://www.rust-lang.org/", "Rust\tProgramming\nLanguage");
        assert_eq!(h.entries.len(), 2, "one entry per URL, non-web pages skipped");
        assert_eq!(h.entries[0].title, "Rust Programming Language", "control characters are cleaned");

        let h = History::load(&dir); // compaction keeps the newest visit per URL
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].url, "https://www.rust-lang.org/");
        assert_eq!(h.search("rust github", 10).len(), 1);
        let s = h.suggest("git", 5);
        assert_eq!(s[0].url, "https://github.com/rust-lang/rust", "address prefix ranks first");
        assert!(h.suggest("  ", 5).is_empty());

        let mut h = h;
        let added = h.import(vec![
            (5, "https://www.rust-lang.org/".into(), "older visit".into()),
            (9_999_999_999, "https://imported.example/".into(), "Imported".into()),
            (1, "file:///C:/x.html".into(), "local".into()),
        ]);
        assert_eq!(added, 1, "a known page isn't added twice, files are skipped");
        assert_eq!(h.entries[0].url, "https://imported.example/", "sorted newest first");
        assert_ne!(h.entries.iter().find(|e| e.url == "https://www.rust-lang.org/").unwrap().title, "older visit");
        h.remove("https://imported.example/");
        h.remove("https://github.com/rust-lang/rust");
        assert_eq!(History::load(&dir).entries.len(), 1);
        h.clear();
        assert!(History::load(&dir).entries.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
