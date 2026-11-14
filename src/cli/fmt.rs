//! `silt fmt [--check] [files...]` — format silt source. With no
//! files, recursively formats every `.silt` under cwd provided we're
//! inside a silt package (or the user passed an explicit `.`). `-` reads
//! the program from stdin and writes the result to stdout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use silt::source::{SourceMap, SourceName};

use crate::cli::package::{die_on_manifest_error_with, find_project_root};
use crate::cli::paths::{
    FILE_OR_DIRECTORY, PathArg, find_silt_files, once_each, path_arg, refuse_empty_walk,
    refuse_path,
};

/// Dispatch `silt fmt [--check] [files...]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut check_mode = false;
    let mut tamper: Option<(String, String)> = None;
    let mut files: Vec<String> = Vec::new();
    for arg in &args[2..] {
        if arg == "--check" {
            if check_mode {
                crate::cli::once_only("fmt", "--check");
            }
            check_mode = true;
        } else if cfg!(debug_assertions) && arg.starts_with("--test-tamper=") {
            // Only in a debug build, and not in the help: for the test
            // of how a refusal is shown. `FROM=>TO` replaces the last
            // FROM by TO in the printer's result before the formatter
            // checks it, as a defect of the printer would.
            if tamper.is_some() {
                crate::cli::once_only("fmt", "--test-tamper");
            }
            let swap = &arg["--test-tamper=".len()..];
            let Some((from, to)) = swap.split_once("=>") else {
                eprintln!("error: silt fmt: --test-tamper takes FROM=>TO");
                process::exit(1);
            };
            tamper = Some((from.to_string(), to.to_string()));
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: silt fmt [--check] [files-or-dirs...]");
            println!("       silt fmt [--check] -");
            println!();
            println!("Formats silt source files in place. Each positional may be a");
            println!("`.silt` file or a directory; directories are expanded recursively");
            println!("to every `.silt` descendant. Pass `.` as the sentinel to recursively");
            println!("format the current directory, or run with no positionals inside a");
            println!("silt package to recursively format every `.silt` under cwd");
            println!("of this package, not of a package inside it.");
            println!();
            println!("`-` reads the program from stdin and writes the formatted program");
            println!("to stdout; with --check it writes nothing and only says whether it");
            println!("is formatted.");
            println!();
            println!("Options:");
            println!("  --check    Check formatting without modifying files");
            process::exit(0);
        } else if arg == "-" {
            files.push(arg.clone());
        } else if arg.starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match arg.as_str() {
                "--checks" | "--Check" | "-check" | "-c" => " (did you mean --check?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!("error: silt fmt: unknown flag '{arg}'{suggestion}");
            eprintln!("Run 'silt fmt --help' for usage.");
            process::exit(1);
        } else {
            files.push(arg.clone());
        }
    }
    if files.iter().any(|f| f == "-") {
        // Stdin is read once, and is the program: no file goes with it.
        if files.len() > 1 {
            eprintln!("error: silt fmt: '-' reads the program from stdin, and takes no other file");
            eprintln!("Run 'silt fmt --help' for usage.");
            process::exit(1);
        }
        format_stdin(check_mode, tamper.as_ref());
    }
    // With no files, or only an explicit `.`, every .silt file under
    // the current directory is formatted. This is risky if the user
    // happens to run `silt fmt` outside a project, so a bare `silt fmt`
    // needs a project anchor (silt.toml), and the recursion is always
    // announced. A `.` beside other positionals is a directory like any
    // other.
    let is_dot = |f: &String| f == "." || f == "./";
    let explicit_dot = files.iter().any(is_dot);
    if files.iter().all(is_dot) {
        files.clear();
    }
    // Each directory positional stands for its `.silt` descendants (as
    // for `silt test <dir>`); a file named twice, by two positionals or
    // two spellings of its path, is handled once. Files found in a
    // directory are not named by the user: each that is changed (or not
    // formatted) is named, and a count ends the output.
    let mut walked = false;
    {
        let mut expanded: Vec<String> = Vec::with_capacity(files.len());
        for f in files.drain(..) {
            match path_arg(&f) {
                PathArg::Package(dir) | PathArg::Dir(dir) => {
                    let found = find_silt_files(Path::new(&dir));
                    if found.is_empty() {
                        refuse_empty_walk("fmt", Path::new(&dir), &dir);
                    }
                    walked = true;
                    expanded.extend(found);
                }
                PathArg::File(file) => expanded.push(file),
                PathArg::NotSilt(file) => refuse_path("fmt", &file, FILE_OR_DIRECTORY),
            }
        }
        files = once_each(expanded);
    }
    let implicit_recursive = files.is_empty();
    if implicit_recursive {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Project boundary is now defined exclusively by `silt.toml`.
        // The previous heuristic accepted `.git` as well; that is gone
        // because v0.7 makes manifest discovery the canonical answer
        // to "am I inside a silt package?".
        let has_anchor = match find_project_root(&cwd) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => die_on_manifest_error_with(e, 2),
        };
        // (A file reached twice, through a link, is handled once.)
        files = once_each(find_silt_files(Path::new(".")));
        if files.is_empty() {
            refuse_empty_walk("fmt", Path::new("."), "current directory");
        }
        if !has_anchor && !explicit_dot {
            eprintln!(
                "error: silt fmt: refusing to recursively format {} — no silt.toml found in this directory or any parent",
                cwd.display()
            );
            eprintln!("                 pass an explicit `.` or file paths to format anyway.");
            process::exit(1);
        }
        let doing = if check_mode { "checking" } else { "formatting" };
        let unasked = if explicit_dot {
            ""
        } else {
            "no files specified; "
        };
        eprintln!(
            "silt fmt: {unasked}recursively {doing} all .silt files under {}",
            cwd.display()
        );
        walked = true;
    }
    if check_mode {
        // Exit-code taxonomy for `silt fmt --check`:
        //   0 — every file is already formatted.
        //   1 — at least one file would be reformatted (the intended
        //       `--check` signal CI tooling keys off of).
        //   2 — at least one file failed to read or parse, or the
        //       formatter refused its own result for it (infra failure);
        //       CI should distinguish this from "diff would be produced".
        //       A usage error (an unknown or repeated flag, nothing to
        //       format, the refusal to recurse outside a package) exits
        //       1, as for every command, before any file is looked at.
        // An infra failure on any file is the dominant outcome — if we
        // hit a parse error we can't *know* whether the file is
        // formatted, so we must not collapse that into exit 1.
        let (mut unformatted, mut infra_errors) = (0, 0);
        for file in &files {
            match check_format(file, tamper.as_ref()) {
                CheckOutcome::Formatted => {}
                CheckOutcome::Unformatted => unformatted += 1,
                CheckOutcome::InfraError => infra_errors += 1,
            }
        }
        if walked {
            eprintln!(
                "{}",
                count_line(files.len(), unformatted, "not formatted", infra_errors)
            );
        }
        if infra_errors > 0 {
            process::exit(2);
        }
        if unformatted > 0 {
            process::exit(1);
        }
    } else {
        let (mut changed, mut failed) = (0, 0);
        for file in &files {
            match format_file(file, tamper.as_ref()) {
                Ok(false) => {}
                Ok(true) => {
                    changed += 1;
                    if walked {
                        eprintln!("{file}: formatted");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    failed += 1;
                }
            }
        }
        if walked {
            eprintln!("{}", count_line(files.len(), changed, "changed", failed));
        }
        if failed > 0 {
            process::exit(2);
        }
    }
}

