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
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Path, PathBuf};

use crate::git::{EscapedDisplay, EscapingWriter, escape_for_display};
use crate::intern::{self, Symbol};
use crate::module::{BUILTIN_MODULES, is_builtin_module};

// Re-exported for callers that want to construct or pattern-match
// `Dependency::Git { ref_spec, .. }` without reaching into `crate::git`.
pub use crate::git::GitRef;

/// A loaded and validated `silt.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: PackageMeta,
    pub dependencies: BTreeMap<Symbol, Dependency>,
    /// Optional `[lints]` table — package-wide lint configuration.
    /// Phase D of the effect-rows proposal exposes
    /// `strict-effects = true` here so a package can opt every
    /// `silt check` / `silt run` / `silt test` invocation into the
    /// strict-effects mode without passing `--strict-effects` on
    /// every CLI run. Absent table or absent field both default to
    /// `false` (the legacy behavior). The CLI flag still wins when
    /// supplied — see `cli::pipeline::resolve_strict_effects`.
    pub lints: LintConfig,
    /// Absolute path to the silt.toml file this manifest was loaded from.
    pub manifest_path: PathBuf,
}

/// `[lints]` table contents.
///
/// Phase D adds `strict_effects` (TOML key `strict-effects`). Future
/// lint flags land here too, so callers can read `manifest.lints.X`
/// uniformly. Defaults to all-off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LintConfig {
    /// Phase D effect-rows mode. When `true`, unannotated user fns
    /// default to `EffectSet::EMPTY` (pure) and the typechecker
    /// rejects effectful calls without an explicit annotation. The
    /// diagnostic includes a copy-paste `help:` line.
    ///
    /// CLI flag `--strict-effects` overrides this field on a per-
    /// invocation basis. Absent field or absent `[lints]` table →
    /// `false` (legacy behavior).
    pub strict_effects: bool,
}

/// `[package]` table contents.
#[derive(Debug, Clone)]
pub struct PackageMeta {
    pub name: Symbol,
    pub version: String,
    pub edition: Option<String>,
}

/// A single entry in `[dependencies]`.
#[derive(Debug, Clone)]
pub enum Dependency {
    /// Path-style dep: `foo = { path = "../foo" }`. Path is stored exactly as
    /// written in the manifest (relative to the manifest file). Resolve to
    /// absolute via `manifest_path.parent().unwrap().join(path)`.
    Path { path: PathBuf },
    /// Git-style dep: `foo = { git = "https://...", rev|branch|tag = "..." }`.
    /// Resolution to a concrete commit SHA happens at lock time
    /// (`Lockfile::resolve`), not at manifest load.
    Git { url: String, ref_spec: GitRef },
    // Future variants: Registry { version }.
}

/// Errors produced when loading or validating a manifest.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Io(std::io::Error, PathBuf),
    /// TOML syntax error or schema mismatch from serde.
    Parse {
        /// The TOML parser's message, as [`toml_error_message`] returns
        /// it: it is shown line by line, so a line break in it must be
        /// the parser's own, never one from a key or a value.
        message: String,
        path: PathBuf,
        /// Byte-offset span within the file, when the underlying parser
        /// provided one. Used for inline diagnostic rendering.
        span: Option<(usize, usize)>,
    },
    /// Manifest parsed structurally, but a validation rule failed.
    Validation { message: String, path: PathBuf },
}

impl fmt::Display for ManifestError {
    // Untrusted text, per variant:
    //   - `Io`: the path, when the manifest is a dependency's: it is
    //     made from the `path` value of the manifest that names it.
    //   - `Parse`: the path as above; the message is the TOML parser's
    //     and quotes keys and values of the file. It is the one text
    //     that is shown on several lines, see `toml_error_message`.
    //   - `Validation`: the path as above; the message quotes names,
    //     versions, keys and values of the file.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Shadows the formatter: nothing below can be written without
        // going through the display rule.
        let mut f = EscapingWriter::new(f);
        match self {
            ManifestError::Io(err, path) => {
                write!(f, "failed to read manifest {}: {}", path.display(), err)
            }
            ManifestError::Parse { message, path, .. } => {
                write!(f, "invalid manifest {}: ", path.display())?;
                f.lines(message)
            }
            ManifestError::Validation { message, path } => {
                write!(f, "invalid manifest {}: {}", path.display(), message)
            }
        }
    }
}

