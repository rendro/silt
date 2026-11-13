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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const REPO: &str = "rendro/silt";

/// What every curl call is given first: only https, a redirect included,
/// and TLS 1.2 or later.
const CURL_HTTPS_ONLY: [&str; 3] = ["--proto", "=https", "--tlsv1.2"];

/// The most redirects a wget download follows ([`download`]).
const WGET_HOPS_MOST: usize = 10;

pub struct UpdateOptions {
    /// If true, print what would happen without downloading or replacing.
    pub dry_run: bool,
    /// If true, replace the binary even when already on the latest version.
    pub force: bool,
}

pub fn run_update(opts: UpdateOptions) -> Result<(), String> {
    let target = detect_target()?;
    let current = crate::VERSION;

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
    let sums_url =
        format!("https://github.com/{REPO}/releases/download/{latest}/silt-{latest}-SHA256SUMS");

    let tmpdir = mkdtemp()?;
    let archive = tmpdir.join(format!("silt.{ext}"));
    let sums_file = tmpdir.join("SHA256SUMS");

    eprintln!("  Downloading {url}");
    download(&url, &archive)?;

    eprintln!("  Downloading {sums_url}");
    if let Err(e) = download(&sums_url, &sums_file) {
        let _ = fs::remove_dir_all(&tmpdir);
        return Err(format!(
            "failed to download SHA256SUMS — refusing to install unverified binary: {e}"
        ));
    }

    eprintln!("  Verifying SHA-256 checksum");
    if let Err(e) = verify_archive(&archive, &sums_file, &asset) {
        let _ = fs::remove_dir_all(&tmpdir);
        return Err(format!(
            "checksum verification failed — refusing to install: {e}"
        ));
    }

    // The checksum comes from the same release as the archive; the
    // attestation is signed by the release workflow, so it also catches a
    // release whose files were replaced. It needs the GitHub CLI.
    match verify_attestation(&archive) {
        Attestation::Verified => eprintln!("  Verified the build attestation"),
        Attestation::Failed(said) => {
            let _ = fs::remove_dir_all(&tmpdir);
            return Err(format!(
                "the build attestation of {asset} does not verify — refusing to install:\n{said}"
            ));
        }
        Attestation::NotChecked => {
            eprintln!(
                "  Build attestation not checked: that needs gh 2.49 or later, logged in to github.com"
            );
        }
    }

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
            .args(CURL_HTTPS_ONLY)
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
    } else if let Some(wget) = which("wget") {
        wget_keeps_to_https(&wget)?;
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

/// Verify `archive` against the entry for `asset_name` in a SHA256SUMS file.
///
/// Fails closed: any problem (missing entry, unreadable file, hash mismatch)
/// returns Err. The caller is expected to abort installation on error.
fn verify_archive(archive: &Path, sums_file: &Path, asset_name: &str) -> Result<(), String> {
    let sums = fs::read_to_string(sums_file).map_err(|e| format!("read SHA256SUMS: {e}"))?;
    let expected = find_expected_hash(&sums, asset_name).ok_or_else(|| {
        format!("no SHA256SUMS entry for {asset_name} — release is missing this asset or sums file is malformed")
    })?;
    let actual = sha256_hex(archive)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "sha256 mismatch for {asset_name}: expected {expected}, got {actual}"
        ))
    }
}

/// Scan a SHA256SUMS body (one `<hash>  <name>` line per entry — both the
/// two-space GNU `sha256sum` format and the one-space `shasum` format are
/// accepted) and return the hash for the matching asset name.
fn find_expected_hash<'a>(sums: &'a str, asset_name: &str) -> Option<&'a str> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on whitespace — handles both "hash  name" (sha256sum) and
        // "hash *name" (binary-mode sha256sum, where the leading char is *).
        let mut parts = line.splitn(2, char::is_whitespace);
        let hash = parts.next()?.trim();
        let rest = parts.next()?.trim();
        // Drop a leading '*' (binary-mode marker) before the filename.
        let name = rest.strip_prefix('*').unwrap_or(rest);
        if name == asset_name {
            return Some(hash);
        }
    }
    None
}

