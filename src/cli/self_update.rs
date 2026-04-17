//! `silt self-update` — update the silt binary from the latest release.

use std::process;

/// Dispatch `silt self-update [--dry-run] [--force]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut dry_run = false;
    let mut force = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("Usage: silt self-update [--dry-run] [--force]");
                println!();
                println!("Download the latest release binary and replace this one.");
                println!();
                println!("Options:");
                println!("  --dry-run    Show the latest version without downloading");
                println!("  --force      Reinstall even when already up to date");
                process::exit(0);
            }
            "--dry-run" => dry_run = true,
            "--force" => force = true,
            other => {
                let suggestion = match other {
                    "--dryrun" | "-n" => " (did you mean --dry-run?)",
                    "-f" | "--reinstall" => " (did you mean --force?)",
                    "--h" | "-help" => " (did you mean --help?)",
                    _ => "",
                };
                eprintln!("silt self-update: unknown flag '{other}'{suggestion}");
                eprintln!("Run 'silt self-update --help' for usage.");
                process::exit(1);
            }
        }
    }
    if let Err(e) = silt::update::run_update(silt::update::UpdateOptions { dry_run, force }) {
        eprintln!("  error: {e}");
        process::exit(1);
    }
}
