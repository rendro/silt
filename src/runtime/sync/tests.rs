//! The concurrency core against the scheduler double: every step of a
//! test is one call, so each interleaving is written down and nothing
//! depends on time or on threads (but for the last test, which only
//! counts).

use std::sync::Arc;
use std::time::Duration;

use super::double::Double;
use super::*;
use crate::value::Value;
use crate::vm::VmError;

const MS: Duration = Duration::from_millis(1);

fn recv(channel: &Arc<Channel>) -> Wait {
    Wait::new(vec![Arm::Recv(channel.clone())])
}

fn send(channel: &Arc<Channel>, value: i64) -> Wait {
    Wait::new(vec![Arm::Send(channel.clone(), Value::Int(value))])
}

/// The arm and the value of a completed receive.
fn received(fired: Fired) -> (usize, i64) {
    match fired {
        Fired::Arm(arm, Outcome::Received(Value::Int(value))) => (arm, value),
        other => panic!("expected a received value, got {other:?}"),
    }
}

fn resumed(double: &Double, task: u64) -> Fired {
    match double.resume(task) {
        Resumed::Fired(fired) => fired,
        Resumed::Cancelled => panic!("task {task} was cancelled"),
    }
}

fn try_value(channel: &Channel, double: &Double) -> Option<i64> {
    match channel.try_receive(double) {
        TryReceive::Value(Value::Int(value)) => Some(value),
        TryReceive::Empty => None,
        other => panic!("expected a value or empty, got {other:?}"),
    }
}

// ── Rendezvous ──────────────────────────────────────────────────────

#[test]
fn rendezvous_send_completes_a_parked_receive() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, recv(&ch)).is_none());
    assert_eq!(ch.waiting(), (1, 0));

    let sent = d.block(2, send(&ch, 7)).expect("a receiver is parked");
    assert!(matches!(sent, Fired::Arm(0, Outcome::Sent)));
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (0, 7));
    assert_eq!(ch.queued(), (0, 0));
}

#[test]
fn rendezvous_receive_completes_a_parked_send() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, send(&ch, 7)).is_none());
    assert_eq!(ch.waiting(), (0, 1));
    // A parked sender is no receiver: a second send finds nobody.
    assert!(matches!(
        ch.try_send(Value::Int(8), &d),
        TrySend::Full(Value::Int(8))
    ));

    let got = d.block(2, recv(&ch)).expect("a sender is parked");
    assert_eq!(received(got), (0, 7));
    assert_eq!(d.woken(), [1]);
    assert!(matches!(resumed(&d, 1), Fired::Arm(0, Outcome::Sent)));
}

#[test]
fn try_receive_takes_the_value_of_a_parked_rendezvous_sender() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, send(&ch, 7)).is_none());
    assert_eq!(try_value(&ch, &d), Some(7));
    assert_eq!(d.woken(), [1]);
    assert_eq!(try_value(&ch, &d), None);
}

#[test]
fn one_receive_completes_one_rendezvous_send() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, send(&ch, 10)).is_none());
    assert!(d.block(2, send(&ch, 20)).is_none());

    assert_eq!(try_value(&ch, &d), Some(10));
    assert_eq!(d.woken(), [1]);
    assert!(d.is_parked(2));

    assert_eq!(try_value(&ch, &d), Some(20));
    assert_eq!(d.woken(), [2]);
}

#[test]
fn try_send_without_receiver_on_rendezvous_is_full() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Full(_)));
    assert!(ch.is_empty());
}

// ── Buffered ────────────────────────────────────────────────────────

#[test]
fn buffer_keeps_order_and_a_receive_admits_one_parked_sender() {
    let d = Double::new();
    let ch = Channel::new(0, 2);
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Sent));
    assert!(matches!(ch.try_send(Value::Int(2), &d), TrySend::Sent));
    assert!(matches!(ch.try_send(Value::Int(3), &d), TrySend::Full(_)));
    assert!(d.block(1, send(&ch, 3)).is_none());
    assert!(d.block(2, send(&ch, 4)).is_none());

    assert_eq!(try_value(&ch, &d), Some(1));
    assert_eq!(d.woken(), [1]);
    assert_eq!(ch.len(), 2);
    assert!(d.is_parked(2));

    assert_eq!(try_value(&ch, &d), Some(2));
    assert_eq!(d.woken(), [2]);
    assert_eq!(try_value(&ch, &d), Some(3));
    assert_eq!(try_value(&ch, &d), Some(4));
    assert_eq!(try_value(&ch, &d), None);
    assert!(d.woken().is_empty());
}

#[test]
fn a_send_hands_its_value_to_a_parked_receiver_not_to_the_buffer() {
    let d = Double::new();
    let ch = Channel::new(0, 1);
    assert!(d.block(1, recv(&ch)).is_none());
    assert!(d.block(2, recv(&ch)).is_none());

    assert!(matches!(ch.try_send(Value::Int(5), &d), TrySend::Sent));
    assert!(ch.is_empty());
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (0, 5));
    assert!(d.is_parked(2));
}

// ── Select ──────────────────────────────────────────────────────────

fn select_recv(a: &Arc<Channel>, b: &Arc<Channel>) -> Wait {
    Wait::new(vec![Arm::Recv(a.clone()), Arm::Recv(b.clone())])
}

