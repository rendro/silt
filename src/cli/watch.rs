//! Watch-mode interceptor: when `--watch` (or `-w`) is present on the
//! command line, strip the flag and hand off to `silt::watch` after
//! doing a dry-validation pass so we don't enter the watch loop on
//! inputs the underlying subcommand would refuse up front.

#[cfg(feature = "watch")]
use std::env;
#[cfg(feature = "watch")]
use std::path::Path;
#[cfg(feature = "watch")]
use std::process;

#[cfg(feature = "watch")]
use crate::cli::help::{check_usage_banner, disasm_usage_banner, run_usage_banner};
#[cfg(feature = "watch")]
use crate::cli::package::find_project_root;

/// If `--watch` / `-w` is present in `args`, handle the watch loop and
/// return `true`. Return `false` to let the caller proceed with normal
/// dispatch.
///
/// On builds without the `watch` feature, the flag is rejected with a
/// fixed message and the process exits.
pub(crate) fn maybe_handle_watch(args: &[String]) -> bool {
    #[cfg(feature = "watch")]
    {
        if args.iter().any(|a| a == "--watch" || a == "-w") {
            handle_watch(args);
            return true;
        }
    }

    #[cfg(not(feature = "watch"))]
    {
        if args.iter().any(|a| a == "--watch" || a == "-w") {
            eprintln!(
                "The 'watch' feature is not enabled. Rebuild with: cargo build --features watch"
            );
            std::process::exit(1);
        }
    }

    false
}

#[cfg(feature = "watch")]
fn handle_watch(args: &[String]) {
    let filtered: Vec<String> = args[1..]
        .iter()
        .filter(|a| *a != "--watch" && *a != "-w")
        .cloned()
        .collect();

    // BEFORE entering the watcher, dry-validate the underlying subcommand
    // so we don't spawn a watcher for a command that's going to fail
    // immediately on every rerun. Two failure modes we catch up front:
    //
    //   1. `--help` / `-h` combined with `--watch` — the user wants
    //      help, not a watcher. Run the subcommand once (which will
    //      print help and exit 0) and return without watching.
    //
    //   2. A subcommand that requires a positional file arg is missing
    //      one — print usage to stderr and exit 1 WITHOUT entering the
    //      watch loop (which would otherwise hang silently forever,
    //      because the initial rerun prints a 1-line usage banner and
    //      the loop just sits there waiting for saves).
    //
    // Subcommands that take no file (repl, init, lsp, fmt with
    // implicit recursion, etc.) are passed through untouched.
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
    if let Some(sub) = filtered.first().map(|s| s.as_str()) {
        let requires_file = matches!(sub, "run" | "check" | "disasm");
        // `silt test` and `silt fmt` take an optional file / path, so
        // they're NOT in the list above — `silt test --watch` alone is
        // legitimate and means "watch the cwd and rerun auto-discovered
        // tests".
        if requires_file {
            // Find the first positional (non-flag) arg after the
            // subcommand name. Flags like `--format json` consume a
            // value; the simple scan below is good enough because
            // our value-taking flags all start with `--`.
            let mut has_positional = false;
            let mut i = 1;
            while i < filtered.len() {
                let a = filtered[i].as_str();
                if a == "--format" {
                    // Skip the flag and its value (if present).
                    i += 2;
                    continue;
                }
                if a.starts_with('-') {
                    i += 1;
                    continue;
                }
                has_positional = true;
                break;
            }
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
    }

    let watch_dir = filtered
        .iter()
        .filter_map(|a| {
            let path = Path::new(a.as_str());
            if a.ends_with(".silt") {
                let parent = path.parent().unwrap_or(Path::new("."));
                Some(if parent.as_os_str().is_empty() {
                    Path::new(".").to_path_buf()
                } else {
                    parent.to_path_buf()
                })
            } else if path.is_dir() {
                Some(path.to_path_buf())
            } else {
                None
            }
        })
        .next()
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| ".".into()));

    silt::watch::watch_and_rerun(&watch_dir, &filtered);
}
