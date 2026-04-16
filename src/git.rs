//! Git operations for v0.8 path-and-git package manager. All ops shell
//! out to the user's system `git` binary; no libgit2 dep. Auth is
//! delegated to the user's existing git credential setup.
//!
//! # Cache layout
//!
//! ```text
//! <cache_root>/silt/git/
//!   <url-hash>/
//!     <resolved-sha>/        # populated checkout at this commit
//!       silt.toml
//!       src/
//!         ...
//! ```
//!
//! `<cache_root>` is `$XDG_CACHE_HOME` on Unix when set, else `~/.cache`
//! when `$HOME` is set, else falls back to `/tmp`. On Windows we use
//! `%LOCALAPPDATA%`. The `<url-hash>` is `sha256(url)` truncated to 16
//! hex chars — short enough to keep paths sane, long enough to make
//! collisions vanishingly unlikely.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

// ── Public types ───────────────────────────────────────────────────────

/// A user-specified ref form for a git dependency.
///
/// `Rev` is locked verbatim. `Branch` is re-fetched (HEAD of the branch)
/// on `silt update`. `Tag` is re-resolved on `silt update` so a moved tag
/// produces a new lockfile SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    /// A commit SHA (full or short, 7-40 hex chars). Locked verbatim.
    Rev(String),
    /// A branch name. Re-fetches HEAD on `silt update`.
    Branch(String),
    /// A tag name. Re-resolves on `silt update` if the tag moved.
    Tag(String),
}

impl GitRef {
    /// Returns the underlying ref string, useful for display and TOML
    /// rendering.
    pub fn as_ref_string(&self) -> &str {
        match self {
            GitRef::Rev(s) | GitRef::Branch(s) | GitRef::Tag(s) => s.as_str(),
        }
    }

    /// Returns a static label for the ref kind: `"rev"`, `"branch"`, or
    /// `"tag"`.
    pub fn kind(&self) -> &'static str {
        match self {
            GitRef::Rev(_) => "rev",
            GitRef::Branch(_) => "branch",
            GitRef::Tag(_) => "tag",
        }
    }
}

/// Errors produced by git operations.
#[derive(Debug)]
pub enum GitError {
    /// `git` binary not found on PATH.
    GitNotInstalled,
    /// Subprocess failed; carries the command + stderr.
    CommandFailed {
        command: String,
        stderr: String,
        exit_code: Option<i32>,
    },
    /// I/O error (cache dir creation, file ops).
    Io {
        context: String,
        error: std::io::Error,
    },
    /// Ref couldn't be resolved (no matching branch/tag/sha).
    RefNotFound { url: String, ref_spec: GitRef },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::GitNotInstalled => write!(
                f,
                "`git` binary not found on PATH; install git to use git dependencies"
            ),
            GitError::CommandFailed {
                command,
                stderr,
                exit_code,
            } => {
                let code = exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into());
                write!(
                    f,
                    "git command failed (exit {code}): `{command}`\nstderr: {}",
                    stderr.trim_end()
                )
            }
            GitError::Io { context, error } => {
                write!(f, "git I/O error ({context}): {error}")
            }
            GitError::RefNotFound { url, ref_spec } => write!(
                f,
                "git ref not found: {} `{}` in {url}",
                ref_spec.kind(),
                ref_spec.as_ref_string()
            ),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitError::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}

// ── Cache path resolution ──────────────────────────────────────────────

/// Returns the cache root directory: `$XDG_CACHE_HOME/silt/git/` on Unix
/// when set (else `$HOME/.cache/silt/git/` on Unix; else
/// `$HOME/.silt/cache/git/` as a final fallback), or
/// `%LOCALAPPDATA%\silt\cache\git\` on Windows.
///
/// Created if it doesn't exist.
pub fn cache_dir() -> Result<PathBuf, GitError> {
    let dir = compute_cache_root()?;
    fs::create_dir_all(&dir).map_err(|e| GitError::Io {
        context: format!("create cache dir {}", dir.display()),
        error: e,
    })?;
    Ok(dir)
}

#[cfg(target_os = "windows")]
fn compute_cache_root() -> Result<PathBuf, GitError> {
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.is_empty() {
            let mut p = PathBuf::from(localapp);
            p.push("silt");
            p.push("cache");
            p.push("git");
            return Ok(p);
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            let mut p = PathBuf::from(profile);
            p.push(".silt");
            p.push("cache");
            p.push("git");
            return Ok(p);
        }
    }
    Err(GitError::Io {
        context: "resolve Windows cache directory".into(),
        error: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "neither LOCALAPPDATA nor USERPROFILE is set",
        ),
    })
}