#[test]
fn a_select_on_two_rendezvous_channels_completes_one_send() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    assert!(d.block(1, select_recv(&a, &b)).is_none());
    assert_eq!((a.waiting(), b.waiting()), ((1, 0), (1, 0)));

    assert!(d.block(2, send(&a, 1)).is_some());
    // The select has its value: it is no receiver on `b` any more.
    assert_eq!(b.waiting(), (0, 0));
    assert!(d.block(3, send(&b, 2)).is_none());
    assert_eq!(d.woken(), [1]);

    assert_eq!(received(resumed(&d, 1)), (0, 1));
    // Back from its wait, the select is off the other queue.
    assert_eq!(b.queued(), (0, 1));
    assert_eq!(try_value(&b, &d), Some(2));
    assert_eq!(d.woken(), [3]);
}

#[test]
fn a_select_completes_its_first_ready_arm_only() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 1), Channel::new(1, 1));
    assert!(matches!(a.try_send(Value::Int(1), &d), TrySend::Sent));
    assert!(matches!(b.try_send(Value::Int(2), &d), TrySend::Sent));

    let fired = d.block(1, select_recv(&a, &b)).expect("both are ready");
    assert_eq!(received(fired), (0, 1));
    assert_eq!((a.len(), b.len()), (0, 1));
    let fired = d.block(1, select_recv(&a, &b)).expect("b is ready");
    assert_eq!(received(fired), (1, 2));
}

#[test]
fn a_send_passes_over_a_select_that_another_arm_completed() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    assert!(d.block(1, select_recv(&a, &b)).is_none());
    assert!(d.block(2, recv(&b)).is_none());

    assert!(d.block(3, send(&a, 1)).is_some());
    // Task 1 is still first on `b`, with its wait over.
    assert_eq!(b.queued(), (2, 0));
    assert!(d.block(3, send(&b, 2)).is_some());
    assert_eq!(d.woken(), [1, 2]);
    assert_eq!(received(resumed(&d, 1)), (0, 1));
    assert_eq!(received(resumed(&d, 2)), (0, 2));
}

#[test]
fn the_send_and_the_receive_arm_of_one_select_do_not_meet() {
    let d = Double::new();
    let a = Channel::new(0, 0);
    let both = Wait::new(vec![
        Arm::Send(a.clone(), Value::Int(1)),
        Arm::Recv(a.clone()),
    ]);
    assert!(d.block(1, both).is_none());
    assert_eq!(a.waiting(), (1, 1));

    assert_eq!(received(d.block(2, recv(&a)).expect("a sender")), (0, 1));
    assert_eq!(d.woken(), [1]);
    assert!(matches!(resumed(&d, 1), Fired::Arm(0, Outcome::Sent)));
    assert_eq!(a.queued(), (0, 0));
}

#[test]
fn two_selects_that_cross_complete_one_arm_each() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    let first = Wait::new(vec![
        Arm::Send(a.clone(), Value::Int(1)),
        Arm::Recv(b.clone()),
    ]);
    let second = Wait::new(vec![
        Arm::Send(b.clone(), Value::Int(2)),
        Arm::Recv(a.clone()),
    ]);
    assert!(d.block(1, first).is_none());

    // The send on `b` comes first in the second select, and task 1
    // receives there: that arm is completed and the other is not.
    let fired = d.block(2, second).expect("task 1 receives on b");
    assert!(matches!(fired, Fired::Arm(0, Outcome::Sent)));
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (1, 2));
    // Task 1's value was never sent.
    assert_eq!((a.queued(), b.queued()), ((0, 0), (0, 0)));
    assert_eq!(try_value(&a, &d), None);
}

#[test]
fn a_select_on_one_channel_twice_is_completed_once() {
    let d = Double::new();
    let a = Channel::new(0, 0);
    assert!(d.block(1, select_recv(&a, &a)).is_none());
    assert_eq!(a.waiting(), (2, 0));
    assert!(d.block(2, send(&a, 1)).is_some());
    assert!(d.block(3, send(&a, 2)).is_none());
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (0, 1));
}

// ── Close ───────────────────────────────────────────────────────────

fn closed(fired: Fired) -> (usize, Close) {
    match fired {
        Fired::Arm(arm, Outcome::Closed(close)) => (arm, close),
        other => panic!("expected a close, got {other:?}"),
    }
}

#[test]
fn close_wakes_every_parked_receiver() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, recv(&ch)).is_none());
    assert!(d.block(2, recv(&ch)).is_none());

    assert!(ch.close(Close::default(), &d));
    assert_eq!(d.woken(), [1, 2]);
    assert!(closed(resumed(&d, 1)).1.failure.is_none());
    assert_eq!(closed(resumed(&d, 2)).0, 0);
    assert!(ch.is_closed());

    // Closed is what everybody finds from now on.
    assert!(matches!(ch.try_receive(&d), TryReceive::Closed(_)));
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Closed(_)));
    closed(d.block(3, recv(&ch)).expect("closed"));
    closed(d.block(3, send(&ch, 1)).expect("closed"));
    assert!(!ch.close(Close::default(), &d));
    assert!(d.woken().is_empty());
}

#[test]
fn close_wakes_parked_senders_and_their_values_are_not_sent() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, send(&ch, 1)).is_none());
    assert!(d.block(2, send(&ch, 2)).is_none());

    assert!(ch.close(Close::default(), &d));
    assert_eq!(d.woken(), [1, 2]);
    closed(resumed(&d, 1));
    closed(resumed(&d, 2));
    assert!(matches!(ch.try_receive(&d), TryReceive::Closed(_)));
}

