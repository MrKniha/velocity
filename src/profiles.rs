//! Profiles: each one is a folder with its own settings, Essentials, history, background and engine
//! data (`engine/`: cookies, logins, site storage). "Default" is the data folder itself, so existing data stays
//! where it was; the others live in `profiles/<name>`. Each profile runs in its own window
//! (`velocity.exe --profile <name>`).

use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT: &str = "Default";

/// What belongs to the Default profile inside the data folder (the rest there is shared: extensions, pictures,
/// other profiles). Renaming Default moves these into its new folder; deleting it removes them.
const DEFAULT_ITEMS: &[&str] = &["settings.ini", "essentials.txt", "history.tsv", "downloads.tsv", "session.tsv", "background", "profile"];

fn default_exists(base: &Path) -> bool {
    DEFAULT_ITEMS.iter().any(|i| base.join(i).exists())
}

/// Move the Default profile's items to `to`, all or nothing (a running Default holds its files open).
fn move_default(base: &Path, to: &Path) -> bool {
    let mut moved = Vec::new();
    for item in DEFAULT_ITEMS.iter().filter(|i| base.join(i).exists()) {
        if fs::rename(base.join(item), to.join(item)).is_err() {
            for back in moved {
                let _ = fs::rename(to.join(back), base.join(back));
            }
            return false;
        }
        moved.push(*item);
    }
    true
}

/// A name the user typed, if it can be a folder name on Windows: 1-32 letters, digits, spaces, - and _.
pub fn valid_name(name: &str) -> Option<String> {
    let name = name.trim();
    let chars_ok = name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_');
    // Device names like CON or NUL can't be used as folder names.
    let upper = name.to_uppercase();
    let device = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4);
    let len = name.chars().count();
    (chars_ok && (1..=32).contains(&len) && !device).then(|| name.to_string())
}

pub fn dir(base: &Path, name: &str) -> PathBuf {
    if name.eq_ignore_ascii_case(DEFAULT) { base.to_path_buf() } else { base.join("profiles").join(name) }
}

/// "Default" first (unless it was deleted or renamed and other profiles exist), then the others alphabetically.
pub fn list(base: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(base.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().and_then(valid_name))
        .filter(|n| !n.eq_ignore_ascii_case(DEFAULT))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    if names.is_empty() || default_exists(base) {
        names.insert(0, DEFAULT.to_string());
    }
    names
}

/// Create a new, empty profile; None if the name is invalid or taken.
pub fn create(base: &Path, name: &str) -> Option<String> {
    let name = valid_name(name)?;
    let taken = list(base).iter().any(|n| n.eq_ignore_ascii_case(&name)) || name.eq_ignore_ascii_case(DEFAULT);
    if taken {
        return None;
    }
    fs::create_dir_all(dir(base, &name)).ok()?;
    Some(name)
}

/// Give a profile a new name (Default too: its files move into a folder of their own). Fails, changing nothing,
/// while the profile is open somewhere, or when the name is invalid or taken.
pub fn rename(base: &Path, old: &str, new: &str) -> bool {
    let Some(new) = valid_name(new) else { return false };
    let known = list(base).into_iter().any(|n| n == old);
    let taken = list(base).iter().any(|n| n.eq_ignore_ascii_case(&new)) || new.eq_ignore_ascii_case(DEFAULT);
    if !known || taken {
        return false;
    }
    let to = dir(base, &new);
    let ok = if old == DEFAULT {
        fs::create_dir_all(&to).is_ok() && (move_default(base, &to) || fs::remove_dir(&to).is_err() && false)
    } else {
        fs::rename(dir(base, old), &to).is_ok()
    };
    if ok {
        if let Some(pic) = avatar_file(base, old) {
            let ext = pic.extension().and_then(|e| e.to_str()).unwrap_or("png").to_string();
            let _ = fs::rename(pic, avatars_dir(base).join(format!("{new}.{ext}")));
        }
    }
    ok
}

/// Delete a profile and all its data. Fails while the profile is open somewhere (the engine holds its files),
/// in which case nothing is deleted: the folder is moved aside first, which Windows refuses while files are in use.
pub fn delete(base: &Path, name: &str) -> bool {
    let Some(name) = valid_name(name) else { return false };
    let trash = base.join(format!("deleting-{name}"));
    let _ = fs::remove_dir_all(&trash); // left over from an earlier delete that was interrupted
    if name.eq_ignore_ascii_case(DEFAULT) {
        // Only Default's own files; the data folder also holds what all profiles share.
        if !default_exists(base) || fs::create_dir_all(&trash).is_err() || !move_default(base, &trash) {
            return false;
        }
    } else {
        let from = dir(base, &name);
        if !from.is_dir() || fs::rename(&from, &trash).is_err() {
            return false;
        }
    }
    let _ = fs::remove_dir_all(&trash);
    remove_avatar(base, &name);
    true
}

/// Profile pictures live together in `avatars/<profile>.<ext>`, the only folder the built-in pages can read
/// them from (`https://avatars.velocity.example/`), so no page can reach a profile's own data.
pub const AVATAR_HOST: &str = "avatars.velocity.example";
const AVATAR_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const AVATAR_MAX: u64 = 10 * 1024 * 1024;

pub fn avatars_dir(base: &Path) -> PathBuf {
    base.join("avatars")
}

fn avatar_file(base: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(avatars_dir(base)).ok()?.flatten().map(|e| e.path()).find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        stem.eq_ignore_ascii_case(name) && AVATAR_EXTS.contains(&ext.as_str())
    })
}

