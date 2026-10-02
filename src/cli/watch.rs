//! Watch-mode interceptor: when `--watch` (or `-w`) is present on the
//! command line, strip the flag and hand off to `silt::watch` after
//! doing a dry-validation pass so we don't enter the watch loop on
//! inputs the underlying subcommand would refuse up front. The watcher
//! is given the entry files of the programs the subcommand runs; it
//! watches the files of their analysis.

#[cfg(feature = "watch")]
use std::env;
#[cfg(feature = "watch")]
use std::path::{Path, PathBuf};
#[cfg(feature = "watch")]
use std::process;

#[cfg(feature = "watch")]
use crate::cli::help::{check_usage_banner, disasm_usage_banner, run_usage_banner};
#[cfg(feature = "watch")]
use crate::cli::package::find_project_root;
#[cfg(feature = "watch")]
use crate::cli::paths::find_silt_files;

/// If `--watch` / `-w` is present in `args`, handle the watch loop and
/// return `true`. Return `false` to let the caller proceed with normal
/// dispatch.
///
/// On builds without the `watch` feature, the flag is rejected with a
/// fixed message and the process exits.
pub(crate) fn maybe_handle_watch(args: &[String]) -> bool {
    #[cfg(feature = "watch")]
    {
        if has_watch_flag_before_separator(args) {
            handle_watch(args);
            return true;
        }
    }

    #[cfg(not(feature = "watch"))]
    {
        if has_watch_flag_before_separator(args) {
            eprintln!(
                "The 'watch' feature is not enabled. Rebuild with: cargo build --features watch"
            );
            std::process::exit(1);
        }
    }

    false
}

/// Index (into `args`) of the first standalone `--` token in `args[1..]`,
/// or `args.len()` when there is none. The `--` separator marks the end of
/// silt's own CLI flags: everything after it is verbatim program args (see
/// `cli::run::dispatch` and `cli::check::dispatch`). We skip `args[0]` (the
/// binary path) so a literal `--` program name can't be misread — argv[0]
/// is never a separator.
fn separator_index(args: &[String]) -> usize {
    args.iter()
        .enumerate()
        .skip(1)
        .find(|(_, a)| a.as_str() == "--")
        .map(|(i, _)| i)
        .unwrap_or(args.len())
}

/// True iff `--watch` / `-w` appears as a silt CLI flag — i.e. BEFORE the
/// first standalone `--` separator. A `-w` (or even `--watch`) that the user
/// passes as a program argument (`silt run prog.silt -- -w`) lives after the
/// separator and must NOT trigger watch mode; it is forwarded to the program
/// via `io.args()` instead. Scanning the entire arg vector (the pre-fix
/// behavior) hijacked such program args and forced watch mode.
fn has_watch_flag_before_separator(args: &[String]) -> bool {
    let sep = separator_index(args);
    args[..sep].iter().any(|a| a == "--watch" || a == "-w")
}

