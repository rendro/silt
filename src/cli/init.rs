//! `silt init` — create a new silt package in the current directory.

use std::fs;
use std::process;

/// Dispatch `silt init`.
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            println!("Usage: silt init");
            println!();
            println!("Create a new silt package in the current directory.");
            println!("Writes silt.toml and src/main.silt; the package name is");
            println!("derived from the directory name.");
            process::exit(0);
        }
    }
    // Reject unknown flags before proceeding.
    for arg in &args[2..] {
        if arg.starts_with('-') && arg != "--help" && arg != "-h" {
            eprintln!("silt init: unknown flag '{arg}'");
            eprintln!("Run 'silt init --help' for usage.");
            process::exit(1);
        }
    }
    init_project();
}

/// Sanitize a directory name into a valid silt identifier:
/// - lowercase
/// - replace any non-`[a-z0-9_]` character with `_`
/// - prefix with `_` if the first character is a digit
///
/// Returns `None` if the result is empty (e.g. dirname was just punctuation
/// that all collapsed to underscores trimmed away — empty/blank input).
fn sanitize_package_name(dirname: &str) -> Option<String> {
    let lowered = dirname.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // Reject if empty (dirname was empty to begin with).
    if out.is_empty() {
        return None;
    }
    // If the first character is a digit, prefix with underscore so the
    // result still satisfies `[a-z_][a-z0-9_]*`.
    if out
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    Some(out)
}

fn init_project() {
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: failed to determine current directory: {e}");
        process::exit(1);
    });

    let manifest_path = cwd.join("silt.toml");
    if manifest_path.exists() {
        eprintln!("silt.toml already exists at {}", manifest_path.display());
        process::exit(1);
    }
    let main_path = cwd.join("src").join("main.silt");
    if main_path.exists() {
        eprintln!("src/main.silt already exists at {}", main_path.display());
        process::exit(1);
    }

    let dirname = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let package_name = match sanitize_package_name(dirname) {
        Some(name) => name,
        None => {
            eprintln!(
                "error: cannot derive a package name from the current directory `{}`",
                cwd.display()
            );
            eprintln!(
                "       the directory needs a name that contains at least one lowercase letter,"
            );
            eprintln!("       digit, or underscore (or rename it to a valid silt identifier).");
            process::exit(1);
        }
    };

    let manifest_contents = format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n");
    let main_contents = "fn main() {\n  println(\"hello, silt!\")\n}\n";

    // Create src/ first so a failure there doesn't leave a stray manifest.
    if let Err(e) = fs::create_dir_all(main_path.parent().unwrap()) {
        eprintln!(
            "error: failed to create directory {}: {e}",
            main_path.parent().unwrap().display()
        );
        process::exit(1);
    }
    if let Err(e) = fs::write(&main_path, main_contents) {
        eprintln!("error writing {}: {e}", main_path.display());
        process::exit(1);
    }
    if let Err(e) = fs::write(&manifest_path, manifest_contents) {
        eprintln!("error writing {}: {e}", manifest_path.display());
        // Best-effort cleanup so a partial init doesn't leave a stray
        // src/main.silt without a manifest pinning it to a package.
        let _ = fs::remove_file(&main_path);
        process::exit(1);
    }

    println!("created silt package `{package_name}`:");
    println!("  {}", manifest_path.display());
    println!("  {}", main_path.display());
    println!();
    println!("  run:   silt run");
    println!("  test:  silt test");
}
