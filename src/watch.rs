use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

/// Returns true if any of the given paths has a `.silt` extension.
/// Extracted so the filtering logic can be unit-tested in isolation
/// from the `notify` event stream and the subprocess rerun loop.
fn any_silt_path_changed(paths: &[PathBuf]) -> bool {
    paths
        .iter()
        .any(|p| p.extension().is_some_and(|ext| ext == "silt"))
}

/// Confirm at least one `.silt` path in the event actually has a
/// modification time newer than `since`. macOS FSEvents can emit
/// coalesced directory-level events that list sibling `.silt` files
/// even when only a non-`.silt` file was modified, so an
/// extension-only filter admits false positives. Checking mtime is a
/// filesystem-level truth check that rejects those.
///
/// Paths that can't be `stat`'d (deleted, permission denied) are
/// skipped rather than treated as modified, because a rerun can't
/// observe a file that's gone anyway.
fn any_silt_path_mtime_newer(paths: &[PathBuf], since: SystemTime) -> bool {
    paths.iter().any(|p| {
        p.extension().is_some_and(|ext| ext == "silt")
            && p.metadata()
                .and_then(|m| m.modified())
                .map(|m| m > since)
                .unwrap_or(false)
    })
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
    // Wall-clock timestamp of the most recent rerun (or startup). Used
    // to reject false-positive watcher events whose paths don't
    // actually have a newer mtime — see `any_silt_path_mtime_newer`.
    let mut last_run_system = SystemTime::now();
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
                last_run_system = SystemTime::now();
                eprint!("\x1B[2J\x1B[H");
                let _ = std::process::Command::new(&exe).args(args).status();
                eprintln!("\n[watch] Watching for changes...");
            }
            Ok(Some(Ok(event))) => {
                if !any_silt_path_changed(&event.paths) {
                    continue;
                }
                // Defensive mtime check: on macOS, FSEvents can report
                // coalesced directory-level events that include sibling
                // `.silt` files when only a non-`.silt` file was
                // modified. The extension filter above admits those as
                // silt-events; the mtime check rejects them.
                if !any_silt_path_mtime_newer(&event.paths, last_run_system) {
                    continue;
                }

                if should_rerun_now(last_run, Instant::now(), debounce) {
                    // Outside the debounce window: rerun now.
                    pending_rerun = false;
                    // Let file writes settle, then drain pending events.
                    std::thread::sleep(Duration::from_millis(100));
                    while rx.try_recv().is_ok() {}

                    last_run = Instant::now();
                    last_run_system = SystemTime::now();
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

    // ── any_silt_path_mtime_newer ─────────────────────────────────
    //
    // Regression lock for the macOS FSEvents coalescing issue: the
    // watcher can report a `.silt` path in an event whose root cause
    // was a modification to a sibling non-`.silt` file. The extension
    // filter admits those false positives; the mtime check rejects
    // them by consulting the filesystem directly.
    #[test]
    fn mtime_check_rejects_stale_silt_path() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("silt_watch_mtime_test_{}.silt", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"fn main() {}").unwrap();
        drop(f);

        // Sleep briefly, then capture "now" — the file's mtime is
        // strictly earlier than this.
        std::thread::sleep(Duration::from_millis(20));
        let since = SystemTime::now();

        assert!(
            !any_silt_path_mtime_newer(std::slice::from_ref(&path), since),
            "unmodified .silt file should not count as newer"
        );

        // Touch the file and verify the check flips.
        std::thread::sleep(Duration::from_millis(20));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"fn main() { 1 }").unwrap();
        drop(f);

        assert!(
            any_silt_path_mtime_newer(std::slice::from_ref(&path), since),
            ".silt file with mtime > since should count as newer"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mtime_check_ignores_non_silt_paths() {
        use std::io::Write;
        // Even a freshly-modified .txt path must not satisfy the
        // silt-specific mtime check.
        let path =
            std::env::temp_dir().join(format!("silt_watch_mtime_test_{}.txt", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"notes").unwrap();
        drop(f);

        let since = SystemTime::UNIX_EPOCH;
        assert!(
            !any_silt_path_mtime_newer(std::slice::from_ref(&path), since),
            "non-silt path must never satisfy the silt mtime check"
        );

        let _ = std::fs::remove_file(&path);
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
