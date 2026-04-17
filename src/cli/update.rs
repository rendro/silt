//! `silt update [<dep-name>]` — regenerate `silt.lock` for the current
//! package's dependencies. Also handles the back-compat redirect for
//! legacy self-update flag shapes so old scripts get a clear pointer.

use std::path::PathBuf;
use std::process;

use silt::intern;
use silt::lockfile::Lockfile;

use crate::cli::package::{die_on_lockfile_error, die_on_manifest_error, find_project_root};

/// Dispatch `silt update [<dep-name>]`.
///
/// `silt update` manages package dependencies in v0.7+. It also
/// keeps a back-compat redirect for legacy self-update flags so
/// scripts that ran `silt update --dry-run` against old binaries
/// get a clear pointer to `silt self-update` rather than a
/// confusing "must be run inside a package" error.
///
/// Argument shapes:
///  - `silt update` (no args): regenerate the lock for the
///    current package's full dep tree.
///  - `silt update <name>`: regenerate the lock, optionally
///    targeting just one dep. For Phase-1 path-only deps this
///    behaves like the no-arg form (path deps don't have
///    versions to bump), but the API is wired up so PR-future-2
///    can implement targeted updates without touching the
///    dispatch.
///  - Legacy self-update flags (`--dry-run`, `--force`,
///    `--version=...`): print the redirect to `self-update` and
///    exit 2. Never silently invoke either path — that would
///    bite anyone scripting the old API.
pub(crate) fn dispatch(args: &[String]) {
    let mut saw_self_update_flag = false;
    let mut wants_help = false;
    let mut positional: Option<String> = None;
    for arg in &args[2..] {
        match arg.as_str() {
            "--help" | "-h" => wants_help = true,
            "--dry-run" | "--force" => saw_self_update_flag = true,
            other if other.starts_with("--version=") => saw_self_update_flag = true,
            other if other.starts_with('-') => {
                eprintln!("silt update: unknown flag '{other}'");
                eprintln!("Run 'silt update --help' for usage.");
                process::exit(1);
            }
            other if positional.is_none() => positional = Some(other.to_string()),
            other => {
                eprintln!("silt update: unexpected extra argument '{other}'");
                process::exit(1);
            }
        }
    }
    if wants_help {
        println!("Usage: silt update [<dep-name>]");
        println!();
        println!("Regenerate `silt.lock` from the current package's `silt.toml`.");
        println!("Resolves the full dependency tree, computes content checksums,");
        println!("and writes the result next to `silt.toml`.");
        println!();
        println!("Arguments:");
        println!("  <dep-name>     Update only the named dep (Phase-1 path deps");
        println!("                 are re-resolved the same way as the no-arg form;");
        println!("                 the argument exists for forward compat).");
        println!();
        println!("To update the silt binary itself, use `silt self-update` instead.");
        process::exit(0);
    }
    // Legacy redirect: keep firing on `--dry-run` / `--force` /
    // `--version=...` no matter where we are, because anyone
    // passing those flags is clearly trying to drive the old
    // self-updater.
    if saw_self_update_flag {
        eprintln!(
            "silt update has been renamed to silt self-update; the new silt update manages package dependencies. To update the silt binary itself, use 'silt self-update'."
        );
        process::exit(2);
    }
    run_dependency_update(positional.as_deref());
}

/// Implementation of `silt update [<dep-name>]`.
///
/// Walks up from cwd to find a `silt.toml`, resolves the full dep tree
/// fresh from disk, computes checksums, and writes `silt.lock` next to
/// the manifest. Always rewrites the entire lock — even when a single
/// dep was named — so other entries refresh in tandem. (For Phase-1
/// path deps this is fine; PR-future-2 will need a richer policy when
/// version arithmetic enters the picture.)
///
/// Outside any package (no `silt.toml` reachable) we print a fixed
/// error message and exit 1 — this is the canonical "must be run
/// inside a silt package" diagnostic that tests pin to.
fn run_dependency_update(target: Option<&str>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (root, manifest) = match find_project_root(&cwd) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!(
                "silt update must be run inside a silt package (no silt.toml found in this directory or any parent)"
            );
            process::exit(1);
        }
        Err(e) => die_on_manifest_error(e),
    };

    if let Some(name) = target {
        // Validate that the named dep actually exists in the manifest
        // before doing the work. Otherwise a typo silently rewrites the
        // lock with the existing dep set and the user is left wondering
        // why their requested update didn't happen.
        let known = manifest
            .dependencies
            .keys()
            .any(|sym| intern::resolve(*sym) == name);
        if !known {
            eprintln!(
                "silt update: dependency `{name}` is not declared in {}",
                manifest.manifest_path.display()
            );
            process::exit(1);
        }
    }

    let lockfile = match Lockfile::resolve(&manifest) {
        Ok(l) => l,
        Err(e) => die_on_lockfile_error(e),
    };
    let lockfile_path = root.join("silt.lock");
    if let Err(e) = lockfile.write(&lockfile_path) {
        die_on_lockfile_error(e);
    }

    // Count of pinned (non-root) packages. Quiet single-line summary
    // matches the tone of `cargo update`'s default output.
    let dep_count = lockfile
        .packages
        .iter()
        .filter(|p| !matches!(p.source, silt::lockfile::LockedSource::Local))
        .count();
    if dep_count == 1 {
        eprintln!("Locked 1 dependency.");
    } else {
        eprintln!("Locked {dep_count} dependencies.");
    }
}