#[test]
fn buffered_values_are_received_after_the_close() {
    let d = Double::new();
    let ch = Channel::new(0, 2);
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Sent));
    assert!(matches!(ch.try_send(Value::Int(2), &d), TrySend::Sent));
    assert!(d.block(1, send(&ch, 3)).is_none());

    assert!(ch.close(Close::default(), &d));
    assert_eq!(d.woken(), [1]);
    closed(resumed(&d, 1));
    assert_eq!(received(d.block(2, recv(&ch)).expect("buffered")), (0, 1));
    assert_eq!(try_value(&ch, &d), Some(2));
    assert!(matches!(ch.try_receive(&d), TryReceive::Closed(_)));
}

#[test]
fn a_close_carries_its_failure_to_every_receiver_and_the_first_close_stands() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, recv(&ch)).is_none());

    let failure = Some(Arc::new(VmError::new("division by zero".into())));
    assert!(ch.close(Close { failure }, &d));
    assert!(!ch.close(Close::default(), &d));

    let message = |close: Close| close.failure.expect("a failure").message.clone();
    assert_eq!(message(closed(resumed(&d, 1)).1), "division by zero");
    let TryReceive::Closed(close) = ch.try_receive(&d) else {
        panic!("closed");
    };
    assert_eq!(message(close), "division by zero");
}

#[test]
fn closing_both_channels_of_a_select_wakes_it_once() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    assert!(d.block(1, select_recv(&a, &b)).is_none());
    assert!(b.close(Close::default(), &d));
    assert!(a.close(Close::default(), &d));
    assert_eq!(d.woken(), [1]);
    assert_eq!(closed(resumed(&d, 1)).0, 1);
}

// ── Cancel ──────────────────────────────────────────────────────────

#[test]
fn a_cancelled_receiver_takes_no_value() {
    let d = Double::new();
    let (rendezvous, buffered) = (Channel::new(0, 0), Channel::new(1, 1));
    assert!(d.block(1, recv(&rendezvous)).is_none());
    assert!(d.block(2, recv(&buffered)).is_none());
    assert!(d.cancel(1));
    assert!(d.cancel(2));
    assert_eq!(d.woken(), [1, 2]);

    // Before either task has run again: nobody receives.
    assert!(matches!(
        rendezvous.try_send(Value::Int(1), &d),
        TrySend::Full(_)
    ));
    assert!(matches!(
        buffered.try_send(Value::Int(2), &d),
        TrySend::Sent
    ));
    assert_eq!(buffered.len(), 1);
    assert!(matches!(d.resume(1), Resumed::Cancelled));
    assert!(matches!(d.resume(2), Resumed::Cancelled));
    assert_eq!((rendezvous.queued(), buffered.queued()), ((0, 0), (0, 0)));
    assert!(d.woken().is_empty());
}

#[test]
fn a_value_passes_a_cancelled_receiver_and_reaches_the_next() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, recv(&ch)).is_none());
    assert!(d.block(2, recv(&ch)).is_none());
    assert!(d.cancel(1));
    assert_eq!(ch.waiting(), (1, 0));

    assert!(d.block(3, send(&ch, 9)).is_some());
    assert_eq!(d.woken(), [1, 2]);
    assert_eq!(received(resumed(&d, 2)), (0, 9));
}

#[test]
fn a_cancelled_sender_sends_nothing() {
    let d = Double::new();
    let (rendezvous, buffered) = (Channel::new(0, 0), Channel::new(1, 1));
    assert!(matches!(
        buffered.try_send(Value::Int(0), &d),
        TrySend::Sent
    ));
    assert!(d.block(1, send(&rendezvous, 1)).is_none());
    assert!(d.block(2, send(&buffered, 2)).is_none());
    assert!(d.cancel(1));
    assert!(d.cancel(2));

    assert_eq!(try_value(&rendezvous, &d), None);
    assert_eq!(try_value(&buffered, &d), Some(0));
    assert_eq!(try_value(&buffered, &d), None);
    assert_eq!(d.woken(), [1, 2]);
}

#[test]
fn a_cancel_after_the_wait_ended_changes_nothing() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    assert!(d.block(1, recv(&ch)).is_none());
    assert!(d.block(2, send(&ch, 4)).is_some());
    assert!(!d.cancel(1));
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (0, 4));

    assert!(d.block(3, recv(&ch)).is_none());
    assert!(d.cancel(3));
    assert!(!d.cancel(3));
    assert_eq!(d.woken(), [3]);
}

#[test]
fn a_cancelled_select_is_taken_off_every_queue_and_out_of_the_timer() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    let cell = Cell::<i64>::new();
    let wait = Wait::new(vec![
        Arm::Recv(a.clone()),
        Arm::Send(b.clone(), Value::Int(1)),
        Arm::Cell(cell.clone()),
    ])
    .deadline(d.timer.deadline_after(10 * MS));
    assert!(d.block(1, wait).is_none());
    assert_eq!((a.queued(), b.queued()), ((1, 0), (0, 1)));
    assert_eq!((cell.waiting(), d.timer.pending()), (1, 1));

    assert!(d.cancel(1));
    assert_eq!(
        (a.waiting(), b.waiting(), cell.waiting()),
        ((0, 0), (0, 0), 0)
    );
    // The scheduler drops a cancelled task without running it.
    d.drop_task(1);
    assert_eq!((a.queued(), b.queued()), ((0, 0), (0, 0)));
    assert_eq!(d.timer.pending(), 0);
    assert_eq!(d.advance(20 * MS), 0);
    assert_eq!(d.woken(), [1]);
}

