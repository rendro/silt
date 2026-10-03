//! Project manifest (`silt.toml`) parsing and validation.
//!
//! A silt package is described by a `silt.toml` at its root. This module
//! handles loading, parsing (via `toml + serde`), validating, and locating
//! manifests on disk. Subsequent PRs (project-root unification, dep
//! resolution, lock file) build on the types defined here.
//!
//! Path and git dependencies are supported as of v0.8; registry deps are
//! still future work.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::diagnostic::{Code, Diagnostic};
use crate::git::escape_for_display;
use crate::intern::{self, Symbol};
use crate::module::{BUILTIN_MODULES, is_builtin_module};
use crate::source::{FileId, SourceMap, SourceName, Span};

// Re-exported for callers that want to construct or pattern-match
// `Dependency::Git { ref_spec, .. }` without reaching into `crate::git`.
pub use crate::git::GitRef;

/// A loaded and validated `silt.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: PackageMeta,
    /// Keyed by the dependency key: the name the importing package
    /// knows the dependency by (`import <key>`).
    pub dependencies: BTreeMap<Symbol, DependencyEntry>,
    /// Absolute path to the silt.toml file this manifest was loaded from.
    pub manifest_path: PathBuf,
    /// The manifest's text in the source map it was loaded into.
    pub file: FileId,
}

/// `[package]` table contents.
#[derive(Debug, Clone)]
pub struct PackageMeta {
    pub name: Symbol,
    /// The value of `name` in the file.
    pub name_span: Span,
    pub version: String,
    pub edition: Option<String>,
}

/// One `[dependencies]` entry and where it is written.
#[derive(Debug, Clone)]
pub struct DependencyEntry {
    pub source: Dependency,
    /// The key, `foo` in `foo = { path = "../foo" }`.
    pub key: Span,
    /// The value, `{ path = "../foo" }`.
    pub value: Span,
}

/// Where a dependency comes from, as written in `[dependencies]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// Path-style dep: `foo = { path = "../foo" }`. Path is stored exactly as
    /// written in the manifest; it is relative to the manifest's directory.
    Path { path: PathBuf },
    /// Git-style dep: `foo = { git = "https://...", rev|branch|tag = "..." }`.
    /// A local URL (`./x`, `../x`) is relative to the manifest's
    /// directory. Resolution to a commit happens when the package graph
    /// is built (`package_graph::resolve_packages`), not at manifest load.
    Git { url: String, ref_spec: GitRef },
    // Future variants: Registry { version }.
}

// ── Raw deserialization layer ─────────────────────────────────────────
//
// We intentionally split parsing (RawManifest) from validation (Manifest).
// Serde's errors are clean for missing/wrongly-typed fields; everything
// else (identifier rules, semver shape, builtin collisions, unknown dep
// kinds) is enforced as a post-step so the messages are tailored.
//
// Each top-level table struct uses `#[serde(deny_unknown_fields)]` so a
// typo like `descrption` under `[package]` or a misspelled section like
// `[depndencies]` is rejected at parse time rather than silently
// ignored. Inline-table dependency parsing (path / git deps below)
// already validates unknown keys explicitly.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    package: RawPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    #[serde(default)]
    edition: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDependency {
    /// Inline-table form: `foo = { path = "..." }` (and, in later phases,
    /// `{ git = "...", rev = "..." }` or `{ version = "..." }`).
    Inline(BTreeMap<String, toml::Value>),
    // Future: bare-version form `foo = "1.2.3"`.
}