#[cfg(feature = "watch")]
fn handle_watch(args: &[String]) {
    // Strip `--watch` / `-w` ONLY from the CLI-flag region (before the
    // first standalone `--`). Everything from the separator onward is
    // forwarded verbatim into the re-invoked subprocess, so a `-w` /
    // `--watch` the user passes as a program arg (`silt run prog.silt
    // -- -w`) survives to `io.args()` instead of being silently dropped.
    // `separator_index` works on the full `args` (it skips `args[0]`); the
    // post-`--` tail starts at `sep` and is spliced through unchanged.
    let sep = separator_index(args);
    let mut filtered: Vec<String> = args[1..sep]
        .iter()
        .filter(|a| *a != "--watch" && *a != "-w")
        .cloned()
        .collect();
    filtered.extend(args[sep..].iter().cloned());

    // BEFORE entering the watcher, dry-validate the underlying subcommand
    // so we don't spawn a watcher for a command that's going to fail
    // immediately on every rerun. Three failure modes we catch up front:
    //
    //   1. `--help` / `-h` combined with `--watch` — the user wants
    //      help, not a watcher. Run the subcommand once (which will
    //      print help and exit 0) and return without watching.
    //
    //   2. The subcommand isn't runnable in a watch context (no
    //      subcommand at all; or `repl`, `init`, `lsp`, `fmt`,
    //      `update`, `add`, `self-update`; or an unknown subcommand).
    //      Re-running these on each save has no meaningful semantics
    //      — they're either interactive (`repl`, `lsp`), one-shot
    //      mutators (`init`, `add`, `update`, `self-update`), or
    //      would create feedback loops (`fmt` rewrites in place).
    //      Reject with a clear error and exit 1 — NOT enter the loop.
    //
    //   3. A runnable subcommand that requires a positional file arg
    //      is missing one outside a silt package — print usage and
    //      exit 1 WITHOUT entering the watch loop (which would
    //      otherwise hang silently forever, because the initial rerun
    //      prints a 1-line usage banner and the loop just sits there
    //      waiting for saves).
    let wants_help = filtered.iter().any(|a| a == "--help" || a == "-h");
    if wants_help {
        // Run the subcommand once so its own help handler fires, then
        // return without entering the watch loop.
        let exe = std::env::current_exe().unwrap_or_else(|e| {
            eprintln!("error: failed to get executable path: {e}");
            process::exit(1);
        });
        let status = std::process::Command::new(&exe).args(&filtered).status();
        match status {
            Ok(s) => process::exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("error: failed to invoke subcommand for --help: {e}");
                process::exit(1);
            }
        }
    }

    // Gate non-runnable subcommands BEFORE entering the watch loop.
    // Only `run`, `check`, `disasm`, and `test` have meaningful
    // re-execute-on-save semantics. Anything else (no subcommand,
    // `repl`, `init`, `lsp`, `fmt`, `update`, `add`, `self-update`,
    // or an unknown subcommand) entered the loop pre-fix and silently
    // sat there waiting for file changes — exactly the failure mode
    // the doc-comment above warns about.
    //
    // We reject the empty-argv case and any subcommand outside the
    // runnable allowlist with a fixed error and exit 1, so users get
    // a clear pointer to the supported subcommands instead of a
    // hanging process.
    const RUNNABLE: &[&str] = &["run", "check", "disasm", "test"];
    let sub = filtered.first().map(|s| s.as_str());
    let runnable = sub.map(|s| RUNNABLE.contains(&s)).unwrap_or(false);
    if !runnable {
        match sub {
            None => {
                eprintln!(
                    "error: --watch requires a runnable subcommand (run, check, test, disasm)"
                );
            }
            Some(s) => {
                eprintln!(
                    "error: silt {s} does not support --watch (only run, check, test, disasm do)"
                );
            }
        }
        process::exit(1);
    }

    // Detect subcommands that require a positional file argument and
    // bail out up front if it's missing. We only gate on the common
    // case (first positional after the subcommand name is missing or
    // is another flag); the subcommand's own validator handles the
    // harder cases after the watcher reruns.
    //
    // Exception: `run`, `check`, and `disasm` no longer require an
    // explicit file when the cwd is inside a silt package (manifest
    // discoverable). In that case the subcommand resolves the entry
    // point to `<root>/src/main.silt`, so we let the watcher start.
    let sub = sub.expect("runnable check above guarantees Some");
    let requires_file = matches!(sub, "run" | "check" | "disasm");
    // `silt test` takes an optional file / path, so it's NOT in
    // the list above — `silt test --watch` alone is legitimate
    // and means "watch the cwd and rerun auto-discovered tests".
    if requires_file {
        let has_positional = first_positional(&filtered).is_some();
        if !has_positional {
            // No positional path — only allowed if we're inside a
            // silt package (manifest reachable from cwd).
            let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
            let in_package = matches!(find_project_root(&cwd), Ok(Some(_)));
            if !in_package {
                let banner = match sub {
                    "run" => format!("Usage: {}", run_usage_banner()),
                    // Keep in sync with check_usage_banner().
                    "check" => {
                        format!("Usage: {}", check_usage_banner())
                    }
                    "disasm" => format!("Usage: {}", disasm_usage_banner()),
                    _ => unreachable!(),
                };
                eprintln!("{banner}");
                process::exit(1);
            }
        }
    }

    let sub = sub.to_string();
    let positional = first_positional(&filtered);
    silt::watch::watch_and_rerun(|| entries(&sub, positional.as_deref()), &filtered);
}

/// The first positional argument of the subcommand in `args` (`args[0]`
/// is the subcommand): its file or path. `--format` takes a value;
/// everything from a `--` on is the program's.
#[cfg(feature = "watch")]
fn first_positional(args: &[String]) -> Option<String> {
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return None;
        }
        if a == "--format" || a == "--filter" {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(a.to_string());
    }
    None
}

/// The entry files of the programs `silt <sub> <positional>` runs: the
/// file given; for `test`, the test files of the directory given or of
/// the working directory; otherwise the package's `src/main.silt` (for
/// `check`, its `src/lib.silt` when it has no main).
#[cfg(feature = "watch")]
fn entries(sub: &str, positional: Option<&str>) -> Vec<PathBuf> {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    if sub == "test" {
        let dir = match positional {
            Some(path) if !Path::new(path).is_dir() => return vec![PathBuf::from(path)],
            Some(path) => PathBuf::from(path),
            None => cwd,
        };
        return find_silt_files(&dir)
            .into_iter()
            .filter(|name| name.ends_with("_test.silt") || name.ends_with(".test.silt"))
            .map(PathBuf::from)
            .collect();
    }
    if let Some(path) = positional {
        return vec![PathBuf::from(path)];
    }
    let Ok(Some((root, _))) = find_project_root(&cwd) else {
        return Vec::new();
    };
    let main = root.join("src").join("main.silt");
    let lib = root.join("src").join("lib.silt");
    if sub == "check" && !main.is_file() && lib.is_file() {
        vec![lib]
    } else {
        vec![main]
    }
}
