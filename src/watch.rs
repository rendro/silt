use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Returns true if any of the given paths has a `.silt` extension.
/// Extracted so the filtering logic can be unit-tested in isolation
/// from the `notify` event stream and the subprocess rerun loop.
fn any_silt_path_changed(paths: &[PathBuf]) -> bool {
    paths
        .iter()
        .any(|p| p.extension().is_some_and(|ext| ext == "silt"))
}

/// Returns true if enough time has elapsed since `last_run` to rerun
/// immediately under the given debounce window. Isolated so the
/// debounce decision can be unit-tested deterministically.
fn should_rerun_now(last_run: Instant, now: Instant, debounce: Duration) -> bool {
    now.duration_since(last_run) > debounce
}

pub fn watch_and_rerun(watch_dir: &Path, args: &[String]) {
    let (tx, rx) = mpsc::channel();

    // Creating the OS watcher can fail in restrictive environments: sandboxes
    // where inotify is disabled, read-only filesystems, containers that cap
    // file descriptors, or platforms where `notify`'s backend can't initialize.
    // Surface a helpful hint so users know they can fall back to a one-shot
    // compile instead of staring at a raw errno.
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .unwrap_or_else(|e| {
        eprintln!(
            "error: failed to start file watcher on {}: {e}. \
             Try running without --watch to compile once.",
            watch_dir.display()
        );
        std::process::exit(1);
    });

    watcher
        .watch(watch_dir, RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!(
                "error: failed to start file watcher on {}: {e}. \
                 Try running without --watch to compile once.",
                watch_dir.display()
            );
            std::process::exit(1);
        });

    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("error: failed to get executable path: {e}");
        std::process::exit(1);
    });

    // Initial run
    eprint!("\x1B[2J\x1B[H");
    let _ = std::process::Command::new(&exe).args(args).status();
    eprintln!("\n[watch] Watching for changes...");

    let debounce = Duration::from_millis(500);
    let mut last_run = Instant::now();
    let mut pending_rerun = false;

    loop {
        // If a save arrived during the debounce window, wait out the remainder
        // and then trigger a rerun so no save is dropped.
        let recv_result = if pending_rerun {
            let elapsed = last_run.elapsed();
            if elapsed >= debounce {
                // Window already expired — fire immediately without blocking.
                Ok(None)
            } else {
                match rx.recv_timeout(debounce - elapsed) {
                    Ok(ev) => Ok(Some(ev)),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
                }
            }
        } else {
            match rx.recv() {
                Ok(ev) => Ok(Some(ev)),
                Err(_) => Err(()),
            }
        };

        match recv_result {
            Err(()) => break,
            Ok(None) => {
                // Debounce window expired with a pending rerun queued.
                pending_rerun = false;
                // Let file writes settle, then drain any events that arrived
                // during the sleep.
                std::thread::sleep(Duration::from_millis(100));
                while rx.try_recv().is_ok() {}

                last_run = Instant::now();
                eprint!("\x1B[2J\x1B[H");
                let _ = std::process::Command::new(&exe).args(args).status();
                eprintln!("\n[watch] Watching for changes...");
            }
            Ok(Some(Ok(event))) => {
                if !any_silt_path_changed(&event.paths) {
                    continue;
                }

                if should_rerun_now(last_run, Instant::now(), debounce) {
                    // Outside the debounce window: rerun now.
                    pending_rerun = false;
                    // Let file writes settle, then drain pending events.
                    std::thread::sleep(Duration::from_millis(100));
                    while rx.try_recv().is_ok() {}

                    last_run = Instant::now();
                    eprint!("\x1B[2J\x1B[H");
                    let _ = std::process::Command::new(&exe).args(args).status();
                    eprintln!("\n[watch] Watching for changes...");
                } else {
                    // Inside the debounce window: mark a pending rerun. The
                    // next loop iteration will wait out the remainder of the
                    // window before firing.
                    pending_rerun = true;
                }
            }
            Ok(Some(Err(e))) => {
                eprintln!("watch error: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── any_silt_path_changed ─────────────────────────────────────

    #[test]
    fn silt_file_detected() {
        let paths = vec![PathBuf::from("/tmp/a.silt")];
        assert!(any_silt_path_changed(&paths));
    }

    #[test]
    fn non_silt_file_ignored() {
        let paths = vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.rs")];
        assert!(!any_silt_path_changed(&paths));
    }

    #[test]
    fn mixed_paths_detected() {
        // A batch containing at least one .silt file still triggers a rerun.
        let paths = vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/main.silt"),
            PathBuf::from("/tmp/b.rs"),
        ];
        assert!(any_silt_path_changed(&paths));
    }

    #[test]
    fn empty_paths_ignored() {
        let paths: Vec<PathBuf> = vec![];
        assert!(!any_silt_path_changed(&paths));
    }

    #[test]
    fn extensionless_path_ignored() {
        let paths = vec![PathBuf::from("/tmp/Makefile")];
        assert!(!any_silt_path_changed(&paths));
    }

    #[test]
    fn silt_substring_not_matched() {
        // A file named `silt.txt` is NOT a .silt file.
        let paths = vec![PathBuf::from("/tmp/silt.txt")];
        assert!(!any_silt_path_changed(&paths));
    }

    // ── should_rerun_now / debounce ───────────────────────────────

    #[test]
    fn debounce_elapsed_allows_rerun() {
        let debounce = Duration::from_millis(500);
        let last = Instant::now() - Duration::from_millis(600);
        let now = Instant::now();
        assert!(
            should_rerun_now(last, now, debounce),
            "after debounce elapses, a rerun should be allowed"
        );
    }

    #[test]
    fn debounce_within_window_suppresses() {
        let debounce = Duration::from_millis(500);
        let last = Instant::now() - Duration::from_millis(100);
        let now = Instant::now();
        assert!(
            !should_rerun_now(last, now, debounce),
            "within the debounce window, a rerun should NOT fire immediately"
        );
    }

    #[test]
    fn debounce_exactly_at_boundary() {
        // At exactly the debounce duration the comparison is strict `>`,
        // so the rerun is still suppressed. Document that behavior here.
        let debounce = Duration::from_millis(500);
        let last = Instant::now();
        let now = last + debounce;
        assert!(
            !should_rerun_now(last, now, debounce),
            "at exactly debounce, rerun should still be suppressed (strict >)"
        );
    }

    #[test]
    fn debounce_just_past_boundary_fires() {
        let debounce = Duration::from_millis(500);
        let last = Instant::now();
        let now = last + debounce + Duration::from_millis(1);
        assert!(should_rerun_now(last, now, debounce));
    }
}