impl Manifest {
    /// Load and validate a manifest from a specific path, adding its
    /// text to `sources` as a [`SourceName::Manifest`].
    ///
    /// Every error is a diagnostic in that file, at the key or value
    /// that broke a rule. Values of the file are shown by the display
    /// rule ([`escape_for_display`]): the manifest can be a dependency's.
    pub fn load(path: &Path, sources: &mut SourceMap) -> Result<Manifest, Diagnostic> {
        let absolute = absolutize(path);
        let shown = SourceName::Manifest(display_path(&absolute));
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                let file = sources.add(shown, "".into());
                return Err(Diagnostic::error(
                    Code::PackageIo,
                    Span::point(file, 0),
                    format!(
                        "cannot read manifest: {}",
                        escape_for_display(&crate::diagnostic::io_error_text(&e))
                    ),
                ));
            }
        };
        let file = sources.add(shown, text.as_str().into());
        let span_of = |range: (usize, usize)| Span {
            file,
            start: range.0 as u32,
            end: range.1 as u32,
        };

        let raw: RawManifest = toml::from_str(&text).map_err(|e| {
            // The parser's message is shown line by line, as
            // `toml_error_message` prepares it: the first line is the
            // message, the others are notes.
            let message = toml_error_message(e.message(), &text);
            let mut lines = message.lines().map(escape_for_display);
            let head = lines.next().unwrap_or_default();
            let span = e
                .span()
                .map(|r| span_of((r.start, r.end)))
                .unwrap_or(Span::point(file, 0));
            let mut d = Diagnostic::error(
                Code::ManifestInvalid,
                span,
                format!("invalid manifest: {head}"),
            );
            d.notes.extend(lines);
            d
        })?;

        // Validation phase ----------------------------------------------------
        // Each rule's error points at the key or value that broke it.
        let doc = toml_edit::ImDocument::parse(text.as_str()).ok();
        let at = |keys: &[&str], key_itself: bool| {
            manifest_span(doc.as_ref(), keys, key_itself)
                .map(span_of)
                .unwrap_or(Span::point(file, 0))
        };
        let invalid = |span: Span, message: String| {
            Diagnostic::error(
                Code::ManifestInvalid,
                span,
                format!("invalid manifest: {}", escape_for_display(&message)),
            )
        };
        validate_package_name_rules(&raw.package.name)
            .map_err(|m| invalid(at(&["package", "name"], false), m))?;
        validate_version(&raw.package.version)
            .map_err(|m| invalid(at(&["package", "version"], false), m))?;

        let mut dependencies = BTreeMap::new();
        for (raw_name, raw_dep) in raw.dependencies {
            let keys = ["dependencies", raw_name.as_str()];
            let key = at(&keys, true);
            let value = at(&keys, false);
            validate_dependency_name(&raw_name).map_err(|m| invalid(key, m))?;
            let source = convert_dependency(&raw_name, raw_dep).map_err(|m| invalid(value, m))?;
            dependencies.insert(
                intern::intern(&raw_name),
                DependencyEntry { source, key, value },
            );
        }

        Ok(Manifest {
            package: PackageMeta {
                name: intern::intern(&raw.package.name),
                name_span: at(&["package", "name"], false),
                version: raw.package.version,
                edition: raw.package.edition,
            },
            dependencies,
            manifest_path: absolute,
            file,
        })
    }

    /// Walk up from `start` looking for `silt.toml`.
    ///
    /// Returns the directory containing the manifest, or `None` if no
    /// manifest is found before the filesystem root. If `start` is a file,
    /// the search begins in its parent directory.
    pub fn find(start: &Path) -> Option<PathBuf> {
        let absolute = absolutize(start);
        let mut current: Option<&Path> = if absolute.is_file() {
            absolute.parent()
        } else {
            Some(absolute.as_path())
        };

        while let Some(dir) = current {
            if dir.join("silt.toml").is_file() {
                return Some(dir.to_path_buf());
            }
            current = dir.parent();
        }
        None
    }

    /// Convenience wrapper: [`find`](Self::find) + [`load`](Self::load).
    ///
    /// Returns `Ok(None)` if no manifest is found between `start` and the
    /// filesystem root; returns `Err` only if a manifest was located but
    /// failed to load or validate.
    pub fn discover(start: &Path, sources: &mut SourceMap) -> Result<Option<Manifest>, Diagnostic> {
        match Self::find(start) {
            Some(dir) => Self::load(&dir.join("silt.toml"), sources).map(Some),
            None => Ok(None),
        }
    }
}