#[test]
fn a_select_tries_its_arms_from_the_first_it_names() {
    let d = Double::new();
    let (a, b) = (Channel::new(0, 1), Channel::new(1, 1));
    for round in 0..4 {
        assert!(matches!(a.try_send(Value::Int(10), &d), TrySend::Sent));
        assert!(matches!(b.try_send(Value::Int(20), &d), TrySend::Sent));
        let fired = d
            .block(1, select_recv(&a, &b).first(round))
            .expect("both arms are ready");
        let (other, expected) = if round % 2 == 0 {
            (&b, (0, 10))
        } else {
            (&a, (1, 20))
        };
        assert_eq!(received(fired), expected);
        assert!(try_value(other, &d).is_some());
    }
}

// ── Between park and commit ─────────────────────────────────────────

fn park_only(d: &Double, task: u64, wait: Wait) -> Parked {
    match park(TaskId(task), wait, &d.timer, d) {
        Park::Parked(parked) => parked,
        Park::Ready(fired) => panic!("expected a park, got {fired:?}"),
        Park::Cancelled => panic!("expected a park, got a cancel"),
    }
}

#[test]
fn a_wait_that_ends_before_the_commit_wakes_nobody() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let parked = park_only(&d, 1, recv(&ch));
    assert_eq!(parked.sources().len(), 1);
    let parked = parked.finish().err().expect("the wait is open");

    // The double would fail the test on a wake: task 1 is not parked.
    assert!(matches!(ch.try_send(Value::Int(3), &d), TrySend::Sent));
    assert!(!parked.commit());
    let Ok(Resumed::Fired(fired)) = parked.finish() else {
        panic!("the wait has ended");
    };
    assert_eq!(received(fired), (0, 3));
}

#[test]
fn a_cancel_before_the_commit_wakes_nobody() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let parked = park_only(&d, 1, recv(&ch));
    assert!(parked.token().cancel(&d));
    assert!(!parked.commit());
    assert!(matches!(parked.finish(), Ok(Resumed::Cancelled)));
    assert!(matches!(ch.try_send(Value::Int(3), &d), TrySend::Full(_)));
}

#[test]
fn a_dropped_task_leaves_no_waiter_behind() {
    let d = Double::new();
    let (a, b, c) = (Channel::new(0, 0), Channel::new(1, 0), Channel::new(2, 0));
    assert!(d.block(1, select_recv(&a, &b)).is_none());
    assert!(d.block(2, send(&c, 1)).is_none());
    d.drop_task(1);
    d.drop_task(2);
    assert_eq!(
        (a.queued(), b.queued(), c.queued()),
        ((0, 0), (0, 0), (0, 0))
    );
    assert!(matches!(a.try_send(Value::Int(1), &d), TrySend::Full(_)));
    assert_eq!(try_value(&c, &d), None);
    assert!(d.woken().is_empty());
}

// ── Cells ───────────────────────────────────────────────────────────

fn join(cell: &Arc<Cell<i64>>) -> Wait {
    Wait::new(vec![Arm::Cell(cell.clone())])
}

#[test]
fn a_cell_wakes_every_waiter_once_and_keeps_its_first_value() {
    let d = Double::new();
    let cell = Cell::<i64>::new();
    assert!(d.block(1, join(&cell)).is_none());
    assert!(d.block(2, join(&cell)).is_none());
    assert_eq!(cell.get(), None);

    assert_eq!(cell.complete(42, &d), Ok(()));
    assert_eq!(d.woken(), [1, 2]);
    assert!(matches!(resumed(&d, 1), Fired::Arm(0, Outcome::Done)));
    assert!(matches!(resumed(&d, 2), Fired::Arm(0, Outcome::Done)));
    assert_eq!(cell.complete(43, &d), Err(43));
    assert_eq!(cell.get(), Some(&42));

    let fired = d.block(3, join(&cell)).expect("complete");
    assert!(matches!(fired, Fired::Arm(0, Outcome::Done)));
    assert!(d.woken().is_empty());
}

#[test]
fn a_cell_and_a_channel_in_one_wait_end_it_once() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let cell = Cell::<i64>::new();
    let wait = || Wait::new(vec![Arm::Cell(cell.clone()), Arm::Recv(ch.clone())]);

    assert!(d.block(1, wait()).is_none());
    assert!(d.block(2, send(&ch, 5)).is_some());
    assert_eq!(cell.complete(1, &d), Ok(()));
    assert_eq!(d.woken(), [1]);
    assert_eq!(received(resumed(&d, 1)), (1, 5));

    // The other way round: the cell first.
    let (ch, cell) = (Channel::new(1, 0), Cell::<i64>::new());
    let wait = Wait::new(vec![Arm::Cell(cell.clone()), Arm::Recv(ch.clone())]);
    assert!(d.block(1, wait).is_none());
    assert_eq!(cell.complete(1, &d), Ok(()));
    assert!(d.block(2, send(&ch, 5)).is_none());
    assert_eq!(d.woken(), [1]);
    assert!(matches!(resumed(&d, 1), Fired::Arm(0, Outcome::Done)));
}

// ── Timer ───────────────────────────────────────────────────────────

