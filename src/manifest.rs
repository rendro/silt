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
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::intern::{self, Symbol};
use crate::module::BUILTIN_MODULES;

// Re-exported for callers that want to construct or pattern-match
// `Dependency::Git { ref_spec, .. }` without reaching into `crate::git`.
pub use crate::git::GitRef;

/// A loaded and validated `silt.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: PackageMeta,
    pub dependencies: BTreeMap<Symbol, Dependency>,
    /// Absolute path to the silt.toml file this manifest was loaded from.
    pub manifest_path: PathBuf,
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io(err, path) => {
                write!(f, "failed to read manifest {}: {}", path.display(), err)
            }
            ManifestError::Parse { message, path, .. } => {
                write!(f, "invalid manifest {}: {}", path.display(), message)
            }
            ManifestError::Validation { message, path } => {
                write!(f, "invalid manifest {}: {}", path.display(), message)
            }
        }
    }
}

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

#[derive(Deserialize)]
struct RawManifest {
    package: RawPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
}

#[derive(Deserialize)]
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
                message: e.message().to_string(),
                path: absolute.clone(),
                span,
            }
        })?;

        // Validation phase ----------------------------------------------------
        validate_identifier(&raw.package.name, "package name", &absolute)?;
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
            let dep = convert_dependency(&raw_name, raw_dep, &absolute)?;
            let sym = intern::intern(&raw_name);
            dependencies.insert(sym, dep);
        }

        Ok(Manifest {
            package: PackageMeta {
                name: intern::intern(&raw.package.name),
                version: raw.package.version,
                edition: raw.package.edition,
            },
            dependencies,
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
/// itself), optionally followed by `-PRERELEASE` and/or `+BUILD`.
///
/// We deliberately avoid a `semver` crate dependency for now; v0.7 only
/// needs to detect obviously-malformed strings. Real precedence rules
/// arrive with the registry workflow in a later phase.
fn is_valid_version(version: &str) -> bool {
    let (core, _suffix) = split_off_build(version);
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
    if let Some(pre) = pre {
        if pre.is_empty() {
            return false;
        }
        for ident in pre.split('.') {
            if ident.is_empty() {
                return false;
            }
            // Pre-release identifiers may be alphanumeric or numeric (no
            // leading zeros for the latter); we keep the check loose here.
            if !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return false;
            }
        }
    }
    true
}

fn split_off_build(version: &str) -> (&str, Option<&str>) {
    match version.split_once('+') {
        Some((core, build)) => (core, Some(build)),
        None => (version, None),
    }
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
             `MAJOR.MINOR.PATCH` (e.g. `0.1.0`)"
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
}
