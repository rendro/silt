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
    /// A commit SHA (full or short, 7-64 hex chars). Locked verbatim.
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
    /// A URL or commit id was rejected before it could reach `git` or a
    /// cache path. Carries the rendered, printable reason.
    InvalidInput(String),
}

impl fmt::Display for GitError {
    // The URL, the ref and the command line are untrusted (they come
    // from a manifest, possibly a transitive dependency's), and git's
    // stderr can carry text sent by the remote. All of it goes through
    // `escape_for_display` so none of it can forge a line of output or
    // drive the terminal.
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
                    "git command failed (exit {code}): `{}`\nstderr: {}",
                    escape_for_display(command),
                    escape_lines(stderr.trim_end())
                )
            }
            GitError::Io { context, error } => {
                write!(f, "git I/O error ({context}): {error}")
            }
            GitError::RefNotFound { url, ref_spec } => write!(
                f,
                "git ref not found: {} `{}` in {}",
                ref_spec.kind(),
                escape_for_display(ref_spec.as_ref_string()),
                escape_for_display(url)
            ),
            GitError::InvalidInput(message) => write!(f, "{message}"),
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

// ── URL and commit-id validation ───────────────────────────────────────
//
// A git URL comes from a `silt.toml` (the local one or any transitive
// dependency's) or from the `silt add` command line, so it is untrusted
// input. Unvalidated, `git = "--upload-pack=<cmd>"` made `git ls-remote`
// run `<cmd>`. `validate_git_url` is the single rule every entry point
// shares: `Manifest::load`, `silt add`, and the git-invoking functions
// in this module.

/// URL schemes accepted by [`validate_git_url`].
const GIT_URL_SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "file://"];

/// The accepted git URL forms, phrased for error messages.
const ACCEPTED_GIT_URL_FORMS: &str = concat!(
    "an `https://`, `http://`, `ssh://`, `git://` or `file://` URL, ",
    "`host:path` or `user@host:path`, ",
    "or a local path starting with `/`, `./` or `../`",
);

/// A git URL rejected by [`validate_git_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidGitUrl {
    /// The offending value, with control and invisible characters
    /// escaped so it is safe to print on a single line.
    pub url: String,
    /// The rule that failed, followed by the accepted forms.
    pub reason: String,
}

impl fmt::Display for InvalidGitUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid git URL `{}`: {}", self.url, self.reason)
    }
}

impl std::error::Error for InvalidGitUrl {}

/// Validate a git dependency URL.
///
/// Rejects, in this order:
///   - the empty string;
///   - anything starting with `-` (git would parse it as an option);
///   - any control character and any whitespace character, anywhere,
///     with one exception: an ordinary space (U+0020) is allowed in a
///     `file://` URL and in a local path, because directory names
///     contain spaces. A space in any other form is rejected;
///   - any invisible or bidirectional formatting character, anywhere
///     (U+00AD, U+200B to U+200F, U+202A to U+202E, U+2060 to U+2064,
///     U+2066 to U+2069, U+FEFF): such a character makes the value
///     read as something it is not;
///   - anything that is not one of the accepted forms below.
///
/// Accepts:
///   - `https://`, `http://`, `ssh://`, `git://` and `file://` URLs with
///     something after the scheme, whose authority (the part up to the
///     first `/`) and the host inside it do not start with `-`;
///   - the scp-like forms `host:path` and `user@host:path`: a non-empty
///     `host` (and, when there is an `@`, a non-empty `user`) before
///     the first `:`, no `/` before that `:`, a `host` that does not
///     start with `-`, and a non-empty `path` that starts with none of
///     `-`, `:` and `//`;
///   - a local path: a value starting with `/`, `./` or `../`.
///
/// The `path` rule of the scp-like forms is what keeps two dangerous
/// shapes out. `<transport>::<address>` (`ext::sh -c ...`) has a path
/// starting with `:`, and `<scheme>://...` with a scheme that is not
/// listed above has a path starting with `//`; git hands both to a
/// `git-remote-<name>` helper program.
///
/// Windows drive-letter paths (`C:\repo`, `C:/repo`) are not covered by
/// the local-path rule. `C:` cannot be told apart from the `host:` of
/// the scp-like form, so such a value is checked as `host:path`: it is
/// accepted in that shape, but a space in it is rejected. A Windows
/// path that contains a space is spelled as a `file://` URL.
pub fn validate_git_url(url: &str) -> Result<(), InvalidGitUrl> {
    let local = url.starts_with("file://") || is_local_path(url);
    let problem = if url.is_empty() {
        "must not be empty"
    } else if url.starts_with('-') {
        "must not start with `-`"
    } else if url.chars().any(|c| is_forbidden_whitespace(c, local)) {
        "must not contain whitespace or control characters \
         (a space is allowed only in a `file://` URL or a local path)"
    } else if url.chars().any(is_invisible_format_char) {
        "must not contain invisible or bidirectional formatting characters"
    } else if has_accepted_url_form(url) {
        return Ok(());
    } else {
        "is not a recognised git URL form"
    };
    Err(InvalidGitUrl {
        url: escape_for_display(url),
        reason: format!("{problem}; expected {ACCEPTED_GIT_URL_FORMS}"),
    })
}

