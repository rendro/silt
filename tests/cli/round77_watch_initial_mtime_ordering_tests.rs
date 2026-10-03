//! Round 77 lock for the watch loop's initial-run ordering (finding
//! WATCH-L1).
//!
//! `src/watch.rs` ignores watcher events for files whose content is what
//! it was when the command last started. The initial run must read that
//! content BEFORE it starts the subprocess: if it were read after, a save
//! that landed while the initial run was still going would look unchanged
//! and be silently dropped, costing the user a recompile.
//!
//! The test drives the real binary: the watched program prints a marker
//! and then sleeps, and the file is rewritten while that initial run is
//! still asleep. The rewrite must trigger a rerun.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct WatchProc {
    child: Child,
    stdout: Arc<Mutex<Vec<u8>>>,
}

impl WatchProc {
    fn spawn(file: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("run")
            .arg("-w")
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn silt run -w");
        let stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut child_stdout = child.stdout.take().expect("piped stdout");
        let sink = Arc::clone(&stdout);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = child_stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        WatchProc { child, stdout }
    }

    fn wait_for(&self, marker: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let snap = String::from_utf8_lossy(&self.stdout.lock().unwrap()).into_owned();
            if snap.contains(marker) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {marker:?}; stdout so far:\n{snap}");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for WatchProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn save_during_initial_run_triggers_a_rerun() {
    let dir =
        std::env::temp_dir().join(format!("silt_round77_watch_initial_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    // The initial run stays alive for 1.5 s after printing its marker.
    fs::write(
        &file,
        "import time\nfn main() {\n  println(\"initial-run-started\")\n  time.sleep(time.ms(1500))\n}\n",
    )
    .unwrap();

    let proc = WatchProc::spawn(&file);
    proc.wait_for("initial-run-started", Duration::from_secs(10));

    // The initial run is still asleep: save a new version now.
    fs::write(
        &file,
        "fn main() {\n  println(\"rerun-after-save-during-initial-run\")\n}\n",
    )
    .unwrap();

    proc.wait_for(
        "rerun-after-save-during-initial-run",
        Duration::from_secs(10),
    );
    drop(proc);
    let _ = fs::remove_dir_all(&dir);
}
