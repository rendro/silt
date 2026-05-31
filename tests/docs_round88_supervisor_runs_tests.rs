//! Round 88 regression lock for the "Supervision and restart" snippet
//! in `docs/concurrency.md`.
//!
//! History: the round-88 audit caught a confirmed BROKEN doc snippet.
//! The supervisor task recursed forever on `channel.receive(outcomes)`,
//! and the `outcomes` channel was never closed. After both workers
//! delivered their `Finished` outcomes, `supervise` blocked on the next
//! receive — at which point the runtime had no other progress to make,
//! so `task.join(sup)` in `main` tripped the deadlock detector:
//!
//! ```text
//! worker 2 handled a
//! worker 1 handled b
//! error[runtime]: joined task failed: deadlock on main thread:
//!   task.join with no progress possible
//! ```
//!
//! The fix threads an `outstanding` count through `supervise` so it
//! returns once every spawned worker has reported. The pedagogical
//! content is unchanged — outcome-channel routing, restart-on-Crashed,
//! and the bounded restart budget all stay.
//!
//! This walker is a **true runtime lock**: it does not merely
//! string-match the snippet, it extracts the literal block from
//! `docs/concurrency.md`, runs it under the `silt` CLI, and asserts on
//! both exit status and stdout content. If anyone reverts the supervisor
//! to a non-terminating shape, this test fails with the deadlock-detector
//! stderr; if anyone removes the worker `println` lines, this test fails
//! on the stdout assertions. A doc-only revert that brings back the
//! infinite-recursion `supervise` body will deadlock the runtime and
//! this test will exit non-zero.

use std::path::Path;
use std::process::Command;

/// Read `docs/concurrency.md` from the repo root.
fn read_concurrency_doc() -> String {
    let path = Path::new("docs/concurrency.md");
    std::fs::read_to_string(path).expect("docs/concurrency.md must be readable")
}

/// Extract the silt fenced block that follows the
/// `### Supervision and restart` heading. The snippet is the one
/// pedagogical example for that subsection; if the heading or its
/// fence shape ever moves, this extractor (and therefore the test)
/// must be updated along with the doc.
fn extract_supervision_snippet(doc: &str) -> String {
    let heading = "### Supervision and restart";
    let heading_pos = doc
        .find(heading)
        .expect("docs/concurrency.md must contain the '### Supervision and restart' subsection");
    let after = &doc[heading_pos..];
    let fence_open_rel = after
        .find("```silt")
        .expect("the '### Supervision and restart' section must contain a ```silt fenced block");
    // Move past the opener line.
    let after_open = &after[fence_open_rel..];
    let after_open_nl = after_open
        .find('\n')
        .expect("fence opener must be followed by a newline");
    let body_start = fence_open_rel + after_open_nl + 1;
    let body_rest = &after[body_start..];
    let fence_close_rel = body_rest.find("\n```").expect(
        "the '### Supervision and restart' silt fence must have a closing ``` on its own line",
    );
    body_rest[..fence_close_rel].to_string()
}

/// Run a silt source via the `silt` CLI. Returns (stdout, stderr, ok).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_docs_round88_supervisor_{}_{label}.silt",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_file(&tmp);
    (stdout, stderr, out.status.success())
}

/// The supervisor snippet must extract cleanly from the doc.
#[test]
fn supervision_snippet_is_extractable_from_concurrency_doc() {
    let doc = read_concurrency_doc();
    let snippet = extract_supervision_snippet(&doc);
    // Pedagogical anchors that the round-88 fix MUST preserve. If the
    // snippet ever drops any of these, the doc has lost the content the
    // section is meant to teach.
    assert!(
        snippet.contains("type Outcome"),
        "snippet must still define the `Outcome` enum (Finished/Crashed) — \
         outcome-channel routing is the section's main lesson. Got:\n{snippet}"
    );
    assert!(
        snippet.contains("Crashed"),
        "snippet must still demonstrate the restart-on-Crashed branch. Got:\n{snippet}"
    );
    assert!(
        snippet.contains("remaining_restarts"),
        "snippet must still demonstrate a bounded restart budget \
         (`remaining_restarts`). Got:\n{snippet}"
    );
    assert!(
        snippet.contains("spawn_worker"),
        "snippet must still spawn workers via `spawn_worker`. Got:\n{snippet}"
    );
    assert!(
        snippet.contains("task.join(sup)"),
        "snippet must still `task.join(sup)` to wait for the supervisor. \
         Got:\n{snippet}"
    );
}

/// The supervisor snippet from `docs/concurrency.md` must compile AND
/// run to completion. The previous (broken) shape exited 1 with a
/// `deadlock on main thread` error; the fixed shape exits 0 with both
/// workers' job-handled lines on stdout.
///
/// This test is the load-bearing one: it FAILS on the old (deadlocking)
/// snippet and PASSES on the fixed one. A test that only string-matched
/// the snippet would be insufficient, because the audit explicitly
/// warned against such weak gates.
#[test]
fn supervision_snippet_runs_to_completion_without_deadlock() {
    let doc = read_concurrency_doc();
    let snippet = extract_supervision_snippet(&doc);
    let (stdout, stderr, ok) = run_silt_raw("runs", &snippet);

    // Single-shot: the deadlock detector now confirms a suspected
    // starvation behind a bounded condvar gate (`confirm_main_starved`
    // in src/builtins/concurrency.rs) before firing, so this correct
    // program no longer false-fires under load. No retry needed. A
    // genuine regression (supervisor reverted to a non-terminating
    // shape) still deadlocks DETERMINISTICALLY and fails this test.
    assert!(
        !stderr.contains("deadlock on main thread"),
        "the supervision snippet tripped the deadlock detector. The \
         supervisor must terminate after every spawned worker has \
         reported its outcome — the recursive `channel.receive` cannot \
         loop forever on a never-closed outcomes channel.\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("task.join with no progress possible"),
        "the supervision snippet tripped `task.join with no progress \
         possible`. The supervisor task is blocked on outcomes after \
         every worker has already finished — the supervise loop must \
         exit when the outstanding-worker count reaches zero.\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
    assert!(
        ok,
        "the supervision snippet must exit 0; the round-88 fix threads \
         an `outstanding` counter through `supervise` so it returns \
         once every spawned worker has reported.\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}"
    );

    // Both workers process exactly one of the two jobs ("a", "b"); the
    // exact assignment is scheduler-dependent, so we assert on each
    // worker's `handled` line independently.
    assert!(
        stdout.contains("worker 1 handled"),
        "stdout must contain a `worker 1 handled <job>` line — worker 1 \
         is one of the two spawned workers and must have processed one \
         of {{a, b}}.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("worker 2 handled"),
        "stdout must contain a `worker 2 handled <job>` line — worker 2 \
         is one of the two spawned workers and must have processed one \
         of {{a, b}}.\nstdout:\n{stdout}"
    );
}
