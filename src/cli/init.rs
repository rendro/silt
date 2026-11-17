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
            eprintln!(
                "error: silt init: unknown flag '{}'",
                crate::git::escape_for_display(arg)
            );
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
                "error: silt init: unexpected argument '{}' (silt init takes no positional arguments; the package name is derived from the current directory)",
                crate::git::escape_for_display(arg)
            );
            let lib = args[2..].iter().any(|a| a == "--lib");
            eprintln!("                  {}", what_to_do_instead(arg, lib));
            process::exit(1);
        }
    }
    let libs = args[2..].iter().filter(|arg| *arg == "--lib").count();
    if libs > 1 {
        crate::cli::once_only("init", "--lib");
    }
    init_project(libs == 1);
}

/// What to do instead of `silt init <arg>`, which takes no argument: the
/// line under the error. A command to paste only where it works as it
/// stands: the name can be pasted (`source::pasteable`), the shell is a
/// POSIX one (on Windows, where `&&` and `mkdir -p` depend on which
/// shell runs it, the same advice in words), and `silt init` would
/// succeed there by its own checks (`init_checks`, `package_name_from`).
/// Else the advice in words, with what stands in the way. `lib` keeps
/// `--lib` in the command.
fn what_to_do_instead(arg: &str, lib: bool) -> String {
    let init = if lib { "silt init --lib" } else { "silt init" };
    let path = std::path::Path::new(arg);
    let shown = crate::git::escape_for_display(arg);
    let command = crate::source::pasteable(arg) && !cfg!(windows);
    if path.is_dir() {
        // (By the directory's real name: `..` has none of its own.)
        let dir = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let here = arg == ".";
        return match (init_checks(&dir, lib), here) {
            (Err(obstacle), true) => {
                format!("{init} here would stop: {}", obstacle.reason())
            }
            (Err(Obstacle::Manifest), false) => {
                format!("'{shown}' already holds a silt package (its silt.toml)")
            }
            (Err(obstacle), false) => format!(
                "'{shown}' is a directory, and {init} there would stop: {}",
                obstacle.reason()
            ),
            (Ok(_), true) => format!("to create the package in this directory, run: {init}"),
            (Ok(_), false) if command => format!(
                "'{arg}' is a directory; to create the package in it, run: cd {arg} && {init}"
            ),
            (Ok(_), false) => format!(
                "'{shown}' is a directory; to create the package in it, go into it and run {init} there"
            ),
        };
    }
    if path.exists() {
        return format!(
            "'{shown}' is a file; to create a package in a new directory, make one with another \
             name, go into it and run {init} there"
        );
    }
    // A new directory: the package is named after it, so one is suggested
    // only with a name that gives a package name init takes.
    let last = path.file_name().and_then(|n| n.to_str()).unwrap_or(arg);
    if let Err(obstacle) = package_name_from(last) {
        return format!(
            "{init} in a directory '{}' would stop: {}; make a directory with another name, go \
             into it and run {init} there",
            crate::git::escape_for_display(last),
            obstacle.reason()
        );
    }
    // A path of more than one name needs its parents made too.
    let mkdir = match arg.trim_end_matches('/').contains('/') {
        true => "mkdir -p",
        false => "mkdir",
    };
    match command {
        true => format!(
            "to create a package in a new directory, run: {mkdir} {arg} && cd {arg} && {init}"
        ),
        false => format!(
            "to create a package in a new directory, make the directory, go into it and run {init} \
             there"
        ),
    }
}