/// A local path: a value starting with `/`, `./` or `../`.
fn is_local_path(url: &str) -> bool {
    url.starts_with('/') || url.starts_with("./") || url.starts_with("../")
}

/// Is `c` a control or whitespace character that a git URL must not
/// contain? `space_allowed` is true for the two local forms, where an
/// ordinary space (and only that) is tolerated.
fn is_forbidden_whitespace(c: char, space_allowed: bool) -> bool {
    if c == ' ' {
        return !space_allowed;
    }
    c.is_control() || c.is_whitespace()
}

/// Invisible and bidirectional formatting characters. The standard
/// library has no test for this category, so the list is explicit:
/// soft hyphen, zero-width space and joiners, the directional marks,
/// embeddings, overrides and isolates, the word joiner and invisible
/// operators, and the byte-order mark.
fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Shape check behind [`validate_git_url`]. Assumes the caller already
/// rejected empty strings, a leading `-`, and every forbidden
/// character.
fn has_accepted_url_form(url: &str) -> bool {
    if is_local_path(url) {
        return true;
    }
    for scheme in GIT_URL_SCHEMES {
        if let Some(rest) = url.strip_prefix(scheme) {
            // The authority is handed to `ssh` for `ssh://` URLs, so
            // neither it nor the host after an optional `user@` may be
            // option-shaped. `file://` URLs have an empty authority.
            let authority = rest.split('/').next().unwrap_or("");
            let host = authority.rsplit('@').next().unwrap_or(authority);
            return !rest.is_empty() && !authority.starts_with('-') && !host.starts_with('-');
        }
    }
    // scp-like `host:path` or `user@host:path`.
    let Some((before_colon, path)) = url.split_once(':') else {
        return false;
    };
    let host = match before_colon.split_once('@') {
        Some(("", _)) => return false,
        Some((_user, host)) => host,
        None => before_colon,
    };
    !host.is_empty()
        && !host.starts_with('-')
        && !before_colon.contains('/')
        && !path.is_empty()
        && !path.starts_with('-')
        // `<transport>::<address>`: a remote-helper program.
        && !path.starts_with(':')
        // `<scheme>://...` with an unlisted scheme: also a helper.
        && !path.starts_with("//")
}

