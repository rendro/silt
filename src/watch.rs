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

    let mut last_run = Instant::now();

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let silt_changed = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|ext| ext == "silt"));

                if silt_changed && last_run.elapsed() > Duration::from_millis(500) {
                    // Let file writes settle, then drain pending events
                    std::thread::sleep(Duration::from_millis(100));
                    while rx.try_recv().is_ok() {}

                    last_run = Instant::now();
                    eprint!("\x1B[2J\x1B[H");
                    let _ = std::process::Command::new(&exe).args(args).status();
                    eprintln!("\n[watch] Watching for changes...");
                }
            }
            Ok(Err(e)) => {
                eprintln!("watch error: {e}");
            }
            Err(_) => break,
        }
    }
}