/// The last line of a `silt fmt` over the files of a directory: how many
/// files, how many of them are `what` ("changed", "not formatted"), and
/// how many could not be read or formatted, if any.
fn count_line(files: usize, done: usize, what: &str, failed: usize) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    let mut line = format!("{files} {noun}, {done} {what}");
    if failed > 0 {
        line += &format!(", {failed} failed");
    }
    line
}

type Tamper = (String, String);

/// Format `source`, the text of the file at `path`. An error is the
/// rendered diagnostic: the lexer's or the parser's, as `silt check`
/// shows it for the same file, or the formatter's refusal of its own
/// result.
fn format_source(source: &str, path: &str, tamper: Option<&Tamper>) -> Result<String, String> {
    // The formatter lexes the text as the only file of its own.
    let mut sources = SourceMap::new();
    let file = sources.add(SourceName::Path(path.into()), source.into());
    let result = match tamper {
        None => silt::format::format(file, source),
        #[cfg(debug_assertions)]
        Some((from, to)) => {
            let (from, to) = (from.clone(), to.clone());
            silt::format::format_with(file, source, move |mut text| {
                if let Some(at) = text.rfind(&from) {
                    text.replace_range(at..at + from.len(), &to);
                }
                text
            })
        }
        // The flag that sets it does not exist in a release build.
        #[cfg(not(debug_assertions))]
        Some(_) => silt::format::format(file, source),
    };
    result.map_err(|diagnostic| silt::diagnostic::render_human(&sources, &diagnostic))
}

