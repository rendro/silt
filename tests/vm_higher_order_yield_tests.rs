//! Regression tests for higher-order builtin re-dispatch when the user
//! callback yields mid-iteration (e.g. an I/O call inside the callback).
//!
//! Background (the bug these tests guard against):
//!
//! Several higher-order builtins iterated by directly calling
//! `vm.invoke_callable(func, &[item])?` and propagating any yield error
//! through `?` without re-pushing their original VM-stack args.  When the
//! callback yielded (because, e.g., `io.read_file` suspended the task),
//! `Op::CallBuiltin`'s yield arm assumes the builtin re-pushed its args so
//! that on resume the opcode can re-read `argc` from the stack and
//! re-dispatch.  Without that re-push, resume saw worker locals on the
//! stack instead of the builtin's args and produced
//! `argc N exceeds stack size M`.
//!
//! Affected builtins:
//!   * `list.min_by`, `list.max_by`, `list.scan`, `list.unfold`
//!   * `stream.fold`, `stream.each`
//!
//! The fix routes `min_by`, `max_by`, and `scan` through the existing
//! `iterate_builtin` resume protocol (preserves accumulated state across
//! yields).  `unfold`, `stream.fold`, and `stream.each` use a manual
//! `SuspendedBuiltin` stash because their iteration source isn't
//! materializable up front (callback-driven for `unfold`, channel-driven
//! for the stream sinks).
//!
//! Each test exercises the runtime path end-to-end via the `silt` CLI so
//! the typechecker isn't enough to catch regressions.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_higher_order_yield_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Run and assert success; return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── list.min_by ─────────────────────────────────────────────────────

/// `list.min_by` with a yielding I/O call inside the callback. Pre-fix
/// this produced `call builtin 'list.min_by': argc 2 exceeds stack
/// size 1` because the callback's yield propagated up through
/// `min_by` without re-pushing its receiver/callback onto the VM
/// stack — `Op::CallBuiltin`'s rewind-and-retry then read garbage.
#[test]
fn list_min_by_callback_yield() {
    let out = run_silt_ok(
        "min_by",
        r#"
import task
import io
import list
fn main() {
  let xs: List(Int) = [3, 1, 2]
  let h = task.spawn(fn() {
    list.min_by(xs, fn(x) {
      let _ = io.read_file("/etc/hostname")
      x
    })
  })
  let v = task.join(h)
  println(v)
}
"#,
    );
    assert_eq!(out.trim(), "Some(1)", "list.min_by [3,1,2] should be Some(1)");
}

// ── list.max_by ─────────────────────────────────────────────────────

#[test]
fn list_max_by_callback_yield() {
    let out = run_silt_ok(
        "max_by",
        r#"
import task
import io
import list
fn main() {
  let xs: List(Int) = [3, 1, 2]
  let h = task.spawn(fn() {
    list.max_by(xs, fn(x) {
      let _ = io.read_file("/etc/hostname")
      x
    })
  })
  let v = task.join(h)
  println(v)
}
"#,
    );
    assert_eq!(out.trim(), "Some(3)", "list.max_by [3,1,2] should be Some(3)");
}

// ── list.scan ───────────────────────────────────────────────────────

/// `list.scan` accumulates a running prefix; each callback invocation
/// updates the running acc.  A yielding callback inside scan must not
/// restart from the seed on resume — that would repeat side effects
/// and silently double-count.  The fix routes scan through
/// `iterate_builtin_with_acc` so partial state survives the yield.
#[test]
fn list_scan_callback_yield() {
    let out = run_silt_ok(
        "scan",
        r#"
import task
import io
import list
fn main() {
  let xs: List(Int) = [1, 2, 3]
  let h = task.spawn(fn() {
    list.scan(xs, 0, fn(acc, x) {
      let _ = io.read_file("/etc/hostname")
      acc + x
    })
  })
  let v = task.join(h)
  println(v)
}
"#,
    );
    // scan seeds the prefix with the initial acc, then appends each
    // running acc: 0, 0+1=1, 1+2=3, 3+3=6.
    assert_eq!(out.trim(), "[0, 1, 3, 6]");
}

// ── list.unfold ─────────────────────────────────────────────────────

/// `list.unfold` is callback-driven (no pre-materialized item list).
/// On callback yield the running `(state, result)` must be stashed
/// in `SuspendedBuiltin` so resume picks up where it left off — a
/// restart-from-seed would loop forever or duplicate output.
#[test]
fn list_unfold_callback_yield() {
    let out = run_silt_ok(
        "unfold",
        r#"
import task
import io
import list
fn main() {
  let h = task.spawn(fn() {
    list.unfold(0, fn(s) {
      let _ = io.read_file("/etc/hostname")
      match s < 3 {
        true -> Some((s * 10, s + 1)),
        false -> None,
      }
    })
  })
  let v = task.join(h)
  println(v)
}
"#,
    );
    // unfold yields s*10 each step and increments state until s >= 3:
    // s=0 -> 0, s=1 -> 10, s=2 -> 20, s=3 -> stop.
    assert_eq!(out.trim(), "[0, 10, 20]");
}

// ── stream.fold ─────────────────────────────────────────────────────

/// `stream.fold` consumes a channel-backed source and folds with a
/// user callback.  When the callback yields (e.g. due to I/O), the
/// running accumulator must be stashed and the args re-pushed so
/// resume continues with the same acc.
#[test]
fn stream_fold_callback_yield() {
    let out = run_silt_ok(
        "stream_fold",
        r#"
import task
import io
import stream
fn main() {
  let h = task.spawn(fn() {
    let s = stream.from_list([1, 2, 3])
    stream.fold(s, 0, fn(acc, x) {
      let _ = io.read_file("/etc/hostname")
      acc + x
    })
  })
  let v = task.join(h)
  println(v)
}
"#,
    );
    assert_eq!(out.trim(), "6", "stream.fold should sum [1,2,3] to 6");
}

// ── stream.each ─────────────────────────────────────────────────────

/// `stream.each` iterates a channel and calls a side-effecting
/// callback.  A yielding callback must not fail the dispatch on
/// resume — even though there is no accumulator, the args still
/// have to be re-pushed for `Op::CallBuiltin`'s retry path to find
/// the right stack shape.
#[test]
fn stream_each_callback_yield() {
    let out = run_silt_ok(
        "stream_each",
        r#"
import task
import io
import stream
fn main() {
  let h = task.spawn(fn() {
    let s = stream.from_list([10, 20, 30])
    stream.each(s, fn(x) {
      let _ = io.read_file("/etc/hostname")
      println(x)
    })
  })
  let _ = task.join(h)
}
"#,
    );
    let lines: Vec<&str> = out.lines().map(|s| s.trim()).collect();
    assert_eq!(
        lines,
        vec!["10", "20", "30"],
        "stream.each should print each element in order"
    );
}