#[test]
fn a_deadline_ends_the_wait_when_the_clock_reaches_it() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let deadline = d.timer.deadline_after(10 * MS);
    assert!(d.block(1, recv(&ch).deadline(deadline)).is_none());
    assert_eq!(d.timer.pending(), 1);
    assert_eq!(d.timer.next_deadline(), deadline);

    assert_eq!(d.advance(9 * MS), 0);
    assert!(d.woken().is_empty());
    assert_eq!(d.advance(MS), 1);
    assert_eq!(d.woken(), [1]);
    assert_eq!(d.timer.pending(), 0);

    // Still on the channel's queue, and no receiver.
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Full(_)));
    assert!(matches!(resumed(&d, 1), Fired::Deadline));
    assert_eq!(ch.queued(), (0, 0));
}

#[test]
fn an_arm_that_wins_disarms_the_deadline() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let deadline = d.timer.deadline_after(10 * MS);
    assert!(d.block(1, recv(&ch).deadline(deadline)).is_none());
    assert!(d.block(2, send(&ch, 1)).is_some());
    // The entry counts until the task is back from its wait.
    assert_eq!(d.timer.pending(), 1);
    assert_eq!(received(resumed(&d, 1)), (0, 1));
    assert_eq!(d.timer.pending(), 0);
    assert_eq!(d.timer.real_wait(), None);
    assert_eq!(d.advance(20 * MS), 0);
    assert_eq!(d.woken(), [1]);
}

#[test]
fn a_deadline_that_has_passed_ends_the_wait_unless_an_arm_is_ready() {
    let d = Double::new();
    let ch = Channel::new(0, 1);
    d.clock.advance(5 * MS);
    let now = Some(d.timer.now());
    assert!(matches!(
        d.block(1, recv(&ch).deadline(now)),
        Some(Fired::Deadline)
    ));
    assert_eq!((ch.queued(), d.timer.pending()), ((0, 0), 0));

    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Sent));
    assert_eq!(
        received(d.block(1, recv(&ch).deadline(now)).unwrap()),
        (0, 1)
    );
}

#[test]
fn a_wait_with_no_arm_is_a_sleep_and_sleeps_end_in_deadline_order() {
    let d = Double::new();
    let sleep = |d: &Double, ms: u32| Wait::new(vec![]).deadline(d.timer.deadline_after(ms * MS));
    assert!(d.block(1, sleep(&d, 30)).is_none());
    assert!(d.block(2, sleep(&d, 10)).is_none());
    assert!(d.block(3, sleep(&d, 10)).is_none());
    assert_eq!(d.timer.pending(), 3);
    // On an embedder's clock the driver polls: only the clock knows
    // when its 10 ms have passed.
    assert_eq!(d.timer.real_wait(), Some(MS));

    assert_eq!(d.advance(10 * MS), 2);
    assert_eq!(d.woken(), [2, 3]);
    assert_eq!(d.advance(100 * MS), 1);
    assert_eq!(d.woken(), [1]);
    assert!(matches!(resumed(&d, 1), Fired::Deadline));
}

#[test]
fn the_timer_closes_a_channel_at_its_time() {
    let d = Double::new();
    let ch = Channel::new(0, 0);
    let deadline = d.timer.deadline_after(10 * MS).unwrap();
    let id = d.timer.close_at(deadline, ch.clone());
    assert_eq!(id.deadline(), deadline);
    assert!(d.block(1, recv(&ch)).is_none());

    assert_eq!(d.advance(10 * MS), 1);
    assert!(ch.is_closed());
    assert_eq!(d.woken(), [1]);
    closed(resumed(&d, 1));
    assert_eq!(d.timer.pending(), 0);
    assert!(!d.timer.disarm(id));

    let id = d.timer.close_at(d.timer.now() + MS, Channel::new(1, 0));
    assert!(d.timer.disarm(id));
    assert_eq!(d.timer.pending(), 0);
}

#[test]
fn a_clock_that_panicked_ends_every_timed_wait() {
    use crate::vm::{Buffer, Clock, HostIo};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Breaks(Arc<AtomicBool>);
    impl Clock for Breaks {
        fn now(&self) -> Duration {
            Duration::ZERO
        }
        fn monotonic(&self) -> Duration {
            assert!(!self.0.load(Ordering::SeqCst), "no time");
            Duration::ZERO
        }
        fn sleep(&self, _: Duration) {}
    }

    let d = Double::new();
    let broken = Arc::new(AtomicBool::new(false));
    let timer = Timer::new(HostIo::buffer(&Buffer::new()).clock(Breaks(broken.clone())));
    let ch = Channel::new(0, 0);
    let wait = recv(&ch).deadline(timer.deadline_after(Duration::from_secs(3600)));
    let Park::Parked(parked) = park(TaskId(1), wait, &timer, &d) else {
        panic!("nothing to receive");
    };
    assert_eq!(timer.fire_due(&d), 0);

    broken.store(true, Ordering::SeqCst);
    assert_eq!(timer.fire_due(&d), 1);
    assert!(matches!(
        parked.finish(),
        Ok(Resumed::Fired(Fired::Deadline))
    ));
    assert_eq!(timer.pending(), 0);
}

// ── The cancel flag ─────────────────────────────────────────────────

