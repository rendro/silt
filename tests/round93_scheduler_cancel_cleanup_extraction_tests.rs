//! Round 93 — locks for the cancel-cleanup extraction in
//! `src/scheduler.rs`.
//!
//! A structural probe found the cancel-while-blocked teardown closure
//! duplicated across the four park arms (Receive / Send / Join / Io)
//! plus a registration-time copy. The sequence is ordering-sensitive:
//!
//!   move guard into closure → take-slot-or-return (idempotency gate)
//!   → `was_io` → `watchdog.remove` → `live_tasks.fetch_sub`
//!   → `wake_graph.on_complete` → `signal_progress`
//!
//! One arm drifting (e.g. dropping `signal_progress`) would resurrect
//! the round-90 starvation false positive on that arm's path only.
//! Round 93 extracted the body into a single `make_cancel_cleanup`
//! helper so per-arm drift is impossible. The Select arm intentionally
//! stays inline (shared `entries` guard vec + `cancelled` flag instead
//! of a single moved-in guard; the watchdog call is omitted because
//! `was_io` is statically false for Select) — see the NOTE comment on
//! that arm.
//!
//! This file locks:
//!
//! 1. **Source shape** — the helper exists exactly once, all five
//!    non-select install sites route through it, and the park arms
//!    install it under the held slot lock.
//! 2. **Behavior** — per family, cancelling a parked task must not
//!    leave the scheduler in a state where a subsequent main-thread
//!    park trips the starvation watchdog (`deadlock on main thread`
//!    false positive) or hangs.
//!
//! Companion coverage (cited, not duplicated here):
//! * `src/scheduler.rs` unit tests
//!   `test_cancel_parked_{receive,send,join,io}_drains_live_tasks` —
//!   direct `live_tasks`-drains-to-zero locks per family (the counter
//!   is crate-private, so those live in the lib test mod).
//! * `tests/cancel_path_waker_leak_tests.rs` (B1-B4) — recv/send
//!   waker-leak symptoms after cancel.
//! * `tests/cancel_path_join_io_waker_leak_tests.rs` — join/io guard
//!   Drop semantics.
//! * `tests/scheduler_cancel_setup_race_tests.rs` +
//!   `tests/round75_io_watchdog_f10_race_tests.rs` — the F10
//!   cancelled-mid-setup else branches (which the extraction left
//!   untouched).

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;
use std::time::Duration;

// ───────────────────────── source-grep locks ─────────────────────────

fn scheduler_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/scheduler.rs");
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The helper is defined exactly once.
#[test]
fn cancel_cleanup_helper_defined_exactly_once() {
    let src = scheduler_source();
    assert_eq!(
        count(&src, "fn make_cancel_cleanup"),
        1,
        "`fn make_cancel_cleanup` must be defined exactly once in \
         src/scheduler.rs — the whole point of round 93 is a single \
         source of truth for the cancel-teardown sequence"
    );
}

/// All five non-select install sites route through the helper:
/// the registration-time install plus the Receive / Send / Join / Io
/// park arms.
#[test]
fn cancel_cleanup_helper_has_five_call_sites() {
    let src = scheduler_source();
    assert_eq!(
        count(&src, "set_cancel_cleanup(make_cancel_cleanup("),
        5,
        "expected exactly 5 `set_cancel_cleanup(make_cancel_cleanup(` \
         call sites (registration-time + Receive + Send + Join + Io). \
         Fewer: an arm regressed to an inline closure (drift hazard). \
         More: a new arm was added — make sure its teardown ordering \
         really matches before routing it through the helper"
    );
}

/// Round-31 lock discipline: every park-arm call site keeps the
/// is_some-check + set under one `task_slot` lock hold. The extraction
/// only moved the closure body, not the locking protocol.
#[test]
fn park_arms_keep_round31_lock_discipline() {
    let src = scheduler_source();
    // Four park arms: lock, check, install-through-helper.
    let pattern = "if slot_guard.is_some() {\n                                handle_for_cancel.set_cancel_cleanup(make_cancel_cleanup(";
    assert_eq!(
        count(&src, pattern),
        4,
        "each of the four park arms must install the helper-built \
         cleanup under the held `task_slot` lock guard (round-31 \
         check+set discipline)"
    );
}

