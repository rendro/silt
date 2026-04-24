//! Regression lock for the `task.cancel` documentation in
//! `docs/concurrency.md`.
//!
//! History: round 52 audit caught doc drift. The previous wording said
//! `task.cancel` "removes the task from the scheduler's ready queue" and
//! that the task "will not execute again" — but the implementation only
//! installs a per-task cleanup closure when the task is parked on a
//! blocking edge (see `src/scheduler.rs` around the
//! `set_cancel_cleanup(...)` call). If the task is currently running on
//! a worker slice, `task.cancel` only sets the handle result
//! (first-writer-wins via `TaskHandle::complete` in `src/value.rs`);
//! the running slice continues to execute, side-effects and all, until
//! its next park point or natural completion.
//!
//! This test does NOT race the scheduler at runtime (that would be
//! flaky). Instead it pins the updated documentation so the corrected
//! sentences cannot silently disappear and the stale sentences cannot
//! silently come back.

use std::path::Path;

fn read_concurrency_doc() -> String {
    let path = Path::new("docs/concurrency.md");
    std::fs::read_to_string(path).expect("docs/concurrency.md must be readable")
}

fn read_channel_task_doc() -> String {
    let path = Path::new("docs/stdlib/channel-task.md");
    std::fs::read_to_string(path).expect("docs/stdlib/channel-task.md must be readable")
}

/// The stale wording from before the round-52 fix must not reappear.
#[test]
fn concurrency_doc_drops_stale_task_cancel_wording() {
    let doc = read_concurrency_doc();
    assert!(
        !doc.contains("removed from the scheduler's ready queue"),
        "docs/concurrency.md still claims task.cancel removes the task from the scheduler's \
         ready queue. That is not what src/builtins/concurrency.rs / src/value.rs::TaskHandle::\
         complete actually do — cancel is handle-metadata-only unless the task is parked."
    );
    assert!(
        !doc.contains("will not execute again"),
        "docs/concurrency.md still claims a cancelled task 'will not execute again'. A task \
         currently running on a worker slice continues until its next park point; only parked \
         tasks are actually torn down by the cancel_cleanup closure."
    );
}

/// The corrected wording must be present.
#[test]
fn concurrency_doc_documents_first_writer_wins_cancel() {
    let doc = read_concurrency_doc();
    // The section heading must still exist.
    assert!(
        doc.contains("### Cancelling: `task.cancel(handle)`"),
        "docs/concurrency.md must still have the `task.cancel` section heading"
    );
    // The updated description must call out first-writer-wins semantics
    // and the `Err(\"cancelled\")` handle result.
    assert!(
        doc.contains("first-writer-wins"),
        "task.cancel docs must describe first-writer-wins semantics (TaskHandle::complete \
         bails out if the result slot is already Some)."
    );
    assert!(
        doc.contains("Err(\"cancelled\")"),
        "task.cancel docs must name the exact error value (`Err(\"cancelled\")`) that \
         `task.join` observes on a cancelled handle."
    );
}

/// The parked-vs-running distinction must be documented.
#[test]
fn concurrency_doc_distinguishes_parked_vs_running_cancel() {
    let doc = read_concurrency_doc();
    // Parked branch: cleanup closure fires, task is dropped from live set.
    assert!(
        doc.contains("parked"),
        "task.cancel docs must describe the 'currently parked' branch (cancel_cleanup \
         closure tears down the wake registrations)."
    );
    // Running branch: slice continues, side effects flush.
    assert!(
        doc.contains("running") && doc.contains("next park"),
        "task.cancel docs must describe the 'currently running' branch, noting that the \
         slice continues until its next park point or natural completion."
    );
    assert!(
        doc.contains("side effects") || doc.contains("side-effects"),
        "task.cancel docs must warn that side effects scheduled by the running slice \
         (writes, spawns, channel sends) run to completion."
    );
}