#[cfg(not(target_os = "windows"))]
fn compute_cache_root() -> Result<PathBuf, GitError> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            let mut p = PathBuf::from(xdg);
            p.push("silt");
            p.push("git");
            return Ok(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            // Prefer the XDG-style default ($HOME/.cache) when XDG_CACHE_HOME
            // is unset; the dotted ~/.silt/cache fallback below is only
            // reached when even $HOME is missing.
            let mut p = PathBuf::from(home);
            p.push(".cache");
            p.push("silt");
            p.push("git");
            return Ok(p);
        }
    }
    // Last-ditch: stash the cache under the system temp dir so the
    // operation can still succeed in headless / sandboxed environments
    // without a HOME variable. This is rare in practice but keeps the
    // function infallible for the happy path.
    let mut p = std::env::temp_dir();
    p.push("silt");
    p.push("cache");
    p.push("git");
    Ok(p)
}

/// Returns the per-(url, sha) cache directory.
///
/// Format: `<cache_dir>/<url-sha256-prefix>/<resolved_sha>/`. The
/// directory is *not* created here — callers (specifically
/// [`fetch_to_cache`]) handle creation/atomic rename.
pub fn cache_for(url: &str, resolved_sha: &str) -> Result<PathBuf, GitError> {
    let root = cache_dir()?;
    let url_hash = url_hash(url);
    Ok(root.join(url_hash).join(resolved_sha))
}

fn url_hash(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    // 16 hex chars = 64 bits. Plenty of room to avoid collisions across
    // a developer's set of git deps; short enough to keep cache paths
    // readable.
    let hex = format!("{:x}", digest);
    hex[..16].to_string()
}

// ── Ref resolution ─────────────────────────────────────────────────────

/// Resolve a [`GitRef`] against the remote URL, returning the commit SHA.
///
/// For `Rev(sha)` we validate the SHA shape (7-40 hex chars) and return
/// it without contacting the network — the actual fetch will fail loudly
/// later if the SHA doesn't exist remotely. For `Branch`/`Tag` we run
/// `git ls-remote <url> <ref>` and parse the SHA out.
pub fn resolve_ref(url: &str, ref_spec: &GitRef) -> Result<String, GitError> {
    match ref_spec {
        GitRef::Rev(sha) => {
            if !is_valid_sha_shape(sha) {
                return Err(GitError::RefNotFound {
                    url: url.to_string(),
                    ref_spec: ref_spec.clone(),
                });
            }
            Ok(sha.to_lowercase())
        }
        GitRef::Branch(name) => {
            let refspec = format!("refs/heads/{name}");
            ls_remote_resolve(url, &refspec).and_then(|maybe_sha| {
                maybe_sha.ok_or_else(|| GitError::RefNotFound {
                    url: url.to_string(),
                    ref_spec: ref_spec.clone(),
                })
            })
        }
        GitRef::Tag(name) => {
            let refspec = format!("refs/tags/{name}");
            ls_remote_resolve(url, &refspec).and_then(|maybe_sha| {
                maybe_sha.ok_or_else(|| GitError::RefNotFound {
                    url: url.to_string(),
                    ref_spec: ref_spec.clone(),
                })
            })
        }
    }
}

fn ls_remote_resolve(url: &str, refspec: &str) -> Result<Option<String>, GitError> {
    let output = run_git(&["ls-remote", url, refspec])?;
    // `git ls-remote` prints `<sha>\t<refname>` lines. Empty stdout =>
    // ref doesn't exist (we map this to RefNotFound at the caller).
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(sha), Some(name)) = (parts.next(), parts.next()) {
            if name == refspec {
                return Ok(Some(sha.to_lowercase()));
            }
        }
    }
    Ok(None)
}

/// Cheap reachability check: `git ls-remote <url> HEAD`. Used by
/// `silt add --git` to fail fast on bad URLs / private-repo-no-auth.
pub fn verify_reachable(url: &str) -> Result<(), GitError> {
    run_git(&["ls-remote", url, "HEAD"]).map(|_| ())
}

