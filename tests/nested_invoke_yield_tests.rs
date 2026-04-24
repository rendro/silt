//! Regression tests for nested `invoke_callable` yield handling (audit
//! round 26 — B5 and L7).
//!
//! ── B5 (BROKEN): nested yield overwrites `suspended_invoke` ─────────
//!
//! `Vm::suspended_invoke` and `Vm::suspended_builtin` were single
//! `Option` slots. When a nested callback yielded (e.g. `io.read_file`
//! inside `task.deadline(dur, fn() { task.deadline(dur, fn() { io.read_file(..) }) })`),
//! the inner yield arm wrote its state to the slot, then the Err
//! propagated up through the outer Rust frames and the outer
//! `invoke_callable` Err arm **overwrote** the slot with the outer
//! state. On resume, the outer's `resume_suspended_invoke` replayed the
//! outer frame, re-entered the inner `CallBuiltin`, which saw
//! `suspended_builtin == None` and restarted the inner callback from
//! scratch — producing duplicate observable side effects (stdout prints,
//! file reads). In the nested `list.map` + I/O variant this caused
//! exponential re-execution of the inner callback.
//!
//! Fix: turn the two slots into LIFO stacks. The `Option` slot is still
//! "the top" and `suspended_*_outer: Vec<_>` holds deeper states; the
//! `push_*` helper on Vm spills the slot into the vec before storing
//! the new state, and `take_*` auto-promotes the next state from the
//! vec into the slot so `.is_some()` readers stay correct.
//!
//! ── L7 (LATENT): Err arms skipped `prune_tco_elided` ────────────────
//!
//! Round-22 added `prune_tco_elided` to the Return / EarlyReturn arms
//! of `invoke_callable` and `resume_suspended_invoke`, but the Err arms
//! in both functions still truncated frames without pruning. A
//! tail-call-heavy callback that erred could leave stale `tco_elided`
//! entries referring to depths above the surviving frame floor; a later
//! unrelated call could then render a phantom "(N tail calls elided)"
//! caller in its call stack.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Render a path for embedding in silt source.
fn path_for_silt(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// Create a unique temporary .silt file.
fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_nested_invoke_yield");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Create a unique temporary file containing `content` and return its path.
fn temp_data_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_nested_invoke_yield_data");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.txt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

struct SiltRun {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_silt(path: &Path) -> SiltRun {
    let output = silt_cmd()
        .arg("run")
        .arg(path)
        .output()
        .expect("failed to run silt binary");
    SiltRun {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

// ── B5 test 1: nested task.deadline + io.read_file ──────────────────

/// Reproduces the audit finding exactly: two layers of `task.deadline`
/// wrapping `io.read_file`. The inner callback must run exactly once,
/// so `enter-inner` prints once and `exit-inner` prints once. Before the
/// fix, `enter-inner` printed twice because the outer yield arm
/// overwrote the inner suspended state.
#[test]
fn test_nested_task_deadline_inner_io_runs_once() {
    let data = temp_data_file("hostname", "the-test-hostname\n");
    let data_path = path_for_silt(&data);

    let src = format!(
        r#"import task
import io
import time

fn main() {{
  let h = task.spawn(fn() {{
    task.deadline(time.seconds(10), fn() {{
      task.deadline(time.seconds(10), fn() {{
        println("enter-inner")
        let _ = io.read_file("{data_path}")
        println("exit-inner")
      }})
    }})
  }})
  task.join(h)
}}
"#
    );
    let path = temp_silt_file("nested_deadline_io", &src);
    let run = run_silt(&path);
    assert_eq!(
        run.code, 0,
        "expected clean exit. stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );

    let enter = run.stdout.matches("enter-inner").count();
    let exit = run.stdout.matches("exit-inner").count();
    assert_eq!(
        enter, 1,
        "expected exactly one 'enter-inner' line (inner callback runs once across nested yield), \
         got {enter}. stdout=\n{}",
        run.stdout
    );
    assert_eq!(
        exit, 1,
        "expected exactly one 'exit-inner' line, got {exit}. stdout=\n{}",
        run.stdout
    );
}

// ── B5 test 2: nested list.map + yielding I/O ───────────────────────

/// Nested `list.map` + I/O variant from the audit finding: before the
/// fix this exploded (~291k lines observed) because the inner
/// `list.map` got re-dispatched from scratch on every outer iteration
/// and every yield. After the fix the inner callback must run exactly
/// once per (outer, inner) pair — 2 * 3 = 6 total invocations.
#[test]
fn test_nested_list_map_inner_io_runs_once_per_pair() {
    let data = temp_data_file("payload", "payload-data\n");
    let data_path = path_for_silt(&data);

    let src = format!(
        r#"import task
import io
import list

fn main() {{
  let h = task.spawn(fn() {{
    let outer = [10, 20]
    let inner = [1, 2, 3]
    let _ = list.map(outer, fn(o) {{
      list.map(inner, fn(i) {{
        let _ = io.read_file("{data_path}")
        println("pair")
        o + i
      }})
    }})
    println("done")
  }})
  task.join(h)
}}
"#
    );
    let path = temp_silt_file("nested_list_map_io", &src);
    let run = run_silt(&path);
    assert_eq!(
        run.code,
        0,
        "expected clean exit. stdout_len={} stderr={:?}",
        run.stdout.len(),
        run.stderr
    );
    let pair_count = run.stdout.matches("pair").count();
    assert_eq!(
        pair_count,
        6,
        "expected exactly 6 'pair' prints (2 outer x 3 inner), got {pair_count}. \
         This failure indicates the inner callback was re-run from scratch across \
         yield/resume — the B5 nested-suspended-state bug. \
         (stdout length = {})",
        run.stdout.len()
    );
    let done_count = run.stdout.matches("done").count();
    assert_eq!(
        done_count,
        1,
        "expected exactly one 'done' (outer list.map returns once), got {done_count}. \
         stdout (first 2k chars):\n{}",
        run.stdout.chars().take(2000).collect::<String>()
    );
}

// ── B5 test 3: single (non-nested) yield still works ───────────────

/// Control test: a simple single-yield scenario (the common path) must
/// still work after the Option→stack rewrite. One layer of
/// `task.deadline` wrapping `io.read_file` should run the callback once.
#[test]
fn test_single_task_deadline_io_runs_once() {
    let data = temp_data_file("single", "single-value\n");
    let data_path = path_for_silt(&data);

    let src = format!(
        r#"import task
import io
import time

fn main() {{
  let h = task.spawn(fn() {{
    task.deadline(time.seconds(10), fn() {{
      println("enter")
      let _ = io.read_file("{data_path}")
      println("exit")
    }})
  }})
  task.join(h)
}}
"#
    );
    let path = temp_silt_file("single_deadline_io", &src);
    let run = run_silt(&path);
    assert_eq!(
        run.code, 0,
        "expected clean exit. stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let enter = run.stdout.matches("enter").count();
    let exit = run.stdout.matches("exit").count();
    // `enter` appears inside `enter` but not inside `exit`, so these
    // counts are independent.
    assert_eq!(
        enter, 1,
        "expected exactly one 'enter', got {enter}. stdout=\n{}",
        run.stdout
    );
    assert_eq!(
        exit, 1,
        "expected exactly one 'exit', got {exit}. stdout=\n{}",
        run.stdout
    );
}

// ── L7 test: Err arm must prune tco_elided in both functions ────────

/// L7 regression: the `Err(e)` arm of `invoke_callable` /
/// `resume_suspended_invoke` now calls `prune_tco_elided` before
/// returning, mirroring round-22's Return/EarlyReturn fix. Without this,
/// a callback error inside a tail-call-heavy caller can leave stale
/// `tco_elided` entries at depths that no longer exist on the physical
/// frame stack.
///
/// This test follows the same shape as
/// `tests/tco_elided_callback_tests.rs::test_tco_elided_callback_no_phantom_frames`
/// but runs inside a `task.spawn` so the error propagates through the
/// yield/resume machinery (exercising the Err arm in invoke_callable
/// under the scheduled-task code path).
#[test]
fn test_tco_elided_pruned_on_err_under_task_spawn() {
    let path = temp_silt_file(
        "tco_err_under_spawn",
        r#"import list
import task

fn helper(x) {
  match x == 0 {
    true -> 1 / 0,
    false -> x
  }
}
fn wrapper(x) { helper(x) }
fn main() {
  let h = task.spawn(fn() {
    list.map([1, 2, 0], wrapper)
  })
  task.join(h)
}
"#,
    );
    let run = run_silt(&path);
    // Expect a runtime error for division by zero.
    assert!(
        run.stderr.contains("division by zero") || run.stdout.contains("division by zero"),
        "expected division-by-zero in output. stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );

    // `wrapper` must appear in the rendered call stack, but only ONCE.
    // Before the Err-arm prune fix, stale tco_elided entries from
    // earlier successful iterations would surface as phantom frames.
    let wrapper_count = run.stderr.matches("-> wrapper").count();
    assert!(
        wrapper_count == 1,
        "expected exactly 1 '-> wrapper' in call stack, got {wrapper_count}. \
         Phantom frames indicate tco_elided was NOT pruned on the Err arm; \
         a count of 0 indicates the call stack was truncated entirely. \
         stderr:\n{}",
        run.stderr
    );
    let helper_count = run.stderr.matches("-> helper").count();
    assert!(
        helper_count == 1,
        "expected exactly 1 '-> helper' in call stack, got {helper_count}. \
         stderr:\n{}",
        run.stderr
    );
}
