//! Self-update: download the latest release binary and replace the one on disk.
//!
//! The `silt update` CLI command resolves the latest version via the GitHub
//! releases `/latest` redirect (same mechanism as `install.sh`), downloads the
//! matching archive for the current target, extracts the `silt` binary, and
//! atomically replaces `current_exe()`. No new Rust dependencies — we shell
//! out to `curl`/`wget` and `tar`/`unzip`, matching the install script so
//! behavior stays in lockstep.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "rendro/silt";

pub struct UpdateOptions {
    /// If true, print what would happen without downloading or replacing.
    pub dry_run: bool,
    /// If true, replace the binary even when already on the latest version.
    pub force: bool,
}

pub fn run_update(opts: UpdateOptions) -> Result<(), String> {
    let target = detect_target()?;
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("  Current version: {current}");
    eprintln!("  Target: {target}");

    let latest = fetch_latest_version()?;
    eprintln!("  Latest version: {latest}");

    let latest_trim = latest.trim_start_matches('v');
    if !opts.force && latest_trim == current {
        eprintln!("\n  Already on the latest version.");
        return Ok(());
    }

    if opts.dry_run {
        eprintln!("\n  Dry run — skipping download.");
        return Ok(());
    }

    let exe_path = env::current_exe().map_err(|e| format!("cannot resolve current exe: {e}"))?;
    let exe_path = fs::canonicalize(&exe_path).unwrap_or(exe_path);

    let (ext, bin_name) = if target.contains("windows") {
        ("zip", "silt.exe")
    } else {
        ("tar.gz", "silt")
    };

    let asset = format!("silt-{latest}-{target}.{ext}");
    let url = format!("https://github.com/{REPO}/releases/download/{latest}/{asset}");

    let tmpdir = mkdtemp()?;
    let archive = tmpdir.join(format!("silt.{ext}"));

    eprintln!("  Downloading {url}");
    download(&url, &archive)?;

    eprintln!("  Extracting");
    extract(&archive, &tmpdir, ext)?;

    let new_bin = tmpdir.join(bin_name);
    if !new_bin.exists() {
        let _ = fs::remove_dir_all(&tmpdir);
        return Err(format!("archive did not contain {bin_name}"));
    }

    replace_binary(&new_bin, &exe_path)?;
    let _ = fs::remove_dir_all(&tmpdir);

    eprintln!("\n  silt updated to {latest} at {}", exe_path.display());
    Ok(())
}

fn detect_target() -> Result<String, String> {
    // Mirror install.sh: we support the same prebuilt targets.
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => return Err(format!("unsupported OS: {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(format!("unsupported architecture: {other}")),
    };
    Ok(format!("{arch}-{os}"))
}

fn fetch_latest_version() -> Result<String, String> {
    // GitHub's /releases/latest returns a 302 to /releases/tag/<version>.
    // We issue a HEAD (no redirect-follow) and parse the Location header.
    let url = format!("https://github.com/{REPO}/releases/latest");

    if which("curl").is_some() {
        let out = Command::new("curl")
            .args(["-fsI", &url])
            .output()
            .map_err(|e| format!("failed to run curl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "curl HEAD {url} failed ({})",
                out.status.code().unwrap_or(-1)
            ));
        }
        let headers = String::from_utf8_lossy(&out.stdout);
        if let Some(v) = parse_location_version(&headers) {
            return Ok(v);
        }
    } else if which("wget").is_some() {
        let out = Command::new("wget")
            .args(["--server-response", "--spider", "--max-redirect=0", &url])
            .output()
            .map_err(|e| format!("failed to run wget: {e}"))?;
        // wget writes the response headers to stderr.
        let headers = String::from_utf8_lossy(&out.stderr);
        if let Some(v) = parse_location_version(&headers) {
            return Ok(v);
        }
    } else {
        return Err("need curl or wget to resolve the latest version".to_string());
    }

    Err("could not parse latest version from GitHub response".to_string())
}

fn parse_location_version(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let line = line.trim_end_matches('\r');
        // Match `Location:` case-insensitively.
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("location:") {
            // Recover the original-case suffix at the same byte offset.
            let offset = line.len() - rest.len();
            let value = line[offset..].trim();
            let tag = value.rsplit('/').next()?.to_string();
            if !tag.is_empty() {
                return Some(tag);
            }
        }
    }
    None
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    if which("curl").is_some() {
        let status = Command::new("curl")
            .args(["-fsSL", url, "-o"])
            .arg(dest)
            .status()
            .map_err(|e| format!("failed to run curl: {e}"))?;
        if !status.success() {
            return Err(format!(
                "curl download failed ({})",
                status.code().unwrap_or(-1)
            ));
        }
        return Ok(());
    }
    if which("wget").is_some() {
        let status = Command::new("wget")
            .arg("-q")
            .arg(url)
            .arg("-O")
            .arg(dest)
            .status()
            .map_err(|e| format!("failed to run wget: {e}"))?;
        if !status.success() {
            return Err(format!(
                "wget download failed ({})",
                status.code().unwrap_or(-1)
            ));
        }
        return Ok(());
    }
    Err("need curl or wget to download the release".to_string())
}