#[test]
fn a_wait_whose_flag_is_set_completes_no_arm() {
    use std::sync::atomic::AtomicBool;

    let d = Double::new();
    let ch = Channel::new(0, 1);
    assert!(matches!(ch.try_send(Value::Int(1), &d), TrySend::Sent));
    let flag = Arc::new(AtomicBool::new(true));
    let wait = recv(&ch).cancel(Some(flag));
    assert!(matches!(
        park(TaskId(1), wait, &d.timer, &d),
        Park::Cancelled
    ));
    assert_eq!((ch.len(), ch.queued()), (1, (0, 0)));
}

#[test]
fn nothing_is_completed_for_a_waiter_once_its_flag_is_set() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let d = Double::new();
    let ch = Channel::new(0, 0);
    let cell = Cell::<i64>::new();
    let flag = Arc::new(AtomicBool::new(false));
    let wait = Wait::new(vec![Arm::Recv(ch.clone()), Arm::Cell(cell.clone())])
        .deadline(d.timer.deadline_after(MS))
        .cancel(Some(flag.clone()));
    assert!(d.block(1, wait).is_none());
    assert!(d.block(2, recv(&ch)).is_none());

    // The canceller has set the flag and not yet ended the wait: the
    // value passes the task and reaches the next receiver, and neither
    // the cell nor the deadline ends its wait.
    flag.store(true, Ordering::SeqCst);
    assert!(matches!(ch.try_send(Value::Int(5), &d), TrySend::Sent));
    assert!(cell.complete(1, &d).is_ok());
    assert_eq!(d.advance(2 * MS), 1);
    assert_eq!(d.woken(), [2]);
    assert_eq!(received(resumed(&d, 2)), (0, 5));

    assert!(d.cancel(1));
    assert_eq!(d.woken(), [1]);
    assert!(matches!(d.resume(1), Resumed::Cancelled));
}

// ── The parked tasks ────────────────────────────────────────────────

/// A registry on the double's timer whose tasks are numbers, and the
/// tasks it has handed back.
type Ready = Arc<parking_lot::Mutex<Vec<(u64, Resumed)>>>;

fn parking(d: &Double) -> (Arc<Parking<u64>>, Ready) {
    let ready = Ready::default();
    let sink = ready.clone();
    let parking = Parking::new(d.timer.clone(), move |task, resumed| {
        sink.lock().push((task, resumed));
    });
    (Arc::new(parking), ready)
}

/// Park task `task` with its flag clear.
fn rest(p: &Parking<u64>, task: u64, wait: Wait) -> Option<Resumed> {
    p.park(TaskId(task), task, wait, || false)
        .map(|(back, resumed)| {
            assert_eq!(back, task);
            resumed
        })
}

/// The one task handed back since the last call.
fn handed_back(ready: &Ready) -> (u64, Resumed) {
    let mut ready = std::mem::take(&mut *ready.lock());
    assert_eq!(ready.len(), 1, "one task is handed back");
    ready.pop().expect("one task")
}

#[test]
fn a_parked_task_is_handed_back_with_what_its_wait_ended_on() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 0);
    assert!(rest(&p, 1, recv(&ch)).is_none());
    assert!(p.is_parked(TaskId(1)));
    assert_eq!(p.waiting(), 1);

    // The sender's wait is over at once: it keeps its task.
    let Some(Resumed::Fired(sent)) = rest(&p, 2, send(&ch, 7)) else {
        panic!("a receiver is parked");
    };
    assert!(matches!(sent, Fired::Arm(0, Outcome::Sent)));
    let (task, Resumed::Fired(fired)) = handed_back(&ready) else {
        panic!("task 1 was not cancelled");
    };
    assert_eq!((task, received(fired)), (1, (0, 7)));
    assert_eq!((p.waiting(), ch.queued()), (0, (0, 0)));
    assert!(!p.is_parked(TaskId(1)));
}

#[test]
fn a_cancelled_parked_task_is_handed_back_and_takes_nothing() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 2);
    let wait = recv(&ch).deadline(d.timer.deadline_after(10 * MS));
    assert!(rest(&p, 1, wait).is_none());

    assert!(p.cancel(TaskId(1)));
    assert!(matches!(handed_back(&ready), (1, Resumed::Cancelled)));
    // It is off the queue and out of the timer, and the values stay.
    assert_eq!(
        (ch.queued(), d.timer.pending(), p.waiting()),
        ((0, 0), 0, 0)
    );
    assert!(matches!(ch.try_send(Value::Int(1), &*p), TrySend::Sent));
    assert!(matches!(ch.try_send(Value::Int(2), &*p), TrySend::Sent));
    assert_eq!(ch.len(), 2);
    // A second cancel, and a cancel of a task that is not parked.
    assert!(!p.cancel(TaskId(1)));
    assert!(!p.cancel(TaskId(9)));
    assert!(ready.lock().is_empty());
}

#[test]
fn a_task_whose_flag_is_set_completes_no_arm() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 2);
    assert!(matches!(ch.try_send(Value::Int(1), &*p), TrySend::Sent));

    let back = p.park(TaskId(1), 1, recv(&ch), || true);
    assert!(matches!(back, Some((1, Resumed::Cancelled))));
    assert_eq!((ch.len(), ch.queued(), p.waiting()), (1, (0, 0), 0));
    assert!(ready.lock().is_empty());
}

