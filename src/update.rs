//! Updates from the project's GitHub Releases: on start Velocity asks GitHub for the latest
//! release; if it is newer, the tab UI shows an "Update" button. One click downloads the
//! installer (`Velocity-Setup-<version>.exe`), checks it against the SHA-256 GitHub publishes for
//! it, starts it silently and closes Velocity; the installer opens Velocity again when done.
//!
//! Downloads go through Windows' own curl.exe (Schannel TLS), so no HTTP library is built in.

use std::path::PathBuf;
use std::process::Command;

/// The GitHub repository ("owner/name") releases come from; `VELOCITY_UPDATE_REPO` at build time
/// overrides it (empty: updates off).
const REPO: &str = match option_env!("VELOCITY_UPDATE_REPO") {
    Some(repo) => repo,
    None => "MrKniha/velocity",
};

#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    url: String,
    sha256: String,
}

fn curl() -> PathBuf {
    // By full path: never a curl.exe that happens to sit in the current folder or on PATH.
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("curl.exe")
}

/// The newest release, if it is newer than this Velocity and has an installer with a checksum.
/// Blocking (network): call it off the UI thread.
pub fn check() -> Option<Update> {
    if REPO.is_empty() {
        return None;
    }
    let out = Command::new(curl())
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: Velocity-updater"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .creation_flags_no_window()
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    latest(&serde_json::from_slice(&out.stdout).ok()?, REPO, env!("CARGO_PKG_VERSION"))
}

/// `release` (GitHub's JSON for one release of `repo`) as an update from `current`, if it is one.
fn latest(release: &serde_json::Value, repo: &str, current: &str) -> Option<Update> {
    let version = release["tag_name"].as_str()?.trim_start_matches(['v', 'V']).to_string();
    if !newer(&version, current) {
        return None;
    }
    let asset = release["assets"].as_array()?.iter().find(|a| {
        a["name"].as_str().is_some_and(|n| n.starts_with("Velocity-Setup-") && n.ends_with(".exe"))
    })?;
    let url = asset["browser_download_url"].as_str()?;
    // Only from GitHub itself, over https.
    if !url.starts_with(&format!("https://github.com/{repo}/releases/download/")) {
        return None;
    }
    // GitHub publishes each asset's SHA-256 as `digest`: without one the file can't be checked, so no update.
    let sha256 = asset["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    (sha256.len() == 64).then(|| Update { version, url: url.to_string(), sha256 })
}

/// Whether dotted version `a` is newer than `b` ("0.10.0" > "0.9.3").
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).take(3).collect::<Vec<_>>();
    parts(a) > parts(b)
}

/// Download `update`'s installer and check it. Blocking; the installer's path, or why it failed.
pub fn download(update: &Update) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("Velocity-Setup-{}.exe", update.version));
    let status = Command::new(curl())
        .args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&path)
        .arg(&update.url)
        .creation_flags_no_window()
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("download failed".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let hash = sha256_hex(&bytes);
    if hash != update.sha256 {
        let _ = std::fs::remove_file(&path);
        return Err("the downloaded file is not the one GitHub lists".into());
    }
    Ok(path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Start the installer for an update (silent; it closes nothing, Velocity exits right after) and
/// let it open Velocity again.
pub fn run_installer(path: &PathBuf) -> std::io::Result<()> {
    Command::new(path).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/UPDATE=1"]).spawn().map(|_| ())
}

trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl NoWindow for Command {
    /// No console window flashing up for curl.
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.3"), "numbers, not text");
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn release_json() {
        let release = |tag: &str, url: &str, digest: &str| {
            serde_json::json!({ "tag_name": tag, "assets": [
                { "name": "Velocity-0.9.0-windows-x64.zip", "browser_download_url": "x", "digest": digest },
                { "name": format!("Velocity-Setup-{}.exe", tag.trim_start_matches('v')), "browser_download_url": url, "digest": digest },
            ]})
        };
        let ok = "https://github.com/me/velocity/releases/download/v0.9.0/Velocity-Setup-0.9.0.exe";
        let digest = format!("sha256:{}", "AB".repeat(32));
        let u = latest(&release("v0.9.0", ok, &digest), "me/velocity", "0.1.0").expect("newer release");
        assert_eq!((u.version.as_str(), u.sha256.len()), ("0.9.0", 64));
        assert!(latest(&release("v0.1.0", ok, &digest), "me/velocity", "0.1.0").is_none(), "not newer");
        assert!(latest(&release("v0.9.0", "https://evil.example/x.exe", &digest), "me/velocity", "0.1.0").is_none(), "only from the repository");
        assert!(latest(&release("v0.9.0", ok, ""), "me/velocity", "0.1.0").is_none(), "no checksum, no update");
    }

    #[test]
    fn hashes() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