/// How a package file's path is shown: relative to the working
/// directory, with `..` when the file is above it (`silt check main.silt`
/// run in `src/` shows `../silt.toml`), written with `/` as a manifest
/// entry is; else (only the root in common) as it is. Both sides are
/// compared in canonical form, a path that does not exist (a missing
/// dependency) by its nearest existing ancestor, so two spellings of one
/// place (on Windows a short 8.3 name, a verbatim prefix) agree.
pub fn display_path(path: &Path) -> PathBuf {
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_path_buf();
    };
    let cwd = crate::source::canonical_path_lenient(&cwd);
    let path = crate::source::canonical_path_lenient(path);
    let same = |a: &std::path::Component, b: &std::path::Component| {
        if cfg!(windows) {
            a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
        } else {
            a == b
        }
    };
    let common = cwd
        .components()
        .zip(path.components())
        .take_while(|(a, b)| same(a, b))
        .count();
    // Only the root in common: the absolute path says more.
    if common <= 1 {
        return path;
    }
    let mut parts: Vec<String> = cwd
        .components()
        .skip(common)
        .map(|_| "..".to_string())
        .collect();
    parts.extend(
        path.components()
            .skip(common)
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    PathBuf::from(parts.join("/"))
}

// ── The TOML parser's message ─────────────────────────────────────────

/// The message of a TOML parser error about the file `source`, ready to
/// be shown line by line.
///
/// The parser separates the parts of its message by line breaks, for
/// example `invalid string`, then `expected ...` on the next line, and
/// those line breaks are kept. But the message also quotes keys of the
/// file, decoded: a key written with an escape sequence, `"a\nb"`, puts
/// a line break of its own into the message, and that one must not
/// start a line of output. The message does not tell the two kinds
/// apart. The file does: a key or a value can hold a line break only
/// if it is written with an escape sequence or as a multi-line string,
/// so only if the file holds a backslash or three quotation marks in a
/// row. For such a file the message is returned as one line, with its
/// line breaks written as `\n`; for every other file it is returned as
/// the parser wrote it.
pub fn toml_error_message(message: &str, source: &str) -> String {
    let value_can_hold_a_line_break =
        source.contains('\\') || source.contains("\"\"\"") || source.contains("'''");
    if value_can_hold_a_line_break {
        escape_for_display(message)
    } else {
        message.to_string()
    }
}

// ── Validation helpers ────────────────────────────────────────────────

/// Convert an absolute or relative path to its absolute form without
/// requiring that it currently exists. We deliberately avoid
/// `canonicalize` because the manifest file is loaded before any path
/// dependencies have been resolved — they may not exist yet.
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Silt identifier rules: lowercase ASCII letter or underscore start,
/// followed by lowercase ASCII letters, digits, or underscores. Matches
/// `^[a-z_][a-z0-9_]*$`.
///
/// Public so other parts of the binary (notably `silt add`, which has to
/// validate dep names before mutating the manifest) can apply the same
/// rule without duplicating the regex.
pub fn is_silt_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Validate a package name: must be a silt identifier AND must not collide
/// with a builtin module name (`io`, `string`, etc.). Returns a
/// human-readable error string describing the rule that failed.
///
/// Single source of truth for package-name acceptance, consulted by:
///   - `Manifest::load` (rejects bad names already on disk)
///   - `silt::cli::init` (rejects bad names before writing silt.toml; e.g.
///     `mkdir io && cd io && silt init` previously silently produced a
///     manifest whose `name = "io"` collided with the stdlib `io` module —
///     round-75 DX-5 GAP fix).
///
/// The error wording mirrors what `silt add` already emits for
/// builtin-colliding dep names (see `src/cli/add.rs:273-279`) so init,
/// add, and the manifest loader report identical canonical shapes.
pub fn validate_package_name(name: &str) -> Result<(), String> {
    if !is_silt_identifier(name) {
        // The message is returned as text, not as an error type that
        // escapes when shown, so the name is escaped here. In the two
        // messages below the name is known to be an identifier.
        return Err(format!(
            "invalid package name `{}`: \
             must match silt identifier rules `[a-z_][a-z0-9_]*`",
            escape_for_display(name)
        ));
    }
    if is_builtin_module(name) {
        return Err(format!(
            "package name `{name}` collides with builtin module `{name}`; \
             pick a different name"
        ));
    }
    if is_reserved_keyword(name) {
        return Err(format!(
            "package name `{name}` is a reserved silt keyword; \
             pick a different name"
        ));
    }
    Ok(())
}