/// Render an untrusted string for an error message. Control characters,
/// the invisible and bidirectional formatting characters, the no-break
/// space (U+00A0) and the line and paragraph separators (U+2028,
/// U+2029) are written as escapes (`\n`, `\t`, `\u{1b}`, `\u{202e}`),
/// so a hostile value cannot forge extra lines, drive the terminal, or
/// disguise what it is. Everything else, non-ASCII letters included, is
/// left as it is.
pub fn escape_for_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let escape = c.is_control()
            || is_invisible_format_char(c)
            || matches!(c, '\u{00A0}' | '\u{2028}' | '\u{2029}');
        if escape {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// [`escape_for_display`] for text that is legitimately several lines
/// long (git's stderr): each line is escaped, the line breaks are kept.
fn escape_lines(s: &str) -> String {
    let lines: Vec<String> = s.lines().map(escape_for_display).collect();
    lines.join("\n")
}

/// [`validate_git_url`] as a [`GitError`], for the functions below that
/// hand the URL to `git`.
fn check_url(url: &str) -> Result<(), GitError> {
    validate_git_url(url).map_err(|e| GitError::InvalidInput(e.to_string()))
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
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        let mut p = PathBuf::from(xdg);
        p.push("silt");
        p.push("git");
        return Ok(p);
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        // Prefer the XDG-style default ($HOME/.cache) when XDG_CACHE_HOME
        // is unset; the dotted ~/.silt/cache fallback below is only
        // reached when even $HOME is missing.
        let mut p = PathBuf::from(home);
        p.push(".cache");
        p.push("silt");
        p.push("git");
        return Ok(p);
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
///
/// `resolved_sha` becomes a path component, so anything that is not a
/// plain hexadecimal commit id is refused: a value such as
/// `../../../../x` (from a hand-edited `silt.lock`) would otherwise
/// point the cache path at an arbitrary directory.
pub fn cache_for(url: &str, resolved_sha: &str) -> Result<PathBuf, GitError> {
    if !is_valid_sha_shape(resolved_sha) {
        return Err(GitError::InvalidInput(format!(
            "invalid resolved commit `{}`: expected 7 to 64 hexadecimal characters",
            escape_for_display(resolved_sha)
        )));
    }
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
/// For `Rev(sha)` we validate the SHA shape (7-64 hex chars) and return
/// it without contacting the network — the actual fetch will fail loudly
/// later if the SHA doesn't exist remotely. For `Branch`/`Tag` we run
/// `git ls-remote -- <url> <ref>` and parse the SHA out.
///
/// The URL must pass [`validate_git_url`]; the branch or tag name is
/// only ever sent as `refs/heads/<name>` / `refs/tags/<name>`, which
/// cannot be option-shaped.
pub fn resolve_ref(url: &str, ref_spec: &GitRef) -> Result<String, GitError> {
    check_url(url)?;
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
    // `--` ends option parsing: whatever `url` holds, git reads it as
    // the repository, never as an option such as `--upload-pack=<cmd>`.
    let output = run_git(&["ls-remote", "--", url, refspec])?;
    // `git ls-remote` prints `<sha>\t<refname>` lines. Empty stdout =>
    // ref doesn't exist (we map this to RefNotFound at the caller).
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(sha), Some(name)) = (parts.next(), parts.next())
            && name == refspec
        {
            return Ok(Some(sha.to_lowercase()));
        }
    }
    Ok(None)
}

/// Cheap reachability check: `git ls-remote -- <url> HEAD`. Used by
/// `silt add --git` to fail fast on bad URLs / private-repo-no-auth.
pub fn verify_reachable(url: &str) -> Result<(), GitError> {
    check_url(url)?;
    run_git(&["ls-remote", "--", url, "HEAD"]).map(|_| ())
}

/// Returns true if `s` is a plausibly-shaped commit SHA: a 7-64
/// character hex string and nothing else. The upper bound is 64, not
/// 40, because a commit of a SHA-256 repository has a 64-character id.
///
/// This is the one rule for every commit id silt handles: a manifest
/// `rev` (offline validation in `resolve_ref`), `silt add --git --rev`
/// (shape-check before the network round-trip), the lockfile's `rev`
/// field, and the id that names a cache directory in [`cache_for`].
/// Being hexadecimal only, an accepted value can be neither a path nor
/// an option.
pub fn is_valid_sha_shape(s: &str) -> bool {
    let len = s.len();
    if !(7..=64).contains(&len) {
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
    check_url(url)?;
    // `cache_for` also vets `resolved_sha`, so by the time it reaches
    // `git checkout` below it is known to be plain hexadecimal.
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
    // don't know whether `--depth=1` would include it. `--` ends option
    // parsing so neither the URL nor the path can be read as an option.
    run_git(&[
        "clone",
        "--quiet",
        "--",
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
///
/// Every invocation is hardened the same way:
///   - `-c protocol.ext.allow=never` refuses the `ext::` transport, which
///     runs an arbitrary command, whatever the user's git config says;
///   - `GIT_TERMINAL_PROMPT=0` makes git fail instead of blocking on a
///     username/password prompt (credential helpers and ssh keys still
///     work).
///
/// Callers that pass a URL or a path put `--` in front of it.
fn run_git(args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .args(["-c", "protocol.ext.allow=never"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| {
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
        assert!(!is_valid_sha_shape("a".repeat(65).as_str())); // too long
    }

    #[test]
    fn sha_shape_accepts_short_and_full() {
        assert!(is_valid_sha_shape("abc1234"));
        assert!(is_valid_sha_shape("ABCDEF1"));
        assert!(is_valid_sha_shape(&"a".repeat(40)));
        // A SHA-256 repository's commit id, and everything in between.
        assert!(is_valid_sha_shape(&"a".repeat(41)));
        assert!(is_valid_sha_shape(&"A".repeat(64)));
    }

    #[test]
    fn sha_shape_rejects_paths_and_options() {
        assert!(!is_valid_sha_shape("../../../../x"));
        assert!(!is_valid_sha_shape("abc1234/../x"));
        assert!(!is_valid_sha_shape("-abc1234"));
        assert!(!is_valid_sha_shape("abc1234\n"));
    }

    #[test]
    fn resolve_ref_accepts_a_full_sha256_rev_offline() {
        // No network and no git: a `Rev` is only shape-checked.
        let rev = "AB".repeat(32);
        let resolved = resolve_ref("https://example.invalid/repo", &GitRef::Rev(rev))
            .expect("a 64-character rev must be accepted");
        assert_eq!(resolved, "ab".repeat(32));
    }

    /// The `reason` of the rejection, panicking if `url` is accepted.
    fn rejection_reason(url: &str) -> String {
        match validate_git_url(url) {
            Ok(()) => panic!("expected `{url}` to be rejected"),
            Err(e) => e.reason,
        }
    }

    const RULE_SPACE: &str = "must not contain whitespace or control characters";
    const RULE_INVISIBLE: &str =
        "must not contain invisible or bidirectional formatting characters";
    const RULE_FORM: &str = "is not a recognised git URL form";

    /// Every character `is_invisible_format_char` lists.
    fn invisible_format_chars() -> Vec<char> {
        let ranges = [
            ('\u{00AD}', '\u{00AD}'),
            ('\u{200B}', '\u{200F}'),
            ('\u{202A}', '\u{202E}'),
            ('\u{2060}', '\u{2064}'),
            ('\u{2066}', '\u{2069}'),
            ('\u{FEFF}', '\u{FEFF}'),
        ];
        let mut chars = Vec::new();
        for (first, last) in ranges {
            chars.extend(first..=last);
        }
        chars
    }

    #[test]
    fn git_url_accepts_network_forms() {
        for url in [
            "https://example.com/foo.git",
            "https://user@example.com/foo.git",
            "http://127.0.0.1:1/__silt_test_unreachable__.git",
            "ssh://git@example.com/pkg.git",
            "ssh://example.com:2222/pkg.git",
            "ssh://[::1]/pkg.git",
            "git://example.com/pkg.git",
            "git@github.com:rendro/silt.git",
        ] {
            assert_eq!(validate_git_url(url), Ok(()), "`{url}` must be accepted");
        }
    }

    #[test]
    fn git_url_accepts_scp_like_form_without_a_user() {
        for url in [
            "example.com:pkg.git",
            "example.com:team/pkg.git",
            "example.com:/srv/repos/pkg.git",
            // An alias from the user's ssh configuration.
            "work-alias:team/pkg.git",
        ] {
            assert_eq!(validate_git_url(url), Ok(()), "`{url}` must be accepted");
        }
    }

    #[test]
    fn git_url_accepts_local_forms() {
        for url in [
            "file:///srv/repos/pkg.git",
            "/srv/repos/pkg.git",
            "./pkg.git",
            "../pkg.git",
            "../../shared/pkg.git",
        ] {
            assert_eq!(validate_git_url(url), Ok(()), "`{url}` must be accepted");
        }
    }

    #[test]
    fn git_url_accepts_a_space_in_local_forms_only() {
        for url in [
            "file:///srv/my repos/pkg.git",
            "/srv/my repos/pkg.git",
            "./my repos/pkg.git",
            "../my repos/pkg.git",
        ] {
            assert_eq!(validate_git_url(url), Ok(()), "{url:?} must be accepted");
        }
        for url in [
            "https://example.com/my repos/pkg.git",
            "http://127.0.0.1:1/my repos/pkg.git",
            "ssh://git@example.com/my repos/pkg.git",
            "git://example.com/my repos/pkg.git",
            "git@example.com:my repos/pkg.git",
            "example.com:my repos/pkg.git",
            // A Windows drive-letter path is checked as `host:path`.
            "C:\\my repos\\pkg.git",
            // Neither local form: the value does not start with one.
            " /srv/repos/pkg.git",
            "my repos/pkg.git",
            "not a url",
            "ext::sh -c touch% /tmp/silt_marker",
        ] {
            assert!(
                rejection_reason(url).starts_with(RULE_SPACE),
                "validate_git_url gave the wrong reason for rejecting {url:?}"
            );
        }
    }

    #[test]
    fn git_url_rejects_the_empty_string() {
        assert!(rejection_reason("").starts_with("must not be empty"));
    }

    #[test]
    fn git_url_rejects_a_leading_dash() {
        for url in [
            "-",
            "--upload-pack=touch /tmp/silt_marker",
            "--upload-pack=x@h:p",
            "-oProxyCommand=x@host:path",
            "-oProxyCommand=x:path",
        ] {
            assert!(
                rejection_reason(url).starts_with("must not start with `-`"),
                "validate_git_url gave the wrong reason for rejecting {url:?}"
            );
        }
    }

    #[test]
    fn git_url_rejects_other_whitespace_and_control_characters_everywhere() {
        // Only U+0020 is tolerated, and only in the local forms.
        let hostile = ['\t', '\n', '\r', '\0', '\u{1b}', '\u{7f}', '\u{85}'];
        let other_space = ['\u{a0}', '\u{2003}', '\u{2028}', '\u{2029}', '\u{3000}'];
        for c in hostile.into_iter().chain(other_space) {
            for url in [
                format!("https://example.com/a{c}b.git"),
                format!("git@example.com:a{c}b.git"),
                format!("example.com:a{c}b.git"),
                format!("file:///srv/a{c}b.git"),
                format!("/srv/a{c}b.git"),
                format!("../a{c}b.git"),
            ] {
                assert!(
                    rejection_reason(&url).starts_with(RULE_SPACE),
                    "validate_git_url gave the wrong reason for rejecting {url:?}"
                );
            }
        }
    }

    #[test]
    fn git_url_rejects_invisible_and_bidirectional_characters_everywhere() {
        let chars = invisible_format_chars();
        assert_eq!(chars.len(), 1 + 5 + 5 + 5 + 4 + 1);
        for c in chars {
            for url in [
                format!("https://example.com/a{c}b.git"),
                format!("https://exam{c}ple.com/pkg.git"),
                format!("git@example.com:a{c}b.git"),
                format!("example.com:a{c}b.git"),
                format!("file:///srv/a{c}b.git"),
                format!("/srv/a{c}b.git"),
                format!("../a{c}b.git"),
                format!("{c}https://example.com/pkg.git"),
            ] {
                assert!(
                    rejection_reason(&url).starts_with(RULE_INVISIBLE),
                    "validate_git_url gave the wrong reason for rejecting {url:?}"
                );
            }
        }
    }

    #[test]
    fn git_url_tells_host_path_from_transport_address() {
        // One `:` is the scp-like form; two make a remote-helper
        // transport, which runs a `git-remote-<transport>` program.
        for (host_path, transport_address) in [
            ("ext:sh", "ext::sh"),
            ("fd:3", "fd::3"),
            ("example.com:pkg.git", "example.com::pkg.git"),
            ("git@example.com:pkg.git", "git@example.com::pkg.git"),
            ("helper:git@host:path", "helper::git@host:path"),
        ] {
            assert_eq!(
                validate_git_url(host_path),
                Ok(()),
                "the scp-like form {host_path:?} must be accepted"
            );
            assert!(
                rejection_reason(transport_address).starts_with(RULE_FORM),
                "validate_git_url gave the wrong reason for rejecting {transport_address:?}"
            );
        }
    }

    #[test]
    fn git_url_rejects_unrecognised_forms() {
        for url in [
            // Unknown schemes would run `git-remote-<scheme>`.
            "foo://example.com/pkg.git",
            "HTTPS://example.com/pkg.git",
            "ext://sh",
            "git+ssh://example.com/pkg.git",
            "user@foo://example.com/pkg.git",
            // A scheme with nothing after it.
            "https://",
            "file://",
            // Option-shaped authority or host, handed to `ssh`.
            "ssh://-oProxyCommand=x/pkg.git",
            "ssh://user@-oProxyCommand=x/pkg.git",
            "git@-oProxyCommand=x:pkg.git",
            // Option-shaped or missing scp path.
            "git@example.com:-pkg.git",
            "example.com:-pkg.git",
            "git@example.com:",
            "example.com:",
            // Missing user or host, or a `/` before the `:`.
            "@example.com:pkg.git",
            "git@:pkg.git",
            ":pkg.git",
            "a/b@example.com:pkg.git",
            "a/b:pkg.git",
            // Relative paths that do not start with `./` or `../`.
            "pkg.git",
            "repos/pkg.git",
            ".hidden/pkg.git",
            ".",
            "..",
        ] {
            assert!(
                rejection_reason(url).starts_with(RULE_FORM),
                "validate_git_url gave the wrong reason for rejecting {url:?}"
            );
        }
    }

    #[test]
    fn git_url_rejection_is_printable_and_names_the_accepted_forms() {
        let err = validate_git_url("https://example.com/a\nb\u{1b}.git").unwrap_err();
        assert_eq!(err.url, "https://example.com/a\\nb\\u{1b}.git");
        let err = validate_git_url("https://example.com/\u{202e}tig.git").unwrap_err();
        assert_eq!(err.url, "https://example.com/\\u{202e}tig.git");
        let rendered = err.to_string();
        assert!(
            rendered.is_ascii(),
            "rendered error must be plain ASCII here: {rendered:?}"
        );
        for form in [
            "`https://`",
            "`http://`",
            "`ssh://`",
            "`git://`",
            "`file://`",
            "`host:path`",
            "`user@host:path`",
            "`/`, `./` or `../`",
        ] {
            assert!(rendered.contains(form), "{form} missing from: {rendered}");
        }
    }

    #[test]
    fn escape_for_display_escapes_everything_that_can_mislead() {
        assert_eq!(escape_for_display("a\nb\tc\rd"), "a\\nb\\tc\\rd");
        assert_eq!(escape_for_display("\u{1b}[31mred"), "\\u{1b}[31mred");
        assert_eq!(escape_for_display("a\u{a0}b"), "a\\u{a0}b");
        assert_eq!(escape_for_display("a\u{2028}b"), "a\\u{2028}b");
        assert_eq!(escape_for_display("a\u{2029}b"), "a\\u{2029}b");
        for c in invisible_format_chars() {
            let escaped = escape_for_display(&format!("a{c}b"));
            assert_eq!(escaped, format!("a\\u{{{:x}}}b", c as u32));
        }
        // Ordinary text, including non-ASCII letters, is left alone.
        let plain = "/srv/my repos/caf\u{e9}/\u{65e5}\u{672c}.git";
        assert_eq!(escape_for_display(plain), plain);
    }

    /// True if `s` holds a character a terminal would act on or hide.
    fn has_unprintable(s: &str, allow_newline: bool) -> bool {
        s.chars().any(|c| {
            if c == '\n' {
                return !allow_newline;
            }
            c.is_control()
                || is_invisible_format_char(c)
                || matches!(c, '\u{a0}' | '\u{2028}' | '\u{2029}')
        })
    }

    #[test]
    fn git_errors_escape_the_ref_the_url_and_the_command() {
        let hostile = "main\nFORGED\u{1b}[2K\u{202e}";
        let escaped = "main\\nFORGED\\u{1b}[2K\\u{202e}";
        for ref_spec in [
            GitRef::Branch(hostile.into()),
            GitRef::Tag(hostile.into()),
            GitRef::Rev(hostile.into()),
        ] {
            let rendered = GitError::RefNotFound {
                url: format!("https://example.com/{hostile}"),
                ref_spec,
            }
            .to_string();
            assert!(!has_unprintable(&rendered, false), "{rendered:?}");
            assert_eq!(rendered.matches(escaped).count(), 2, "{rendered}");
        }

        let rendered = GitError::CommandFailed {
            command: format_command("git", &["ls-remote", "--", "u", hostile]),
            stderr: format!("fatal: first line\nremote: {hostile}\n"),
            exit_code: Some(128),
        }
        .to_string();
        // The only line breaks left are the one silt writes before
        // `stderr:` and the ones git itself wrote.
        assert!(!has_unprintable(&rendered, true), "{rendered:?}");
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(
            lines,
            [
                format!("git command failed (exit 128): `git ls-remote -- u {escaped}`"),
                "stderr: fatal: first line".to_string(),
                "remote: main".to_string(),
                "FORGED\\u{1b}[2K\\u{202e}".to_string(),
            ]
        );
    }

    #[test]
    fn cache_for_refuses_a_path_shaped_commit() {
        // Rejected before the cache root is computed or created.
        for sha in ["../../../../x", "..", "abc1234/x", "-abc1234", ""] {
            let err = cache_for("https://example.com/foo", sha)
                .expect_err("path-shaped commit must be refused")
                .to_string();
            assert!(
                err.contains("invalid resolved commit"),
                "unexpected error for {sha:?}: {err}"
            );
        }
    }

    #[test]
    fn git_entry_points_refuse_an_option_shaped_url() {
        // Each of these fails in the URL check, before `git` is spawned.
        let url = "--upload-pack=touch /tmp/silt_marker";
        let errors = [
            resolve_ref(url, &GitRef::Branch("main".into()))
                .expect_err("resolve_ref must refuse")
                .to_string(),
            verify_reachable(url)
                .expect_err("verify_reachable must refuse")
                .to_string(),
            fetch_to_cache(url, "abc1234")
                .expect_err("fetch_to_cache must refuse")
                .to_string(),
        ];
        for err in errors {
            assert!(
                err.contains("invalid git URL") && err.contains("must not start with `-`"),
                "unexpected error: {err}"
            );
        }
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
