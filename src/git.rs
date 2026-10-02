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

use std::fmt::{self, Write as _};
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
    /// A commit id, full or a prefix of at least 7 hex characters. A
    /// prefix is resolved to the one commit it names (never to a tag or
    /// a branch of the same name), and the lock stores the full id.
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
    /// A short `rev` is the prefix of several commits.
    AmbiguousRev { url: String, rev: String },
    /// A checkout holds a symbolic link, at `path` (relative to the
    /// checkout). A dependency's files must be its own.
    Symlink { path: PathBuf },
}

/// What every line of git's own output starts with when silt shows it.
/// A remote can send that text, so no line of it may be taken for one
/// of silt's.
const GIT_OUTPUT_PREFIX: &str = "  git: ";

impl fmt::Display for GitError {
    // Untrusted text, per variant:
    //   - `GitNotInstalled`: none.
    //   - `CommandFailed`: `command` holds the URL and the ref, which
    //     come from a manifest; `stderr` is git's output, which can
    //     carry text sent by the remote.
    //   - `Io`: none from a manifest. `context` holds a cache path
    //     built from the environment, `error` is the operating
    //     system's text.
    //   - `RefNotFound`: `url` and `ref_spec` come from a manifest.
    //   - `InvalidInput`: the message quotes a URL from a manifest or a
    //     commit id from a lockfile.
    //   - `AmbiguousRev`: both fields come from a manifest.
    //   - `Symlink`: the path is a file name of the repository.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Shadows the formatter: nothing below can be written without
        // going through the display rule.
        let mut f = EscapingWriter::new(f);
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
                write!(f, "git command failed (exit {code}): `{command}`")?;
                // Each line git wrote gets a line of its own, marked
                // as git's. An empty line is the prefix alone, without
                // its trailing space.
                for line in stderr.trim_end().lines() {
                    f.line_break()?;
                    if line.is_empty() {
                        write!(f, "{}", GIT_OUTPUT_PREFIX.trim_end())?;
                    } else {
                        write!(f, "{GIT_OUTPUT_PREFIX}{line}")?;
                    }
                }
                Ok(())
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
            GitError::InvalidInput(message) => write!(f, "{message}"),
            GitError::AmbiguousRev { url, rev } => write!(
                f,
                "rev `{rev}` is the prefix of more than one commit in {url}; \
                 write the full commit id"
            ),
            GitError::Symlink { path } => write!(
                f,
                "the repository holds a symbolic link, `{}`; a dependency may not \
                 contain symbolic links",
                path.display()
            ),
        }
    }
}

impl EscapedDisplay for GitError {}

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
    // Both fields quote the URL, which comes from a manifest or from
    // the command line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = EscapingWriter::new(f);
        write!(f, "invalid git URL `{}`: {}", self.url, self.reason)
    }
}

impl EscapedDisplay for InvalidGitUrl {}

impl std::error::Error for InvalidGitUrl {}

