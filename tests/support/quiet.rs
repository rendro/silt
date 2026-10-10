//! Whether a silt program that only waits is ever woken (Linux).
//!
//! The program runs as a process of its own, so every thread of that
//! process is the program's: its main thread, the scheduler's workers,
//! the timer, the I/O pool. The test waits until the process has come
//! to rest (two looks a quarter of a second apart that show the same
//! threads and no switch), and then takes ONE window: in it no thread
//! may give up the processor of its own accord even once, which a
//! thread does every time it is woken and goes back to sleep. A
//! wake-up of any thread with any period shorter than the window
//! fails the test; there is no second try.

#![cfg(all(target_os = "linux", feature = "tcp"))]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A task waits in `tcp.accept`, and the program itself in another.
pub const ACCEPTS: &str = r#"
import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  when let Ok(other) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let _ = task.spawn { -> tcp.accept(listener) }
  println("waiting")
  let _ = tcp.accept(other)
}
"#;

/// A task serves HTTP, and the program itself waits in `tcp.accept`.
#[cfg(feature = "http")]
pub const SERVER: &str = r#"
import http
import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  when let Ok(other) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let _ = task.spawn { ->
    http.serve(listener) { _req -> http.Response { status: 200, body: "ok", headers: #{} } }
  }
  println("waiting")
  let _ = tcp.accept(other)
}
"#;

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

/// For each thread of the process, by its id: its name, and how often
/// it has given up the processor of its own accord.
fn voluntary_switches(pid: u32) -> BTreeMap<u32, (String, u64)> {
    let mut threads = BTreeMap::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return threads;
    };
    for task in tasks.flatten() {
        let Ok(id) = task.file_name().to_string_lossy().parse() else {
            continue;
        };
        // A thread that has just ended has no status any more.
        let Ok(status) = std::fs::read_to_string(task.path().join("status")) else {
            continue;
        };
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(|value| value.trim().to_string())
        };
        let switches = field("voluntary_ctxt_switches:").and_then(|n| n.parse().ok());
        threads.insert(
            id,
            (field("Name:").unwrap_or_default(), switches.unwrap_or(0)),
        );
    }
    threads
}

/// Run `source`, which prints a line when it has started everything
/// and then only waits, and assert that no thread of it is woken in
/// `window`.
pub fn assert_never_woken(source: &str, window: Duration) {
    static FILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = FILES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let file = std::env::temp_dir().join(format!("silt_quiet_{}_{n}.silt", std::process::id()));
    std::fs::write(&file, source).expect("the program's file");
    let mut child = Command::new(silt_bin())
        .arg("run")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn silt");
    let mut started = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut started)
        .expect("the program's line");
    let pid = child.id();

    // The program has started its waits; they come to rest.
    let patience = Instant::now() + Duration::from_secs(60);
    let mut before = voluntary_switches(pid);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = voluntary_switches(pid);
        if now == before {
            break;
        }
        before = now;
        if Instant::now() >= patience {
            break;
        }
    }
    std::thread::sleep(window);
    let after = voluntary_switches(pid);
    let alive = child.try_wait().ok().flatten().is_none();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&file);

    assert_eq!(started, "waiting\n", "the program did not start");
    assert!(alive, "the program ended");
    assert!(before.len() >= 2, "no threads to look at: {before:?}");
    let woken: Vec<String> = after
        .iter()
        .filter(|(id, thread)| before.get(id) != Some(thread))
        .map(|(id, (name, switches))| {
            let was = before.get(id).map_or(0, |(_, switches)| *switches);
            format!("{name} ({id}): {} times", switches - was)
        })
        .chain(
            before
                .iter()
                .filter(|(id, _)| !after.contains_key(id))
                .map(|(id, (name, _))| format!("{name} ({id}): ended")),
        )
        .collect();
    assert!(
        woken.is_empty(),
        "threads of a program that only waits were woken within {window:?}: {woken:?}"
    );
}
