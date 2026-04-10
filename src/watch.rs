use notify::{RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn watch_and_rerun(watch_dir: &Path, args: &[String]) {
    let (tx, rx) = mpsc::channel();

    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .unwrap_or_else(|e| {
        eprintln!("error: failed to create file watcher: {e}");
        std::process::exit(1);
    });

    watcher
        .watch(watch_dir, RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!("error: failed to watch {}: {e}", watch_dir.display());
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
                let silt_changed = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|ext| ext == "silt"));

                if !silt_changed {
                    continue;
                }

                if last_run.elapsed() > debounce {
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