fn extract(archive: &Path, into: &Path, ext: &str) -> Result<(), String> {
    match ext {
        "tar.gz" => {
            let status = Command::new("tar")
                .arg("xzf")
                .arg(archive)
                .arg("-C")
                .arg(into)
                .status()
                .map_err(|e| format!("failed to run tar: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "tar extract failed ({})",
                    status.code().unwrap_or(-1)
                ));
            }
        }
        "zip" => {
            let status = Command::new("unzip")
                .arg("-q")
                .arg(archive)
                .arg("-d")
                .arg(into)
                .status()
                .map_err(|e| format!("failed to run unzip: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "unzip extract failed ({})",
                    status.code().unwrap_or(-1)
                ));
            }
        }
        other => return Err(format!("unsupported archive ext: {other}")),
    }
    Ok(())
}

#[cfg(unix)]
fn replace_binary(new_bin: &Path, exe_path: &Path) -> Result<(), String> {
    // On Unix, replacing a running binary is safe: rename(2) unlinks the old
    // inode, but processes holding an fd to it keep running from the original
    // file. Subsequent invocations pick up the new binary.
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(new_bin)
        .map_err(|e| format!("stat {}: {e}", new_bin.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(new_bin, perms).map_err(|e| format!("chmod {}: {e}", new_bin.display()))?;

    // Try atomic rename first (same filesystem), fall back to copy + rename.
    if fs::rename(new_bin, exe_path).is_ok() {
        return Ok(());
    }
    // Fallback: stage a sibling file next to exe_path then rename over.
    let parent = exe_path.parent().ok_or("exe has no parent directory")?;
    let staged = parent.join(format!(".silt.update.{}", std::process::id()));
    fs::copy(new_bin, &staged).map_err(|e| format!("copy to {}: {e}", staged.display()))?;
    let mut perms = fs::metadata(&staged)
        .map_err(|e| format!("stat {}: {e}", staged.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(&staged, perms).map_err(|e| format!("chmod {}: {e}", staged.display()))?;
    fs::rename(&staged, exe_path).map_err(|e| {
        let _ = fs::remove_file(&staged);
        format!("rename {} -> {}: {e}", staged.display(), exe_path.display())
    })?;
    Ok(())
}

#[cfg(windows)]
fn replace_binary(new_bin: &Path, exe_path: &Path) -> Result<(), String> {
    // On Windows, a running .exe is locked and cannot be overwritten directly.
    // The convention is to rename the running exe to a sibling name, then move
    // the new one into place. The old one can be deleted on the next invocation.
    let parent = exe_path.parent().ok_or("exe has no parent directory")?;
    let old = parent.join("silt.old.exe");
    let _ = fs::remove_file(&old);
    fs::rename(exe_path, &old)
        .map_err(|e| format!("rename {} -> {}: {e}", exe_path.display(), old.display()))?;
    if let Err(e) = fs::rename(new_bin, exe_path) {
        // Roll back so the user still has a working binary.
        let _ = fs::rename(&old, exe_path);
        return Err(format!(
            "rename {} -> {}: {e}",
            new_bin.display(),
            exe_path.display()
        ));
    }
    eprintln!(
        "  Note: previous binary left at {} — remove on next invocation.",
        old.display()
    );
    Ok(())
}

fn mkdtemp() -> Result<PathBuf, String> {
    let base = env::temp_dir();
    for attempt in 0..32 {
        let candidate = base.join(format!("silt-update-{}-{}", std::process::id(), attempt));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("mkdir {}: {e}", candidate.display())),
        }
    }
    Err("could not create temp directory".to_string())
}

fn which(cmd: &str) -> Option<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let path = env::var_os("PATH")?;
    for entry in path.to_string_lossy().split(sep) {
        if entry.is_empty() {
            continue;
        }
        let candidate = Path::new(entry).join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let with_exe = Path::new(entry).join(format!("{cmd}.exe"));
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_location_header_for_version_tag() {
        let headers = "HTTP/2 302\r\n\
                       Content-Type: text/html; charset=utf-8\r\n\
                       Location: https://github.com/rendro/silt/releases/tag/v0.5.0\r\n\
                       \r\n";
        assert_eq!(parse_location_version(headers), Some("v0.5.0".to_string()));
    }

    #[test]
    fn parses_lowercase_location_header() {
        let headers = "HTTP/2 302\r\n\
                       location: https://github.com/rendro/silt/releases/tag/v1.2.3\r\n";
        assert_eq!(parse_location_version(headers), Some("v1.2.3".to_string()));
    }

    #[test]
    fn returns_none_for_missing_location() {
        let headers = "HTTP/2 200\r\nContent-Type: text/html\r\n";
        assert_eq!(parse_location_version(headers), None);
    }
}
