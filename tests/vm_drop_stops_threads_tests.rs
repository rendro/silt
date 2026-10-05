//! Dropping a `Vm` ends the threads that served its program: the
//! scheduler's workers and watchdog, the timer thread and the I/O
//! workers, whatever timers were pending.
//!
//! A test binary of its own, with one test: it counts the threads of
//! the process.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use silt::session::testing::compile_str;
use silt::{Buffer, Clock, HostIo, Vm};

/// The number of threads of this process.
fn threads() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .expect("a Threads line in /proc/self/status");
    line.trim().parse().unwrap()
}

/// A clock that never moves, and counts how often it is read.
#[derive(Clone, Default)]
struct Frozen {
    reads: Arc<AtomicU64>,
}

impl Clock for Frozen {
    fn now(&self) -> Duration {
        Duration::from_secs(1_000_000)
    }
    fn monotonic(&self) -> Duration {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Duration::ZERO
    }
    fn sleep(&self, _duration: Duration) {}
}

/// A program that ends with a task asleep for an hour, a channel
/// timeout an hour away, and I/O workers that have run an operation.
const SOURCE: &str = r#"
import channel
import io
import task
import time
fn main() {
  let _ = task.spawn({ -> time.sleep(time.hours(1)) })
  let _ = channel.timeout(3600000)
  let reader = task.spawn({ -> io.read_file("/no/such/file") })
  let _ = task.join(reader)
  println("done")
}
"#;

#[test]
fn dropped_vms_leave_no_threads() {
    let program = compile_str(SOURCE).unwrap_or_else(|errors| panic!("{errors:?}"));
    let baseline = threads();
    let frozen = Frozen::default();

    for round in 0..20 {
        let out = Buffer::new();
        // The system clock and an embedder's clock, in turn: on the
        // latter the timer thread reads the clock every millisecond.
        let io = if round % 2 == 0 {
            HostIo::buffer(&out)
        } else {
            HostIo::buffer(&out).clock(frozen.clone())
        };
        let mut vm = Vm::new(io);
        vm.run_program(&program).unwrap();
        assert_eq!(out.contents(), "done\n");
        if round == 0 {
            assert!(threads() > baseline, "the program started no thread");
        }
        drop(vm);
    }

    // The threads are told to end, not joined.
    let give_up = Instant::now() + Duration::from_secs(10);
    while threads() > baseline {
        assert!(
            Instant::now() < give_up,
            "{} threads are left of the dropped VMs",
            threads() - baseline
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Nobody reads the embedder's clock any more.
    let reads = frozen.reads.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(frozen.reads.load(Ordering::SeqCst), reads);
}
