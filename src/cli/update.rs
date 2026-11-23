//! `silt update` — regenerate `silt.lock` for the current package's
//! dependencies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;

use crate::git::{GitRef, escape_for_display};
use crate::lockfile::{LockedPackage, LockedSource, Lockfile};
use crate::package_graph::{LockPolicy, resolve_packages};
use crate::source::SourceMap;

use crate::cli::package::{PackageFailure, die_on_manifest_error, find_project_root};

/// Dispatch `silt update`: regenerate the lock for the current
/// package's whole dependency tree. It takes no arguments.
pub(crate) fn dispatch(args: &[String]) {
    let mut wants_help = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--help" | "-h" => wants_help = true,
            other if other.starts_with('-') => {
                eprintln!(
                    "error: silt update: unknown flag '{}'",
                    escape_for_display(other)
                );
                eprintln!("Run 'silt update --help' for usage.");
                process::exit(1);
            }
            other => {
                eprintln!(
                    "error: silt update: unexpected extra argument '{}'",
                    escape_for_display(other)
                );
                process::exit(1);
            }
        }
    }
    if wants_help {
        println!("Usage: silt update");
        println!();
        println!("Regenerate `silt.lock` from the current package's `silt.toml`.");
        println!("Resolves the full dependency tree, computes the checksums of git");
        println!("dependencies, and writes the result next to `silt.toml`. Prints a");
        println!("line for each dependency whose pin changed.");
        println!();
        println!("To update the silt binary itself, use `silt self-update` instead.");
        process::exit(0);
    }
    run_dependency_update();
}

/// Implementation of `silt update`.
///
/// Walks up from cwd to find a `silt.toml`, resolves the full dep tree
/// fresh from disk, computes git checksums, and writes `silt.lock` next to
/// the manifest, the entire lock. Then says what changed in it: a line for
/// each dependency whose pin moved, was added or was removed, or that the
/// lock is up to date.
///
/// Outside any package (no `silt.toml` reachable) we print a fixed
/// error message and exit 1 — this is the canonical "must be run
/// inside a silt package" diagnostic that tests pin to.
fn run_dependency_update() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (root, _) = match find_project_root(&cwd) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!(
                "error: silt update: must be run inside a silt package (no silt.toml found in this directory or any parent)"
            );
            process::exit(1);
        }
        Err(e) => die_on_manifest_error(e),
    };

    // (A lock that cannot be read is no pin: every dependency is new.)
    let lock_path = root.join("silt.lock");
    let read = || {
        Lockfile::load(&lock_path, &mut SourceMap::new())
            .ok()
            .flatten()
    };
    let before = read();
    let mut sources = SourceMap::new();
    if let Err(diagnostics) = resolve_packages(&root, LockPolicy::Refresh, &mut sources) {
        die_on_manifest_error(PackageFailure {
            sources,
            diagnostics,
        });
    }
    let after = read();
    let changes = lock_changes(before.as_ref(), after.as_ref(), &root);
    if changes.is_empty() {
        eprintln!("silt.lock is up to date.");
    }
    for change in changes {
        eprintln!("{change}");
    }
}

/// What changed between the lock `before` and the lock `after` (of the
/// package in `root`): a line for each dependency, by name, whose pin is
/// new, gone or another. The package itself is not one.
fn lock_changes(before: Option<&Lockfile>, after: Option<&Lockfile>, root: &Path) -> Vec<String> {
    let dependencies = |lock: Option<&Lockfile>| -> BTreeMap<String, LockedPackage> {
        lock.map(|lock| lock.packages.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|p| p.source != LockedSource::Local)
            .map(|p| (p.name.clone(), p.clone()))
            .collect()
    };
    let (old, new) = (dependencies(before), dependencies(after));
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| {
            let change = match (old.get(name), new.get(name)) {
                (None, Some(new)) => format!("added, {}", pin(&new.source)),
                (Some(_), None) => "removed".to_string(),
                (Some(old), Some(new)) => change(old, new, root)?,
                (None, None) => return None,
            };
            Some(format!("{}: {change}", escape_for_display(name)))
        })
        .collect()
}

/// How the pin of one dependency changed from `old` to `new`, or `None`
/// when it did not.
fn change(old: &LockedPackage, new: &LockedPackage, root: &Path) -> Option<String> {
    let version = (old.version != new.version).then(|| {
        format!(
            "version {} -> {}",
            escape_for_display(&old.version),
            escape_for_display(&new.version)
        )
    });
    let source = if old.source.same_place(&new.source, root) {
        match &new.source {
            // The lock held another checksum for the commit (edited by
            // hand): it has the commit's again.
            LockedSource::Git { resolved_sha, .. } if old.checksum != new.checksum => Some(
                format!("the checksum of {} written again", short(resolved_sha)),
            ),
            _ => None,
        }
    } else {
        Some(match (&old.source, &new.source) {
            (
                LockedSource::Git {
                    url: old_url,
                    ref_spec: old_ref,
                    resolved_sha: old_sha,
                },
                LockedSource::Git {
                    url: new_url,
                    ref_spec: new_ref,
                    resolved_sha: new_sha,
                },
            ) if old_url == new_url && old_ref == new_ref => {
                let moved = match new_ref {
                    GitRef::Tag(_) => "now names another commit",
                    _ => "moved",
                };
                format!(
                    "{} -> {} ({} `{}` {moved})",
                    short(old_sha),
                    short(new_sha),
                    new_ref.kind(),
                    escape_for_display(new_ref.as_ref_string())
                )
            }
            (LockedSource::Git { url: old_url, .. }, LockedSource::Git { url: new_url, .. })
                if old_url != new_url =>
            {
                format!(
                    "{} -> {}, from {}",
                    pin(&old.source),
                    pin(&new.source),
                    crate::git::display_url(new_url)
                )
            }
            _ => format!("{} -> {}", pin(&old.source), pin(&new.source)),
        })
    };
    match (source, version) {
        (None, None) => None,
        (Some(source), None) => Some(source),
        (None, Some(version)) => Some(version),
        (Some(source), Some(version)) => Some(format!("{source}, {version}")),
    }
}

/// A pin in words: tag `v1.0.0` at 3b8d81f, commit 3b8d81f, path ../calc.
fn pin(source: &LockedSource) -> String {
    match source {
        LockedSource::Local => "this package".to_string(),
        LockedSource::Path { path } => format!("path {}", crate::source::shown_path(path)),
        LockedSource::Git {
            ref_spec: GitRef::Rev(_),
            resolved_sha,
            ..
        } => format!("commit {}", short(resolved_sha)),
        LockedSource::Git {
            ref_spec,
            resolved_sha,
            ..
        } => format!(
            "{} `{}` at {}",
            ref_spec.kind(),
            escape_for_display(ref_spec.as_ref_string()),
            short(resolved_sha)
        ),
    }
}

/// The first seven characters of the commit id `sha`.
fn short(sha: &str) -> String {
    escape_for_display(&sha[..sha.len().min(7)])
}