/// The consumer guidance must explain how to detect "task is settled"
/// after `task.cancel`. Round 60 reversed the earlier "join and treat
/// the Err as authoritative" advice because `task.join` actually raises
/// on a cancelled handle (see round-60 notes below). The replacement
/// guidance is the sentinel-channel handshake pattern, which must be
/// documented by name.
#[test]
fn concurrency_doc_explains_post_cancel_settled_signal() {
    let doc = read_concurrency_doc();
    assert!(
        doc.contains("task.cancel") && doc.contains("task.join"),
        "task.cancel docs must still reference both task.cancel and task.join \
         (even if only to warn that joining a cancelled handle raises)."
    );
    let lower = doc.to_lowercase();
    assert!(
        lower.contains("sentinel channel") || lower.contains("channel handshake"),
        "task.cancel docs must recommend a sentinel-channel handshake (or \
         equivalent channel-based pattern) as the way to get a non-raising \
         'task is settled' signal after task.cancel. The previous \
         'join-and-treat-Err-as-authoritative' guidance was incorrect \
         because task.join raises on a cancelled handle."
    );
    // The stale "authoritative" / "task is done" guidance must be gone.
    assert!(
        !lower.contains("authoritative \"task is done\"")
            && !lower.contains("authoritative 'task is done'"),
        "task.cancel docs still recommend treating the Err(\"cancelled\") \
         return from task.join as the authoritative 'task is done' signal. \
         That advice is wrong — task.join raises on a cancelled handle; \
         use a sentinel channel instead."
    );
}

// ─────────────────────────────────────────────────────────────────────
// Round 59: the stdlib reference page at docs/stdlib/channel-task.md
// carried the same stale wording that round 52 removed from
// docs/concurrency.md ("The task will not execute further"). The two
// pages are the two places a reader can land on looking up
// `task.cancel`, so both must tell the same story. These assertions
// mirror the concurrency.md locks against the stdlib page.
// ─────────────────────────────────────────────────────────────────────

/// The stdlib page must not reassert the stale "won't execute further"
/// wording (any near-variant form).
#[test]
fn channel_task_doc_drops_stale_task_cancel_wording() {
    let doc = read_channel_task_doc();
    assert!(
        !doc.contains("will not execute further"),
        "docs/stdlib/channel-task.md still claims a cancelled task 'will not \
         execute further'. A running slice continues until its next park \
         point; only parked tasks are torn down immediately. Mirror the \
         corrected wording from docs/concurrency.md."
    );
    assert!(
        !doc.contains("will not execute again"),
        "docs/stdlib/channel-task.md reintroduced the round-52 stale phrase \
         'will not execute again'. Fix to the cooperative-request wording."
    );
    assert!(
        !doc.contains("removed from the scheduler's ready queue"),
        "docs/stdlib/channel-task.md imported the other round-52 stale phrase \
         ('removed from the scheduler's ready queue'). Remove it."
    );
}

/// The stdlib page must describe the first-writer-wins/`Err("cancelled")`
/// semantics that concurrency.md is authoritative on, so the two pages
/// agree.
#[test]
fn channel_task_doc_documents_cooperative_cancel_semantics() {
    let doc = read_channel_task_doc();
    assert!(
        doc.contains("## `task.cancel`"),
        "docs/stdlib/channel-task.md must still have a `task.cancel` section"
    );
    assert!(
        doc.contains("first-writer-wins"),
        "docs/stdlib/channel-task.md must describe first-writer-wins \
         semantics for task.cancel, matching docs/concurrency.md."
    );
    assert!(
        doc.contains("Err(\"cancelled\")"),
        "docs/stdlib/channel-task.md must name the exact error value \
         `Err(\"cancelled\")` that the cancelled handle resolves to."
    );
    let lower = doc.to_lowercase();
    assert!(
        lower.contains("cooperative"),
        "docs/stdlib/channel-task.md must call task.cancel a cooperative \
         request, not a synchronous stop signal (round-52 correction)."
    );
}

