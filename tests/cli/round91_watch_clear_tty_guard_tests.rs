//! Round 91 lock: watch mode must not write a raw ANSI clear-screen
//! escape to stderr when stderr is not a terminal.
//!
//! Background: `src/watch.rs` previously emitted `eprint!("\x1B[2J\x1B[H")`
//! at three (re)run sites with no TTY guard. Running
//! `silt run app.silt --watch 2> watch.log` wrote the literal escape
//! bytes (`^[[2J^[[H`) into the redirected log on every run. The fix
//! routes every site through a `clear_screen_seq()` helper that returns
//! `""` when stderr is not a terminal.
//!
//! The test runs `silt run -w` with stderr captured by a pipe, lets it
//! do its initial run and one rerun, and checks that the captured stderr
//! holds no clear-screen escape.

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn drain(mut pipe: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buf);
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    buf
}

fn wait_for(buf: &Arc<Mutex<Vec<u8>>>, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snap = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
        if snap.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {marker:?}; output so far:\n{snap}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn watch_writes_no_clear_screen_escape_to_piped_stderr() {
    let dir = std::env::temp_dir().join(format!("silt_round91_watch_tty_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    fs::write(&file, "fn main() {\n  println(\"first-run\")\n}\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg("-w")
        .arg(&file)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt run -w");
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());

    // The initial run, then a rerun: each run prints the clear-screen
    // sequence when stderr is a tty.
    wait_for(&stdout, "first-run");
    wait_for(&stderr, "[watch] Watching for changes");
    fs::write(&file, "fn main() {\n  println(\"second-run\")\n}\n").unwrap();
    wait_for(&stdout, "second-run");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);

    let err = String::from_utf8_lossy(&stderr.lock().unwrap()).into_owned();
    assert!(
        !err.contains("\x1B[2J") && !err.contains("\x1B[H"),
        "watch mode wrote a clear-screen escape to a non-terminal stderr: {err:?}"
    );
}
