//! Round 77 lock test for the watch loop's initial-run mtime ordering.
//!
//! ## Background — finding WATCH-L1
//!
//! `src/watch.rs` runs the user's compile command three times: once at
//! startup (the "initial run"), once when a file save fires after the
//! debounce window expires (the "immediate rerun"), and once when a
//! save arrives during the debounce window (the "debounced rerun").
//!
//! Each rerun captures `last_run_system = SystemTime::now()` so the
//! mtime check in `any_silt_path_mtime_newer` can reject coalesced
//! macOS FSEvents that don't reflect a real edit. For correctness,
//! the timestamp MUST be captured BEFORE the subprocess runs — if it
//! were captured after, any save that landed during the subprocess
//! would have an mtime strictly less than `last_run_system` and would
//! be silently dropped, costing the user a recompile.
//!
//! Both rerun branches in `src/watch.rs` already follow that ordering
//! (assignment first, subprocess second). Pre-fix, the initial-run
//! block did the OPPOSITE: it ran the subprocess at line 89 and then
//! captured `last_run_system` at line 97. This test locks the corrected
//! ordering so a future refactor can't silently regress it.
//!
//! ## Strategy — source-level lock (Option A)
//!
//! Driving the real watch loop from a unit test would require spawning
//! a subprocess and racing a filesystem write against it, which is
//! flaky. Instead, this test reads `src/watch.rs` via `include_str!`
//! and asserts a structural invariant: in the function body of
//! `watch_and_rerun`, the FIRST occurrence of
//! `last_run_system = SystemTime::now()` must precede the FIRST
//! `Command::new(&exe)` call site.
//!
//! Because the rerun branches set `last_run_system` *and then* call
//! `Command::new(&exe)`, the very first occurrence of each in source
//! order is the initial-run pair. So checking "first assignment
//! position < first subprocess call position" is exactly the ordering
//! invariant for the initial branch.

#[test]
fn initial_run_assigns_last_run_system_before_subprocess() {
    let src = include_str!("../src/watch.rs");

    // Locate the body of `watch_and_rerun` so we don't accidentally
    // match unrelated text from doc comments at the top of the file.
    let fn_start = src
        .find("pub fn watch_and_rerun(")
        .expect("src/watch.rs must define `pub fn watch_and_rerun(...)`");
    let body = &src[fn_start..];

    // First wall-clock timestamp capture in the function body. This is
    // the initial-run assignment after the round-77 fix; pre-fix it
    // came AFTER the initial subprocess call.
    let assign_pos = body.find("last_run_system = SystemTime::now()").expect(
        "watch_and_rerun must capture `last_run_system = SystemTime::now()` \
             — the mtime check in any_silt_path_mtime_newer depends on it",
    );

    // First subprocess invocation in the function body. Both rerun
    // branches and the initial branch use `Command::new(&exe)` — the
    // very first hit corresponds to the initial run.
    let cmd_pos = body.find("Command::new(&exe)").expect(
        "watch_and_rerun must invoke the compile subprocess via \
             `Command::new(&exe)`",
    );

    assert!(
        assign_pos < cmd_pos,
        "WATCH-L1 regression: in `watch_and_rerun`, the initial-run \
         assignment `last_run_system = SystemTime::now()` (byte \
         offset {assign_pos}) must precede the initial subprocess call \
         `Command::new(&exe)` (byte offset {cmd_pos}). Pre-fix, the \
         subprocess ran first and the timestamp was captured after, \
         so any save during the initial compile had an mtime strictly \
         less than `last_run_system` and was silently dropped by the \
         mtime check. Both rerun branches set the timestamp BEFORE \
         the subprocess; the initial branch must match."
    );
}

#[test]
fn last_run_instant_also_precedes_initial_subprocess() {
    // The `Instant` half of the timestamp pair is used for the debounce
    // decision in `should_rerun_now`. It is less safety-critical than
    // `last_run_system` (the worst case is a slightly-too-eager rerun,
    // not a dropped save), but the round-77 fix moved both
    // assignments together for clarity. This lock catches a partial
    // revert that puts only one of the two assignments back below the
    // subprocess.
    let src = include_str!("../src/watch.rs");

    let fn_start = src
        .find("pub fn watch_and_rerun(")
        .expect("src/watch.rs must define `pub fn watch_and_rerun(...)`");
    let body = &src[fn_start..];

    let assign_pos = body.find("last_run = Instant::now()").expect(
        "watch_and_rerun must capture `last_run = Instant::now()` for the \
             debounce window check in should_rerun_now",
    );
    let cmd_pos = body
        .find("Command::new(&exe)")
        .expect("watch_and_rerun must invoke the compile subprocess");

    assert!(
        assign_pos < cmd_pos,
        "WATCH-L1 partial regression: `last_run = Instant::now()` \
         (byte offset {assign_pos}) must precede the initial \
         `Command::new(&exe)` (byte offset {cmd_pos}). The round-77 \
         fix moved both timestamp captures above the initial \
         subprocess together; don't split them."
    );
}