#[test]
fn a_cancel_that_misses_the_registry_is_found_by_the_flag() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 0);
    // The flag is clear when the task starts to park and set when it
    // is in the registry: the cancel came in between and found no
    // entry.
    let reads = AtomicUsize::new(0);
    let flag = || {
        let set = reads.fetch_add(1, Ordering::SeqCst) > 0;
        if set {
            assert!(!p.is_parked(TaskId(1)), "not given up yet");
        } else {
            assert!(!p.cancel(TaskId(1)));
        }
        set
    };
    let back = p.park(TaskId(1), 1, recv(&ch), flag);
    assert!(matches!(back, Some((1, Resumed::Cancelled))));
    assert_eq!((ch.queued(), p.waiting()), ((0, 0), 0));
    assert!(matches!(ch.try_send(Value::Int(1), &*p), TrySend::Full(_)));
    assert!(ready.lock().is_empty());
}

#[test]
fn a_wait_that_ends_while_the_task_is_parked_keeps_the_task() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 0);
    // A sender on another thread gets in after the task is on the
    // queue and before its worker has given it up.
    let reads = AtomicUsize::new(0);
    let flag = || {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            assert!(matches!(ch.try_send(Value::Int(5), &*p), TrySend::Sent));
        }
        false
    };
    let Some((1, Resumed::Fired(fired))) = p.park(TaskId(1), 1, recv(&ch), flag) else {
        panic!("the wait ended before the task was given up");
    };
    assert_eq!(received(fired), (0, 5));
    assert_eq!((ch.queued(), p.waiting()), ((0, 0), 0));
    assert!(ready.lock().is_empty());
}

#[test]
fn a_deadline_hands_the_task_back() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 0);
    let wait = recv(&ch).deadline(d.timer.deadline_after(10 * MS));
    assert!(rest(&p, 1, wait).is_none());

    d.clock.advance(10 * MS);
    assert_eq!(d.timer.fire_due(&*p), 1);
    assert!(matches!(
        handed_back(&ready),
        (1, Resumed::Fired(Fired::Deadline))
    ));
    assert_eq!((ch.queued(), d.timer.pending()), ((0, 0), 0));
}

/// A wait with a deadline leaves no timer entry behind, whichever way
/// its task ends before the deadline: cancelled before it parks,
/// cancelled between the park and the registry, its wait over before
/// it is given up, cancelled while parked, or dropped at shutdown.
#[test]
fn a_deadline_entry_does_not_outlive_its_wait() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let d = Double::new();
    let (p, ready) = parking(&d);
    let ch = Channel::new(0, 0);
    let timed = || recv(&ch).deadline(d.timer.deadline_after(10 * MS));
    let left = || (d.timer.pending(), ch.queued(), p.waiting());
    let nothing = (0, (0, 0), 0);

    // The flag is set when the task gets to its wait.
    let flag = Arc::new(AtomicBool::new(true));
    let back = p.park(TaskId(1), 1, timed().cancel(Some(flag)), || false);
    assert!(matches!(back, Some((1, Resumed::Cancelled))));
    assert_eq!(left(), nothing);

    // The cancel comes while the task is on its way into the registry.
    let reads = AtomicUsize::new(0);
    let back = p.park(TaskId(2), 2, timed(), || {
        reads.fetch_add(1, Ordering::SeqCst) > 0
    });
    assert!(matches!(back, Some((2, Resumed::Cancelled))));
    assert_eq!(left(), nothing);

    // The wait is over before the task is given up.
    let reads = AtomicUsize::new(0);
    let back = p.park(TaskId(3), 3, timed(), || {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            assert!(matches!(ch.try_send(Value::Int(5), &*p), TrySend::Sent));
        }
        false
    });
    assert!(matches!(back, Some((3, Resumed::Fired(_)))));
    assert_eq!(left(), nothing);

    // Cancelled while parked.
    assert!(rest(&p, 4, timed()).is_none());
    assert_eq!(d.timer.pending(), 1);
    assert!(p.cancel(TaskId(4)));
    assert!(matches!(handed_back(&ready), (4, Resumed::Cancelled)));
    assert_eq!(left(), nothing);

    // Dropped at shutdown.
    assert!(rest(&p, 5, timed()).is_none());
    assert_eq!(p.shutdown(), [(TaskId(5), 5)]);
    assert_eq!(left(), nothing);
    assert_eq!(d.advance(20 * MS), 0);
    assert!(ready.lock().is_empty());
}

#[test]
fn shutdown_gives_every_parked_task_and_leaves_nothing_queued() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));
    let cell = Cell::<i64>::new();
    assert!(rest(&p, 2, send(&a, 1)).is_none());
    let wait = Wait::new(vec![Arm::Recv(b.clone()), Arm::Cell(cell.clone())])
        .deadline(d.timer.deadline_after(10 * MS));
    assert!(rest(&p, 1, wait).is_none());

    let tasks = p.shutdown();
    assert_eq!(tasks, [(TaskId(1), 1), (TaskId(2), 2)]);
    assert_eq!((a.queued(), b.queued()), ((0, 0), (0, 0)));
    assert_eq!((cell.waiting(), d.timer.pending(), p.waiting()), (0, 0, 0));
    // Nothing reaches a task that is gone, and nothing parks any more.
    assert_eq!(try_value(&a, &d), None);
    assert!(cell.complete(1, &*p).is_ok());
    assert!(matches!(rest(&p, 3, recv(&b)), Some(Resumed::Cancelled)));
    assert_eq!(b.queued(), (0, 0));
    assert!(p.shutdown().is_empty());
    assert!(ready.lock().is_empty());
}

