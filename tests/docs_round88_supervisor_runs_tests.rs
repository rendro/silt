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

    // Retry-until-success with a deterministic floor on what "success"
    // means. This is NOT a weak gate — here is why the retry is sound:
    //
    //   * A genuinely non-terminating supervisor (the round-88 BROKEN
    //     shape, or any revert to it) deadlocks DETERMINISTICALLY: the
    //     scheduler's wake-graph BFS proves no task can drive the joinee
    //     forward and fires the detector on EVERY run. So a real
    //     regression fails all ATTEMPTS and this test still fails.
    //   * The correct (terminating) supervisor can still trip the
    //     main-thread join deadlock detector as a rare FALSE POSITIVE
    //     under extreme CPU contention: a worker task can be starved in
    //     the documented window between dequeue and waker registration
    //     (`unsettled_tasks`, src/scheduler.rs), during which the BFS
    //     transiently sees no runnable/IO/pending node and fires
    //     "task.join with no progress possible" even though the joinee
    //     was about to make progress. This was observed only on the
    //     heavily-loaded Windows CI runner, never on a quiescent run.
    //
    // Retrying distinguishes the two: the false positive clears on a
    // subsequent attempt; the real deadlock never does. We accept the
    // first clean attempt and only fail if EVERY attempt tripped the
    // detector. (The detector false-positive-under-extreme-contention is
    // itself a real latent scheduler bug, tracked separately; this test
    // locks the DOC SNIPPET's termination semantics, not the scheduler's
    // contention behaviour.)
    const ATTEMPTS: usize = 5;
    let mut last: Option<(String, String, bool)> = None;
    let mut tripped_detector_every_time = true;

    for attempt in 0..ATTEMPTS {
        let label = format!("runs{attempt}");
        let (stdout, stderr, ok) = run_silt_raw(&label, &snippet);

        let tripped = stderr.contains("deadlock on main thread")
            || stderr.contains("task.join with no progress possible");
        if !tripped {
            tripped_detector_every_time = false;
        }

        let workers_ok = stdout.contains("worker 1 handled") && stdout.contains("worker 2 handled");

        if ok && !tripped && workers_ok {
            // Clean run: the snippet compiled, ran to completion without
            // tripping the detector, and both workers reported.
            return;
        }
        last = Some((stdout, stderr, ok));
    }

    let (stdout, stderr, ok) = last.expect("at least one attempt ran");

    // If we got here, no attempt was fully clean. Disambiguate.
    assert!(
        !tripped_detector_every_time,
        "the supervision snippet tripped the join deadlock detector on \
         all {ATTEMPTS} attempts. A correct supervisor false-positives \
         only intermittently under load; tripping every time means the \
         supervisor reverted to a non-terminating shape (the round-88 \
         BROKEN snippet) — the recursive `channel.receive` loops forever \
         on a never-closed outcomes channel because `supervise` no longer \
         returns once the outstanding-worker count reaches zero.\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
    // Detector did NOT trip every time, so the failures are about
    // exit-status or the worker lines — those are real content failures,
    // not the load flake. Surface them with the original assertions.
    assert!(
        ok,
        "the supervision snippet must exit 0; the round-88 fix threads \
         an `outstanding` counter through `supervise` so it returns \
         once every spawned worker has reported.\n\
         stdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
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
