//! `silt init [--lib]` — create a new silt package in the current
//! directory: a program (`src/main.silt`) or a library (`src/lib.silt`),
//! with a first test beside it.

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
            println!("src/lib.silt, a library for other packages to import, and a");
            println!("first test beside it (src/main_test.silt or src/lib_test.silt);");
            println!("the package name is derived from the directory name.");
            println!();
            println!("Options:");
            println!("  --lib    A library: src/lib.silt instead of src/main.silt");
            process::exit(0);
        }
    }
    // Reject unknown flags before proceeding.
    for arg in &args[2..] {
        if arg.starts_with('-') && arg != "--help" && arg != "-h" && arg != "--lib" {
            eprintln!("error: silt init: unknown flag '{arg}'");
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
                "error: silt init: unexpected argument '{arg}' (silt init takes no positional arguments; the package name is derived from the current directory)"
            );
            eprintln!(
                "                  to create a package in a new directory, run: mkdir {arg} && cd {arg} && silt init"
            );
            process::exit(1);
        }
    }
    let libs = args[2..].iter().filter(|arg| *arg == "--lib").count();
    if libs > 1 {
        crate::cli::once_only("init", "--lib");
    }
    init_project(libs == 1);
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
        eprintln!("error: silt init: failed to determine current directory: {e}");
        process::exit(1);
    });

    let manifest_path = cwd.join("silt.toml");
    if manifest_path.exists() {
        eprintln!(
            "error: silt init: silt.toml already exists at {}",
            manifest_path.display()
        );
        process::exit(1);
    }
    let (entry, test) = match lib {
        true => ("lib.silt", "lib_test.silt"),
        false => ("main.silt", "main_test.silt"),
    };
    let entry_path = cwd.join("src").join(entry);
    let test_path = cwd.join("src").join(test);
    for (name, path) in [(entry, &entry_path), (test, &test_path)] {
        if path.exists() {
            eprintln!(
                "error: silt init: src/{name} already exists at {}",
                path.display()
            );
            process::exit(1);
        }
    }

    let dirname = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let package_name = match sanitize_package_name(dirname) {
        Some(name) => name,
        None => {
            eprintln!(
                "error: silt init: cannot derive a package name from the current directory `{}`",
                cwd.display()
            );
            eprintln!(
                "                  the directory needs a name that contains at least one lowercase letter,"
            );
            eprintln!(
                "                  digit, or underscore (or rename it to a valid silt identifier)."
            );
            process::exit(1);
        }
    };
    // Reject package names that collide with a builtin module (`io`,
    // `string`, …). Routed through the manifest module's
    // `validate_package_name` so init / add / Manifest::load share one
    // acceptance rule. Mirrors `silt add`'s wording for builtin
    // collisions on dep names — see `src/cli/add.rs:273-279`.
    // Previously `mkdir io && cd io && silt init`
    // silently wrote `name = "io"`, producing a package whose top-level
    // name shadowed the stdlib `io` module.
    if let Err(msg) = silt::manifest::validate_package_name(&package_name) {
        eprintln!("error: silt init: {msg}");
        eprintln!(
            "                  (the package name was derived from the directory name `{dirname}`;"
        );
        eprintln!(
            "                   rename the directory or run `silt init` from a directory whose name"
        );
        eprintln!("                   is not a keyword or a stdlib module).");
        process::exit(1);
    }

    let manifest_contents = format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n");
    // The test imports the module it tests, as a test file of a package
    // does, and calls a `pub` function of it.
    let (entry_contents, test_contents) = match lib {
        true => (
            "pub fn hello() -> String {\n  \"hello, silt!\"\n}\n",
            "import lib\nimport test\n\nfn test_hello() {\n  test.assert_eq(lib.hello(), \"hello, silt!\")\n}\n",
        ),
        false => (
            "pub fn greeting() -> String {\n  \"hello, silt!\"\n}\n\nfn main() {\n  println(greeting())\n}\n",
            "import main\nimport test\n\nfn test_greeting() {\n  test.assert_eq(main.greeting(), \"hello, silt!\")\n}\n",
        ),
    };

    // Create src/ first so a failure there doesn't leave a stray manifest.
    if let Err(e) = fs::create_dir_all(entry_path.parent().unwrap()) {
        eprintln!(
            "error: silt init: failed to create directory {}: {e}",
            entry_path.parent().unwrap().display()
        );
        process::exit(1);
    }
    // Best-effort cleanup so a partial init doesn't leave stray source
    // files without a manifest pinning them to a package.
    let written = [
        (&entry_path, entry_contents),
        (&test_path, test_contents),
        (&manifest_path, manifest_contents.as_str()),
    ];
    for (i, (path, contents)) in written.iter().enumerate() {
        if let Err(e) = fs::write(path, contents) {
            eprintln!("error writing {}: {e}", path.display());
            for (earlier, _) in &written[..i] {
                let _ = fs::remove_file(earlier);
            }
            process::exit(1);
        }
    }

    println!("created silt package `{package_name}`:");
    println!("  {}", manifest_path.display());
    println!("  {}", entry_path.display());
    println!("  {}", test_path.display());
    println!();
    match lib {
        true => println!("  check: silt check"),
        false => println!("  run:   silt run"),
    }
    println!("  test:  silt test");
}