/// Compute the SHA-256 of `path` as a lowercase hex string.
fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{:02x}", byte).expect("write to String never fails");
    }
    Ok(hex)
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    if which("curl").is_some() {
        let status = Command::new("curl")
            .args(CURL_HTTPS_ONLY)
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
    if let Some(wget) = which("wget") {
        // wget follows a redirect wherever it leads (its --https-only is
        // for recursive downloads only), so each redirect is followed
        // here, and only to https.
        wget_keeps_to_https(&wget)?;
        let mut url = url.to_string();
        for _ in 0..WGET_HOPS_MOST {
            let out = Command::new("wget")
                .args(["--max-redirect=0", "--server-response", "-O"])
                .arg(dest)
                .arg(&url)
                .output()
                .map_err(|e| format!("failed to run wget: {e}"))?;
            if out.status.success() {
                return Ok(());
            }
            match next_hop(&url, &String::from_utf8_lossy(&out.stderr))? {
                Some(next) => url = next,
                None => {
                    return Err(format!(
                        "wget download failed ({})",
                        out.status.code().unwrap_or(-1)
                    ));
                }
            }
        }
        return Err(format!(
            "wget download failed: more than {WGET_HOPS_MOST} redirects"
        ));
    }
    Err("need curl or wget to download the release".to_string())
}

/// That the wget `wget` can be kept to https: it must know
/// `--max-redirect`, as [`download`] follows each redirect itself (GNU
/// wget does; busybox's wget does not).
fn wget_keeps_to_https(wget: &Path) -> Result<(), String> {
    let out = Command::new(wget)
        .arg("--help")
        .output()
        .map_err(|e| format!("failed to run wget: {e}"))?;
    let help = [out.stdout, out.stderr].concat();
    match String::from_utf8_lossy(&help).contains("--max-redirect") {
        true => Ok(()),
        false => Err(WGET_WITHOUT_MAX_REDIRECT.to_string()),
    }
}

/// Why a wget without `--max-redirect` is not used.
const WGET_WITHOUT_MAX_REDIRECT: &str = "this wget cannot be kept to https (it has no \
    --max-redirect, as busybox's wget): install curl or GNU wget";

/// Where a redirect from `current` (an https URL) that wget did not
/// follow leads, read from what wget said with `--server-response` (the
/// server's headers are indented): `None` when there is no redirect, an
/// error when it leads elsewhere than https. A Location without a scheme
/// is read as a browser reads it: `//host/path` on that host, `/path` on
/// this host, any other path beside the one asked for.
fn next_hop(current: &str, said: &str) -> Result<Option<String>, String> {
    let location = said
        .lines()
        .filter(|line| line.starts_with("  "))
        .filter_map(|line| {
            let line = line.trim();
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_string())
        })
        .next_back();
    let Some(location) = location else {
        return Ok(None);
    };
    let Some(rest) = current.strip_prefix("https://") else {
        return Err(format!("a download from {current}, not https"));
    };
    let host = rest.split('/').next().unwrap_or(rest);
    let scheme = location
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        });
    let next = match scheme {
        Some(scheme) if scheme.eq_ignore_ascii_case("https") && location[6..].starts_with("//") => {
            format!("https://{}", &location[8..])
        }
        Some(_) => {
            return Err(format!(
                "a download was redirected to {location}, which is not https"
            ));
        }
        None => match location.strip_prefix("//") {
            Some(elsewhere) => format!("https://{elsewhere}"),
            None if location.starts_with('/') => format!("https://{host}{location}"),
            None => {
                let asked = current.split(['?', '#']).next().unwrap_or(current);
                let dir = asked.rsplit_once('/').map_or(asked, |(dir, _)| dir);
                format!("{dir}/{location}")
            }
        },
    };
    Ok(Some(next))
}

/// What the GitHub CLI says of the build attestation of an archive.
#[derive(Debug, PartialEq)]
enum Attestation {
    Verified,
    /// It does not verify: what gh said.
    Failed(String),
    /// It cannot be checked here: no `gh`, a `gh` without the
    /// `attestation` command (before 2.49), or a `gh` not logged in to
    /// github.com.
    NotChecked,
}

/// Check the build provenance that the release workflow signed for
/// `archive`, with the `gh` found on PATH.
fn verify_attestation(archive: &Path) -> Attestation {
    match which("gh") {
        Some(gh) => attestation_by(&gh, archive),
        None => Attestation::NotChecked,
    }
}

