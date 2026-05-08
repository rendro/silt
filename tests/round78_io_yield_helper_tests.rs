//! Round 78: lock tests for the `Vm::submit_io_or_run` / `park_on_completion`
//! helpers that consolidate the previously-24-site IO yield-and-park
//! protocol into a single source of truth.
//!
//! Two layers of locks:
//!
//! 1. **Structural source-grep locks** — assert the protocol cannot
//!    drift back to per-site hand-rolling. The helper extraction is
//!    only safe so long as it remains the single owner of
//!    `pending_io = Some(...)` and the only `for arg in args` push
//!    sequence used by the IO builtins. If a future refactor
//!    re-introduces an inline park sequence, the matching count goes
//!    up and these tests fail before any runtime regression can occur.
//!
//! 2. **Behavioural sync/async equivalence** — drive the same builtin
//!    from both the main thread (sync fallback) and inside a spawned
//!    task (yield path) and assert observable values match. This
//!    catches a regression where the helper's worker-path closure
//!    diverges from its sync-path branch — semantically the whole
//!    point of the helper is that those two paths are the *same*
//!    closure, so an asymmetry would mean something rewired the
//!    helper to take two closures or skipped the sync branch.
//!
//! Routes covered by the behavioural tests: `io.read_file` /
//! `io.write_file` (default IoUnknown factory), `time.sleep`
//! (timer-backed park, `park_on_completion` only — no IO pool, no
//! entry guard), and `http.get` is omitted because a real HTTP fetch
//! during `cargo test` is not practical; its source-grep lock is
//! enough.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;

// ── Structural locks ────────────────────────────────────────────────

/// The single source of truth for `pending_io = Some(...)` is
/// `Vm::park_on_completion`. Any other site is a regression: a
/// future refactor that re-inlines the park sequence into a builtin
/// breaks the "one place owns the protocol" invariant the helper
/// extraction was built to enforce.
#[test]
fn pending_io_assignment_lives_only_inside_park_helper() {
    let total = count_in_dir("src/", "pending_io = Some")
        + count_in_dir("src/builtins/", "pending_io = Some")
        + count_in_dir("src/vm/", "pending_io = Some");
    // Three identical roots; we want the count from src/ only.
    let in_src = count_in_dir("src/", "pending_io = Some");
    let _ = total; // shadow to avoid unused
    assert_eq!(
        in_src, 1,
        "expected exactly 1 `pending_io = Some(...)` site (inside \
         Vm::park_on_completion); found {in_src}. Round 78's helper \
         consolidates this — re-inlining a park sequence in any \
         builtin breaks the lockstep invariant."
    );
}

/// Every file in `src/builtins/` (except `concurrency.rs` and
/// `common.rs`) must contain ZERO `for arg in args` re-push loops —
/// they all route through `Vm::submit_io_or_run` /
/// `Vm::run_or_submit_io` / `Vm::park_with_reason`.
///
/// Round-79 T4 widening (audit finding T4): the original ban-list
/// hard-coded only 4 files (`io.rs`, `tcp.rs`, `postgres.rs`,
/// `data.rs`), so a new builtin module that hand-rolled the
/// pre-helper protocol would silently slip past. The lock now
/// discovers files by reading `src/builtins/` and excludes only:
///   * `concurrency.rs` — `channel.each` has round-robin yield
///     sites that don't set a block_reason (different protocol
///     shape, out of scope for the round-78 helper).
///   * `common.rs` — helper utilities, no IO routing.
#[test]
fn io_builtins_do_not_hand_roll_args_pushback() {
    const EXCLUDED: &[&str] = &["concurrency.rs", "common.rs"];
    let builtins_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/builtins");
    let entries = std::fs::read_dir(&builtins_dir)
        .unwrap_or_else(|e| panic!("read_dir {builtins_dir:?}: {e}"));

    let mut scanned: Vec<String> = Vec::new();
    let mut violations: Vec<(String, usize)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let fname = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        if EXCLUDED.contains(&fname.as_str()) {
            continue;
        }
        // Skip mod.rs — re-export glue, no IO routing.
        if fname == "mod.rs" {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let n = src.matches("for arg in args").count();
        scanned.push(fname.clone());
        if n > 0 {
            violations.push((fname, n));
        }
    }

    // Sanity: the four originally-named files must still be in the
    // scanned set — guards against the loop accidentally skipping
    // everything (e.g. wrong dir, glob mismatch).
    for required in &["io.rs", "tcp.rs", "postgres.rs", "data.rs"] {
        assert!(
            scanned.iter().any(|s| s == required),
            "round 78 T4: expected scan to include `{required}`; \
             scanned={scanned:?}"
        );
    }

    assert!(
        violations.is_empty(),
        "round 78 T4: builtin file(s) hand-roll `for arg in args` \
         re-push — route through Vm::submit_io_or_run / \
         Vm::run_or_submit_io / Vm::park_with_reason instead. \
         Violations (file, count): {violations:?}. \
         Scanned files: {scanned:?}."
    );
}