/// Is `name` a reserved silt keyword? Consults both `lexer::KEYWORDS`
/// (keyword-shaped tokens like `loop`, `match`, `type`) and
/// `lexer::KEYWORD_LITERALS` (`true`, `false`, lexed as `Token::Bool`).
///
/// A package/dependency named after a reserved word lexes as that keyword
/// rather than a `Token::Ident`, so `import loop` can never parse — the
/// same import-shadowing footgun that round-75 fixed for builtin module
/// names. Rejecting up front (init / add / Manifest::load) keeps the
/// failure where the user can act on it instead of surfacing as a
/// confusing parser diagnostic far from the offending name.
pub fn is_reserved_keyword(name: &str) -> bool {
    crate::lexer::KEYWORDS.contains(&name) || crate::lexer::KEYWORD_LITERALS.contains(&name)
}

/// The rules a `[package].name` follows: an identifier, not a builtin
/// module, not a keyword.
fn validate_package_name_rules(name: &str) -> Result<(), String> {
    validate_identifier(name, "package name")?;
    // Builtin-collision check: a manifest whose `[package].name`
    // equals a stdlib module (`io`, `string`, …) is rejected for
    // the same reason `silt add` rejects builtin-colliding dep
    // names — the import name would shadow the stdlib. Routed
    // through the same `is_builtin_module` predicate so init /
    // add / load all share one source of truth.
    if is_builtin_module(name) {
        return Err(format!(
            "package name `{name}` collides with builtin module `{name}`; \
                 pick a different name"
        ));
    }
    // Reserved-keyword check: a package named after a keyword
    // (`loop`, `match`, …) lexes as that keyword, so `import loop`
    // can never parse. Same import-shadowing footgun as the builtin
    // collision above; share the predicate so init/add/load agree.
    if is_reserved_keyword(name) {
        return Err(format!(
            "package name `{name}` is a reserved silt keyword; \
                 pick a different name"
        ));
    }
    Ok(())
}

/// The rules a dependency's name follows: an identifier, not a builtin
/// module, not a keyword.
fn validate_dependency_name(name: &str) -> Result<(), String> {
    validate_identifier(name, "dependency name")?;
    if BUILTIN_MODULES.contains(&name) {
        return Err(format!(
            "dependency name `{name}` collides with builtin module `{name}`; \
                 pick a different name"
        ));
    }
    if is_reserved_keyword(name) {
        return Err(format!(
            "dependency name `{name}` is a reserved silt keyword; \
                 pick a different name"
        ));
    }
    Ok(())
}

/// The byte range in `doc` of the value at the table path `keys`, or of
/// its key with `key_itself`.
fn manifest_span(
    doc: Option<&toml_edit::ImDocument<&str>>,
    keys: &[&str],
    key_itself: bool,
) -> Option<(usize, usize)> {
    let (last, tables) = keys.split_last()?;
    let mut table: &dyn toml_edit::TableLike = doc?.as_table();
    for key in tables {
        table = table.get(key)?.as_table_like()?;
    }
    let range = if key_itself {
        table.get_key_value(last)?.0.span()?
    } else {
        table.get(last)?.span()?
    };
    Some((range.start, range.end))
}

fn validate_identifier(name: &str, role: &str) -> Result<(), String> {
    if is_silt_identifier(name) {
        return Ok(());
    }
    let detail = if name.is_empty() {
        "must not be empty".to_string()
    } else if name.chars().any(|c| c.is_ascii_uppercase()) {
        "must be lowercase (snake_case); uppercase letters are not allowed".to_string()
    } else if name.contains('.') || name.contains('-') || name.contains(' ') {
        "must contain only lowercase letters, digits, and underscores".to_string()
    } else if name
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        "must not start with a digit".to_string()
    } else {
        "must match the silt identifier rules `[a-z_][a-z0-9_]*`".to_string()
    };
    Err(format!("invalid {role} `{name}`: {detail}"))
}

