//! `silt remove <name>` — drop a dependency from the current package's
//! `silt.toml` and regenerate `silt.lock` without it.

use std::fs;
use std::process;

use silt::git::escape_for_display;
use silt::intern;
use silt::package_graph::{LockPolicy, resolve_packages};
use silt::source::SourceMap;

use crate::cli::package::{PackageFailure, die_on_manifest_error, find_project_root};

/// Why `silt remove` failed.
enum RemoveError {
    /// A message of one line.
    Message(String),
    /// A usage error: the message, then a line pointing at `--help`.
    Usage(String),
    /// Diagnostics in the manifests or the lockfile.
    Package(PackageFailure),
}

impl From<String> for RemoveError {
    fn from(message: String) -> Self {
        RemoveError::Message(message)
    }
}

impl From<PackageFailure> for RemoveError {
    fn from(failure: PackageFailure) -> Self {
        RemoveError::Package(failure)
    }
}

/// Dispatch `silt remove <name>`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: silt remove <name>");
        println!();
        println!("Remove a dependency from the current package's silt.toml,");
        println!("then regenerate silt.lock without it (and without what only");
        println!("it needed).");
        println!();
        println!("Arguments:");
        println!("  <name>             The name the dependency is imported as.");
        println!();
        println!("Example:");
        println!("  silt remove calc");
        process::exit(0);
    }
    match remove(&args[2..]) {
        Ok(()) => {}
        Err(RemoveError::Package(failure)) => die_on_manifest_error(failure),
        Err(RemoveError::Message(message)) => {
            eprintln!("error: {}", escape_for_display(&message));
            process::exit(1);
        }
        Err(RemoveError::Usage(message)) => {
            eprintln!("error: {}", escape_for_display(&message));
            eprintln!("Run 'silt remove --help' for usage.");
            process::exit(1);
        }
    }
}

/// Remove the dependency the arguments name: from silt.toml (its
/// formatting and comments kept, by `toml_edit`), then from silt.lock,
/// which the package graph rewrites. If the graph does not resolve
/// without it, silt.toml gets its old text back.
fn remove(args: &[String]) -> Result<(), RemoveError> {
    // One name, which is no flag.
    let name = match args {
        [name] if !name.starts_with('-') => name.clone(),
        _ => {
            return Err(RemoveError::Usage(
                "silt remove takes the name of one dependency: silt remove <name>".into(),
            ));
        }
    };

    let cwd = std::env::current_dir().map_err(|e| format!("failed to determine cwd: {e}"))?;
    let (root, manifest) = match find_project_root(&cwd)? {
        Some(pair) => pair,
        None => {
            return Err("silt remove must be run inside a silt package"
                .to_string()
                .into());
        }
    };
    if !manifest
        .dependencies
        .keys()
        .any(|key| intern::resolve(*key) == name)
    {
        return Err(format!("silt remove: no dependency '{name}' in silt.toml").into());
    }

    let manifest_path = root.join("silt.toml");
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "failed to read {}: {}",
            manifest_path.display(),
            silt::diagnostic::io_error_text(&e)
        )
    })?;
    let mut doc = manifest_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("silt.toml is not TOML: {}", e.message()))?;
    // The manifest loaded, so its dependencies are a table (a header
    // table or an inline one) that holds the name.
    let removed = doc
        .get_mut("dependencies")
        .and_then(|deps| deps.as_table_like_mut())
        .and_then(|deps| deps.remove(&name));
    if removed.is_none() {
        return Err(format!("silt remove: no dependency '{name}' in silt.toml").into());
    }
    fs::write(&manifest_path, doc.to_string())
        .map_err(|e| format!("failed to write {}: {e}", manifest_path.display()))?;

    let mut sources = SourceMap::new();
    if let Err(mut diagnostics) = resolve_packages(&root, LockPolicy::Update, &mut sources) {
        fs::write(&manifest_path, &manifest_text)
            .map_err(|e| format!("failed to restore {}: {e}", manifest_path.display()))?;
        for d in &mut diagnostics {
            d.notes
                .push("silt remove left silt.toml as it was".to_string());
        }
        return Err(RemoveError::Package(PackageFailure {
            sources,
            diagnostics,
        }));
    }
    println!("Removed dependency '{}'", escape_for_display(&name));
    Ok(())
}
