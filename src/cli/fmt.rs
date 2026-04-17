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
        let mut any_unformatted = false;
        for file in &files {
            if !check_format(file) {
                any_unformatted = true;
            }
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

/// Check if a file is already formatted. Returns true if it is, false otherwise.
/// Prints a message for files that would be changed.
fn check_format(path: &str) -> bool {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return false;
        }
    };
    match silt::formatter::format(&source) {
        Ok(formatted) => {
            if source == formatted {
                true
            } else {
                eprintln!("{path}: not formatted");
                false
            }
        }
        Err(e) => {
            eprintln!("{}", render_fmt_error(&e, &source, path));
            false
        }
    }
}