// ─────────────────────── behavioral FP locks ───────────────────────
//
// Per family: park a victim task on the family's edge, cancel it,
// then drive main through a watchdog-guarded rendezvous receive fed
// by a fresh worker. If the cancel cleanup for that arm drifted
// (dropped `wake_graph.on_complete` or `signal_progress`), the
// main-thread starvation watchdog can fire a `deadlock on main
// thread` false positive — or main hangs until the trial budget.

const TRIALS: usize = 3;

fn assert_cancel_then_main_park_is_clean(family: &str, src: &str) {
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    for i in 0..TRIALS {
        let outcome = runner.run_trial();
        assert!(
            !outcome.timed_out,
            "{family} trial {i}: timed out — main's post-cancel park \
             never woke; outcome={outcome:?}"
        );
        assert!(
            !outcome.saw_deadlock(),
            "{family} trial {i}: starvation false positive after \
             cancelling a parked {family} task; outcome={outcome:?}"
        );
        assert!(
            outcome.ok(),
            "{family} trial {i}: expected clean completion; \
             outcome={outcome:?}"
        );
        assert_eq!(
            outcome.result,
            Some(Value::Int(42)),
            "{family} trial {i}: post-cancel rendezvous must deliver 42; \
             outcome={outcome:?}"
        );
    }
}

#[test]
fn cancel_parked_receive_then_main_park_no_false_positive() {
    assert_cancel_then_main_park_is_clean(
        "receive",
        r#"
import channel
import task
import time

fn main() {
  -- Victim parks on a rendezvous receive nobody will ever feed.
  let dead = channel.new(0)
  let victim = task.spawn(fn() { channel.receive(dead) })
  time.sleep(time.ms(50))
  task.cancel(victim)
  time.sleep(time.ms(50))
  -- Post-cancel probe: main parks on a watchdog-guarded receive.
  let ch = channel.new(0)
  let feeder = task.spawn(fn() { channel.send(ch, 42) })
  let got = match channel.receive(ch) {
    Message(v) -> v
    _ -> 0
  }
  let _ = task.join(feeder)
  got
}
"#,
    );
}

#[test]
fn cancel_parked_send_then_main_park_no_false_positive() {
    assert_cancel_then_main_park_is_clean(
        "send",
        r#"
import channel
import task
import time

fn main() {
  -- Victim parks on a rendezvous send nobody will ever drain.
  let dead = channel.new(0)
  let victim = task.spawn(fn() { channel.send(dead, 1) })
  time.sleep(time.ms(50))
  task.cancel(victim)
  time.sleep(time.ms(50))
  let ch = channel.new(0)
  let feeder = task.spawn(fn() { channel.send(ch, 42) })
  let got = match channel.receive(ch) {
    Message(v) -> v
    _ -> 0
  }
  let _ = task.join(feeder)
  got
}
"#,
    );
}

#[test]
fn cancel_parked_join_then_main_park_no_false_positive() {
    assert_cancel_then_main_park_is_clean(
        "join",
        r#"
import channel
import task
import time

fn main() {
  -- Joinee parks forever; victim parks on task.join(joinee).
  let dead = channel.new(0)
  let joinee = task.spawn(fn() { channel.receive(dead) })
  let victim = task.spawn(fn() { task.join(joinee) })
  time.sleep(time.ms(50))
  task.cancel(victim)
  -- Tear down the joinee too so no parked task outlives the probe.
  task.cancel(joinee)
  time.sleep(time.ms(50))
  let ch = channel.new(0)
  let feeder = task.spawn(fn() { channel.send(ch, 42) })
  let got = match channel.receive(ch) {
    Message(v) -> v
    _ -> 0
  }
  let _ = task.join(feeder)
  got
}
"#,
    );
}

#[test]
fn cancel_parked_io_then_main_park_no_false_positive() {
    assert_cancel_then_main_park_is_clean(
        "io",
        r#"
import channel
import task
import time

fn main() {
  -- Victim parks on BlockReason::Io via a timer-backed sleep far
  -- beyond the trial budget; only the cancel can release it.
  let victim = task.spawn(fn() { time.sleep(time.ms(600000)) })
  time.sleep(time.ms(50))
  task.cancel(victim)
  time.sleep(time.ms(50))
  let ch = channel.new(0)
  let feeder = task.spawn(fn() { channel.send(ch, 42) })
  let got = match channel.receive(ch) {
    Message(v) -> v
    _ -> 0
  }
  let _ = task.join(feeder)
  got
}
"#,
    );
}