/// `silt fmt -`: format the program read from stdin and write the result
/// to stdout; with `--check`, write nothing to stdout and say whether it
/// is formatted. Exits as for a file: 1 when `--check` finds it not
/// formatted, 2 when it cannot be read or formatted.
fn format_stdin(check_mode: bool, tamper: Option<&Tamper>) -> ! {
    let name = crate::cli::paths::STDIN_NAME;
    let source = match std::io::read_to_string(std::io::stdin()) {
        Ok(source) => source,
        Err(e) => {
            eprintln!(
                "error reading {name}: {}",
                silt::diagnostic::io_error_text(&e)
            );
            process::exit(2);
        }
    };
    let formatted = match format_source(&source, name, tamper) {
        Ok(formatted) => formatted,
        Err(e) => {
            eprintln!("{e}");
            process::exit(2);
        }
    };
    if check_mode {
        if formatted != source {
            eprintln!("{name}: not formatted");
            process::exit(1);
        }
    } else {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        if let Err(e) = out
            .write_all(formatted.as_bytes())
            .and_then(|()| out.flush())
        {
            eprintln!("error writing to stdout: {e}");
            process::exit(2);
        }
    }
    process::exit(0);
}

/// Format the file at `path` in place: whether it was changed.
fn format_file(path: &str, tamper: Option<&Tamper>) -> Result<bool, String> {
    let source = fs::read_to_string(path).map_err(|e| {
        format!(
            "error reading {path}: {}",
            silt::diagnostic::io_error_text(&e)
        )
    })?;
    let formatted = format_source(&source, path, tamper)?;
    // Skip the write when the file is already formatted. An
    // unconditional `fs::write` bumps the file's mtime even though the
    // bytes are identical, which spuriously retriggers `--watch` loops
    // and mtime-based build tools on every no-op format (e.g. editor
    // format-on-save alongside `silt run --watch`). rustfmt/gofmt skip
    // identical writes for exactly this reason.
    if formatted == source {
        return Ok(false);
    }
    replace_file(Path::new(path), &formatted).map_err(|e| {
        format!(
            "error writing {path}: {}",
            silt::diagnostic::io_error_text(&e)
        )
    })?;
    Ok(true)
}

/// Give the file at `path` the contents `text`, all of it or none: the
/// text goes to a new file beside it, which then takes its place. A
/// `silt fmt` that is interrupted leaves the source as it was, never
/// half of it.
fn replace_file(path: &Path, text: &str) -> std::io::Result<()> {
    // A link stays a link: the file it names is the one replaced.
    let target = fs::canonicalize(path)?;
    let permissions = fs::metadata(&target)?.permissions();
    // Taking a file's place needs no leave to write to it; ask for it
    // all the same.
    if permissions.readonly() {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(format!(".{}.fmt-tmp", process::id()));
    let staged = target.with_file_name(name);
    let written = fs::write(&staged, text)
        .and_then(|()| fs::set_permissions(&staged, permissions))
        .and_then(|()| fs::rename(&staged, &target));
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written
}

/// Three-way result for `silt fmt --check` on a single file. Previously
/// the checker returned `bool`, which collapsed "file needs formatting"
/// (the intended `--check` signal) with "couldn't read the file" and
/// "file didn't parse" (infra failures). CI callers had no way to
/// distinguish a format drift from a broken file — both manifested as
/// exit 1. The enum lets the dispatcher escalate infra failures to
/// exit 2 while keeping drift on exit 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckOutcome {
    /// File exists, parsed cleanly, and is already formatted.
    Formatted,
    /// File exists and parsed cleanly, but formatting would change it.
    Unformatted,
    /// File could not be read, the formatter rejected the source
    /// (lex/parse error), or the formatter refused its own result. The
    /// check is inconclusive — we cannot assert the file is formatted,
    /// so this must not be mistaken for drift.
    InfraError,
}

/// Check if a file is already formatted. Prints a diagnostic on any
/// non-`Formatted` outcome (same stderr messages as before).
fn check_format(path: &str, tamper: Option<&Tamper>) -> CheckOutcome {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error reading {path}: {}",
                silt::diagnostic::io_error_text(&e)
            );
            return CheckOutcome::InfraError;
        }
    };
    match format_source(&source, path, tamper) {
        Ok(formatted) => {
            if source == formatted {
                CheckOutcome::Formatted
            } else {
                eprintln!("{path}: not formatted");
                CheckOutcome::Unformatted
            }
        }
        Err(e) => {
            eprintln!("{e}");
            CheckOutcome::InfraError
        }
    }
}