/// Lightweight semver shape check: `MAJOR.MINOR.PATCH` where each component
/// is a non-empty run of ASCII digits with no leading zeros (except `0`
/// itself), optionally followed by `-PRERELEASE`, and then optionally by
/// `+BUILD`. `PRERELEASE` and `BUILD` are each one or more identifiers
/// separated by `.`, an identifier being a non-empty run of ASCII
/// letters, digits and `-`.
///
/// Every part of the string is checked, the build part included: the
/// version is written to `silt.lock` and shown in messages.
///
/// We deliberately avoid a `semver` crate dependency for now; v0.7 only
/// needs to detect obviously-malformed strings. Real precedence rules
/// arrive with the registry workflow in a later phase.
fn is_valid_version(version: &str) -> bool {
    let (core, build) = split_off_build(version);
    if build.is_some_and(|build| !is_identifier_list(build)) {
        return false;
    }
    let (core, pre) = match core.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (core, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    for part in &parts {
        if !is_numeric_id(part) {
            return false;
        }
    }
    // Pre-release identifiers may be alphanumeric or numeric (no leading
    // zeros for the latter); we keep the check loose here.
    if pre.is_some_and(|pre| !is_identifier_list(pre)) {
        return false;
    }
    true
}

fn split_off_build(version: &str) -> (&str, Option<&str>) {
    match version.split_once('+') {
        Some((core, build)) => (core, Some(build)),
        None => (version, None),
    }
}

/// One or more identifiers separated by `.`, each a non-empty run of
/// ASCII letters, digits and `-`.
fn is_identifier_list(s: &str) -> bool {
    s.split('.').all(|identifier| {
        !identifier.is_empty()
            && identifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn is_numeric_id(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Disallow leading zeros except "0" itself.
    !(s.len() > 1 && s.starts_with('0'))
}

fn validate_version(version: &str) -> Result<(), String> {
    if is_valid_version(version) {
        return Ok(());
    }
    Err(format!(
        "invalid package version `{version}`: must be a semver string of the form \
             `MAJOR.MINOR.PATCH` (e.g. `0.1.0`); a `-PRERELEASE` and a `+BUILD` part \
             may follow, each made of ASCII letters, digits, `-` and `.`"
    ))
}

fn convert_dependency(name: &str, raw: RawDependency) -> Result<Dependency, String> {
    match raw {
        RawDependency::Inline(table) => {
            let has_path = table.contains_key("path");
            let has_git = table.contains_key("git");

            // Registry deps are still future work; surface a forward-looking
            // diagnostic rather than silently treating `version` as garbage.
            if table.contains_key("version") || table.contains_key("registry") {
                return Err(format!(
                    "dependency `{name}`: registry/version dependencies are not yet \
                         supported; use `path` or `git` instead"
                ));
            }

            if has_path && has_git {
                return Err(format!(
                    "dependency `{name}`: cannot specify both `path` and `git`; pick one"
                ));
            }

            if has_git {
                return convert_git_dependency(name, &table);
            }

            // Default arm: path dependency.
            convert_path_dependency(name, &table)
        }
    }
}

fn convert_path_dependency(
    name: &str,
    table: &BTreeMap<String, toml::Value>,
) -> Result<Dependency, String> {
    let path_value = table.get("path").ok_or_else(|| {
        format!("dependency `{name}`: missing required key `path` (or use `git` for a git dep)")
    })?;
    let path_str = path_value
        .as_str()
        .ok_or_else(|| format!("dependency `{name}`: `path` must be a string"))?;

    for key in table.keys() {
        if key != "path" {
            return Err(format!(
                "dependency `{name}`: unknown key `{key}` (only `path` is recognized for path deps)"
            ));
        }
    }

    Ok(Dependency::Path {
        path: PathBuf::from(path_str),
    })
}

fn convert_git_dependency(
    name: &str,
    table: &BTreeMap<String, toml::Value>,
) -> Result<Dependency, String> {
    let url = table
        .get("git")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("dependency `{name}`: `git` must be a string URL"))?
        .to_string();

    // The URL is handed to `git` at lock time, and this manifest may be
    // a transitive dependency's, so it is untrusted input: an
    // option-shaped value such as `--upload-pack=<cmd>` would make git
    // run `<cmd>`. Every manifest goes through this one check.
    crate::git::validate_git_url(&url).map_err(|e| format!("dependency `{name}`: {e}"))?;

    // Tally which ref forms are present so we can give a tailored error
    // for the multiple-forms case rather than just "missing".
    let mut ref_forms: Vec<(&str, &toml::Value)> = Vec::new();
    for key in ["rev", "branch", "tag"] {
        if let Some(v) = table.get(key) {
            ref_forms.push((key, v));
        }
    }

    let ref_spec = match ref_forms.len() {
        0 => {
            return Err(format!(
                "dependency `{name}`: git dependency requires exactly one of `rev`, \
                     `branch`, or `tag`"
            ));
        }
        1 => {
            let (key, value) = ref_forms[0];
            let s = value
                .as_str()
                .ok_or_else(|| format!("dependency `{name}`: `{key}` must be a string"))?
                .to_string();
            match key {
                // A full commit id or a prefix of one; a prefix is
                // resolved to the commit when the graph is built.
                "rev" if !crate::git::is_valid_sha_shape(&s) => {
                    return Err(format!(
                        "dependency `{name}`: `rev` must be a commit id \
                         (7 to 64 hexadecimal characters), got `{s}`"
                    ));
                }
                "rev" => GitRef::Rev(s),
                "branch" => GitRef::Branch(s),
                "tag" => GitRef::Tag(s),
                _ => unreachable!("ref_forms keys are restricted above"),
            }
        }
        _ => {
            let mentioned: Vec<&str> = ref_forms.iter().map(|(k, _)| *k).collect();
            return Err(format!(
                "dependency `{name}`: git dependency must specify exactly one of `rev`, \
                     `branch`, or `tag` (found: {})",
                mentioned.join(", ")
            ));
        }
    };

    // Reject anything that isn't `git` + the chosen ref form. Caller
    // already excluded `path`, `version`, `registry` upstream, but we
    // still surface unknown keys here for typo-friendliness
    // (e.g. `branch_pattern`).
    for key in table.keys() {
        match key.as_str() {
            "git" | "rev" | "branch" | "tag" => {}
            other => {
                return Err(format!(
                    "dependency `{name}`: unknown key `{other}` for a git dependency \
                         (allowed keys: `git`, `rev`, `branch`, `tag`)"
                ));
            }
        }
    }

    Ok(Dependency::Git { url, ref_spec })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_rules() {
        assert!(is_silt_identifier("foo"));
        assert!(is_silt_identifier("foo_bar"));
        assert!(is_silt_identifier("foo123"));
        assert!(is_silt_identifier("_priv"));
        assert!(!is_silt_identifier(""));
        assert!(!is_silt_identifier("Foo"));
        assert!(!is_silt_identifier("foo.bar"));
        assert!(!is_silt_identifier("foo-bar"));
        assert!(!is_silt_identifier("1foo"));
    }

    #[test]
    fn version_rules() {
        assert!(is_valid_version("0.1.0"));
        assert!(is_valid_version("1.0.0"));
        assert!(is_valid_version("10.20.30"));
        assert!(is_valid_version("1.0.0-alpha"));
        assert!(is_valid_version("1.0.0-alpha.1"));
        assert!(is_valid_version("1.0.0+build.5"));
        assert!(is_valid_version("1.0.0-rc.1+build.7"));
        assert!(!is_valid_version(""));
        assert!(!is_valid_version("v1"));
        assert!(!is_valid_version("1"));
        assert!(!is_valid_version("1.0"));
        assert!(!is_valid_version("abc"));
        assert!(!is_valid_version("01.0.0")); // leading zero
        assert!(!is_valid_version("1.0.0-")); // empty pre-release
    }

    #[test]
    fn version_rules_cover_the_build_part() {
        for version in [
            "1.0.0+5",
            "1.0.0+build",
            "1.0.0+build.5",
            "1.0.0+2024-01-31.abc-def",
            "1.0.0-rc.1+build.7",
            "1.0.0-rc-1+build-7",
        ] {
            assert!(is_valid_version(version), "{version:?} must be accepted");
        }
        for version in [
            "1.0.0+",
            "1.0.0+.",
            "1.0.0+a.",
            "1.0.0+.a",
            "1.0.0+a..b",
            "1.0.0+a+b",
            "1.0.0+a b",
            "1.0.0+a_b",
            "1.0.0+a/b",
            "1.0.0+caf\u{e9}",
            "1.0.0+a\u{7f}b",
            "1.0.0+a\nb",
            "1.0.0+a\u{1b}[2Kb",
            "1.0.0+a\u{202e}b",
            "1.0.0-rc.1+",
            "1.0+build",
        ] {
            assert!(!is_valid_version(version), "{version:?} must be rejected");
        }
    }

    const HOSTILE: &str = "x\nerror: FORGED\u{1b}[2K\u{202e}";
    const HOSTILE_ESCAPED: &str = "x\\nerror: FORGED\\u{1b}[2K\\u{202e}";

    #[test]
    fn toml_error_message_keeps_only_the_line_breaks_of_the_parser() {
        let message = "invalid string\nexpected `\"`, `'`";
        // No key and no value of these files can hold a line break.
        for source in [
            "",
            "[package]\nname = oops\n",
            "[package]\nname = \"app\"\nnote = 'it''s'\n",
        ] {
            assert_eq!(toml_error_message(message, source), message, "{source:?}");
        }
        // One of these can: an escape sequence, a multi-line string.
        for source in [
            "\"a\\nb\" = 1\n",
            "\"a\\u000Ab\" = 1\n",
            "# a comment with a \\\nname = oops\n",
            "a = \"\"\"\nb\"\"\"\n",
            "a = '''\nb'''\n",
        ] {
            assert_eq!(
                toml_error_message(message, source),
                "invalid string\\nexpected `\"`, `'`",
                "{source:?}"
            );
        }
    }

    /// The diagnostic of loading `text` as a manifest, and the map it
    /// points into.
    fn load_error(tag: &str, text: &str) -> (SourceMap, Diagnostic) {
        let dir =
            std::env::temp_dir().join(format!("silt_manifest_unit_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("silt.toml");
        fs::write(&path, text).unwrap();
        let mut sources = SourceMap::new();
        let d = Manifest::load(&path, &mut sources).expect_err("the manifest must be rejected");
        let _ = fs::remove_dir_all(&dir);
        (sources, d)
    }

    fn assert_printable(d: &Diagnostic) {
        for line in std::iter::once(&d.message).chain(&d.notes) {
            assert!(
                !line.chars().any(crate::git::needs_escape),
                "a line of the diagnostic is not printable: {line:?}"
            );
        }
    }

    #[test]
    fn parse_error_shows_the_lines_of_the_parser_and_no_other() {
        // A file without escape sequences: the parser's lines, the
        // first as the message and the rest as notes.
        let (_, plain) = load_error("plain", "[package]\nname = oops\n");
        assert_eq!(plain.code, Code::ManifestInvalid);
        assert!(plain.message.starts_with("invalid manifest: "), "{plain:?}");
        assert!(!plain.notes.is_empty(), "{plain:?}");
        assert_printable(&plain);
        // A file with a key that holds a line break: one line.
        let source = "\"x\\nerror: FORGED\\u001b[2K\\u202e\" = 1\n";
        let (_, quoted) = load_error("quoted", source);
        assert!(quoted.notes.is_empty(), "{quoted:?}");
        assert!(quoted.message.contains(HOSTILE_ESCAPED), "{quoted:?}");
        assert_printable(&quoted);
    }

    #[test]
    fn validation_error_escapes_the_value_and_points_at_it() {
        let text = "[package]\nname = \"app\"\nversion = \"x\\nerror: FORGED\\u001b[2K\\u202e\"\n";
        let (sources, d) = load_error("version", text);
        assert!(d.message.contains(HOSTILE_ESCAPED), "{d:?}");
        assert_printable(&d);
        let file = sources.file(d.span.file);
        assert!(matches!(file.path, SourceName::Manifest(_)));
        assert!(
            file.text[d.span.start_offset()..d.span.end_offset()].starts_with("\"x"),
            "{d:?}"
        );
    }

    #[test]
    fn unreadable_manifest_is_a_diagnostic_in_that_file() {
        let mut sources = SourceMap::new();
        let d = Manifest::load(Path::new("/nonexistent/silt_unit/silt.toml"), &mut sources)
            .expect_err("a missing manifest cannot load");
        assert_eq!(d.code, Code::PackageIo);
        assert!(matches!(
            sources.file(d.span.file).path,
            SourceName::Manifest(_)
        ));
    }

    #[test]
    fn package_name_rejection_escapes_the_name() {
        let message = validate_package_name(HOSTILE).unwrap_err();
        assert!(
            message.contains(HOSTILE_ESCAPED) && !message.chars().any(crate::git::needs_escape),
            "{message:?}"
        );
    }
}