// ─────────────────────────────────────────────────────────────────────
// Round 60 — B10 / G1-docs fix: task.join(h) on a cancelled handle
// does NOT return `Err("cancelled")` as a value; it RAISES a runtime
// error `joined task failed: cancelled` at the call site (see
// `src/builtins/concurrency.rs:627-633` and `:656-658`). Rounds 52/59
// left behind snippets and prose implying the Err-as-value shape on
// both docs/concurrency.md and docs/stdlib/channel-task.md. These
// tests lock the corrected wording in.
//
// B11 fix: task.deadline's `Err` payload is a typed variant
// (`IoUnknown(msg)`, `TcpTimeout`, `HttpTimeout`) per
// `src/scheduler.rs:220-232`, not a stringly-typed `Err(msg)`.
// ─────────────────────────────────────────────────────────────────────

/// docs/concurrency.md must not reassert that `task.join` on a cancelled
/// handle "returns Err(\"cancelled\")" — that is the round-60 drift. The
/// join actually raises a runtime error, and prose/snippets that said
/// otherwise must stay gone.
#[test]
fn concurrency_doc_task_join_does_not_return_err_cancelled_as_value() {
    let doc = read_concurrency_doc();
    assert!(
        !doc.contains("returns Err(\"cancelled\") once the task is settled"),
        "docs/concurrency.md still carries the round-60 stale phrase \
         '-- returns Err(\"cancelled\") once the task is settled'. \
         `task.join` raises `joined task failed: cancelled`; it does not \
         return an Err value."
    );
    assert!(
        !doc.contains("let r = task.join(h)  -- Err(\"cancelled\")"),
        "docs/concurrency.md still carries the stale `let r = \
         task.join(h)  -- Err(\"cancelled\")` snippet. That snippet \
         terminates with a runtime error at the task.join call."
    );
}

/// The `task.join` section must explicitly document the raises-on-failure
/// contract. The prose that shipped in earlier rounds ("propagates the
/// error") was too thin — round 60 requires a concrete mention of the
/// `joined task failed` error shape.
#[test]
fn concurrency_doc_task_join_documents_raises_on_failure() {
    let doc = read_concurrency_doc();
    assert!(
        doc.contains("joined task failed"),
        "docs/concurrency.md must name the concrete runtime-error shape \
         `joined task failed: <msg>` that `task.join` raises when the \
         task errored, panicked, or was cancelled (round-60 G1-docs)."
    );
    // Must also note there is no try/catch so the user knows this is
    // terminal for the joining task. Normalize whitespace so prose that
    // wraps across a newline still matches.
    let flat: String = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat_lower = flat.to_lowercase();
    assert!(
        flat_lower.contains("no try/catch")
            || flat_lower.contains("no `try`/`catch`")
            || flat_lower.contains("has no `try`")
            || flat_lower.contains("has no try"),
        "docs/concurrency.md must tell the reader that silt has no \
         try/catch, so a joined failure is terminal and they must \
         use a channel handshake / sentinel for expected cancellation. \
         (Searched flattened doc for 'no try/catch' or similar.)"
    );
}

/// Mirror assertions on the stdlib page — both entry points must tell
/// the same story about `task.join` raising on cancellation/failure.
#[test]
fn channel_task_doc_task_join_documents_raises_on_failure() {
    let doc = read_channel_task_doc();
    assert!(
        doc.contains("joined task failed"),
        "docs/stdlib/channel-task.md `task.join` section must name the \
         `joined task failed: <msg>` runtime-error shape (round-60 G1-docs)."
    );
    assert!(
        !doc.contains("returns Err(\"cancelled\") once the task is settled"),
        "docs/stdlib/channel-task.md still carries the round-60 stale \
         phrase '-- returns Err(\"cancelled\") once the task is settled'. \
         task.join raises; it does not return the Err as a value."
    );
    assert!(
        !doc.contains("let _ = task.join(h)  -- returns Err(\"cancelled\")"),
        "docs/stdlib/channel-task.md still carries the stale cancel-then- \
         join snippet that claims the join returns an Err value."
    );
}

