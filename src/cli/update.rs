//! `silt update` — regenerate `silt.lock` for the current package's
//! dependencies.

use std::path::PathBuf;
use std::process;

use silt::git::escape_for_display;
use silt::package_graph::{LockPolicy, resolve_packages};
use silt::source::SourceMap;

use crate::cli::package::{PackageFailure, die_on_manifest_error, find_project_root};

/// Dispatch `silt update`: regenerate the lock for the current
/// package's whole dependency tree. It takes no arguments.
pub(crate) fn dispatch(args: &[String]) {
    let mut wants_help = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--help" | "-h" => wants_help = true,
            other if other.starts_with('-') => {
                eprintln!("silt update: unknown flag '{}'", escape_for_display(other));
                eprintln!("Run 'silt update --help' for usage.");
                process::exit(1);
            }
            other => {
                eprintln!(
                    "silt update: unexpected extra argument '{}'",
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
        println!("Resolves the full dependency tree, computes content checksums,");
        println!("and writes the result next to `silt.toml`.");
        println!();
        println!("To update the silt binary itself, use `silt self-update` instead.");
        process::exit(0);
    }
    run_dependency_update();
}

/// Implementation of `silt update`.
///
/// Walks up from cwd to find a `silt.toml`, resolves the full dep tree
/// fresh from disk, computes checksums, and writes `silt.lock` next to
/// the manifest, the entire lock.
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
                "silt update must be run inside a silt package (no silt.toml found in this directory or any parent)"
            );
            process::exit(1);
        }
        Err(e) => die_on_manifest_error(e),
    };

    let mut sources = SourceMap::new();
    let graph = match resolve_packages(&root, LockPolicy::Refresh, &mut sources) {
        Ok(graph) => graph,
        Err(diagnostics) => die_on_manifest_error(PackageFailure {
            sources,
            diagnostics,
        }),
    };

    // Count of pinned (non-root) packages. Quiet single-line summary
    // matches the tone of `cargo update`'s default output.
    let dep_count = graph.packages.len() - 1;
    if dep_count == 1 {
        eprintln!("Locked 1 dependency.");
    } else {
        eprintln!("Locked {dep_count} dependencies.");
    }
}