fn is_valid_sha_shape(s: &str) -> bool {
    let len = s.len();
    if !(7..=40).contains(&len) {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

// ── Fetch ──────────────────────────────────────────────────────────────

/// Fetch the repo at `resolved_sha` into the cache and return the
/// checkout directory.
///
/// Idempotent: if the cache dir already exists with a `silt.toml` we
/// take that as a sign the cache is populated and skip the fetch.
/// Otherwise we clone into a sibling `.tmp` dir and atomically rename
/// on success — this avoids leaving a half-populated cache after an
/// interrupted clone.
///
/// The caller is responsible for resolving Branch/Tag specs to a SHA
/// first (via [`resolve_ref`]); this function only knows about SHAs.
pub fn fetch_to_cache(url: &str, resolved_sha: &str) -> Result<PathBuf, GitError> {
    let dest = cache_for(url, resolved_sha)?;
    if dest.join("silt.toml").is_file() {
        return Ok(dest);
    }

    // Ensure parent (`<cache>/<url-hash>/`) exists; the per-SHA leaf
    // directory itself is created by `git clone`.
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| GitError::Io {
            context: format!("create cache parent {}", parent.display()),
            error: e,
        })?;
    }

    // Atomic-ish: clone into <dest>.tmp, then rename to <dest>.
    let tmp = with_tmp_suffix(&dest);
    if tmp.exists() {
        // Stale tmp from a previous interrupted clone.
        fs::remove_dir_all(&tmp).map_err(|e| GitError::Io {
            context: format!("remove stale tmp dir {}", tmp.display()),
            error: e,
        })?;
    }

    // Full clone (not shallow): the user picked a specific SHA and we
    // don't know whether `--depth=1` would include it.
    run_git(&[
        "clone",
        "--quiet",
        url,
        tmp.to_str().ok_or_else(|| GitError::Io {
            context: "tmp path is not valid UTF-8".into(),
            error: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-UTF-8 cache path"),
        })?,
    ])?;
    run_git(&[
        "-C",
        tmp.to_str().expect("checked above"),
        "checkout",
        "--quiet",
        resolved_sha,
    ])?;

    if dest.exists() {
        // Race: another process populated the cache between our existence
        // check and the rename. Discard our tmp and return the existing
        // dir if it has a silt.toml; otherwise propagate as an Io error.
        if dest.join("silt.toml").is_file() {
            let _ = fs::remove_dir_all(&tmp);
            return Ok(dest);
        }
        fs::remove_dir_all(&dest).map_err(|e| GitError::Io {
            context: format!("remove pre-existing cache leaf {}", dest.display()),
            error: e,
        })?;
    }

    fs::rename(&tmp, &dest).map_err(|e| GitError::Io {
        context: format!("rename {} -> {}", tmp.display(), dest.display()),
        error: e,
    })?;

    Ok(dest)
}

fn with_tmp_suffix(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    let mut tmp = dest.to_path_buf();
    tmp.set_file_name(name);
    tmp
}

// ── Subprocess plumbing ────────────────────────────────────────────────

/// Run `git <args>...`, capture stdout+stderr, and convert any failure
/// into a structured [`GitError`]. Returns stdout as a UTF-8 string.
fn run_git(args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitError::GitNotInstalled
        } else {
            GitError::Io {
                context: "spawn `git`".into(),
                error: e,
            }
        }
    })?;
    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format_command("git", args),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn format_command(bin: &str, args: &[&str]) -> String {
    let mut s = String::from(bin);
    for a in args {
        s.push(' ');
        s.push_str(a);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_shape_rejects_obvious_garbage() {
        assert!(!is_valid_sha_shape(""));
        assert!(!is_valid_sha_shape("xyz"));
        assert!(!is_valid_sha_shape("abc12")); // too short
        assert!(!is_valid_sha_shape("g".repeat(40).as_str())); // non-hex
        assert!(!is_valid_sha_shape("a".repeat(41).as_str())); // too long
    }

    #[test]
    fn sha_shape_accepts_short_and_full() {
        assert!(is_valid_sha_shape("abc1234"));
        assert!(is_valid_sha_shape("ABCDEF1"));
        assert!(is_valid_sha_shape(&"a".repeat(40)));
    }

    #[test]
    fn url_hash_is_stable_and_short() {
        let h1 = url_hash("https://example.com/foo");
        let h2 = url_hash("https://example.com/foo");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        let h3 = url_hash("https://example.com/bar");
        assert_ne!(h1, h3);
    }

    #[test]
    fn git_ref_kind_and_string() {
        let r = GitRef::Rev("abc1234".into());
        assert_eq!(r.kind(), "rev");
        assert_eq!(r.as_ref_string(), "abc1234");
        let b = GitRef::Branch("main".into());
        assert_eq!(b.kind(), "branch");
        assert_eq!(b.as_ref_string(), "main");
        let t = GitRef::Tag("v1.0".into());
        assert_eq!(t.kind(), "tag");
        assert_eq!(t.as_ref_string(), "v1.0");
    }
}
