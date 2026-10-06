//! The token of one wait, and the scheduler's side of a wake.

use parking_lot::{Mutex, MutexGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::channel::Close;
use super::queue::Wakes;
use crate::value::Value;

/// The scheduler's name for a task.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

/// The scheduler, as this module sees it.
pub trait Wake: Send + Sync {
    /// The wait of the parked task `task` has ended: count it as
    /// runnable and queue it. Called once for each park that
    /// [`Parked::commit`](super::Parked::commit) confirmed, from
    /// whichever thread ended the wait, with no lock of this module
    /// held.
    fn wake(&self, task: TaskId);
}

/// How an arm of a wait was completed.
#[derive(Debug)]
pub enum Outcome {
    /// A receive arm took this value.
    Received(Value),
    /// A send arm's value was taken by the channel.
    Sent,
    /// The arm's channel is closed: a receive arm found it closed and
    /// empty; a send arm's value was not sent.
    Closed(Close),
    /// A cell arm's cell is complete.
    Done,
}

/// How a wait ended.
#[derive(Debug)]
pub enum Fired {
    /// The arm at this index of [`Wait::arms`](super::Wait) was
    /// completed.
    Arm(usize, Outcome),
    /// The deadline passed first.
    Deadline,
}

/// What a task finds when it comes back from a park.
#[derive(Debug)]
pub enum Resumed {
    Fired(Fired),
    /// The task was cancelled while it waited. No arm was completed:
    /// it took no value and sent none.
    Cancelled,
}

enum State {
    /// On the queues, while the task's worker has not yet given the
    /// task up: ending the wait now wakes nobody.
    Armed,
    /// On the queues, the task parked: ending the wait wakes it.
    Parked,
    Fired(Fired),
    Cancelled,
    /// Fired, and the task has taken the result.
    Taken,
}

/// One wait of one task. It sits on the queue of every arm of the wait
/// and in the timer, and the first that claims it ends the wait: every
/// later one finds it no longer waiting and passes over it.
pub struct Token {
    task: TaskId,
    /// The cancel flag of the task ([`Wait::cancel`](super::Wait)).
    /// Once it is set nobody completes an arm for the task: its wait
    /// ends only by the cancel.
    cancel: Option<Arc<AtomicBool>>,
    state: Mutex<State>,
}

impl Token {
    pub(super) fn new(task: TaskId, cancel: Option<Arc<AtomicBool>>) -> Arc<Token> {
        Arc::new(Token {
            task,
            cancel,
            state: Mutex::new(State::Armed),
        })
    }

    /// The task that waits.
    pub fn task(&self) -> TaskId {
        self.task
    }

    /// Whether the wait is still open.
    pub fn is_waiting(&self) -> bool {
        matches!(*self.state.lock(), State::Armed | State::Parked)
    }

    /// Hold the token so that nobody else can end the wait; `None` when
    /// the wait has ended. The caller holds the lock of the queue it
    /// found the token on, completes the operation, and then calls
    /// [`Claim::fire`].
    pub(super) fn claim(&self) -> Option<Claim<'_>> {
        if is_set(&self.cancel) {
            return None;
        }
        let state = self.state.lock();
        matches!(*state, State::Armed | State::Parked).then_some(Claim {
            task: self.task,
            state,
        })
    }

    /// End the wait because its task is cancelled. `false` when the
    /// wait had already ended: an arm that was completed stays
    /// completed. Whoever owns the task's [`Parked`](super::Parked)
    /// drops it, which takes the token off its queues.
    pub fn cancel(&self, wake: &dyn Wake) -> bool {
        let parked = {
            let mut state = self.state.lock();
            let parked = match *state {
                State::Armed => false,
                State::Parked => true,
                State::Fired(_) | State::Cancelled | State::Taken => return false,
            };
            *state = State::Cancelled;
            parked
        };
        if parked {
            wake.wake(self.task);
        }
        true
    }

    /// End the wait without a wake: its task is gone.
    pub(super) fn abandon(&self) {
        let mut state = self.state.lock();
        if matches!(*state, State::Armed | State::Parked) {
            *state = State::Cancelled;
        }
    }

    /// See [`Parked::commit`](super::Parked::commit).
    pub(super) fn commit(&self) -> bool {
        let mut state = self.state.lock();
        match *state {
            State::Armed => {
                *state = State::Parked;
                true
            }
            State::Parked => true,
            State::Fired(_) | State::Cancelled | State::Taken => false,
        }
    }

    /// How the wait ended; `None` while it is open.
    pub(super) fn take(&self) -> Option<Resumed> {
        let mut state = self.state.lock();
        match *state {
            State::Armed | State::Parked => None,
            State::Cancelled => Some(Resumed::Cancelled),
            State::Fired(_) => match std::mem::replace(&mut *state, State::Taken) {
                State::Fired(fired) => Some(Resumed::Fired(fired)),
                _ => unreachable!(),
            },
            State::Taken => unreachable!("the result of a wait is taken once"),
        }
    }
}

/// Whether a task's cancel flag is set.
pub(super) fn is_set(cancel: &Option<Arc<AtomicBool>>) -> bool {
    cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
}

/// A token whose wait only the holder can end.
pub(super) struct Claim<'a> {
    task: TaskId,
    state: MutexGuard<'a, State>,
}

impl Claim<'_> {
    /// End the wait with `fired`. If the task is parked, it is added to
    /// `wakes`.
    pub(super) fn fire(mut self, fired: Fired, wakes: &mut Wakes) {
        if matches!(*self.state, State::Parked) {
            wakes.push(self.task);
        }
        *self.state = State::Fired(fired);
    }
}
