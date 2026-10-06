//! What a task waits for, and the park that starts the wait.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::cell::Completion;
use super::channel::{Channel, TryReceive, TrySend};
use super::queue::{Waiter, Wakes};
use super::timer::{Timer, TimerId};
use super::token::{Fired, Outcome, Resumed, TaskId, Token, Wake, is_set};
use crate::value::Value;

/// One thing a wait can end on.
pub enum Arm {
    /// A value from the channel, or its close.
    Recv(Arc<Channel>),
    /// The channel takes the value, or is closed.
    Send(Arc<Channel>, Value),
    /// The cell is complete.
    Cell(Arc<dyn Completion>),
}

/// What a task waits for: the first arm that can be completed, or the
/// deadline if it comes first. A receive, a send or a join is a wait
/// with one arm; a `select` has several; a sleep has none and a
/// deadline. With no arm and no deadline the wait ends only by a
/// cancel.
pub struct Wait {
    pub arms: Vec<Arm>,
    /// A reading of the clock's monotonic time (see
    /// [`Timer::deadline_after`]).
    pub deadline: Option<Duration>,
    /// The arm that is tried first; the others follow in their order,
    /// around the end. A `select` picks it at random, so that of
    /// several arms that are ready none is always the one taken.
    pub first: usize,
    /// The cancel flag of the task that waits. Whoever cancels the
    /// task sets it first. From then on no arm of the wait is
    /// completed, by the task or for it: a cancelled task takes
    /// nothing and sends nothing, even while it is on its way into the
    /// wait.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Wait {
    pub fn new(arms: Vec<Arm>) -> Wait {
        Wait {
            arms,
            deadline: None,
            first: 0,
            cancel: None,
        }
    }

    pub fn deadline(self, deadline: Option<Duration>) -> Wait {
        Wait { deadline, ..self }
    }

    pub fn first(self, first: usize) -> Wait {
        Wait { first, ..self }
    }

    pub fn cancel(self, cancel: Option<Arc<AtomicBool>>) -> Wait {
        Wait { cancel, ..self }
    }
}

/// An arm without its value: what a parked task is queued on.
#[derive(Clone)]
pub enum Source {
    Recv(Arc<Channel>),
    Send(Arc<Channel>),
    Cell(Arc<dyn Completion>),
}

impl Source {
    fn channel(&self) -> Option<&Arc<Channel>> {
        match self {
            Source::Recv(channel) | Source::Send(channel) => Some(channel),
            Source::Cell(_) => None,
        }
    }
}

/// The result of [`park`].
pub enum Park {
    /// The wait ended at once: the task goes on.
    Ready(Fired),
    /// The task is on the queues of its arms.
    Parked(Parked),
    /// The task's cancel flag is set: nothing was completed.
    Cancelled,
}

