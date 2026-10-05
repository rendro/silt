//! `silt fmt [--check] [files...]` — format silt source. With no
//! files, recursively formats every `.silt` under cwd provided we're
//! inside a silt package (or the user passed an explicit `.`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use silt::source::{SourceMap, SourceName};

use crate::cli::package::{die_on_manifest_error, find_project_root};
use crate::cli::paths::find_silt_files;

/// Dispatch `silt fmt [--check] [files...]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut check_mode = false;
    let mut tamper: Option<(String, String)> = None;
    let mut files: Vec<String> = Vec::new();
    for arg in &args[2..] {
        if arg == "--check" {
            check_mode = true;
        } else if let Some(swap) = arg.strip_prefix("--test-tamper=") {
            // Not in the help: for the test of how a refusal is shown.
            // `FROM=>TO` replaces the last FROM by TO in the printer's
            // result before the formatter checks it, as a defect of the
            // printer would.
            let Some((from, to)) = swap.split_once("=>") else {
                eprintln!("silt fmt: --test-tamper takes FROM=>TO");
                process::exit(1);
            };
            tamper = Some((from.to_string(), to.to_string()));
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: silt fmt [--check] [files-or-dirs...]");
            println!();
            println!("Formats silt source files in place. Each positional may be a");
            println!("`.silt` file or a directory; directories are expanded recursively");
            println!("to every `.silt` descendant. Pass `.` as the sentinel to recursively");
            println!("format the current directory, or run with no positionals inside a");
            println!("silt package to recursively format every `.silt` under cwd.");
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
    // Expand any explicit directory positionals into their `.silt`
    // descendants. Pre-fix `silt fmt src/` ran straight to
    // `format_file`, which `read_to_string`'d the directory and
    // surfaced the raw `Is a directory (os error 21)` message. After
    // fix, fmt mirrors `silt test <dir>` and recurses with
    // `find_silt_files`. We do this *after* the explicit-dot strip so
    // `.` keeps its existing implicit-recursive sentinel meaning.
    {
        let mut expanded: Vec<String> = Vec::with_capacity(files.len());
        for f in files.drain(..) {
            let p = Path::new(&f);
            if p.is_dir() {
                let found = find_silt_files(p);
                if found.is_empty() {
                    eprintln!("silt fmt: no .silt files found in {f}");
                    process::exit(1);
                }
                expanded.extend(found);
            } else {
                expanded.push(f);
            }
        }
        files = expanded;
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
        if check_mode {
            eprintln!(
                "silt fmt: no files specified; recursively checking all .silt files under {}",
                cwd.display()
            );
        } else {
            eprintln!(
                "silt fmt: no files specified; recursively formatting all .silt files under {}",
                cwd.display()
            );
        }
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
        //   2 — at least one file failed to read or parse, or the
        //       formatter refused its own result for it (infra failure);
        //       CI should distinguish this from "diff would be produced".
        // An infra failure on any file is the dominant outcome — if we
        // hit a parse error we can't *know* whether the file is
        // formatted, so we must not collapse that into exit 1.
        let mut any_unformatted = false;
        let mut any_infra_error = false;
        for file in &files {
            match check_format(file, tamper.as_ref()) {
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
            if let Err(e) = format_file(file, tamper.as_ref()) {
                eprintln!("{e}");
                any_failed = true;
            }
        }
        if any_failed {
            process::exit(1);
        }
    }
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
        Some((from, to)) => {
            let (from, to) = (from.clone(), to.clone());
            silt::format::format_with(file, source, move |mut text| {
                if let Some(at) = text.rfind(&from) {
                    text.replace_range(at..at + from.len(), &to);
                }
                text
            })
        }
    };
    result.map_err(|diagnostic| silt::diagnostic::render_human(&sources, &diagnostic))
}

fn format_file(path: &str, tamper: Option<&Tamper>) -> Result<(), String> {
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
        return Ok(());
    }
    fs::write(path, formatted).map_err(|e| format!("error writing {path}: {e}"))?;
    Ok(())
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