/// `Vm::park_with_reason` is the **single** place that owns the
/// args-pushback + yield_signal sequence for parks that set a
/// `BlockReason`. Any other site that hand-rolls
/// `for arg in args { vm.push(arg.clone()); } return
/// Err(VmError::yield_signal())` for a block-reason park is a
/// regression (channel.each's round-robin yields are excluded —
/// they don't set a block_reason).
#[test]
fn park_block_reason_assignments_route_through_helper() {
    // Outside vm/mod.rs and the helper itself, no file should set
    // `block_reason = Some(BlockReason::...)` directly — every
    // builtin should call `park_with_reason` instead.
    let mut violations: Vec<String> = Vec::new();
    let dirs = ["src/builtins/", "src/scheduler.rs", "src/vm/execute.rs"];
    for rel in dirs {
        let abs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        if abs.is_dir() {
            walk(&abs, &mut |p| {
                if p.extension().is_some_and(|e| e == "rs") {
                    let src = std::fs::read_to_string(p).unwrap_or_default();
                    if src.contains("vm.block_reason = Some(BlockReason::") {
                        violations.push(p.display().to_string());
                    }
                }
            });
        } else {
            let src = std::fs::read_to_string(&abs).unwrap_or_default();
            if src.contains("vm.block_reason = Some(BlockReason::") {
                violations.push(abs.display().to_string());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "found `vm.block_reason = Some(BlockReason::...)` outside the \
         park_with_reason helper in: {violations:?}. Route through \
         Vm::park_with_reason instead."
    );
}

/// `Vm::park_on_completion` is the helper that must remain present
/// in `src/vm/mod.rs`. If a future refactor renames or deletes it
/// the IO builtins lose their consolidation point and one of the
/// other locks above will start to drift; pin the symbol explicitly
/// so the misalignment surfaces here too.
#[test]
fn park_on_completion_helper_is_present_in_vm_mod() {
    let src = include_str!("../src/vm/mod.rs");
    assert!(
        src.contains("pub(crate) fn park_on_completion("),
        "Vm::park_on_completion is missing from src/vm/mod.rs — round 78 \
         depends on this helper as the single owner of the IO park \
         protocol."
    );
    assert!(
        src.contains("pub(crate) fn submit_io_or_run<F>("),
        "Vm::submit_io_or_run is missing from src/vm/mod.rs — round 78 \
         IO builtins route through this helper."
    );
    assert!(
        src.contains("pub(crate) fn run_or_submit_io<F>("),
        "Vm::run_or_submit_io is missing from src/vm/mod.rs — round 78 \
         tcp.read/write/read_exact use this for the post-guard half."
    );
}

// ── Behavioural sync/async equivalence ──────────────────────────────

/// `io.read_file` must produce the same `Ok(content)` shape whether
/// called from the main thread (sync fallback) or from inside a
/// spawned task (yield path). The whole point of `submit_io_or_run`
/// is that both paths share one closure body — an asymmetry would
/// mean the helper grew a divergent branch.
#[test]
fn io_read_file_produces_same_ok_shape_in_sync_and_async_paths() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let path = dir.join(format!("silt_round78_read_{pid}.txt"));
    std::fs::write(&path, b"round78-payload").expect("seed file");
    let path_str = path.to_string_lossy().to_string();

    // Sync: main-thread read.
    let sync_src = format!(
        r#"
import io
fn main() = match io.read_file("{path_str}") {{
  Ok(s) -> s
  Err(_) -> "FAIL"
}}
"#,
        path_str = path_str.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let sync_outcome = InProcessRunner::new(&sync_src)
        .with_budget(Duration::from_secs(5))
        .run_trial();
    assert!(sync_outcome.ok(), "sync run failed: {sync_outcome:?}");

    // Async: read inside a spawned task.
    let async_src = format!(
        r#"
import io
import task
fn worker() -> String = match io.read_file("{path_str}") {{
  Ok(s) -> s
  Err(_) -> "FAIL"
}}
fn main() -> String {{
  let h = task.spawn(worker)
  task.join(h)
}}
"#,
        path_str = path_str.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let async_outcome = InProcessRunner::new(&async_src)
        .with_budget(Duration::from_secs(5))
        .run_trial();
    assert!(async_outcome.ok(), "async run failed: {async_outcome:?}");

    let _ = std::fs::remove_file(&path);

    assert_eq!(
        sync_outcome.result, async_outcome.result,
        "sync and async io.read_file must produce identical Ok(content); \
         got sync={:?} async={:?}",
        sync_outcome.result, async_outcome.result,
    );
    assert_eq!(
        sync_outcome.result,
        Some(Value::String("round78-payload".into())),
        "expected Ok(\"round78-payload\")"
    );
}

/// `time.sleep` parks via `park_on_completion` (no entry guard, no
/// IoPool). It returns `Value::Unit` regardless of whether it ran
/// the full sleep or short-circuited because of an already-past
/// deadline. Sync vs spawned-task must agree on the value shape;
/// the helper's only branch difference is the timer-park path.
#[test]
fn time_sleep_returns_unit_in_sync_and_async_paths() {
    let sync_src = r#"
import time
fn main() -> () = time.sleep(time.ms(1))
"#;
    let sync = InProcessRunner::new(sync_src)
        .with_budget(Duration::from_secs(5))
        .run_trial();
    assert!(sync.ok(), "sync sleep failed: {sync:?}");

    let async_src = r#"
import time
import task
fn worker() -> () = time.sleep(time.ms(1))
fn main() -> () {
  let h = task.spawn(worker)
  task.join(h)
}
"#;
    let async_outcome = InProcessRunner::new(async_src)
        .with_budget(Duration::from_secs(5))
        .run_trial();
    assert!(async_outcome.ok(), "async sleep failed: {async_outcome:?}");

    assert_eq!(sync.result, Some(Value::Unit));
    assert_eq!(async_outcome.result, Some(Value::Unit));
}

/// `io.write_file` round-trip: write inside a spawned task, then
/// read back on the main thread (sync). Locks that the helper's
/// async path actually committed the write to disk before resuming
/// — a regression where the worker closure was dropped without
/// running would surface here as a missing file.
#[test]
fn io_write_file_inside_task_actually_writes_to_disk() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let path = dir.join(format!("silt_round78_write_{pid}.txt"));
    let _ = std::fs::remove_file(&path);
    let path_str = path.to_string_lossy().to_string();

    let src = format!(
        r#"
import io
import task
fn worker() -> () {{
  let _ = io.write_file("{path_str}", "round78-write-payload")
  ()
}}
fn main() -> () {{
  let h = task.spawn(worker)
  task.join(h)
}}
"#,
        path_str = path_str.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let outcome = InProcessRunner::new(&src)
        .with_budget(Duration::from_secs(5))
        .run_trial();
    assert!(outcome.ok(), "spawn+write run failed: {outcome:?}");

    let written = std::fs::read_to_string(&path)
        .expect("write_file inside spawned task must persist to disk");
    assert_eq!(written, "round78-write-payload");

    let _ = std::fs::remove_file(&path);
}

// ── Helpers ─────────────────────────────────────────────────────────

fn count_in_dir(rel: &str, needle: &str) -> usize {
    let abs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut total = 0;
    walk(&abs, &mut |p| {
        if p.extension().is_some_and(|e| e == "rs") {
            let src = std::fs::read_to_string(p).unwrap_or_default();
            total += src.matches(needle).count();
        }
    });
    total
}

fn walk(dir: &std::path::Path, f: &mut dyn FnMut(&std::path::Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, f);
        } else {
            f(&path);
        }
    }
}