/// Validate a git dependency URL.
///
/// Rejects, in this order:
///   - the empty string;
///   - anything starting with `-` (git would parse it as an option);
///   - any character its form does not allow, see the table;
///   - anything that is not one of the accepted forms below.
///
/// Which characters a URL may hold depends on its form. It is in a
/// local form if it starts with `file://`, `/`, `./` or `../`. Every
/// other value is checked as a network form: `https://`, `http://`,
/// `ssh://`, `git://`, `host:path` and `user@host:path`.
///
/// | Character | Network forms | Local forms |
/// |---|---|---|
/// | a control character | rejected | rejected |
/// | U+0020, the ordinary space | rejected | accepted |
/// | any other whitespace character | rejected | rejected |
/// | an invisible or direction-control character | rejected | rejected |
/// | any other character that is not ASCII and not a letter or a digit | rejected | accepted |
/// | anything else | accepted | accepted |
///
/// So a network form holds printable ASCII without the space, and
/// letters and digits of any script. A local form holds whatever a
/// directory name holds and a reader can see: spaces, and punctuation,
/// symbols and combining marks of any script. The invisible and
/// direction-control characters are listed at
/// `is_invisible_format_char`; one of them makes a value read as
/// something it is not. The two rules stand on their own: neither is
/// derived from the rule for showing text ([`needs_escape`]).
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
    let is_forbidden: fn(char) -> bool = if local {
        is_forbidden_in_local_form
    } else {
        is_forbidden_in_network_form
    };
    let problem = if url.is_empty() {
        "must not be empty".to_string()
    } else if url.starts_with('-') {
        "must not start with `-`".to_string()
    } else if url.chars().any(is_forbidden) {
        // Which rule to name: whitespace and control characters first,
        // then the invisible characters, then the rest.
        let is_space_or_control = |c: char| c.is_control() || c.is_whitespace();
        if url
            .chars()
            .any(|c| is_forbidden(c) && is_space_or_control(c))
        {
            // The hint names what the value's own form allows.
            if local {
                "must not contain whitespace or control characters \
                 (the ordinary space is the only one a `file://` URL or a local path \
                 may contain)"
                    .to_string()
            } else {
                "must not contain whitespace or control characters \
                 (a space is allowed only in a `file://` URL or a local path)"
                    .to_string()
            }
        } else if url.chars().any(is_invisible_format_char) {
            "must not contain invisible or bidirectional formatting characters".to_string()
        } else {
            "must not contain non-ASCII characters other than letters and digits \
             (they are allowed only in a `file://` URL or a local path)"
                .to_string()
        }
    } else if has_accepted_url_form(url) {
        return Ok(());
    } else {
        match unaccepted_scheme(url) {
            Some(scheme) => format!(
                "is not a recognised git URL form: the scheme `{scheme}://` is not accepted, \
                 use `ssh://` or `https://` instead"
            ),
            None => "is not a recognised git URL form".to_string(),
        }
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

/// The scheme of a URL written as `<scheme>://...` whose scheme is not
/// one of `GIT_URL_SCHEMES`, such as `git+ssh` or `ftp`.
fn unaccepted_scheme(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once("://")?;
    let scheme_shaped = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let accepted = GIT_URL_SCHEMES
        .iter()
        .any(|known| known.strip_suffix("://") == Some(scheme));
    (scheme_shaped && !accepted).then_some(scheme)
}

/// The character rule of the network forms: is `c` a character that a
/// git URL of a network form must not hold? That is every control
/// character, every whitespace character, every invisible or
/// direction-control character, and every character that is not ASCII
/// and not a letter or a digit.
fn is_forbidden_in_network_form(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || is_invisible_format_char(c)
        || (!c.is_ascii() && !c.is_alphanumeric())
}

/// The character rule of the local forms: is `c` a character that a git
/// URL of a local form must not hold? That is every control character,
/// every whitespace character but the ordinary space (U+0020), and
/// every invisible or direction-control character. Everything else is
/// allowed, because directory names hold it.
fn is_forbidden_in_local_form(c: char) -> bool {
    c.is_control() || (c.is_whitespace() && c != ' ') || is_invisible_format_char(c)
}

/// Invisible and direction-control characters: characters that show
/// nothing, or that change how the text around them is shown. The
/// standard library has no test for this, so the list is explicit. It
/// is Unicode's `Default_Ignorable_Code_Point` set, plus the
/// interlinear annotation characters and the blank Braille pattern:
///
///   - U+00AD, the soft hyphen;
///   - U+034F, the combining grapheme joiner;
///   - U+061C, the Arabic letter mark;
///   - U+115F, U+1160, U+3164 and U+FFA0, the Hangul fillers (blank,
///     and letters as far as [`char::is_alphanumeric`] is concerned);
///   - U+17B4 and U+17B5, the Khmer inherent vowels;
///   - U+180B to U+180F, the Mongolian variation selectors and vowel
///     separator;
///   - U+200B to U+200F, the zero-width space, the joiners and the
///     directional marks;
///   - U+202A to U+202E, the directional embeddings and overrides;
///   - U+2060 to U+206F, the word joiner, the invisible operators, the
///     directional isolates and the deprecated format characters;
///   - U+2800, the blank Braille pattern;
///   - U+FE00 to U+FE0F, the variation selectors;
///   - U+FEFF, the byte-order mark;
///   - U+FFF0 to U+FFFB, reserved and the interlinear annotation
///     characters;
///   - U+1BCA0 to U+1BCA3, the shorthand format controls;
///   - U+1D173 to U+1D17A, the musical format controls;
///   - U+E0000 to U+E0FFF, the tag characters and the variation
///     selectors supplement.
fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
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

/// [`validate_git_url`] as a [`GitError`], for the functions below that
/// hand the URL to `git`.
fn check_url(url: &str) -> Result<(), GitError> {
    validate_git_url(url).map_err(|e| GitError::InvalidInput(e.to_string()))
}

// ── Printing untrusted text ────────────────────────────────────────────
//
// A `silt.toml` can be a dependency's, or a dependency's dependency's,
// and a `silt.lock` can be edited by hand, so every value read from
// either is untrusted. So is what `git` prints: a remote can send text.
// Printed as it is, a line break in such a value starts a line that
// reads like one of silt's own, and an escape character drives the
// terminal.
//
// One rule, `needs_escape`, says which characters are shown as escapes.
// The error types of this module, of `manifest` and of `lockfile` apply
// it to their whole message, not value by value: their `Display` impls
// write through an `EscapingWriter`, so a variant added later is
// covered as well.

/// The display rule: is `c` shown as an escape (`\n`, `\u{1b}`,
/// `\u{202e}`) when silt prints untrusted text?
///
/// The rule is a deny list. Escaped are exactly:
///   - every control character;
///   - every whitespace character ([`char::is_whitespace`]) other than
///     the ordinary space (U+0020): the no-break and the other unusual
///     spaces, and the line and paragraph separators;
///   - every invisible or direction-control character (the list is at
///     `is_invisible_format_char`).
///
/// Everything else is shown as it is: letters and digits, punctuation,
/// symbols and combining marks of any script. A directory named
/// `Projekte – 2024`, or a file name with a decomposed accent, reads as
/// it does in a file listing.
///
/// A backslash is shown as it is, and no character of an escape is
/// itself escaped, so escaping a text twice gives the same result as
/// escaping it once.
pub fn needs_escape(c: char) -> bool {
    c.is_control() || (c.is_whitespace() && c != ' ') || is_invisible_format_char(c)
}

/// Render an untrusted string for a message, by the display rule
/// ([`needs_escape`]): a hostile value cannot forge extra lines, drive
/// the terminal, or disguise what it is.
pub fn escape_for_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if needs_escape(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// What the `Display` impl of an error type writes to, in place of the
/// formatter. Everything written through it, the fixed words of the
/// message as much as the values, is escaped by the display rule
/// ([`needs_escape`]). The message itself decides where a line ends,
/// with [`EscapingWriter::line_break`]; a line break inside a value is
/// shown as `\n`.
pub struct EscapingWriter<'a, 'b> {
    out: &'a mut fmt::Formatter<'b>,
}

impl<'a, 'b> EscapingWriter<'a, 'b> {
    /// Wrap the formatter a `Display` impl was given.
    pub fn new(out: &'a mut fmt::Formatter<'b>) -> Self {
        EscapingWriter { out }
    }

    /// End the current line of the message and start the next one.
    pub fn line_break(&mut self) -> fmt::Result {
        self.out.write_char('\n')
    }

    /// Write `text` line by line: each line escaped, its line breaks
    /// kept. Only for text whose line breaks are known not to come
    /// from an untrusted value; a line break that does would start a
    /// line of output of its own.
    pub fn lines(&mut self, text: &str) -> fmt::Result {
        for (index, line) in text.lines().enumerate() {
            if index > 0 {
                self.line_break()?;
            }
            self.write_str(line)?;
        }
        Ok(())
    }

    /// Write an error that escapes its own message, keeping the line
    /// breaks that message makes.
    pub fn nested(&mut self, inner: &dyn EscapedDisplay) -> fmt::Result {
        fmt::Display::fmt(inner, &mut *self.out)
    }
}

impl fmt::Write for EscapingWriter<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            if needs_escape(c) {
                for escaped in c.escape_default() {
                    self.out.write_char(escaped)?;
                }
            } else {
                self.out.write_char(c)?;
            }
        }
        Ok(())
    }
}

