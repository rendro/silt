//! `silt check [--format json] [--deny-warnings] <file>...` — analyse
//! and compile programs without executing them, reporting diagnostics.
//! `-` reads the program from stdin.

use std::collections::HashSet;
use std::path::Path;
use std::process;

use silt::diagnostic::{Diagnostic, SourceView, render_human};
use silt::session::{Entry, LockPolicy, Session};

use crate::cli::help::check_usage_banner;
use crate::cli::package::{EntryPointKind, resolve_package_entry_point_in};
use crate::cli::paths::{
    CanonicalFiles, FILE_OR_DIRECTORY, PathArg, ProgramFiles, STDIN_NAME, door_diagnostics,
    find_silt_files, open_entry, open_stdin, package_test_files, path_arg, refuse_empty_walk,
    refuse_path, unicode_path,
};

/// Output format for `silt check` — human-readable by default, or
/// machine-readable JSON when `--format json` is passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

/// Dispatch `silt check [--format json] [--deny-warnings] <file>...`.
pub(crate) fn dispatch(args: &[String]) {
    let mut files: Vec<String> = Vec::new();
    let mut format = OutputFormat::Human;
    let mut deny_warnings = false;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--" {
            // `silt check` runs no program, so it takes no program
            // arguments: `--` is an argument too many.
            eprintln!("error: silt check: unexpected extra argument '--'");
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        } else if args[i] == "--format" || args[i].starts_with("--format=") {
            if format == OutputFormat::Json {
                crate::cli::once_only("check", "--format");
            }
            if let Some(value) = args[i].strip_prefix("--format=") {
                // GNU-style `--format=json` form, to match `silt add
                // --path=...` and every other subcommand that accepts an
                // `=`-joined value.
                if value == "json" {
                    format = OutputFormat::Json;
                    i += 1;
                } else {
                    eprintln!("error: silt check: --format requires 'json'");
                    process::exit(1);
                }
            } else if i + 1 < args.len() && args[i + 1] == "json" {
                format = OutputFormat::Json;
                i += 2;
            } else {
                eprintln!("error: silt check: --format requires 'json'");
                process::exit(1);
            }
        } else if args[i] == "--deny-warnings" {
            if deny_warnings {
                crate::cli::once_only("check", "--deny-warnings");
            }
            deny_warnings = true;
            i += 1;
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", check_usage_banner());
            println!();
            println!("Options:");
            println!("  --format json       Emit diagnostics as JSON");
            println!("  --deny-warnings     Exit with 1 when there is a warning");
            println!("  --watch, -w         Re-run on file changes");
            println!();
            println!("Each file is checked as a program of its own; '-' reads the");
            println!("program from stdin. A package's directory is checked as a package:");
            println!("every module of it, its test files, and each other .silt file under");
            println!("src/ as a program of its own. Another directory stands for the .silt");
            println!("files under it. With no path, inside a silt package, the package is");
            println!("checked.");
            process::exit(0);
        } else if args[i] == "-" {
            files.push(args[i].clone());
            i += 1;
        } else if args[i].starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match args[i].as_str() {
                "--formats" | "-format" | "-f" => " (did you mean --format?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!(
                "error: silt check: unknown flag '{}'{}",
                args[i], suggestion
            );
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        } else {
            files.push(args[i].clone());
            i += 1;
        }
    }
    // Stdin is read once, and is the program: no file goes with it.
    if files.len() > 1 && files.iter().any(|f| f == "-") {
        eprintln!("error: silt check: '-' reads the program from stdin, and takes no other file");
        eprintln!("Run 'silt check --help' for usage.");
        process::exit(1);
    }
    if format == OutputFormat::Json {
        crate::cli::package::print_manifest_errors_as_json();
    }
    let programs: Vec<PackageFiles> = match files.is_empty() {
        false => programs_of(&files),
        true => match package_files() {
            Ok(Some(files)) => vec![files],
            Ok(None) => {
                crate::cli::no_file("check");
                eprintln!("Usage: {}", check_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    check_files(&programs, format, deny_warnings);
}

/// The programs `silt check <paths>` checks: a file as a program of its
/// own, the package of a package's directory, and each `.silt` file
/// under another directory (as `silt fmt` finds them) as a program of
/// its own. A path that names one of them again is left out; a path that
/// is none of them is a usage error (exit 1).
fn programs_of(paths: &[String]) -> Vec<PackageFiles> {
    let file = |entry: String| PackageFiles {
        entry,
        modules: Vec::new(),
        tests: Vec::new(),
    };
    let mut programs = Vec::new();
    for arg in paths {
        if arg == "-" {
            programs.push(file(arg.clone()));
            continue;
        }
        match path_arg(arg) {
            PathArg::File(path) => programs.push(file(path)),
            PathArg::Package(dir) => match package_files_in(Path::new(&dir)) {
                Ok(Some(files)) => programs.push(files),
                Ok(None) => {}
                Err(()) => process::exit(1),
            },
            PathArg::Dir(dir) => {
                let found = find_silt_files(Path::new(&dir));
                if found.is_empty() {
                    refuse_empty_walk("check", Path::new(&dir), &dir);
                }
                programs.extend(found.into_iter().map(file));
            }
            PathArg::NotSilt(path) => refuse_path("check", &path, FILE_OR_DIRECTORY),
        }
    }
    // One file given twice (two spellings of its path, a link and what
    // it leads to) is checked once, and so is one package.
    let mut seen = HashSet::new();
    programs.retain(|program| {
        let package = !program.modules.is_empty() || !program.tests.is_empty();
        program.entry == "-"
            || seen.insert((
                package,
                silt::source::canonical_path(Path::new(&program.entry)),
            ))
    });
    programs
}

/// The files of a program that `silt check` checks: a file named on the
/// command line, or a package.
pub(crate) struct PackageFiles {
    /// `src/main.silt`, or `src/lib.silt` in a library; the file named,
    /// or `-` for stdin.
    pub(crate) entry: String,
    /// Every other `.silt` file of `src/` but for the test files,
    /// imported or not: the modules (the files directly in `src/`), then
    /// the files in its sub-directories, which are no modules and are
    /// each checked as a program of their own, as `silt check src` does.
    pub(crate) modules: Vec<String>,
    /// The test files of the package.
    pub(crate) tests: Vec<String>,
}

/// The files of the package of the working directory, so that one
/// command sees every error and warning of the package. `Ok(None)`
/// outside a package; `Err` when the package has no entry file (said).
/// A library (`src/lib.silt` with no `src/main.silt`) is checked too:
/// that is the shape of a dependency.
pub(crate) fn package_files() -> Result<Option<PackageFiles>, ()> {
    package_files_in(&std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

/// [`package_files`] for the package that the directory `dir` is in.
pub(crate) fn package_files_in(dir: &Path) -> Result<Option<PackageFiles>, ()> {
    let Some(entry) = resolve_package_entry_point_in(dir, EntryPointKind::AllowLib)? else {
        return Ok(None);
    };
    let root = entry
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."));
    let tests = package_test_files(root);
    let modules = package_modules(root, &entry, &tests);
    Ok(Some(PackageFiles {
        entry: entry.to_string_lossy().into_owned(),
        modules,
        tests,
    }))
}

/// The files a session has read, by their canonical paths. The session
/// only adds files, so each one is made canonical once, when it is first
/// seen: asking about every module of a large package costs one call per
/// file, not one per pair of a module and a file read.
#[derive(Default)]
struct Loaded {
    /// How many of the session's files are in `paths`.
    seen: usize,
    paths: HashSet<std::path::PathBuf>,
}

impl Loaded {
    /// Whether the session has read the file at `path` (as an import of
    /// a file checked before).
    fn has(&mut self, session: &Session, path: &Path) -> bool {
        let names = session.sources().files().map(|file| &file.path);
        self.has_in(names, path, &mut silt::source::canonical_path)
    }

    /// [`Loaded::has`] of the session whose files are named `names`, in
    /// the order read, with `canonical` making a path canonical.
    fn has_in<'a>(
        &mut self,
        names: impl Iterator<Item = &'a silt::source::SourceName>,
        path: &Path,
        canonical: &mut impl FnMut(&Path) -> std::path::PathBuf,
    ) -> bool {
        for name in names.skip(self.seen) {
            self.seen += 1;
            if let silt::source::SourceName::Path(p) = name {
                self.paths.insert(canonical(p));
            }
        }
        self.paths.contains(&canonical(path))
    }
}

/// The files of `src/` of the package at `root` that the package check
/// checks besides its entry file `entry` and its test files `tests`
/// (checked as tests): every `.silt` file directly in `src/` (a module),
/// sorted, then every `.silt` file in a sub-directory of `src/`, sorted,
/// as `silt check src` finds them (not those of a package kept inside).
/// Such a file is no module, and no import reaches it: it is checked as
/// a program of its own, its imports the package's modules.
fn package_modules(root: &Path, entry: &Path, tests: &[String]) -> Vec<String> {
    let src = root.join("src");
    let Ok(dir) = std::fs::read_dir(&src) else {
        return Vec::new();
    };
    let mut files: Vec<String> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "silt") && p.is_file())
        .map(unicode_path)
        .collect();
    files.sort();
    let below = find_silt_files(&src).into_iter().filter(|file| {
        let path = Path::new(file);
        path.parent().is_some_and(|dir| dir != src)
    });
    files.extend(below);
    without_entry_and_tests(files, entry, tests, silt::source::canonical_path)
}

/// `files`, in their order, but for the entry file `entry` and the test
/// files `tests`, told apart by their canonical paths (`canonical`). Each
/// path is made canonical once: the entry and the tests into a set, then
/// one look-up for each file, so a package with many files and many tests
/// costs one call per file, not one per pair of them.
fn without_entry_and_tests(
    files: Vec<String>,
    entry: &Path,
    tests: &[String],
    mut canonical: impl FnMut(&Path) -> std::path::PathBuf,
) -> Vec<String> {
    let mut apart: HashSet<std::path::PathBuf> = tests
        .iter()
        .map(|test| canonical(Path::new(test)))
        .collect();
    apart.insert(canonical(entry));
    files
        .into_iter()
        .filter(|file| !apart.contains(&canonical(Path::new(file))))
        .collect()
}

/// What checking a program found.
pub(crate) struct Checked {
    /// The session it was checked in: its sources name the files.
    pub(crate) session: Session,
    /// The files that could not be read, each with why.
    pub(crate) unreadable: Vec<(String, std::io::Error)>,
    /// What was found, each diagnostic once, in the order found.
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Checked {
    /// Whether a file could not be read or an error was found.
    pub(crate) fn failed(&self) -> bool {
        !self.unreadable.is_empty() || self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Check the entry file `path` (stdin for `-`), the test files `tests`
/// and the modules `modules` of its package in one session. A module
/// that the entry or a test imports is checked there, as an import (its
/// errors show where it is imported); only the others are opened as files
/// of their own. `Err` when the entry file cannot be read.
pub(crate) fn check_package(
    path: &str,
    modules: &[String],
    tests: &[String],
) -> std::io::Result<Checked> {
    // `silt check` checks each file as a module: the session's analysis
    // and the compile step, as the language server does. A module needs
    // no `main`; `silt run` asks for one. A `main` the file binds must be
    // able to start, and so must its test functions in a test file.
    let (mut session, file) = match path {
        "-" => open_stdin(LockPolicy::Update)?,
        path => open_entry(path, LockPolicy::Update)?,
    };
    let mut unreadable = Vec::new();
    // Each once, in the order found (a set beside the list: a program
    // can have many thousands of warnings).
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    let mut check = |session: &mut Session, file| {
        let compiled = session.compile(file, Entry::Module);
        for d in door_diagnostics(session, file, &compiled) {
            if seen.insert(d.clone()) {
                diagnostics.push(d);
            }
        }
    };
    check(&mut session, file);
    for test in tests {
        match session.open(Path::new(test)) {
            Ok(file) => check(&mut session, file),
            Err(e) => unreadable.push((test.clone(), e)),
        }
    }
    let mut loaded = Loaded::default();
    for module in modules {
        if loaded.has(&session, Path::new(module)) {
            continue;
        }
        match session.open(Path::new(module)) {
            Ok(file) => check(&mut session, file),
            Err(e) => unreadable.push((module.clone(), e)),
        }
    }
    Ok(Checked {
        session,
        unreadable,
        diagnostics,
    })
}

/// Check each of `programs`, as a program of its own, and report what
/// was found: each diagnostic once, also where two programs import one
/// module; under `--format json` all of it as one JSON array.
fn check_files(programs: &[PackageFiles], format: OutputFormat, deny_warnings: bool) {
    let mut failed = false;
    let mut warnings = 0;
    let mut json: Vec<serde_json::Value> = Vec::new();
    // What was shown, as rendered with each file named by its canonical
    // path: a file reached by two spellings of its path (in two
    // programs) is one file.
    let mut shown: HashSet<String> = HashSet::new();
    let mut printed = false;
    for program in programs {
        let path = match program.entry.as_str() {
            "-" => STDIN_NAME,
            path => path,
        };
        let checked = check_package(&program.entry, &program.modules, &program.tests);
        let checked = match checked {
            Ok(checked) => checked,
            Err(e) => {
                failed = true;
                let unreadable = [(path.to_string(), e)];
                match format {
                    OutputFormat::Json => json.extend(json_errors(
                        &silt::source::SourceMap::new(),
                        &unreadable,
                        &[],
                    )),
                    OutputFormat::Human => report_unreadable(&unreadable),
                }
                continue;
            }
        };
        failed |= checked.failed();
        let files = ProgramFiles::new(path, checked.session.sources());
        // (One program's diagnostics are each once already: what two
        // programs both found is told apart by its text, rendered once
        // more, which a program with many thousands of warnings pays
        // for. A single program does not.)
        let diagnostics: Vec<Diagnostic> = (checked.diagnostics.iter())
            .filter(|d| {
                programs.len() == 1
                    || shown.insert(render_human(&CanonicalFiles(checked.session.sources()), d))
            })
            .cloned()
            .collect();
        warnings += diagnostics.iter().filter(|d| d.is_warning()).count();
        match format {
            OutputFormat::Json => {
                json.extend(json_errors(&files, &checked.unreadable, &diagnostics))
            }
            OutputFormat::Human => {
                report_unreadable(&checked.unreadable);
                // F14: separate diagnostics with blank lines, also those
                // of two programs.
                if printed && !diagnostics.is_empty() {
                    eprintln!();
                }
                printed |= !diagnostics.is_empty();
                silt::diagnostic::eprint_all(&files, &diagnostics);
            }
        }
    }
    if format == OutputFormat::Json {
        match serde_json::to_string(&json) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("internal error: failed to serialize diagnostics: {e}");
                process::exit(1);
            }
        }
    }

    if failed {
        process::exit(1);
    }
    if deny_warnings && warnings > 0 {
        let noun = if warnings == 1 { "warning" } else { "warnings" };
        eprintln!("error: silt check: {warnings} {noun}, denied by --deny-warnings");
        process::exit(1);
    }
}

/// The files that could not be read, each with why: "error reading
/// <file>: <why>" lines.
fn report_unreadable(unreadable: &[(String, std::io::Error)]) {
    for (path, e) in unreadable {
        eprintln!(
            "error reading {path}: {}",
            silt::diagnostic::io_error_text(e)
        );
    }
}

/// A file that could not be read as an entry of the JSON array: the
/// fields of a diagnostic (`diagnostic::render_json`), with the file as
/// its place and no position in it.
fn unreadable_json(path: &str, e: &std::io::Error) -> serde_json::Value {
    serde_json::json!({
        "code": silt::diagnostic::Code::ModuleNotFound.id(),
        "col": 0,
        "end_col": 0,
        "end_line": 0,
        "file": path,
        "help": [],
        "kind": "compile",
        "labels": [],
        "line": 0,
        "message": format!("error reading {path}: {}", silt::diagnostic::io_error_text(e)),
        "notes": [],
        "severity": "error",
    })
}

/// The files that could not be read, then `errors`, as entries of the
/// JSON array (see `diagnostic::render_json`). Nothing in it is colored:
/// the renderer writes no escape sequences.
fn json_errors(
    files: &dyn SourceView,
    unreadable: &[(String, std::io::Error)],
    errors: &[Diagnostic],
) -> Vec<serde_json::Value> {
    unreadable
        .iter()
        .map(|(path, e)| unreadable_json(path, e))
        .chain(
            errors
                .iter()
                .map(|d| silt::diagnostic::render_json(files, d)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use silt::source::SourceName;

    use super::{Loaded, without_entry_and_tests};

    /// The files a session has read are made canonical once each, as
    /// the session reads them: asking about 400 modules of a session
    /// that grows to 800 files makes 800 + 400 calls, not one per pair.
    #[test]
    fn a_file_read_is_made_canonical_once() {
        let mut names: Vec<SourceName> = Vec::new();
        let mut loaded = Loaded::default();
        let mut calls = 0;
        let mut canonical = |path: &Path| {
            calls += 1;
            path.to_path_buf()
        };
        let mut found = 0;
        for n in 0..400 {
            // Each module checked reads it and one import of its own.
            names.push(SourceName::Path(PathBuf::from(format!("src/d/i{n}.silt"))));
            let module = PathBuf::from(format!("src/d/m{n}.silt"));
            found += usize::from(loaded.has_in(names.iter(), &module, &mut canonical));
            names.push(SourceName::Path(module));
            let import = PathBuf::from(format!("src/d/i{n}.silt"));
            found += usize::from(loaded.has_in(names.iter(), &import, &mut canonical));
        }
        assert_eq!(
            found, 400,
            "each import is found, no module before it is read"
        );
        assert_eq!(calls, 800 + 800, "one call per file read and per question");
    }

    /// The entry and the test files are left out, the rest keep their
    /// order, and each path is made canonical once: as many calls as
    /// paths, however many files and tests there are (the walk was
    /// once one call per pair of a file and a test).
    #[test]
    fn each_path_is_made_canonical_once() {
        let files: Vec<String> = (0..400)
            .map(|n| format!("src/d{}/f{n}.silt", n % 20))
            .chain((0..400).map(|n| format!("src/d{}/f{n}_test.silt", n % 20)))
            .chain(["src/main.silt".to_string()])
            .collect();
        let tests: Vec<String> = (0..400)
            .map(|n| format!("./src/d{}/f{n}_test.silt", n % 20))
            .collect();
        let mut calls = 0;
        // (The canonical path of `./x` is `x` here.)
        let canonical = |path: &Path| {
            calls += 1;
            PathBuf::from(path.to_string_lossy().trim_start_matches("./"))
        };
        let kept = without_entry_and_tests(files, Path::new("./src/main.silt"), &tests, canonical);
        let expected: Vec<String> = (0..400)
            .map(|n| format!("src/d{}/f{n}.silt", n % 20))
            .collect();
        assert_eq!(kept, expected);
        assert_eq!(
            calls,
            801 + 400 + 1,
            "one call per file, test file and the entry"
        );
    }
}
