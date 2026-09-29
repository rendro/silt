//! Round 77 BROKEN VM-1 lock: `Op::CallMethod`'s qualified-global
//! arm and method-as-record-field arm must use
//! `invoke_callable_resumable` rather than `invoke_callable` when
//! dispatching a user-defined method.
//!
//! Pre-fix: when the method body yielded inside `task.spawn` (e.g.
//! the body called a yielding builtin like `io.read_file` or
//! `io.write_file`), the suspended-invoke state was saved but on
//! resume `Op::CallMethod` re-ran `invoke_callable` from scratch,
//! pushing a fresh frame at ip=0 and re-executing the entire method
//! body — duplicating side effects (println, mutation, foreign-fn
//! calls). The visible repro at `/tmp/repro_vm_callmethod.silt`
//! printed `before-io` twice instead of once.
//!
//! Post-fix: the qualified-global arm at `src/vm/execute.rs:2185` and
//! the record-field arm at `:2212` both call `invoke_callable_resumable`,
//! which detects `self.suspended_invoke.is_some()` on resume and
//! continues the suspended invocation rather than re-running the
//! callable from scratch.
//!
//! This test exercises the runtime path through the in-process
//! runner — a typecheck-only lock would be insufficient because the
//! typechecker has no visibility into the yield/resume cycle. We
//! count side-effect occurrences via a counter file written from
//! inside the trait method body. With the fix the method body runs
//! exactly once per call (counter = 1); pre-fix it ran twice
//! (counter >= 2).
//!
//! Coverage:
//! 1. Qualified-global arm: `Int.bump(self)` registered as a global
//!    via `trait Counter for Int { fn bump(self) -> Int }`.
//! 2. Record-field-as-callable arm: `r.f()` where `f` is a function
//!    stored as a record field, with the function body yielding via
//!    `io.write_file`.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;

/// Return a unique per-test counter file path so concurrent test
/// runs (cargo runs tests in parallel by default) cannot clobber
/// each other. Includes process id and a per-call atomic counter.
fn unique_counter_path(label: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    dir.join(format!("silt_round77_{label}_{pid}_{seq}.txt"))
        .to_string_lossy()
        .replace('\\', "/")
}

/// Qualified-global arm regression: a trait method dispatched via the
/// `Type.method` global table must NOT re-execute its body when its
/// internal yield resumes.
#[test]
fn round77_qualified_global_arm_method_runs_exactly_once_after_yield() {
    let counter_path = unique_counter_path("qualified");
    // Best-effort: scrub any stale file before we start.
    let _ = std::fs::remove_file(&counter_path);

    let src = format!(
        r#"
import io
import int
import task

trait Counter {{ fn bump(self) -> Int }}
trait Counter for Int {{
  fn bump(self) -> Int {{
    let cur = match io.read_file("{path}") {{
      Ok(s) -> match int.parse(s) {{
        Ok(n) -> n
        Err(_) -> 0
      }}
      Err(_) -> 0
    }}
    let next = cur + 1
    let _ = io.write_file("{path}", "{{next}}")
    next
  }}
}}

fn main() -> Int {{
  let _ = io.write_file("{path}", "0")
  let h = task.spawn(fn() {{
    let n = 7
    n.bump()
  }})
  let _ = task.join(h)
  match io.read_file("{path}") {{
    Ok(s) -> match int.parse(s) {{
      Ok(n) -> n
      Err(_) -> -1
    }}
    Err(_) -> -1
  }}
}}
"#,
        path = counter_path
    );

    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    let outcome = runner.run_trial();

    // Clean up before assertions so a failure doesn't leak the file.
    let _ = std::fs::remove_file(&counter_path);

    assert!(
        outcome.error_message.is_none(),
        "trial errored: {:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    // The counter must reflect EXACTLY one execution of the trait
    // method's body. Pre-fix, the body would re-run from ip=0 on
    // resume, producing counter >= 2.
    match outcome.result {
        Some(silt::value::Value::Int(n)) => {
            assert_eq!(
                n, 1,
                "trait method body must run exactly once across yield/resume; got counter={n} \
                 (pre-fix this was >=2 because Op::CallMethod re-ran invoke_callable from scratch)"
            );
        }
        other => panic!("expected Int return value from main, got {other:?}"),
    }
}

/// Record-field-as-callable arm regression: `r.f()` where `f` is a
/// function stored as a record field must not re-execute the
/// callable's body when its internal yield resumes. Same observable
/// (counter file) but routed through the second `invoke_callable`
/// call site at `Op::CallMethod`.
#[test]
fn round77_record_field_callable_arm_runs_exactly_once_after_yield() {
    let counter_path = unique_counter_path("recordfield");
    let _ = std::fs::remove_file(&counter_path);

    // Build a record with a function field; call it via `r.bump()`.
    // The compiler resolves this through the record-field arm of
    // Op::CallMethod when no `RecordType.bump` global exists.
    let src = format!(
        r#"
import io
import int
import task

fn make_bump() -> Fn() -> Int {{
  fn() {{
    let cur = match io.read_file("{path}") {{
      Ok(s) -> match int.parse(s) {{
        Ok(n) -> n
        Err(_) -> 0
      }}
      Err(_) -> 0
    }}
    let next = cur + 1
    let _ = io.write_file("{path}", "{{next}}")
    next
  }}
}}

type Holder {{ bump: Fn() -> Int }}

fn main() -> Int {{
  let _ = io.write_file("{path}", "0")
  let h = task.spawn(fn() {{
    let r = Holder {{ bump: make_bump() }}
    r.bump()
  }})
  let _ = task.join(h)
  match io.read_file("{path}") {{
    Ok(s) -> match int.parse(s) {{
      Ok(n) -> n
      Err(_) -> -1
    }}
    Err(_) -> -1
  }}
}}
"#,
        path = counter_path
    );

    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    let outcome = runner.run_trial();

    let _ = std::fs::remove_file(&counter_path);

    assert!(
        outcome.error_message.is_none(),
        "trial errored: {:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    match outcome.result {
        Some(silt::value::Value::Int(n)) => {
            assert_eq!(
                n, 1,
                "record-field callable body must run exactly once across yield/resume; \
                 got counter={n}"
            );
        }
        other => panic!("expected Int return value from main, got {other:?}"),
    }
}