/// What stops `silt init` in a directory, before it writes anything.
enum Obstacle {
    /// The directory holds a silt.toml.
    Manifest,
    /// A file init would write exists: `src/<name>`.
    File(&'static str),
    /// The directory's name gives no package name.
    NoName,
    /// The package name the directory's name gives is refused: why.
    Refused(String),
}

impl Obstacle {
    /// The obstacle in a few words, for the advice of `what_to_do_instead`.
    fn reason(&self) -> String {
        match self {
            Obstacle::Manifest => "silt.toml already exists".to_string(),
            Obstacle::File(name) => format!("src/{name} already exists"),
            Obstacle::NoName => "the directory's name gives no package name".to_string(),
            Obstacle::Refused(why) => why.trim_end_matches("; pick a different name").to_string(),
        }
    }
}

/// The files `silt init` writes besides silt.toml, under `src/`: the
/// module and its first test.
fn entry_files(lib: bool) -> (&'static str, &'static str) {
    match lib {
        true => ("lib.silt", "lib_test.silt"),
        false => ("main.silt", "main_test.silt"),
    }
}

/// The package name `silt init` gives a package in a directory named
/// `dirname`, or why there is none: the name sanitized
/// (`sanitize_package_name`) and accepted by the manifest's rule
/// (`validate_package_name`: no stdlib module, no keyword).
fn package_name_from(dirname: &str) -> Result<String, Obstacle> {
    let name = sanitize_package_name(dirname).ok_or(Obstacle::NoName)?;
    match crate::manifest::validate_package_name(&name) {
        Ok(()) => Ok(name),
        Err(why) => Err(Obstacle::Refused(why)),
    }
}

/// The checks `silt init` makes in `dir` before it writes anything: no
/// silt.toml, none of the files it writes, a package name from the
/// directory's name. The package name, or the first obstacle.
fn init_checks(dir: &std::path::Path, lib: bool) -> Result<String, Obstacle> {
    if dir.join("silt.toml").exists() {
        return Err(Obstacle::Manifest);
    }
    let (entry, test) = entry_files(lib);
    for name in [entry, test] {
        if dir.join("src").join(name).exists() {
            return Err(Obstacle::File(name));
        }
    }
    package_name_from(dir.file_name().and_then(|n| n.to_str()).unwrap_or(""))
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
    let (entry, test) = entry_files(lib);
    let entry_path = cwd.join("src").join(entry);
    let test_path = cwd.join("src").join(test);
    let dirname = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let package_name = match init_checks(&cwd, lib) {
        Ok(name) => name,
        Err(Obstacle::Manifest) => {
            eprintln!(
                "error: silt init: silt.toml already exists at {}",
                crate::source::shown_path(&manifest_path)
            );
            process::exit(1);
        }
        Err(Obstacle::File(name)) => {
            eprintln!(
                "error: silt init: src/{name} already exists at {}",
                crate::source::shown_path(cwd.join("src").join(name))
            );
            process::exit(1);
        }
        Err(Obstacle::NoName) => {
            eprintln!(
                "error: silt init: cannot derive a package name from the current directory `{}`",
                crate::source::shown_path(&cwd)
            );
            eprintln!(
                "                  the directory needs a name that contains at least one lowercase letter,"
            );
            eprintln!(
                "                  digit, or underscore (or rename it to a valid silt identifier)."
            );
            process::exit(1);
        }
        // A package name that collides with a builtin module (`io`,
        // `string`, ...) or is a keyword: the manifest's rule, shared by
        // init, add and Manifest::load. Previously `mkdir io && cd io &&
        // silt init` silently wrote `name = "io"`, a package whose name
        // shadowed the stdlib `io` module.
        Err(Obstacle::Refused(msg)) => {
            eprintln!("error: silt init: {msg}");
            eprintln!(
                "                  (the package name was derived from the directory name `{}`;",
                crate::source::shown_path(dirname)
            );
            eprintln!(
                "                   rename the directory or run `silt init` from a directory whose name"
            );
            eprintln!("                   is not a keyword or a stdlib module).");
            process::exit(1);
        }
    };

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
            crate::source::shown_path(entry_path.parent().unwrap())
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
            eprintln!("error writing {}: {e}", crate::source::shown_path(path));
            for (earlier, _) in &written[..i] {
                let _ = fs::remove_file(earlier);
            }
            process::exit(1);
        }
    }

    println!("created silt package `{package_name}`:");
    println!("  {}", crate::source::shown_path(&manifest_path));
    println!("  {}", crate::source::shown_path(&entry_path));
    println!("  {}", crate::source::shown_path(&test_path));
    println!();
    match lib {
        true => println!("  check: silt check"),
        false => println!("  run:   silt run"),
    }
    println!("  test:  silt test");
}