/// [`verify_attestation`] with the GitHub CLI `gh`.
fn attestation_by(gh: &Path, archive: &Path) -> Attestation {
    let runs = |args: &[&str]| {
        Command::new(gh)
            .args(args)
            .output()
            .is_ok_and(|out| out.status.success())
    };
    if !runs(&["attestation", "--help"]) || !runs(&["auth", "status", "--hostname", "github.com"]) {
        return Attestation::NotChecked;
    }
    match Command::new(gh)
        .args(["attestation", "verify"])
        .arg(archive)
        .args(["--repo", REPO])
        .output()
    {
        Ok(out) if out.status.success() => Attestation::Verified,
        Ok(out) => Attestation::Failed(
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .trim_end()
            .to_string(),
        ),
        Err(e) => Attestation::Failed(format!("failed to run gh: {e}")),
    }
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

    #[test]
    fn finds_expected_hash_in_gnu_sha256sum_format() {
        // GNU sha256sum default uses two spaces between hash and filename.
        let sums = "aaaa  silt-v0.5.1-x86_64-unknown-linux-gnu.tar.gz\n\
                    bbbb  silt-v0.5.1-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            find_expected_hash(sums, "silt-v0.5.1-x86_64-unknown-linux-gnu.tar.gz"),
            Some("aaaa")
        );
        assert_eq!(
            find_expected_hash(sums, "silt-v0.5.1-aarch64-apple-darwin.tar.gz"),
            Some("bbbb")
        );
    }

    #[test]
    fn finds_expected_hash_with_binary_mode_marker() {
        // sha256sum -b prefixes the filename with `*`; shasum's -a 256 -b does
        // the same. We strip it before comparing so both formats verify.
        let sums = "cccc *silt-v0.5.1-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            find_expected_hash(sums, "silt-v0.5.1-x86_64-pc-windows-msvc.zip"),
            Some("cccc")
        );
    }

    #[test]
    fn ignores_comments_and_blank_lines_in_sums() {
        let sums = "\n\
                    # generated by ci\n\
                    \n\
                    dddd  silt-v0.5.1-x86_64-unknown-linux-gnu.tar.gz\n";
        assert_eq!(
            find_expected_hash(sums, "silt-v0.5.1-x86_64-unknown-linux-gnu.tar.gz"),
            Some("dddd")
        );
    }

    #[test]
    fn returns_none_when_asset_missing_from_sums() {
        let sums = "eeee  silt-v0.5.1-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            find_expected_hash(sums, "silt-v0.5.1-x86_64-unknown-linux-gnu.tar.gz"),
            None
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // "abc" → SHA-256 known vector from NIST.
        let dir = tempdir();
        let path = dir.join("abc");
        fs::write(&path, b"abc").expect("write");
        let hex = sha256_hex(&path).expect("hash");
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_archive_rejects_tampered_file() {
        let dir = tempdir();
        let archive = dir.join("silt.tar.gz");
        fs::write(&archive, b"fake archive").expect("write");
        let real_hash = sha256_hex(&archive).expect("hash");
        // Assume an attacker swapped the archive after the sums file was
        // generated — the sums file still references the old hash.
        let wrong_hash = "0".repeat(64);
        assert_ne!(real_hash, wrong_hash);
        let sums = format!("{wrong_hash}  silt-test.tar.gz\n");
        let sums_path = dir.join("SHA256SUMS");
        fs::write(&sums_path, &sums).expect("write");
        let err = verify_archive(&archive, &sums_path, "silt-test.tar.gz")
            .expect_err("expected mismatch");
        assert!(err.contains("sha256 mismatch"), "got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_archive_accepts_matching_hash() {
        let dir = tempdir();
        let archive = dir.join("silt.tar.gz");
        fs::write(&archive, b"hello world").expect("write");
        let hash = sha256_hex(&archive).expect("hash");
        let sums_path = dir.join("SHA256SUMS");
        fs::write(&sums_path, format!("{hash}  silt-test.tar.gz\n")).expect("write");
        verify_archive(&archive, &sums_path, "silt-test.tar.gz").expect("should verify");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curl_downloads_over_https_only() {
        // A redirect to http is refused, and so is a TLS older than 1.2.
        assert_eq!(CURL_HTTPS_ONLY, ["--proto", "=https", "--tlsv1.2"]);
    }

    #[test]
    fn a_wget_redirect_is_followed_only_to_https() {
        let from = "https://github.com/rendro/silt/releases/download/v1/a.tar.gz";
        let said = |location: &str| {
            format!(
                "--2026-11-11--  {from}\nHTTP request sent, awaiting response... \n  \
                 HTTP/1.1 302 Found\n  Location: {location}\r\nLocation: {location} [following]\n\
                 0 redirections exceeded.\n"
            )
        };
        let to = "https://release-assets.githubusercontent.com/a?sig=1";
        assert_eq!(next_hop(from, &said(to)), Ok(Some(to.to_string())));
        assert_eq!(
            next_hop(from, &said("/rendro/silt/other")),
            Ok(Some("https://github.com/rendro/silt/other".to_string()))
        );
        // A Location without a scheme, read as a browser reads it.
        for (to, next) in [
            ("//cdn.example/a", "https://cdn.example/a"),
            (
                "b.tar.gz?x=1",
                "https://github.com/rendro/silt/releases/download/v1/b.tar.gz?x=1",
            ),
            (
                "../v2/a.tar.gz",
                "https://github.com/rendro/silt/releases/download/v1/../v2/a.tar.gz",
            ),
            ("HTTPS://cdn.example/a", "https://cdn.example/a"),
        ] {
            assert_eq!(
                next_hop(from, &said(to)),
                Ok(Some(next.to_string())),
                "{to}"
            );
        }
        assert_eq!(
            next_hop(&format!("{from}?sig=a/b"), &said("c")),
            Ok(Some(
                "https://github.com/rendro/silt/releases/download/v1/c".to_string()
            ))
        );
        for refused in [
            "http://example.com/a",
            "ftp://example.com/a",
            "HTTP://example.com/a",
            "file:/etc/passwd",
            "https:/example.com/a",
        ] {
            let error = next_hop(from, &said(refused)).expect_err(refused);
            assert!(error.contains("which is not https"), "{error}");
        }
        assert_eq!(next_hop(from, "  HTTP/1.1 404 Not Found\n"), Ok(None));
    }

    /// A wget that cannot be told not to follow a redirect (busybox's)
    /// is not used, and says what is needed instead.
    #[cfg(unix)]
    #[test]
    fn a_wget_without_max_redirect_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let dir = env::temp_dir().join(format!("silt_update_busybox_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let wget = dir.join("wget");
        fs::write(
            &wget,
            "#!/bin/sh\necho 'BusyBox v1.36.1 multi-call binary.' >&2\n\
             echo 'Usage: wget [-cqS] [-O FILE] URL' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&wget, fs::Permissions::from_mode(0o755)).unwrap();
        let busybox = wget_keeps_to_https(&wget);
        fs::write(
            &wget,
            "#!/bin/sh\necho '       --max-redirect=NUMBER       maximum redirections allowed'\n",
        )
        .unwrap();
        let gnu = wget_keeps_to_https(&wget);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(busybox, Err(WGET_WITHOUT_MAX_REDIRECT.to_string()));
        assert_eq!(gnu, Ok(()));
    }

    /// A stub `gh` in `dir`: `attestation --help` exits with `help` (1
    /// for a gh before 2.49), `auth status` with `auth`, and `attestation
    /// verify` with `verify`, printing a line when it fails.
    #[cfg(unix)]
    fn stub_gh(dir: &Path, help: u8, auth: u8, verify: u8) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let gh = dir.join(format!("gh-{help}{auth}{verify}"));
        fs::write(
            &gh,
            format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n  \"attestation --help\") exit {help} ;;\n  \
                 \"auth status\") [ \"$3 $4\" = \"--hostname github.com\" ] || exit 8; exit {auth} ;;\n  \
                 \"attestation verify\") [ {help} = 0 ] || exit 1; \
                 [ {verify} = 0 ] || echo 'no attestation matches'; \
                 [ \"$4 $5\" = \"--repo {REPO}\" ] || exit 9; exit {verify} ;;\nesac\nexit 1\n"
            ),
        )
        .expect("write");
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod");
        gh
    }

    #[cfg(unix)]
    #[test]
    fn the_attestation_is_checked_with_a_logged_in_gh() {
        let dir = tempdir();
        let archive = dir.join("silt.tar.gz");
        fs::write(&archive, b"archive").expect("write");
        assert_eq!(
            attestation_by(&stub_gh(&dir, 0, 0, 0), &archive),
            Attestation::Verified
        );
        assert_eq!(
            attestation_by(&stub_gh(&dir, 0, 0, 1), &archive),
            Attestation::Failed("no attestation matches".to_string())
        );
        // Not logged in, and a gh that has no attestation command (as
        // Ubuntu 24.04's 2.45): not checked, not refused.
        assert_eq!(
            attestation_by(&stub_gh(&dir, 0, 1, 0), &archive),
            Attestation::NotChecked
        );
        assert_eq!(
            attestation_by(&stub_gh(&dir, 1, 0, 1), &archive),
            Attestation::NotChecked
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempdir() -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "silt-update-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }
}