/// When the profile was last used (unix seconds, 0 = never): its history file is rewritten on every start
/// and every visit.
pub fn last_used(base: &Path, name: &str) -> u64 {
    fs::metadata(dir(base, name).join("history.tsv")).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// "<file name>?v=<modified time>" of the profile's picture, or empty when it has none.
pub fn avatar(base: &Path, name: &str) -> String {
    let Some(path) = avatar_file(base, name) else { return String::new() };
    let ver = fs::metadata(&path).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    format!("{}?v={ver}", path.file_name().and_then(|f| f.to_str()).unwrap_or(""))
}

/// Copy an image as the profile's picture, replacing the old one.
pub fn set_avatar(base: &Path, name: &str, src: &Path) -> bool {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let size_ok = fs::metadata(src).is_ok_and(|m| m.is_file() && m.len() <= AVATAR_MAX);
    if !AVATAR_EXTS.contains(&ext.as_str()) || !size_ok || valid_name(name).is_none() {
        return false;
    }
    remove_avatar(base, name);
    let _ = fs::create_dir_all(avatars_dir(base));
    fs::copy(src, avatars_dir(base).join(format!("{name}.{ext}"))).is_ok()
}

pub fn remove_avatar(base: &Path, name: &str) {
    while let Some(p) = avatar_file(base, name) {
        if fs::remove_file(p).is_err() {
            break;
        }
    }
}

/// Open a profile in a new window, optionally at a web address (a link Windows handed to Velocity).
pub fn launch(name: &str, url: Option<&str>) {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--profile").arg(name);
        if let Some(url) = url {
            cmd.arg(url);
        }
        let _ = cmd.spawn();
    }
}

/// The profile named on the command line (`--profile <name>`), created if needed; None without the flag.
pub fn from_args(base: &Path) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--profile")?;
    Some(match args.get(i + 1).and_then(|n| valid_name(n)) {
        Some(n) if !n.eq_ignore_ascii_case(DEFAULT) => list(base)
            .into_iter()
            .find(|x| x.eq_ignore_ascii_case(&n))
            .or_else(|| create(base, &n))
            .unwrap_or_else(|| DEFAULT.to_string()),
        _ => DEFAULT.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_lifecycle() {
        assert_eq!(valid_name("  Work "), Some("Work".into()));
        for bad in ["", "a/b", "..", "con", "COM1", "x\\y", &"a".repeat(33)] {
            assert_eq!(valid_name(bad), None, "{bad}");
        }
        let base = std::env::temp_dir().join("velocity-profiles-test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        assert_eq!(list(&base), ["Default"], "a fresh install has just Default");
        fs::write(base.join("history.tsv"), "").unwrap(); // Default has been used
        assert_eq!(create(&base, "Work"), Some("Work".into()));
        assert_eq!(create(&base, "work"), None, "names are case-insensitive");
        assert_eq!(create(&base, "default"), None);
        create(&base, "Alpha").unwrap();
        assert_eq!(list(&base), ["Default", "Alpha", "Work"]);
        assert_eq!(dir(&base, "Default"), base);
        fs::write(dir(&base, "Work").join("settings.ini"), "x").unwrap();
        let pic = base.join("me.PNG");
        fs::write(&pic, "png").unwrap();
        assert!(avatar(&base, "Work").is_empty());
        assert!(set_avatar(&base, "Work", &pic));
        assert!(avatar(&base, "work").starts_with("Work.png?v="), "found case-insensitively");
        let jpg = base.join("me.jpg");
        fs::write(&jpg, "jpg").unwrap();
        assert!(set_avatar(&base, "Work", &jpg));
        assert!(avatar(&base, "Work").starts_with("Work.jpg?v="), "the old picture is replaced");
        assert!(!set_avatar(&base, "Work", &base.join("settings.ini")), "only images");
        assert!(delete(&base, "Work"));
        assert!(avatar(&base, "Work").is_empty(), "deleting a profile removes its picture");
        assert_eq!(list(&base), ["Default", "Alpha"], "Default is listed while it has data");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_and_delete_default() {
        let base = std::env::temp_dir().join("velocity-profiles-default-test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("profile/EBWebView")).unwrap(); // an older version's browser data moves too
        fs::write(base.join("settings.ini"), "tabs_top=true").unwrap();
        fs::create_dir_all(base.join("extensions/ubo")).unwrap();
        create(&base, "Work").unwrap();
        assert!(!rename(&base, "Default", "work"), "taken (case-insensitive)");
        assert!(!rename(&base, "Default", "a/b"), "invalid");
        assert!(rename(&base, "Default", "Home"));
        assert_eq!(fs::read_to_string(base.join("profiles/Home/settings.ini")).unwrap(), "tabs_top=true", "Default's files moved");
        assert!(base.join("profiles/Home/profile/EBWebView").is_dir());
        assert!(base.join("extensions/ubo").is_dir(), "shared things stay");
        assert_eq!(list(&base), ["Home", "Work"], "an empty Default isn't listed next to other profiles");
        assert!(rename(&base, "Work", "Job"));
        assert_eq!(list(&base), ["Home", "Job"]);

        fs::write(base.join("history.tsv"), "x").unwrap(); // Default used again
        assert_eq!(list(&base)[0], "Default");
        assert!(delete(&base, "Default"));
        assert!(!base.join("history.tsv").exists() && base.join("extensions/ubo").is_dir(), "only Default's own files go");
        assert_eq!(list(&base), ["Home", "Job"]);
        let _ = fs::remove_dir_all(&base);
    }
}
