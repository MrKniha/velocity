//! Import from other browsers installed for this Windows user: history and bookmarks-bar sites from Chromium
//! browsers (Chrome, Edge, Brave, Opera, Vivaldi) and Firefox, plus cookies (staying logged in to sites) from
//! Firefox only. Chromium browsers seal cookies and passwords so that only they can read them, so those can't
//! be brought over. Files are copied first: a running browser keeps them locked.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

pub struct Browser {
    pub id: &'static str,
    pub name: &'static str,
    /// Its profile folder (Chromium "Default", or the default Firefox profile).
    pub dir: PathBuf,
    pub firefox: bool,
}

impl Browser {
    pub fn has_history(&self) -> bool {
        self.dir.join(if self.firefox { "places.sqlite" } else { "History" }).is_file()
    }
    pub fn has_bookmarks(&self) -> bool {
        self.dir.join(if self.firefox { "places.sqlite" } else { "Bookmarks" }).is_file()
    }
    pub fn has_cookies(&self) -> bool {
        self.firefox && self.dir.join("cookies.sqlite").is_file()
    }
}

/// The browsers found for this user, with a default profile.
pub fn find() -> Vec<Browser> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    let chromium = [
        ("chrome", "Google Chrome", local.join(r"Google\Chrome\User Data\Default")),
        ("edge", "Microsoft Edge", local.join(r"Microsoft\Edge\User Data\Default")),
        ("brave", "Brave", local.join(r"BraveSoftware\Brave-Browser\User Data\Default")),
        ("vivaldi", "Vivaldi", local.join(r"Vivaldi\User Data\Default")),
        ("opera", "Opera", roaming.join(r"Opera Software\Opera Stable\Default")),
        ("operagx", "Opera GX", roaming.join(r"Opera Software\Opera GX Stable\Default")),
    ];
    let mut found: Vec<Browser> = chromium
        .into_iter()
        .map(|(id, name, dir)| Browser { id, name, dir, firefox: false })
        .filter(|b| b.has_history() || b.has_bookmarks())
        .collect();
    if let Some(dir) = firefox_profile(&roaming.join(r"Mozilla\Firefox")) {
        found.push(Browser { id: "firefox", name: "Firefox", dir, firefox: true });
    }
    found
}

/// The default Firefox profile from profiles.ini: the [Install…] section's Default=, else the profile marked Default=1.
fn firefox_profile(root: &Path) -> Option<PathBuf> {
    let ini = fs::read_to_string(root.join("profiles.ini")).ok()?;
    let (mut install, mut marked, mut path, mut relative, mut default) = (None, None, None::<String>, true, false);
    let mut in_install = false;
    let mut flush = |path: &Option<String>, relative: bool, default: bool| {
        if default && marked.is_none() {
            marked = path.as_ref().map(|p| if relative { root.join(p) } else { PathBuf::from(p) });
        }
    };
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(&path, relative, default);
            (path, relative, default) = (None, true, false);
            in_install = line.starts_with("[Install");
        } else if let Some((k, v)) = line.split_once('=') {
            match (in_install, k) {
                (true, "Default") => install = install.or(Some(root.join(v))),
                (false, "Path") => path = Some(v.to_string()),
                (false, "IsRelative") => relative = v == "1",
                (false, "Default") => default = v == "1",
                _ => {}
            }
        }
    }
    flush(&path, relative, default);
    install.or(marked).filter(|p| p.join("places.sqlite").is_file())
}

/// Open a copy of a database (and its write-ahead log) so a running browser's lock doesn't get in the way.
fn open_copy(file: &Path, tmp: &Path) -> Result<Connection, String> {
    let name = file.file_name().unwrap_or_default();
    let copy = tmp.join(name);
    fs::copy(file, &copy).map_err(|_| "couldn't read its data: close it and try again".to_string())?;
    for side in ["-wal", "-journal"] {
        let mut extra = file.as_os_str().to_owned();
        extra.push(side);
        let mut to = copy.as_os_str().to_owned();
        to.push(side);
        let _ = fs::copy(PathBuf::from(extra), PathBuf::from(to));
    }
    Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e| e.to_string())
}

