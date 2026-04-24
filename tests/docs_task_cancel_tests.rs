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

/// The consumer guidance must recommend pairing `task.cancel` with
/// `task.join` and treating the `Err("cancelled")` result as authoritative.
#[test]
fn concurrency_doc_recommends_cancel_then_join_pattern() {
    let doc = read_concurrency_doc();
    assert!(
        doc.contains("task.cancel") && doc.contains("task.join"),
        "task.cancel docs must recommend pairing task.cancel with task.join"
    );
    // Loose check for the authoritative-done guidance.
    let lower = doc.to_lowercase();
    assert!(
        lower.contains("authoritative") || lower.contains("actually done") || lower.contains("task is done"),
        "task.cancel docs must tell the reader that the Err(\"cancelled\") result of \
         task.join is the authoritative 'task is done' signal."
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