impl EscapedDisplay for ManifestError {}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ManifestError::Io(err, _) => Some(err),
            _ => None,
        }
    }
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
    #[serde(default)]
    lints: Option<RawLints>,
}

/// Raw `[lints]` table. We accept the kebab-case `strict-effects` key
/// (TOML / Cargo convention). Unknown keys are rejected at parse time
/// (`deny_unknown_fields`) so typos surface immediately rather than
/// being silently ignored — when a new lint flag lands, add the field
/// here.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLints {
    #[serde(rename = "strict-effects", default)]
    strict_effects: Option<bool>,
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
    /// Load and validate a manifest from a specific path.
    ///
    /// Returns a [`ManifestError`] tagged with the file path so callers can
    /// produce diagnostics that point at the right location.
    pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
        let path_buf = path.to_path_buf();
        let absolute = absolutize(&path_buf);
        let text = fs::read_to_string(path).map_err(|e| ManifestError::Io(e, absolute.clone()))?;

        let raw: RawManifest = toml::from_str(&text).map_err(|e| {
            // toml 0.8 exposes a span() method giving byte offsets within
            // the input; surface it for downstream diagnostic rendering.
            let span = e.span().map(|r| (r.start, r.end));
            ManifestError::Parse {
                message: toml_error_message(e.message(), &text),
                path: absolute.clone(),
                span,
            }
        })?;

        // Validation phase ----------------------------------------------------
        validate_identifier(&raw.package.name, "package name", &absolute)?;
        // Builtin-collision check: a manifest whose `[package].name`
        // equals a stdlib module (`io`, `string`, …) is rejected for
        // the same reason `silt add` rejects builtin-colliding dep
        // names — the import name would shadow the stdlib. Routed
        // through the same `is_builtin_module` predicate so init /
        // add / load all share one source of truth (round-75 DX-5
        // GAP fix; see also `validate_package_name` below).
        if is_builtin_module(&raw.package.name) {
            return Err(ManifestError::Validation {
                message: format!(
                    "package name `{}` collides with builtin module `{}`; \
                     pick a different name",
                    raw.package.name, raw.package.name
                ),
                path: absolute,
            });
        }
        // Reserved-keyword check: a package named after a keyword
        // (`loop`, `match`, …) lexes as that keyword, so `import loop`
        // can never parse. Same import-shadowing footgun as the builtin
        // collision above; share the predicate so init/add/load agree.
        if is_reserved_keyword(&raw.package.name) {
            return Err(ManifestError::Validation {
                message: format!(
                    "package name `{}` is a reserved silt keyword; \
                     pick a different name",
                    raw.package.name
                ),
                path: absolute,
            });
        }
        validate_version(&raw.package.version, &absolute)?;

        let mut dependencies = BTreeMap::new();
        for (raw_name, raw_dep) in raw.dependencies {
            validate_identifier(&raw_name, "dependency name", &absolute)?;
            if BUILTIN_MODULES.contains(&raw_name.as_str()) {
                return Err(ManifestError::Validation {
                    message: format!(
                        "dependency name `{raw_name}` collides with builtin module `{raw_name}`; \
                         pick a different name"
                    ),
                    path: absolute,
                });
            }
            if is_reserved_keyword(&raw_name) {
                return Err(ManifestError::Validation {
                    message: format!(
                        "dependency name `{raw_name}` is a reserved silt keyword; \
                         pick a different name"
                    ),
                    path: absolute,
                });
            }
            let dep = convert_dependency(&raw_name, raw_dep, &absolute)?;
            let sym = intern::intern(&raw_name);
            dependencies.insert(sym, dep);
        }

        let lints = match raw.lints {
            Some(raw_lints) => LintConfig {
                strict_effects: raw_lints.strict_effects.unwrap_or(false),
            },
            None => LintConfig::default(),
        };

        Ok(Manifest {
            package: PackageMeta {
                name: intern::intern(&raw.package.name),
                version: raw.package.version,
                edition: raw.package.edition,
            },
            dependencies,
            lints,
            manifest_path: absolute,
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
    pub fn discover(start: &Path) -> Result<Option<Manifest>, ManifestError> {
        match Self::find(start) {
            Some(dir) => Self::load(&dir.join("silt.toml")).map(Some),
            None => Ok(None),
        }
    }
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

fn validate_identifier(name: &str, role: &str, manifest_path: &Path) -> Result<(), ManifestError> {
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
    Err(ManifestError::Validation {
        message: format!("invalid {role} `{name}`: {detail}"),
        path: manifest_path.to_path_buf(),
    })
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

fn validate_version(version: &str, manifest_path: &Path) -> Result<(), ManifestError> {
    if is_valid_version(version) {
        return Ok(());
    }
    Err(ManifestError::Validation {
        message: format!(
            "invalid package version `{version}`: must be a semver string of the form \
             `MAJOR.MINOR.PATCH` (e.g. `0.1.0`); a `-PRERELEASE` and a `+BUILD` part \
             may follow, each made of ASCII letters, digits, `-` and `.`"
        ),
        path: manifest_path.to_path_buf(),
    })
}

fn convert_dependency(
    name: &str,
    raw: RawDependency,
    manifest_path: &Path,
) -> Result<Dependency, ManifestError> {
    match raw {
        RawDependency::Inline(table) => {
            let has_path = table.contains_key("path");
            let has_git = table.contains_key("git");

            // Registry deps are still future work; surface a forward-looking
            // diagnostic rather than silently treating `version` as garbage.
            if table.contains_key("version") || table.contains_key("registry") {
                return Err(ManifestError::Validation {
                    message: format!(
                        "dependency `{name}`: registry/version dependencies are not yet \
                         supported; use `path` or `git` instead"
                    ),
                    path: manifest_path.to_path_buf(),
                });
            }

            if has_path && has_git {
                return Err(ManifestError::Validation {
                    message: format!(
                        "dependency `{name}`: cannot specify both `path` and `git`; pick one"
                    ),
                    path: manifest_path.to_path_buf(),
                });
            }

            if has_git {
                return convert_git_dependency(name, &table, manifest_path);
            }

            // Default arm: path dependency.
            convert_path_dependency(name, &table, manifest_path)
        }
    }
}

fn convert_path_dependency(
    name: &str,
    table: &BTreeMap<String, toml::Value>,
    manifest_path: &Path,
) -> Result<Dependency, ManifestError> {
    let path_value = table.get("path").ok_or_else(|| ManifestError::Validation {
        message: format!(
            "dependency `{name}`: missing required key `path` (or use `git` for a git dep)"
        ),
        path: manifest_path.to_path_buf(),
    })?;
    let path_str = path_value
        .as_str()
        .ok_or_else(|| ManifestError::Validation {
            message: format!("dependency `{name}`: `path` must be a string"),
            path: manifest_path.to_path_buf(),
        })?;

    for key in table.keys() {
        if key != "path" {
            return Err(ManifestError::Validation {
                message: format!(
                    "dependency `{name}`: unknown key `{key}` (only `path` is recognized for path deps)"
                ),
                path: manifest_path.to_path_buf(),
            });
        }
    }

    Ok(Dependency::Path {
        path: PathBuf::from(path_str),
    })
}

fn convert_git_dependency(
    name: &str,
    table: &BTreeMap<String, toml::Value>,
    manifest_path: &Path,
) -> Result<Dependency, ManifestError> {
    let url = table
        .get("git")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ManifestError::Validation {
            message: format!("dependency `{name}`: `git` must be a string URL"),
            path: manifest_path.to_path_buf(),
        })?
        .to_string();

    // The URL is handed to `git` at lock time, and this manifest may be
    // a transitive dependency's, so it is untrusted input: an
    // option-shaped value such as `--upload-pack=<cmd>` would make git
    // run `<cmd>`. Every manifest goes through this one check.
    crate::git::validate_git_url(&url).map_err(|e| ManifestError::Validation {
        message: format!("dependency `{name}`: {e}"),
        path: manifest_path.to_path_buf(),
    })?;

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
            return Err(ManifestError::Validation {
                message: format!(
                    "dependency `{name}`: git dependency requires exactly one of `rev`, \
                     `branch`, or `tag`"
                ),
                path: manifest_path.to_path_buf(),
            });
        }
        1 => {
            let (key, value) = ref_forms[0];
            let s = value
                .as_str()
                .ok_or_else(|| ManifestError::Validation {
                    message: format!("dependency `{name}`: `{key}` must be a string"),
                    path: manifest_path.to_path_buf(),
                })?
                .to_string();
            match key {
                "rev" => GitRef::Rev(s),
                "branch" => GitRef::Branch(s),
                "tag" => GitRef::Tag(s),
                _ => unreachable!("ref_forms keys are restricted above"),
            }
        }
        _ => {
            let mentioned: Vec<&str> = ref_forms.iter().map(|(k, _)| *k).collect();
            return Err(ManifestError::Validation {
                message: format!(
                    "dependency `{name}`: git dependency must specify exactly one of `rev`, \
                     `branch`, or `tag` (found: {})",
                    mentioned.join(", ")
                ),
                path: manifest_path.to_path_buf(),
            });
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
                return Err(ManifestError::Validation {
                    message: format!(
                        "dependency `{name}`: unknown key `{other}` for a git dependency \
                         (allowed keys: `git`, `rev`, `branch`, `tag`)"
                    ),
                    path: manifest_path.to_path_buf(),
                });
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

    #[test]
    fn parse_error_shows_the_lines_of_the_parser_and_no_other() {
        let path = PathBuf::from("/srv/app/silt.toml");
        // A file without escape sequences: the parser's two lines.
        let plain = ManifestError::Parse {
            message: toml_error_message("invalid string\nexpected `\"`, `'`", "name = oops\n"),
            path: path.clone(),
            span: None,
        };
        assert_eq!(
            plain.to_string(),
            "invalid manifest /srv/app/silt.toml: invalid string\nexpected `\"`, `'`"
        );
        // A file with a key that holds a line break: one line.
        let source = "\"x\\nerror: FORGED\\u001b[2K\\u202e\" = 1\n";
        let quoted = ManifestError::Parse {
            message: toml_error_message(&format!("unknown field `{HOSTILE}`"), source),
            path,
            span: None,
        };
        assert_eq!(
            quoted.to_string(),
            format!("invalid manifest /srv/app/silt.toml: unknown field `{HOSTILE_ESCAPED}`")
        );
    }

    #[test]
    fn every_error_variant_escapes_its_whole_message() {
        let path = PathBuf::from(format!("/srv/{HOSTILE}/silt.toml"));
        // The file the parser's message is about: it holds the value
        // as an escape sequence.
        let source = "\"x\\nerror: FORGED\\u001b[2K\\u202e\" = 1\n";
        let errors = [
            ManifestError::Io(
                std::io::Error::new(std::io::ErrorKind::NotFound, HOSTILE),
                path.clone(),
            ),
            ManifestError::Parse {
                message: toml_error_message(&format!("unknown field `{HOSTILE}`"), source),
                path: path.clone(),
                span: None,
            },
            ManifestError::Validation {
                message: format!("invalid package name `{HOSTILE}`"),
                path,
            },
        ];
        for err in errors {
            let rendered = err.to_string();
            assert!(
                !rendered.chars().any(crate::git::needs_escape),
                "the message must be one printable line: {rendered:?}"
            );
            // Once in the path, once in the rest of the message.
            assert_eq!(rendered.matches(HOSTILE_ESCAPED).count(), 2, "{rendered}");
        }
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