fn temp_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("velocity-import-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// (unix seconds, url, title), newest first, at most `limit`.
pub fn history(b: &Browser, limit: usize) -> Result<Vec<(u64, String, String)>, String> {
    let tmp = temp_dir()?;
    let (file, sql) = if b.firefox {
        ("places.sqlite", "SELECT last_visit_date / 1000000, url, IFNULL(title, '') FROM moz_places WHERE last_visit_date IS NOT NULL ORDER BY last_visit_date DESC LIMIT ?1")
    } else {
        // Chromium counts microseconds since 1601.
        ("History", "SELECT last_visit_time / 1000000 - 11644473600, url, IFNULL(title, '') FROM urls WHERE last_visit_time > 0 ORDER BY last_visit_time DESC LIMIT ?1")
    };
    let db = open_copy(&b.dir.join(file), &tmp)?;
    let rows = db
        .prepare(sql)
        .and_then(|mut q| {
            q.query_map([limit as i64], |r| Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string());
    drop(db);
    let _ = fs::remove_dir_all(&tmp);
    rows.map(|v| v.into_iter().filter(|(_, u, _): &(u64, String, String)| u.starts_with("http")).collect())
}

/// The sites on the bookmarks bar (folders flattened), in order.
pub fn bookmarks_bar(b: &Browser) -> Result<Vec<String>, String> {
    if b.firefox {
        let tmp = temp_dir()?;
        let db = open_copy(&b.dir.join("places.sqlite"), &tmp)?;
        let rows = db
            .prepare(
                "WITH RECURSIVE bar(id) AS (SELECT id FROM moz_bookmarks WHERE guid = 'toolbar_____' \
                 UNION ALL SELECT b.id FROM moz_bookmarks b JOIN bar ON b.parent = bar.id WHERE b.type = 2) \
                 SELECT p.url FROM moz_bookmarks b JOIN moz_places p ON b.fk = p.id WHERE b.type = 1 AND b.parent IN bar ORDER BY b.parent, b.position",
            )
            .and_then(|mut q| q.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
            .map_err(|e| e.to_string());
        drop(db);
        let _ = fs::remove_dir_all(&tmp);
        return rows;
    }
    let text = fs::read_to_string(b.dir.join("Bookmarks")).map_err(|_| "couldn't read its bookmarks".to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    fn walk(node: &serde_json::Value, out: &mut Vec<String>) {
        for child in node["children"].as_array().into_iter().flatten() {
            match child["type"].as_str() {
                Some("url") => out.extend(child["url"].as_str().map(String::from)),
                Some("folder") => walk(child, out),
                _ => {}
            }
        }
    }
    walk(&json["roots"]["bookmark_bar"], &mut out);
    Ok(out)
}

pub struct Cookie {
    pub name: String,
    pub value: String,
    pub host: String,
    pub path: String,
    pub expires: f64, // unix seconds
    pub secure: bool,
    pub http_only: bool,
    pub same_site: i64, // Firefox: 0 none, 1 lax, 2 strict
}

/// Firefox's cookies that haven't expired yet.
pub fn firefox_cookies(b: &Browser) -> Result<Vec<Cookie>, String> {
    let tmp = temp_dir()?;
    let db = open_copy(&b.dir.join("cookies.sqlite"), &tmp)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let rows = db
        .prepare("SELECT name, value, host, path, expiry, isSecure, isHttpOnly, IFNULL(sameSite, 0) FROM moz_cookies")
        .and_then(|mut q| {
            q.query_map([], |r| {
                let expiry: i64 = r.get(4)?;
                // Newer Firefox versions store milliseconds, older ones seconds.
                let expires = if expiry > 100_000_000_000 { expiry as f64 / 1000.0 } else { expiry as f64 };
                Ok(Cookie {
                    name: r.get(0)?,
                    value: r.get(1)?,
                    host: r.get(2)?,
                    path: r.get(3)?,
                    expires,
                    secure: r.get::<_, i64>(5)? != 0,
                    http_only: r.get::<_, i64>(6)? != 0,
                    same_site: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string());
    drop(db);
    let _ = fs::remove_dir_all(&tmp);
    rows.map(|v| v.into_iter().filter(|c| c.expires > now && !c.host.is_empty()).collect())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Reads the browsers really installed on this PC (copies only); run with `cargo test real_browsers -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_browsers() {
        for b in find() {
            let h = history(&b, 5000).map(|v| v.len());
            let bm = bookmarks_bar(&b).map(|v| v.len());
            let c = if b.has_cookies() { firefox_cookies(&b).map(|v| v.len()) } else { Ok(0) };
            println!("{}: history {h:?}, bookmarks bar {bm:?}, cookies {c:?}", b.name);
        }
    }

    #[test]
    fn chromium_history_and_bookmarks() {
        let dir = temp("velocity-import-chromium");
        let db = Connection::open(dir.join("History")).unwrap();
        db.execute_batch(
            "CREATE TABLE urls (id INTEGER PRIMARY KEY, url TEXT, title TEXT, last_visit_time INTEGER);
             INSERT INTO urls (url, title, last_visit_time) VALUES
               ('https://old.example/', 'Old', 13000000000000000),
               ('https://new.example/', 'New', 13400000000000000),
               ('chrome://settings/', 'Settings', 13500000000000000);",
        )
        .unwrap();
        drop(db);
        fs::write(
            dir.join("Bookmarks"),
            r#"{"roots":{"bookmark_bar":{"children":[{"type":"url","url":"https://github.com/"},
                {"type":"folder","children":[{"type":"url","url":"https://news.ycombinator.com/"}]}]}}}"#,
        )
        .unwrap();
        let b = Browser { id: "chrome", name: "Chrome", dir: dir.clone(), firefox: false };
        let h = history(&b, 10).unwrap();
        assert_eq!(h.len(), 2, "only web pages");
        assert_eq!(h[0].1, "https://new.example/");
        assert_eq!(h[0].0, 13400000000 - 11644473600, "1601 microseconds become unix seconds");
        assert_eq!(bookmarks_bar(&b).unwrap(), ["https://github.com/", "https://news.ycombinator.com/"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn firefox_profile_history_bookmarks_cookies() {
        let root = temp("velocity-import-firefox");
        let prof = root.join("Profiles").join("abc.default-release");
        fs::create_dir_all(&prof).unwrap();
        fs::write(root.join("profiles.ini"), "[Profile0]\nName=x\nIsRelative=1\nPath=Profiles/other\n\n[Install1234]\nDefault=Profiles/abc.default-release\n").unwrap();
        let db = Connection::open(prof.join("places.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT, last_visit_date INTEGER);
             CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, parent INTEGER, position INTEGER, guid TEXT);
             INSERT INTO moz_places VALUES (1, 'https://mozilla.org/', 'Mozilla', 1700000000000000), (2, 'https://wiki.example/', NULL, NULL);
             INSERT INTO moz_bookmarks VALUES (10, 2, NULL, 1, 0, 'toolbar_____'), (11, 1, 2, 10, 0, 'a'), (12, 1, 1, 99, 0, 'b');",
        )
        .unwrap();
        drop(db);
        let db = Connection::open(prof.join("cookies.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER);
             INSERT INTO moz_cookies VALUES ('sid', 'v', '.example.com', '/', 4102444800000, 1, 1, 1), ('old', 'v', 'example.com', '/', 1000, 0, 0, 0);",
        )
        .unwrap();
        drop(db);
        assert_eq!(firefox_profile(&root), Some(root.join("Profiles/abc.default-release")), "the [Install] default wins");
        let b = Browser { id: "firefox", name: "Firefox", dir: prof, firefox: true };
        let h = history(&b, 10).unwrap();
        assert_eq!(h, [(1700000000, "https://mozilla.org/".to_string(), "Mozilla".to_string())]);
        assert_eq!(bookmarks_bar(&b).unwrap(), ["https://wiki.example/"], "only the toolbar");
        let c = firefox_cookies(&b).unwrap();
        assert_eq!(c.len(), 1, "expired cookies are skipped");
        assert_eq!((c[0].expires, c[0].same_site), (4102444800.0, 1), "milliseconds become seconds");
        let _ = fs::remove_dir_all(&root);
    }
}
