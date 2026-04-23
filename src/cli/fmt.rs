//! `silt fmt [--check] [files...]` — format silt source. With no
//! files, recursively formats every `.silt` under cwd provided we're
//! inside a silt package (or the user passed an explicit `.`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use silt::errors::SourceError;

use crate::cli::package::{die_on_manifest_error, find_project_root};
use crate::cli::paths::find_silt_files;

/// Dispatch `silt fmt [--check] [files...]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut check_mode = false;
    let mut files: Vec<String> = Vec::new();
    for arg in &args[2..] {
        if arg == "--check" {
            check_mode = true;
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: silt fmt [--check] [files...]");
            println!();
            println!("Options:");
            println!("  --check    Check formatting without modifying files");
            process::exit(0);
        } else if arg.starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match arg.as_str() {
                "--checks" | "--Check" | "-check" | "-c" => " (did you mean --check?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!("silt fmt: unknown flag '{arg}'{suggestion}");
            eprintln!("Run 'silt fmt --help' for usage.");
            process::exit(1);
        } else {
            files.push(arg.clone());
        }
    }
    // If no files given (or just an explicit `.`), find all .silt files
    // in the current directory recursively. This is risky if the user
    // happens to run `silt fmt` outside a project, so we require a
    // project anchor (silt.toml, .git) OR an explicit `.` argument,
    // and always emit a loud warning + file preview when the recursion
    // is triggered implicitly.
    let explicit_dot = files.iter().any(|f| f == "." || f == "./");
    if explicit_dot {
        // Strip the `.` marker; we'll treat it as the recursive sentinel.
        files.retain(|f| f != "." && f != "./");
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
            Err(e) => die_on_manifest_error(e),
        };
        files = find_silt_files(Path::new("."));
        if files.is_empty() {
            eprintln!("no .silt files found in current directory");
            process::exit(1);
        }
        if !has_anchor && !explicit_dot {
            eprintln!(
                "silt fmt: refusing to recursively format {} — no silt.toml found in this directory or any parent",
                cwd.display()
            );
            eprintln!("         pass an explicit `.` or file paths to format anyway.");
            process::exit(1);
        }
        eprintln!(
            "silt fmt: no files specified; recursively formatting all .silt files under {}",
            cwd.display()
        );
        let preview = files.iter().take(5).collect::<Vec<_>>();
        for f in &preview {
            eprintln!("  {f}");
        }
        if files.len() > preview.len() {
            eprintln!("  ... ({} more)", files.len() - preview.len());
        }
    }
    if check_mode {
        // Exit-code taxonomy for `silt fmt --check`:
        //   0 — every file is already formatted.
        //   1 — at least one file would be reformatted (the intended
        //       `--check` signal CI tooling keys off of).
        //   2 — at least one file failed to read or parse (infra failure);
        //       CI should distinguish this from "diff would be produced".
        // An infra failure on any file is the dominant outcome — if we
        // hit a parse error we can't *know* whether the file is
        // formatted, so we must not collapse that into exit 1.
        let mut any_unformatted = false;
        let mut any_infra_error = false;
        for file in &files {
            match check_format(file) {
                CheckOutcome::Formatted => {}
                CheckOutcome::Unformatted => any_unformatted = true,
                CheckOutcome::InfraError => any_infra_error = true,
            }
        }
        if any_infra_error {
            process::exit(2);
        }
        if any_unformatted {
            process::exit(1);
        }
    } else {
        let mut any_failed = false;
        for file in &files {
            if let Err(e) = format_file(file) {
                eprintln!("{e}");
                any_failed = true;
            }
        }
        if any_failed {
            process::exit(1);
        }
    }
}

fn format_file(path: &str) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|e| format!("error reading {path}: {e}"))?;
    let formatted =
        silt::formatter::format(&source).map_err(|e| render_fmt_error(&e, &source, path))?;
    fs::write(path, formatted).map_err(|e| format!("error writing {path}: {e}"))?;
    Ok(())
}

/// Render a formatter lex/parse failure as a structured `SourceError` with
/// the source-line snippet and caret. Without this, `silt fmt` would
/// surface the bare `ParseError::Display` string (just `[line:col] msg`)
/// and users would lose the context they get from `silt run` /
/// `silt check` on the same file.
fn render_fmt_error(err: &silt::formatter::FmtError, source: &str, path: &str) -> String {
    match err {
        silt::formatter::FmtError::Lex(e) => {
            format!("{}", SourceError::from_lex_error(e, source, path))
        }
        silt::formatter::FmtError::Parse(e) => {
            format!("{}", SourceError::from_parse_error(e, source, path))
        }
    }
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
    /// File could not be read, or the formatter rejected the source
    /// (lex/parse error). The check is inconclusive — we cannot assert
    /// the file is formatted, so this must not be mistaken for drift.
    InfraError,
}

/// Check if a file is already formatted. Prints a diagnostic on any
/// non-`Formatted` outcome (same stderr messages as before).
fn check_format(path: &str) -> CheckOutcome {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return CheckOutcome::InfraError;
        }
    };
    match silt::formatter::format(&source) {
        Ok(formatted) => {
            if source == formatted {
                CheckOutcome::Formatted
            } else {
                eprintln!("{path}: not formatted");
                CheckOutcome::Unformatted
            }
        }
        Err(e) => {
            eprintln!("{}", render_fmt_error(&e, &source, path));
            CheckOutcome::InfraError
        }
    }
}