/// Start the wait `wait` of `task`.
///
/// The arms are tried in their order, from [`Wait::first`] on, and the
/// first that can be completed now is completed: the wait is over. If none can and the
/// deadline has passed, the wait ends with [`Fired::Deadline`].
/// Otherwise the task is queued on every arm and in `timer`; see the
/// [module documentation](super) for what the scheduler does with the
/// [`Parked`].
///
/// With the task's cancel flag set ([`Wait::cancel`]) nothing is
/// tried: the result is [`Park::Cancelled`].
pub fn park(task: TaskId, wait: Wait, timer: &Arc<Timer>, wake: &dyn Wake) -> Park {
    let Wait {
        arms,
        deadline,
        first,
        cancel,
    } = wait;
    let mut payloads = Vec::with_capacity(arms.len());
    let mut sources = Vec::with_capacity(arms.len());
    for arm in arms {
        let (source, payload) = match arm {
            Arm::Recv(channel) => (Source::Recv(channel), None),
            Arm::Send(channel, value) => (Source::Send(channel), Some(value)),
            Arm::Cell(cell) => (Source::Cell(cell), None),
        };
        sources.push(source);
        payloads.push(payload);
    }
    // The clock is an embedder's code: it is not called under a lock.
    let expired = deadline.is_some_and(|deadline| timer.now() >= deadline);

    // All channels of the wait are locked together, in address order,
    // from the first look at an arm until the task is on every queue.
    // So while this task completes an arm no peer can find it on the
    // queue of another, and ending a wait never takes more than one
    // claim: that of the peer.
    let mut channels: Vec<&Arc<Channel>> = sources.iter().filter_map(Source::channel).collect();
    channels.sort_by_key(|channel| Arc::as_ptr(channel));
    channels.dedup_by(|a, b| Arc::ptr_eq(a, b));
    let lock_of = |channel: &Arc<Channel>| {
        channels
            .binary_search_by_key(&Arc::as_ptr(channel), |c| Arc::as_ptr(c))
            .expect("every channel of the wait is locked")
    };
    let mut states: Vec<_> = channels.iter().map(|c| c.state.lock()).collect();

    // Read under the locks: an operation on one of the channels that
    // comes after the cancel finds that this task took nothing.
    if is_set(&cancel) {
        return Park::Cancelled;
    }

    let mut wakes = Wakes::default();
    let mut ready = None;
    let count = sources.len();
    for arm in (0..count).map(|i| (first + i) % count) {
        let source = &sources[arm];
        let outcome = match source {
            Source::Recv(channel) => match states[lock_of(channel)].receive(&mut wakes) {
                TryReceive::Value(value) => Some(Outcome::Received(value)),
                TryReceive::Closed(close) => Some(Outcome::Closed(close)),
                TryReceive::Empty => None,
            },
            Source::Send(channel) => {
                let value = payloads[arm].take().expect("a send arm has its value");
                match states[lock_of(channel)].send(value, &mut wakes) {
                    TrySend::Sent => Some(Outcome::Sent),
                    TrySend::Closed(close) => Some(Outcome::Closed(close)),
                    TrySend::Full(value) => {
                        payloads[arm] = Some(value);
                        None
                    }
                }
            }
            Source::Cell(cell) => cell.is_done().then_some(Outcome::Done),
        };
        if let Some(outcome) = outcome {
            ready = Some(Fired::Arm(arm, outcome));
            break;
        }
    }
    if ready.is_none() && expired {
        ready = Some(Fired::Deadline);
    }
    if let Some(fired) = ready {
        drop(states);
        wakes.send(wake);
        return Park::Ready(fired);
    }

    let token = Token::new(task, cancel);
    for (arm, source) in sources.iter().enumerate() {
        let waiter = Waiter {
            token: token.clone(),
            arm,
            payload: payloads[arm].take(),
        };
        match source {
            Source::Recv(channel) => states[lock_of(channel)].recvq.push_back(waiter),
            Source::Send(channel) => states[lock_of(channel)].sendq.push_back(waiter),
            Source::Cell(_) => {}
        }
    }
    drop(states);
    // From here a peer may end the wait at any moment. A cell that was
    // completed since the look above ends it here; the token is not
    // parked yet, so nothing of this reaches `wakes`.
    for (arm, source) in sources.iter().enumerate() {
        if let Source::Cell(cell) = source {
            let waiter = Waiter {
                token: token.clone(),
                arm,
                payload: None,
            };
            cell.enqueue(waiter, &mut wakes);
        }
    }
    let timer = deadline.map(|deadline| (timer.clone(), timer.fire_at(deadline, token.clone())));
    Park::Parked(Parked {
        token,
        sources,
        timer,
    })
}

/// A task's place on the queues of its wait. The task owns it while it
/// waits; dropping it takes the task off every queue and out of the
/// timer.
pub struct Parked {
    token: Arc<Token>,
    sources: Vec<Source>,
    timer: Option<(Arc<Timer>, TimerId)>,
}

impl Parked {
    /// The wait's token: what a cancel of the task needs
    /// ([`Token::cancel`]).
    pub fn token(&self) -> &Arc<Token> {
        &self.token
    }

    /// What the task is queued on, in the order of the wait's arms.
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The wait's deadline.
    pub fn deadline(&self) -> Option<Duration> {
        self.timer.as_ref().map(|(_, id)| id.deadline())
    }

    /// Give the task up. `true`: it is parked, and one
    /// [`Wake::wake`] will name it when its wait ends. `false`: the
    /// wait has ended already and no wake comes; the caller still has
    /// the task and goes on with [`Parked::finish`].
    ///
    /// The caller has already put the task where a wake finds it and
    /// counted it as not runnable: the wake can come before `commit`
    /// returns.
    pub fn commit(&self) -> bool {
        self.token.commit()
    }

    /// How the wait ended. While it is still open the `Parked` comes
    /// back.
    pub fn finish(self) -> Result<Resumed, Parked> {
        match self.token.take() {
            Some(resumed) => Ok(resumed),
            None => Err(self),
        }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        // A task that is dropped while it waits takes nothing more.
        self.token.abandon();
        for source in &self.sources {
            match source {
                Source::Recv(channel) | Source::Send(channel) => channel.forget(&self.token),
                Source::Cell(cell) => cell.forget(&self.token),
            }
        }
        if let Some((timer, id)) = self.timer.take() {
            timer.disarm(id);
        }
    }
}
