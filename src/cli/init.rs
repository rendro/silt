//! `silt init [--lib]` — create a new silt package in the current
//! directory: a program (`src/main.silt`) or a library (`src/lib.silt`).

use std::fs;
use std::process;

/// Dispatch `silt init [--lib]`.
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            println!("Usage: silt init [--lib]");
            println!();
            println!("Create a new silt package in the current directory.");
            println!("Writes silt.toml and src/main.silt, a program, or with --lib");
            println!("src/lib.silt, a library for other packages to import; the");
            println!("package name is derived from the directory name.");
            println!();
            println!("Options:");
            println!("  --lib    A library: src/lib.silt instead of src/main.silt");
            process::exit(0);
        }
    }
    // Reject unknown flags before proceeding.
    for arg in &args[2..] {
        if arg.starts_with('-') && arg != "--help" && arg != "-h" && arg != "--lib" {
            eprintln!("silt init: unknown flag '{arg}'");
            eprintln!("Run 'silt init --help' for usage.");
            process::exit(1);
        }
    }
    // Reject positional arguments. Users coming from `cargo new <name>`
    // / `npm init <name>` will reach for `silt init my_project` and
    // expect either Cargo-style honoring or a clear rejection — the
    // pre-fix behavior silently ignored the positional and named the
    // package after cwd, which was a footgun.
    for arg in &args[2..] {
        if !arg.starts_with('-') {
            eprintln!(
                "silt init: unexpected argument '{arg}' (silt init takes no positional arguments; the package name is derived from the current directory)"
            );
            eprintln!(
                "       to create a package in a new directory, run: mkdir {arg} && cd {arg} && silt init"
            );
            process::exit(1);
        }
    }
    init_project(args[2..].iter().any(|arg| arg == "--lib"));
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

/// Create the package: a library (`src/lib.silt`) when `lib`, a
/// program (`src/main.silt`) otherwise.
fn init_project(lib: bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: failed to determine current directory: {e}");
        process::exit(1);
    });

    let manifest_path = cwd.join("silt.toml");
    if manifest_path.exists() {
        eprintln!("silt.toml already exists at {}", manifest_path.display());
        process::exit(1);
    }
    let entry = match lib {
        true => "lib.silt",
        false => "main.silt",
    };
    let entry_path = cwd.join("src").join(entry);
    if entry_path.exists() {
        eprintln!("src/{entry} already exists at {}", entry_path.display());
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
    // Reject package names that collide with a builtin module (`io`,
    // `string`, …). Routed through the manifest module's
    // `validate_package_name` so init / add / Manifest::load share one
    // acceptance rule. Mirrors `silt add`'s wording for builtin
    // collisions on dep names — see `src/cli/add.rs:273-279`.
    // Round-75 DX-5 GAP fix: previously `mkdir io && cd io && silt init`
    // silently wrote `name = "io"`, producing a package whose top-level
    // name shadowed the stdlib `io` module.
    if let Err(msg) = silt::manifest::validate_package_name(&package_name) {
        eprintln!("silt init: {msg}");
        eprintln!("       (the package name was derived from the directory name `{dirname}`;");
        eprintln!("        rename the directory or run `silt init` from a directory whose name");
        eprintln!("        is not a stdlib module).");
        process::exit(1);
    }

    let manifest_contents = format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n");
    let entry_contents = match lib {
        true => "pub fn hello() -> String {\n  \"hello, silt!\"\n}\n",
        false => "fn main() {\n  println(\"hello, silt!\")\n}\n",
    };

    // Create src/ first so a failure there doesn't leave a stray manifest.
    if let Err(e) = fs::create_dir_all(entry_path.parent().unwrap()) {
        eprintln!(
            "error: failed to create directory {}: {e}",
            entry_path.parent().unwrap().display()
        );
        process::exit(1);
    }
    if let Err(e) = fs::write(&entry_path, entry_contents) {
        eprintln!("error writing {}: {e}", entry_path.display());
        process::exit(1);
    }
    if let Err(e) = fs::write(&manifest_path, manifest_contents) {
        eprintln!("error writing {}: {e}", manifest_path.display());
        // Best-effort cleanup so a partial init doesn't leave a stray
        // source file without a manifest pinning it to a package.
        let _ = fs::remove_file(&entry_path);
        process::exit(1);
    }

    println!("created silt package `{package_name}`:");
    println!("  {}", manifest_path.display());
    println!("  {}", entry_path.display());
    println!();
    match lib {
        true => println!("  check: silt check"),
        false => println!("  run:   silt run"),
    }
    println!("  test:  silt test");
}