/// B11: docs/stdlib/channel-task.md must describe `task.deadline` in
/// terms of the typed variants (`IoUnknown(msg)`, `TcpTimeout`,
/// `HttpTimeout`), not a stringly-typed `Err(msg)`. The in-page match
/// snippet must use the typed-variant form.
#[test]
fn channel_task_doc_task_deadline_uses_typed_err_variants() {
    let doc = read_channel_task_doc();

    // Stringly-typed shape must not reappear.
    assert!(
        !doc.contains("returns `Err(\"I/O timeout (task.deadline exceeded)\")`"),
        "docs/stdlib/channel-task.md still claims task.deadline returns \
         a stringly-typed `Err(\"I/O timeout (task.deadline exceeded)\")`. \
         The scheduler fires the module's own typed variant \
         (IoUnknown(msg), TcpTimeout, HttpTimeout)."
    );
    assert!(
        !doc.contains("Err(msg) -> println(msg)  -- \"I/O timeout (task.deadline exceeded)\""),
        "docs/stdlib/channel-task.md task.deadline snippet still matches \
         on `Err(msg)` as a String. Match on the typed `IoUnknown(msg)` \
         variant instead."
    );

    // Typed-variant shape must be present.
    assert!(
        doc.contains("Err(IoUnknown(msg))"),
        "docs/stdlib/channel-task.md task.deadline section must show the \
         `Err(IoUnknown(msg))` typed-variant form used by io.*/fs.* on \
         deadline expiry."
    );
    assert!(
        doc.contains("Err(TcpTimeout)"),
        "docs/stdlib/channel-task.md task.deadline section must mention \
         `Err(TcpTimeout)` for tcp.* deadline expiry."
    );
    assert!(
        doc.contains("Err(HttpTimeout)"),
        "docs/stdlib/channel-task.md task.deadline section must mention \
         `Err(HttpTimeout)` for http.* deadline expiry."
    );
}

// ─────────────────────────────────────────────────────────────────────
// L10 fix: Round 60 adds a true runtime walker alongside the existing
// phrase-grep tests. The walker extracts the rewritten cancel+channel
// snippet from the rewritten docs and runs it end-to-end via the
// silt CLI, then extracts a second snippet that demonstrates the
// `task.join` raising shape and asserts the exit status + stderr.
//
// Pattern lifted from tests/aliased_import_runtime_tests.rs:
// write a temp file, invoke `silt run`, assert on stdout/stderr/exit.
// ─────────────────────────────────────────────────────────────────────

/// Run a silt source via the `silt` CLI. Returns (stdout, stderr, ok).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_docs_task_cancel_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// The rewritten cancel-via-sentinel-channel snippet must compile and
/// run cleanly (no runtime error, exit 0). This is the replacement for
/// the old `let r = task.join(h)  -- Err("cancelled")` snippet.
#[test]
fn task_cancel_sentinel_channel_snippet_runs_clean() {
    // Mirrors the ```silt``` block in the "Cancelling:
    // `task.cancel(handle)`" section of docs/concurrency.md after the
    // round-60 rewrite. If someone drops it back to the old shape,
    // this test also changes — and that is the point.
    let src = r#"
import channel
import task
fn main() {
  let done = channel.new(1)
  let h = task.spawn(fn() {
    -- long-running work
    channel.send(done, 42)
  })
  task.cancel(h)
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("sentinel_ok", src);
    assert!(
        ok,
        "rewritten cancel+sentinel snippet must run cleanly; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// The raises-on-failure contract from the `task.join` section must
/// actually fire at runtime: `task.cancel(h)` followed by `task.join(h)`
/// must terminate with a non-zero exit and a stderr mentioning
/// `joined task failed: cancelled`. This is the lock for the prose
/// "joined task failed: <msg>" on both doc pages.
#[test]
fn task_cancel_join_snippet_surfaces_runtime_error() {
    let src = r#"
import task
fn main() {
  let h = task.spawn(fn() { 42 })
  task.cancel(h)
  let _ = task.join(h)
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("join_raises", src);
    assert!(
        !ok,
        "task.cancel followed by task.join must exit non-zero; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("joined task failed: cancelled"),
        "task.join on cancelled handle must surface \
         'joined task failed: cancelled' on stderr (the exact phrase \
         both doc pages now document); got stderr={stderr:?}"
    );
}