/// Marks an error type whose `Display` impl writes its whole message
/// through an [`EscapingWriter`]. Only such a type can be handed to
/// [`EscapingWriter::nested`].
pub trait EscapedDisplay: fmt::Display {}

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

/// Returns the per-(url, commit) cache directory.
///
/// Format: `<cache_dir>/<url-sha256-prefix>/<commit>/`. The
/// directory is *not* created here — callers (specifically
/// [`fetch_to_cache`]) handle creation/atomic rename.
///
/// `commit` becomes a path component, and one commit must have one
/// directory, so only a full commit id ([`is_full_commit_id`]) is
/// accepted: a value such as `../../../../x` (from a hand-edited
/// `silt.lock`) would otherwise point the cache path at an arbitrary
/// directory, and a prefix would give one commit several.
pub fn cache_for(url: &str, commit: &str) -> Result<PathBuf, GitError> {
    if !is_full_commit_id(commit) {
        return Err(GitError::InvalidInput(format!(
            "invalid commit id `{}`: expected 40 or 64 hexadecimal characters",
            escape_for_display(commit)
        )));
    }
    let root = cache_dir()?;
    Ok(root.join(url_hash(url)).join(commit.to_lowercase()))
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

/// Resolve a [`GitRef`] against the remote URL, returning the full
/// commit id.
///
/// A full `Rev` is returned without contacting the network — the fetch
/// fails loudly later if the commit doesn't exist remotely. A short
/// `Rev` is resolved in a clone of the repository to the one commit
/// whose id it starts with; a tag or a branch of the same name is never
/// taken for it (see [`fetch_rev`]). For `Branch`/`Tag` we run
/// `git ls-remote -- <url> <ref>` and parse the id out.
///
/// The URL must pass [`validate_git_url`]; the branch or tag name is
/// only ever sent as `refs/heads/<name>` / `refs/tags/<name>`, which
/// cannot be option-shaped.
pub fn resolve_ref(url: &str, ref_spec: &GitRef) -> Result<String, GitError> {
    check_url(url)?;
    match ref_spec {
        GitRef::Rev(rev) if is_full_commit_id(rev) => Ok(rev.to_lowercase()),
        GitRef::Rev(rev) => fetch_rev(url, rev).map(|(commit, _)| commit),
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
/// This is the shape of a `rev` a manifest or `silt add --rev` may
/// give: a full id or a prefix of one. Being hexadecimal only, an
/// accepted value can be neither a path nor an option.
pub fn is_valid_sha_shape(s: &str) -> bool {
    let len = s.len();
    if !(7..=64).contains(&len) {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Returns true if `s` is a full commit id: 40 hexadecimal characters
/// (SHA-1) or 64 (SHA-256). The one shape a lockfile `rev` and a cache
/// directory name have.
pub fn is_full_commit_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ── Fetch ──────────────────────────────────────────────────────────────

/// Fetch the repo at the full commit id `commit` into the cache and
/// return the checkout directory. See [`fetch_rev`].
pub fn fetch_to_cache(url: &str, commit: &str) -> Result<PathBuf, GitError> {
    check_url(url)?;
    if !is_full_commit_id(commit) {
        return Err(GitError::InvalidInput(format!(
            "invalid commit id `{}`: expected 40 or 64 hexadecimal characters",
            escape_for_display(commit)
        )));
    }
    fetch_rev(url, commit).map(|(_, dir)| dir)
}

/// Fetch the commit `rev` names into the cache: returns its full id and
/// the checkout directory.
///
/// `rev` is a full commit id or a prefix of one (7 or more hexadecimal
/// characters). A prefix is resolved in the clone with `git rev-parse
/// --disambiguate`, which matches object ids only: a tag or a branch
/// that happens to be named like the prefix is never taken for it, as
/// `git checkout <prefix>` would. Exactly one commit must match.
///
/// Idempotent: if the cache dir already exists with a `silt.toml` we
/// take that as a sign the cache is populated and skip the fetch.
/// Otherwise we clone into a sibling `.tmp` dir and atomically rename
/// on success — this avoids leaving a half-populated cache after an
/// interrupted clone. A checkout that holds a symbolic link (outside
/// `.git`) is rejected and not cached.
pub fn fetch_rev(url: &str, rev: &str) -> Result<(String, PathBuf), GitError> {
    check_url(url)?;
    if !is_valid_sha_shape(rev) {
        return Err(GitError::RefNotFound {
            url: url.to_string(),
            ref_spec: GitRef::Rev(rev.to_string()),
        });
    }
    let rev = rev.to_lowercase();
    if is_full_commit_id(&rev) {
        let dest = cache_for(url, &rev)?;
        if dest.join("silt.toml").is_file() {
            return Ok((rev, dest));
        }
    }

    // `<cache>/<url-hash>/`; the per-commit leaf directory itself is
    // created by the rename below.
    let url_dir = cache_dir()?.join(url_hash(url));
    fs::create_dir_all(&url_dir).map_err(|e| GitError::Io {
        context: format!("create cache parent {}", url_dir.display()),
        error: e,
    })?;

    // Atomic-ish: clone into <url-hash>/<rev>.tmp, then rename.
    let tmp = url_dir.join(format!("{rev}.tmp"));
    if tmp.exists() {
        // Stale tmp from a previous interrupted clone.
        fs::remove_dir_all(&tmp).map_err(|e| GitError::Io {
            context: format!("remove stale tmp dir {}", tmp.display()),
            error: e,
        })?;
    }
    let tmp_str = tmp.to_str().ok_or_else(|| GitError::Io {
        context: "tmp path is not valid UTF-8".into(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-UTF-8 cache path"),
    })?;

    // Full clone (not shallow): the user picked a specific commit and
    // we don't know whether `--depth=1` would include it. `--` ends
    // option parsing so neither the URL nor the path can be read as an
    // option.
    run_git(&["clone", "--quiet", "--", url, tmp_str])?;
    let result = finish_checkout(url, &rev, &tmp, tmp_str);
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

/// The rest of [`fetch_rev`] once `tmp` holds a clone: resolve `rev`,
/// check the commit out, refuse symbolic links, move it into place.
fn finish_checkout(
    url: &str,
    rev: &str,
    tmp: &Path,
    tmp_str: &str,
) -> Result<(String, PathBuf), GitError> {
    let commit = if is_full_commit_id(rev) {
        rev.to_string()
    } else {
        commit_with_prefix(url, rev, tmp_str)?
    };
    let dest = cache_for(url, &commit)?;
    if dest.join("silt.toml").is_file() {
        let _ = fs::remove_dir_all(tmp);
        return Ok((commit, dest));
    }

    // `commit` is a full id here, which git reads as an object id even
    // if a ref of the same name exists.
    run_git(&["-C", tmp_str, "checkout", "--quiet", "--detach", &commit])?;
    if let Some(path) = find_symlink(tmp).map_err(|e| GitError::Io {
        context: format!("scan checkout {}", tmp.display()),
        error: e,
    })? {
        return Err(GitError::Symlink { path });
    }

    if dest.exists() {
        // Race: another process populated the cache between our existence
        // check and the rename. Discard our tmp and return the existing
        // dir if it has a silt.toml; otherwise propagate as an Io error.
        if dest.join("silt.toml").is_file() {
            let _ = fs::remove_dir_all(tmp);
            return Ok((commit, dest));
        }
        fs::remove_dir_all(&dest).map_err(|e| GitError::Io {
            context: format!("remove pre-existing cache leaf {}", dest.display()),
            error: e,
        })?;
    }

    fs::rename(tmp, &dest).map_err(|e| GitError::Io {
        context: format!("rename {} -> {}", tmp.display(), dest.display()),
        error: e,
    })?;

    Ok((commit, dest))
}

/// The one commit of the clone at `repo` whose id starts with `prefix`.
/// `git rev-parse --disambiguate` lists the objects of every type with
/// that prefix and looks at no ref; the commits among them are kept.
fn commit_with_prefix(url: &str, prefix: &str, repo: &str) -> Result<String, GitError> {
    let listed = run_git(&["-C", repo, "rev-parse", &format!("--disambiguate={prefix}")])?;
    let mut commits = Vec::new();
    for object in listed.split_whitespace() {
        if !is_full_commit_id(object) {
            continue;
        }
        let kind = run_git(&["-C", repo, "cat-file", "-t", object])?;
        if kind.trim() == "commit" {
            commits.push(object.to_lowercase());
        }
    }
    match commits.len() {
        1 => Ok(commits.remove(0)),
        0 => Err(GitError::RefNotFound {
            url: url.to_string(),
            ref_spec: GitRef::Rev(prefix.to_string()),
        }),
        _ => Err(GitError::AmbiguousRev {
            url: url.to_string(),
            rev: prefix.to_string(),
        }),
    }
}

/// The first symbolic link under `dir`, as a path relative to `dir`,
/// not descending into a top-level `.git`. Nothing is followed: a link
/// to a directory is reported, not walked.
pub fn find_symlink(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    fn walk(base: &Path, dir: &Path) -> std::io::Result<Option<PathBuf>> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Ok(Some(path.strip_prefix(base).unwrap_or(&path).to_path_buf()));
            }
            if kind.is_dir() {
                if dir == base && entry.file_name() == ".git" {
                    continue;
                }
                if let Some(found) = walk(base, &path)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    walk(dir, dir)
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
///
/// On failure the error shows the command as it was run, hardening
/// arguments included.
fn run_git(args: &[&str]) -> Result<String, GitError> {
    let mut full_args = vec!["-c", "protocol.ext.allow=never"];
    full_args.extend_from_slice(args);
    let output = Command::new("git")
        .args(&full_args)
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
            command: format_command("git", &full_args),
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
    const RULE_NON_ASCII: &str =
        "must not contain non-ASCII characters other than letters and digits";
    const RULE_FORM: &str = "is not a recognised git URL form";

    /// Every character `is_invisible_format_char` lists.
    fn invisible_format_chars() -> Vec<char> {
        let ranges = [
            ('\u{00AD}', '\u{00AD}'),
            ('\u{034F}', '\u{034F}'),
            ('\u{061C}', '\u{061C}'),
            ('\u{115F}', '\u{1160}'),
            ('\u{17B4}', '\u{17B5}'),
            ('\u{180B}', '\u{180F}'),
            ('\u{200B}', '\u{200F}'),
            ('\u{202A}', '\u{202E}'),
            ('\u{2060}', '\u{206F}'),
            ('\u{2800}', '\u{2800}'),
            ('\u{3164}', '\u{3164}'),
            ('\u{FE00}', '\u{FE0F}'),
            ('\u{FEFF}', '\u{FEFF}'),
            ('\u{FFA0}', '\u{FFA0}'),
            ('\u{FFF0}', '\u{FFFB}'),
            ('\u{1BCA0}', '\u{1BCA3}'),
            ('\u{1D173}', '\u{1D17A}'),
            ('\u{E0000}', '\u{E0FFF}'),
        ];
        let mut chars = Vec::new();
        for (first, last) in ranges {
            chars.extend(first..=last);
        }
        chars
    }

    /// Characters that passed the explicit list this module used to
    /// have, although they are invisible or control direction.
    const ONCE_MISSED_INVISIBLE: [char; 11] = [
        '\u{061C}',
        '\u{180E}',
        '\u{FFF9}',
        '\u{FFFB}',
        '\u{E0001}',
        '\u{E0061}',
        '\u{1D173}',
        '\u{034F}',
        '\u{3164}',
        '\u{FE0F}',
        '\u{2065}',
    ];

    /// Spaces other than U+0020, the line and paragraph separators
    /// included.
    const UNUSUAL_SPACES: [char; 7] = [
        '\u{a0}', '\u{1680}', '\u{2003}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{3000}',
    ];

    /// Non-ASCII punctuation and symbols that are neither spaces nor
    /// invisible: an en dash, a copyright sign, an ellipsis, a euro
    /// sign and a full-width parenthesis.
    const NON_ASCII_PUNCTUATION: [char; 5] =
        ['\u{2013}', '\u{a9}', '\u{2026}', '\u{20ac}', '\u{ff08}'];

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
        for c in hostile.into_iter().chain(UNUSUAL_SPACES) {
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
        assert_eq!(
            chars.len(),
            1 + 1 + 1 + 2 + 2 + 5 + 5 + 5 + 16 + 1 + 1 + 16 + 1 + 1 + 12 + 4 + 8 + 4096
        );
        for c in ONCE_MISSED_INVISIBLE {
            assert!(chars.contains(&c), "U+{:04X} is not listed", c as u32);
        }
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
    fn git_url_accepts_non_ascii_punctuation_in_local_forms_only() {
        for c in NON_ASCII_PUNCTUATION {
            for url in [
                format!("file:///srv/a{c}b/pkg.git"),
                format!("/srv/a{c}b/pkg.git"),
                format!("./a{c}b/pkg.git"),
                format!("../a{c}b/pkg.git"),
            ] {
                assert_eq!(validate_git_url(&url), Ok(()), "{url:?} must be accepted");
            }
            for url in [
                format!("https://example.com/a{c}b.git"),
                format!("https://exam{c}ple.com/pkg.git"),
                format!("http://127.0.0.1:1/a{c}b.git"),
                format!("ssh://git@example.com/a{c}b.git"),
                format!("git://example.com/a{c}b.git"),
                format!("git@example.com:a{c}b.git"),
                format!("example.com:a{c}b.git"),
                // Neither local form, so checked as a network form.
                format!("a{c}b/pkg.git"),
            ] {
                assert!(
                    rejection_reason(&url).starts_with(RULE_NON_ASCII),
                    "validate_git_url gave the wrong reason for rejecting {url:?}"
                );
            }
        }
    }

    #[test]
    fn git_url_accepts_letters_and_digits_of_any_script_in_every_form() {
        // Latin with a diacritic, Cyrillic, Han, and an Arabic-Indic
        // digit.
        for name in [
            "caf\u{e9}",
            "\u{43f}\u{440}",
            "\u{65e5}\u{672c}",
            "v\u{661}",
        ] {
            for url in [
                format!("https://example.com/{name}.git"),
                format!("https://{name}.example/pkg.git"),
                format!("ssh://git@example.com/{name}.git"),
                format!("git@example.com:{name}.git"),
                format!("example.com:{name}.git"),
                format!("file:///srv/{name}/pkg.git"),
                format!("/srv/{name}/pkg.git"),
                format!("../{name}/pkg.git"),
            ] {
                assert_eq!(validate_git_url(&url), Ok(()), "{url:?} must be accepted");
            }
        }
    }

    #[test]
    fn git_url_rejection_names_the_replacement_for_an_unaccepted_scheme() {
        for (url, scheme) in [
            ("git+ssh://example.com/pkg.git", "`git+ssh://`"),
            ("ftp://example.com/pkg.git", "`ftp://`"),
            ("HTTPS://example.com/pkg.git", "`HTTPS://`"),
        ] {
            let reason = rejection_reason(url);
            assert!(reason.starts_with(RULE_FORM), "{url:?}: {reason}");
            assert!(
                reason.contains(&format!("the scheme {scheme} is not accepted"))
                    && reason.contains("use `ssh://` or `https://` instead"),
                "the rejection of {url:?} must name the replacement: {reason}"
            );
        }
        // An accepted scheme, or no scheme at all: nothing to replace.
        for url in ["https://", "ssh://-oProxyCommand=x/pkg.git", "pkg.git"] {
            let reason = rejection_reason(url);
            assert!(reason.starts_with(RULE_FORM), "{url:?}: {reason}");
            assert!(!reason.contains("the scheme"), "{url:?}: {reason}");
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
        let shown_as_escapes = invisible_format_chars()
            .into_iter()
            .chain(ONCE_MISSED_INVISIBLE)
            .chain(UNUSUAL_SPACES);
        for c in shown_as_escapes {
            assert!(needs_escape(c), "U+{:04X} must be escaped", c as u32);
            let escaped = escape_for_display(&format!("a{c}b"));
            assert_eq!(escaped, format!("a\\u{{{:x}}}b", c as u32));
        }
        // Every control character.
        for c in ('\0'..='\u{1f}').chain('\u{7f}'..='\u{9f}') {
            assert!(needs_escape(c), "U+{:04X} must be escaped", c as u32);
        }
    }

    #[test]
    fn escape_for_display_leaves_what_a_reader_can_see() {
        for c in ' '..='~' {
            assert!(!needs_escape(c), "{c:?} must not be escaped");
        }
        // Punctuation and symbols outside ASCII, a combining mark, a
        // private-use character and the replacement character.
        let visible = NON_ASCII_PUNCTUATION
            .into_iter()
            .chain(['\u{301}', '\u{e000}', '\u{fffd}']);
        for c in visible {
            assert!(!needs_escape(c), "U+{:04X} must not be escaped", c as u32);
        }
        for plain in [
            "/srv/my repos/caf\u{e9}/\u{65e5}\u{672c}/\u{43f}\u{440}/v\u{661}.git",
            // An en dash, a decomposed accent, Han.
            "/home/me/Projekte \u{2013} 2024/cafe\u{301}/\u{65e5}\u{672c}/silt.toml",
            "C:\\Users\\me\\my repos\\pkg.git",
        ] {
            assert_eq!(escape_for_display(plain), plain);
        }
    }

    #[test]
    fn escaping_twice_is_escaping_once() {
        let hostile = "a\nb\u{1b}[2K\u{202e}\u{3164}\u{2003}\u{2013}c\\n'\"";
        let once = escape_for_display(hostile);
        assert_eq!(
            once,
            "a\\nb\\u{1b}[2K\\u{202e}\\u{3164}\\u{2003}\u{2013}c\\n'\""
        );
        assert_eq!(escape_for_display(&once), once);
    }

    #[test]
    fn a_url_that_passes_holds_nothing_the_display_rule_escapes() {
        for c in '\0'..=char::MAX {
            let code = c as u32;
            // The network rule is the local rule and more.
            if is_forbidden_in_local_form(c) {
                assert!(is_forbidden_in_network_form(c), "U+{code:04X}");
            }
            // What is shown as an escape is accepted in no form.
            if needs_escape(c) {
                assert!(is_forbidden_in_local_form(c), "U+{code:04X}");
            }
        }
        // The two rules differ in the space and in what is visible but
        // neither ASCII nor a letter or a digit.
        assert!(is_forbidden_in_network_form(' ') && !is_forbidden_in_local_form(' '));
        for c in NON_ASCII_PUNCTUATION.into_iter().chain(['\u{301}']) {
            assert!(is_forbidden_in_network_form(c) && !is_forbidden_in_local_form(c));
        }
    }

    #[test]
    fn url_with_two_kinds_of_forbidden_characters_names_the_first_rule() {
        // An allowed space does not make the rule for spaces apply.
        let reason = rejection_reason("/srv/my repos/a\u{202e}b.git");
        assert!(reason.starts_with(RULE_INVISIBLE), "{reason}");
        let reason = rejection_reason("https://example.com/a\u{2013}b\u{202e}c.git");
        assert!(reason.starts_with(RULE_INVISIBLE), "{reason}");
        let reason = rejection_reason("https://example.com/a\u{2013}b\u{202e}c d.git");
        assert!(reason.starts_with(RULE_SPACE), "{reason}");
    }

    /// An error that writes `text`, a line break of its own, and `text`
    /// again, all through an [`EscapingWriter`].
    struct TwoLines(&'static str);

    impl fmt::Display for TwoLines {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut f = EscapingWriter::new(f);
            write!(f, "first: {}", self.0)?;
            f.line_break()?;
            write!(f, "second: {}", self.0)
        }
    }

    impl EscapedDisplay for TwoLines {}

    /// An error that wraps a [`TwoLines`].
    struct Wrapper(TwoLines);

    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut f = EscapingWriter::new(f);
            write!(f, "wrapper\u{1b}: ")?;
            f.nested(&self.0)
        }
    }

    #[test]
    fn escaping_writer_escapes_everything_but_its_own_line_breaks() {
        let rendered = Wrapper(TwoLines("x\ny\u{202e}")).to_string();
        assert_eq!(
            rendered,
            "wrapper\\u{1b}: first: x\\ny\\u{202e}\nsecond: x\\ny\\u{202e}"
        );
        // The same error shown with `{}` inside another message, which
        // is written through the writer, has its line break escaped
        // too: only `nested` keeps it.
        struct Inline(TwoLines);
        impl fmt::Display for Inline {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut f = EscapingWriter::new(f);
                write!(f, "inline: {}", self.0)
            }
        }
        assert_eq!(
            Inline(TwoLines("x")).to_string(),
            "inline: first: x\\nsecond: x"
        );
    }

    /// An error that writes `text` with [`EscapingWriter::lines`].
    struct Lines(&'static str);

    impl fmt::Display for Lines {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut f = EscapingWriter::new(f);
            write!(f, "message: ")?;
            f.lines(self.0)
        }
    }

    #[test]
    fn escaping_writer_keeps_the_lines_of_a_text_and_escapes_each() {
        assert_eq!(
            Lines("one\u{1b}\r\ntwo\u{202e}\n\nfour\n").to_string(),
            "message: one\\u{1b}\ntwo\\u{202e}\n\nfour"
        );
        assert_eq!(Lines("one").to_string(), "message: one");
        assert_eq!(Lines("").to_string(), "message: ");
    }

    /// True if `s` holds a character a terminal would act on or hide.
    fn has_unprintable(s: &str, allow_newline: bool) -> bool {
        s.chars().any(|c| {
            if c == '\n' {
                return !allow_newline;
            }
            needs_escape(c)
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
        // The only line breaks left are the ones git itself wrote, and
        // each line of git's is marked.
        assert!(!has_unprintable(&rendered, true), "{rendered:?}");
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(
            lines,
            [
                format!("git command failed (exit 128): `git ls-remote -- u {escaped}`"),
                "  git: fatal: first line".to_string(),
                "  git: remote: main".to_string(),
                "  git: FORGED\\u{1b}[2K\\u{202e}".to_string(),
            ]
        );
    }

    #[test]
    fn every_line_of_git_output_is_marked() {
        // Blank lines, a carriage return inside a line, Windows line
        // ends, and lines shaped like silt's own output.
        let stderr = "remote: one\r\n\nerror: all checks passed\n  = note: fine\rwarning: x\n\n";
        let rendered = GitError::CommandFailed {
            command: "git clone".into(),
            stderr: stderr.into(),
            exit_code: None,
        }
        .to_string();
        assert!(!has_unprintable(&rendered, true), "{rendered:?}");
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(
            lines,
            [
                "git command failed (exit ?): `git clone`",
                "  git: remote: one",
                "  git:",
                "  git: error: all checks passed",
                "  git:   = note: fine\\rwarning: x",
            ]
        );

        // Nothing but the first line when git printed nothing.
        let rendered = GitError::CommandFailed {
            command: "git clone".into(),
            stderr: " \n".into(),
            exit_code: Some(1),
        }
        .to_string();
        assert_eq!(rendered, "git command failed (exit 1): `git clone`");
    }

    #[test]
    fn cache_for_refuses_a_path_shaped_commit() {
        // Rejected before the cache root is computed or created.
        // A prefix is refused too: one commit, one directory.
        for sha in [
            "../../../../x",
            "..",
            "abc1234/x",
            "-abc1234",
            "",
            "abc1234",
        ] {
            let err = cache_for("https://example.com/foo", sha)
                .expect_err("path-shaped commit must be refused")
                .to_string();
            assert!(
                err.contains("invalid commit id"),
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
            fetch_to_cache(url, &"a".repeat(40))
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