#[test]
fn tasks_are_stuck_when_all_wait_and_nothing_is_pending() {
    let d = Double::new();
    let (p, ready) = parking(&d);
    let (a, b) = (Channel::new(4, 0), Channel::new(5, 0));
    let stuck = |live: usize, external: usize| p.stuck(|| live, || external);
    assert!(stuck(0, 0).is_none(), "nothing waits");

    assert!(rest(&p, 2, send(&b, 1)).is_none());
    assert!(stuck(2, 0).is_none(), "task 1 runs");
    assert!(rest(&p, 1, select_recv(&a, &a)).is_none());
    assert!(stuck(3, 0).is_none(), "a third task runs");
    assert!(stuck(2, 1).is_none(), "an I/O operation is in flight");

    let tasks = stuck(2, 0).expect("both tasks wait on each other");
    let on: Vec<(u64, Vec<String>)> = tasks
        .iter()
        .map(|stuck| {
            let on = stuck.on.iter().map(|source| match source {
                Source::Recv(channel) => format!("receive {}", channel.id()),
                Source::Send(channel) => format!("send {}", channel.id()),
                Source::Cell(_) => "cell".to_string(),
            });
            (stuck.task.0, on.collect())
        })
        .collect();
    assert_eq!(
        on,
        [
            (1, vec!["receive 4".to_string(), "receive 4".to_string()]),
            (2, vec!["send 5".to_string()]),
        ]
    );

    // A timer that will close a channel can still end a wait.
    let id = d
        .timer
        .close_at(d.timer.deadline_after(MS).expect("in range"), a.clone());
    assert!(stuck(2, 0).is_none(), "a timer is armed");
    assert!(d.timer.disarm(id));
    assert!(stuck(2, 0).is_some());

    // A wait with a deadline is never stuck; when the deadline has
    // handed its task back, the other is.
    assert!(p.cancel(TaskId(1)));
    assert!(matches!(handed_back(&ready), (1, Resumed::Cancelled)));
    let wait = recv(&a).deadline(d.timer.deadline_after(MS));
    assert!(rest(&p, 1, wait).is_none());
    assert!(stuck(2, 0).is_none(), "task 1 has a deadline");
    d.clock.advance(MS);
    assert_eq!(d.timer.fire_due(&*p), 1);
    assert!(stuck(2, 0).is_none(), "task 1 runs again");
    assert_eq!(stuck(1, 0).expect("task 1 has ended").len(), 1);
}

// ── Threads ─────────────────────────────────────────────────────────

/// Four threads cross two rendezvous channels with selects whose arms
/// are in opposite orders. Every value sent is received exactly once,
/// and the test ends: no lock order is violated and no wake is lost.
#[test]
fn selects_on_threads_deliver_every_value_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, Thread};

    const ROUNDS: i64 = 2_000;

    /// Each task is a thread; a wake unparks it.
    struct Threads {
        threads: parking_lot::Mutex<Vec<Option<Thread>>>,
        woken: Vec<AtomicBool>,
    }
    impl Wake for Threads {
        fn wake(&self, task: TaskId) {
            let task = task.0 as usize;
            assert!(!self.woken[task].swap(true, Ordering::SeqCst), "two wakes");
            let thread = self.threads.lock()[task].clone();
            thread.expect("a task's thread is known").unpark();
        }
    }
    impl Threads {
        fn wait(&self, task: usize, wait: Wait, timer: &Arc<Timer>) -> Fired {
            let parked = match park(TaskId(task as u64), wait, timer, self) {
                Park::Ready(fired) => return fired,
                Park::Parked(parked) => parked,
                Park::Cancelled => panic!("nobody cancels"),
            };
            if parked.commit() {
                while !self.woken[task].swap(false, Ordering::SeqCst) {
                    thread::park();
                }
            }
            match parked.finish() {
                Ok(Resumed::Fired(fired)) => fired,
                _ => panic!("woken with the wait open, or cancelled"),
            }
        }
    }

    let sched = Arc::new(Threads {
        threads: parking_lot::Mutex::new(vec![None; 4]),
        woken: (0..4).map(|_| AtomicBool::new(false)).collect(),
    });
    let timer = Double::new().timer;
    let (a, b) = (Channel::new(0, 0), Channel::new(1, 0));

    let handles: Vec<_> = (0..4usize)
        .map(|task| {
            let (sched, timer) = (sched.clone(), timer.clone());
            let (first, second) = if task % 2 == 0 {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            };
            thread::spawn(move || {
                sched.threads.lock()[task] = Some(thread::current());
                let mut got = Vec::new();
                for round in 0..ROUNDS {
                    if task < 2 {
                        let value = Value::Int(task as i64 * ROUNDS + round);
                        let arms = vec![
                            Arm::Send(first.clone(), value.clone()),
                            Arm::Send(second.clone(), value),
                        ];
                        let fired = sched.wait(task, Wait::new(arms), &timer);
                        assert!(matches!(fired, Fired::Arm(_, Outcome::Sent)));
                    } else {
                        let arms = vec![Arm::Recv(first.clone()), Arm::Recv(second.clone())];
                        got.push(received(sched.wait(task, Wait::new(arms), &timer)).1);
                    }
                }
                got
            })
        })
        .collect();

    let mut got: Vec<i64> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("no thread panicked"))
        .collect();
    got.sort_unstable();
    assert_eq!(got, (0..2 * ROUNDS).collect::<Vec<_>>());
    assert_eq!((a.queued(), b.queued()), ((0, 0), (0, 0)));
}
