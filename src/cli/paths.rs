//! Filesystem path helpers used across several CLI subcommands:
//! recursive .silt discovery for `silt fmt`, and the path-relative
//! helper that `silt add` uses when recording dependency paths in
//! `silt.toml`. (Lexical `.`/`..` normalization is NOT defined here:
//! `silt add` delegates to `silt::lockfile::normalize_path`, the
//! single definition, so the manifest form and lockfile resolution
//! can never normalize differently.)

use std::fs;
use std::path::{Path, PathBuf};

/// Recursively find all .silt files in a directory.
///
/// Skips directories that the LSP preloader also skips (the shared
/// `silt::file_discovery::should_skip_dir` policy): `target/`, `.git/`,
/// `node_modules/`, and anything under `fuzz/corpus/`. Without that
/// filter, `silt fmt <dir>` would happily rewrite vendored or generated
/// `.silt` files (a workspace that vendors a sibling silt project under
/// `vendor/silt-src/target/`, or has a `.silt` artefact deposited under
/// `target/`, would otherwise see its contents rewritten by an
/// unsuspecting `silt fmt .`), and `silt test` would try to run them.
/// Audit round 87 LATENT.
pub(crate) fn find_silt_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if silt::file_discovery::should_skip_dir(&path) {
                continue;
            }
            results.extend(find_silt_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with(".silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

/// Render an error/frame path in the same style the user typed on the
/// command line (audit rounds 17/21 policy, F13 + G1/G2):
///
/// - user typed an **absolute** path → render `candidate` absolute,
///   joining relative candidates onto `cwd` when available;
/// - user typed a **relative** path → render `candidate` relative by
///   stripping `cwd` when it is a prefix, otherwise fall back to the
///   candidate as-is (e.g. `strip_prefix` failure under a symlinked
///   cwd);
/// - no `cwd` available (rare: deleted working dir) → candidate as-is.
///
/// Round-101 LATENT: this body used to be a byte-identical 21-line
/// closure duplicated between `silt run` (src/cli/run.rs) and
/// `silt test` (src/cli/test.rs) error rendering — an
/// acknowledged-mirror drift hazard. Hoisted here so both subcommands
/// share one implementation.
///
/// Locks: unit tests below (`display_path_for_*`), source-grep lock
/// tests/round101_display_path_helper_lock_tests.rs, plus the
/// behavioral rendering locks in tests/cli_test_rendering_tests.rs
/// (`test_run_module_error_paths_consistently_normalized`,
/// `test_test_setup_error_paths_normalized`,
/// `test_cross_module_call_stack_uses_consistent_path_style`).
pub(crate) fn display_path_for(
    user_path_is_absolute: bool,
    cwd: Option<&Path>,
    candidate: &Path,
) -> String {
    if user_path_is_absolute {
        if candidate.is_absolute() {
            candidate.display().to_string()
        } else if let Some(cwd) = cwd {
            cwd.join(candidate).display().to_string()
        } else {
            candidate.display().to_string()
        }
    } else if let Some(cwd) = cwd {
        match candidate.strip_prefix(cwd) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => candidate.display().to_string(),
        }
    } else {
        candidate.display().to_string()
    }
}

/// Express `target` as a path relative to `base`, using `..` segments
/// where necessary. Returns `None` only when the inputs differ in
/// rootedness (one absolute, one relative) — there's no sensible
/// relative form in that case and the caller falls back to absolute.
///
/// Rolling our own keeps us off the `pathdiff` crate; the logic is
/// 20 lines and the v0.7 manifest only needs ASCII-cleanly-named
/// paths anyway.
pub(crate) fn relative_from(base: &Path, target: &Path) -> Option<PathBuf> {
    if base.is_absolute() != target.is_absolute() {
        return None;
    }
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    // Find the longest common prefix.
    let mut shared = 0;
    while shared < base_components.len()
        && shared < target_components.len()
        && base_components[shared] == target_components[shared]
    {
        shared += 1;
    }
    let mut result = PathBuf::new();
    for _ in shared..base_components.len() {
        result.push("..");
    }
    for comp in &target_components[shared..] {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        // base == target — express that as `.` rather than the empty
        // string so toml_edit emits a syntactically valid path.
        result.push(".");
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Round-101: direct behavioral locks for the shared path-display
    // policy that `silt run` / `silt test` error rendering delegate to.
    // These pin the exact semantics the previously-duplicated closures
    // implemented, so a future edit to the helper that changes any of
    // the three branches trips here without needing a full CLI run.

    /// Relative user path + cwd is a prefix of the candidate → the
    /// cwd prefix is stripped and the path renders relative.
    #[test]
    fn display_path_for_relative_user_path_strips_cwd_prefix() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("/work/proj/src/helper.silt");
        assert_eq!(
            display_path_for(false, Some(cwd), candidate),
            "src/helper.silt"
        );
    }

    /// Relative user path but strip_prefix fails (candidate outside
    /// cwd, e.g. symlinked working directory) → candidate rendered
    /// as-is rather than mangled.
    #[test]
    fn display_path_for_relative_user_path_strip_prefix_failure_falls_back() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("/elsewhere/helper.silt");
        assert_eq!(
            display_path_for(false, Some(cwd), candidate),
            "/elsewhere/helper.silt"
        );
    }

    /// Absolute user path + relative candidate → candidate is joined
    /// onto cwd so everything renders absolute, matching what the user
    /// typed.
    #[test]
    fn display_path_for_absolute_user_path_absolutizes_relative_candidate() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("src/helper.silt");
        // `join` inserts the platform separator between cwd and the
        // candidate; the candidate's own `/` separators are preserved
        // verbatim by Display.
        let expected = format!("/work/proj{}src/helper.silt", std::path::MAIN_SEPARATOR);
        assert_eq!(display_path_for(true, Some(cwd), candidate), expected);
    }

    /// Absolute user path + already-absolute candidate → unchanged.
    #[test]
    fn display_path_for_absolute_user_path_keeps_absolute_candidate() {
        let candidate = Path::new("/work/proj/src/helper.silt");
        assert_eq!(
            display_path_for(true, Some(Path::new("/work/proj")), candidate),
            "/work/proj/src/helper.silt"
        );
    }

    /// No cwd available (deleted working directory) → candidate
    /// rendered as-is in both user-path styles.
    #[test]
    fn display_path_for_no_cwd_renders_candidate_as_is() {
        let candidate = Path::new("src/helper.silt");
        assert_eq!(display_path_for(false, None, candidate), "src/helper.silt");
        assert_eq!(display_path_for(true, None, candidate), "src/helper.silt");
    }
}
