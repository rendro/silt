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
    /// task sets it first. From then on the task waits for nothing,
    /// and nobody completes an arm for it: a wait that is open ends
    /// only by the cancel, and one that begins is not queued
    /// ([`Park::Cancelled`]). And the task takes nothing new: of a
    /// wait that begins, a send that can be done at once is done (the
    /// task gives what it holds) and a cell that is complete is found
    /// so, but no receive is tried, whether or not a value is there.
    /// A wait that begins when the time of its task is over
    /// ([`Wait::task_deadline`]) is under the same rule.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The time at which the task that waits is over (a reading like
    /// [`Wait::deadline`]): the task of a function that
    /// `task.deadline` runs has one. It ends the wait like the wait's
    /// own deadline, in the same claim of the same token, so the wait
    /// ends by an arm or by it and never by both; but the task does
    /// not go on then ([`Resumed::Expired`](super::Resumed)). The
    /// scheduler puts it on the waits of the task, like the cancel
    /// flag.
    pub task_deadline: Option<Duration>,
    /// `false` for the one wait that the deadline of its task does not
    /// end ([`Wait::unbounded`]).
    pub bounded: bool,
}

impl Wait {
    pub fn new(arms: Vec<Arm>) -> Wait {
        Wait {
            arms,
            deadline: None,
            first: 0,
            cancel: None,
            task_deadline: None,
            bounded: true,
        }
    }

    /// A wait that the deadline of its task does not end: what it
    /// waits for ends by that time itself. The wait of `task.deadline`
    /// for the task of its function is one: that task's time is no
    /// later than the caller's.
    pub fn unbounded(self) -> Wait {
        Wait {
            bounded: false,
            ..self
        }
    }

    /// The wait of a task whose time is over at `task_deadline`,
    /// unless the wait is [unbounded](Wait::unbounded).
    pub fn task_deadline(self, task_deadline: Option<Duration>) -> Wait {
        match self.bounded {
            true => Wait {
                task_deadline,
                ..self
            },
            false => self,
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
    /// Whether this is a wait on a channel that a timer will close.
    pub fn closes_by_timer(&self) -> bool {
        self.channel()
            .is_some_and(|channel| channel.closes_by_timer())
    }

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
    /// The task's cancel flag was set when the wait began
    /// ([`Wait::cancel`]) and none of the arms that are tried then (a
    /// send, a cell) could be completed at once: nothing was taken or
    /// sent.
    Cancelled,
    /// The time of the task was over when the wait began
    /// ([`Wait::task_deadline`]) and none of the arms that are tried
    /// then (a send, a cell) could be completed at once: nothing was
    /// taken or sent.
    Expired,
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
/// When the task's cancel flag is set ([`Wait::cancel`]), or the time
/// of the task is over ([`Wait::task_deadline`]), the task takes
/// nothing new and waits for nothing: its receive arms are not tried
/// (`takes_nothing_new`, below), a send that can be done at once is
/// done and a cell that is complete is found so, and if none of those
/// ends the wait the task is not queued: [`Park::Cancelled`],
/// [`Park::Expired`]. The two differ in the name of the refusal only.
pub fn park(task: TaskId, wait: Wait, timer: &Arc<Timer>, wake: &dyn Wake) -> Park {
    let Wait {
        mut arms,
        deadline,
        first,
        cancel,
        task_deadline,
        bounded: _,
    } = wait;
    // The clock is an embedder's code: it is not called under a lock.
    let now = (deadline.is_some() || task_deadline.is_some()).then(|| timer.now());
    let over = |at: Option<Duration>| at.is_some_and(|at| now.is_some_and(|now| now >= at));
    let (expired, task_expired) = (over(deadline), over(task_deadline));
    let times = Times {
        deadline,
        expired,
        task_deadline,
        task_expired,
    };
    // The usual wait: one send or one receive.
    if arms.len() == 1 && !matches!(arms[0], Arm::Cell(_)) {
        let arm = arms.pop().expect("one arm");
        return park_one(task, arm, times, cancel, timer, wake);
    }
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
    // comes after this look finds the task on the queue, with its
    // flag, or not there at all.
    let cancelled = is_set(&cancel);
    let no_takes = takes_nothing_new(cancelled, task_expired);

    let mut wakes = Wakes::default();
    let mut ready = None;
    let count = sources.len();
    for arm in (0..count).map(|i| (first + i) % count) {
        let source = &sources[arm];
        let outcome = match source {
            Source::Recv(_) if no_takes => None,
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
    if ready.is_none() && cancelled {
        return Park::Cancelled;
    }
    if ready.is_none() && task_expired {
        return Park::Expired;
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
    let timer = times.arm(timer, &token);
    Park::Parked(Parked {
        token,
        sources,
        timer,
    })
}

/// Whether the task of a wait that begins takes nothing new: its
/// receive arms are not tried then. That holds for a task that was
/// cancelled (`cancelled`: its flag is set) and for one whose time is
/// over (`task_expired`). This is the one condition;
/// `Vm::takes_nothing_new` is the same one for the operations that
/// look before they wait.
fn takes_nothing_new(cancelled: bool, task_expired: bool) -> bool {
    cancelled || task_expired
}

/// The two times that can end a wait, and whether each had come when
/// the wait began.
#[derive(Copy, Clone)]
struct Times {
    deadline: Option<Duration>,
    expired: bool,
    task_deadline: Option<Duration>,
    task_expired: bool,
}

impl Times {
    /// Put the wait of `token` in the timer: one entry, at the earlier
    /// of the two times, which says whose time it is. When both are
    /// the same reading it is the task's.
    fn arm(self, timer: &Arc<Timer>, token: &Arc<Token>) -> Option<(Arc<Timer>, TimerId)> {
        let id = match (self.deadline, self.task_deadline) {
            (Some(own), Some(task)) if own < task => timer.fire_at(own, token.clone()),
            (_, Some(task)) => timer.expire_at(task, token.clone()),
            (Some(own), None) => timer.fire_at(own, token.clone()),
            (None, None) => return None,
        };
        Some((timer.clone(), id))
    }
}

/// [`park`] for a wait whose one arm is on a channel: the same steps,
/// with one lock and nothing to sort.
fn park_one(
    task: TaskId,
    arm: Arm,
    times: Times,
    cancel: Option<Arc<AtomicBool>>,
    timer: &Arc<Timer>,
    wake: &dyn Wake,
) -> Park {
    let Times {
        expired,
        task_expired,
        ..
    } = times;
    let mut wakes = Wakes::default();
    let (source, token) = match arm {
        Arm::Recv(channel) => {
            let mut state = channel.state.lock();
            let cancelled = is_set(&cancel);
            let outcome = match takes_nothing_new(cancelled, task_expired) {
                true => None,
                false => match state.receive(&mut wakes) {
                    TryReceive::Value(value) => Some(Outcome::Received(value)),
                    TryReceive::Closed(close) => Some(Outcome::Closed(close)),
                    TryReceive::Empty => None,
                },
            };
            let ready = match outcome {
                Some(outcome) => Some(Fired::Arm(0, outcome)),
                None if cancelled => return Park::Cancelled,
                None if task_expired => return Park::Expired,
                None => expired.then_some(Fired::Deadline),
            };
            if let Some(fired) = ready {
                drop(state);
                wakes.send(wake);
                return Park::Ready(fired);
            }
            let token = Token::new(task, cancel);
            state.recvq.push_back(Waiter {
                token: token.clone(),
                arm: 0,
                payload: None,
            });
            drop(state);
            (Source::Recv(channel), token)
        }
        Arm::Send(channel, value) => {
            let mut state = channel.state.lock();
            let cancelled = is_set(&cancel);
            let (outcome, value) = match state.send(value, &mut wakes) {
                TrySend::Sent => (Some(Outcome::Sent), None),
                TrySend::Closed(close) => (Some(Outcome::Closed(close)), None),
                TrySend::Full(value) => (None, Some(value)),
            };
            let ready = match outcome {
                Some(outcome) => Some(Fired::Arm(0, outcome)),
                None if cancelled => return Park::Cancelled,
                None if task_expired => return Park::Expired,
                None => expired.then_some(Fired::Deadline),
            };
            if let Some(fired) = ready {
                drop(state);
                wakes.send(wake);
                return Park::Ready(fired);
            }
            let token = Token::new(task, cancel);
            state.sendq.push_back(Waiter {
                token: token.clone(),
                arm: 0,
                payload: value,
            });
            drop(state);
            (Source::Send(channel), token)
        }
        Arm::Cell(_) => unreachable!("the one arm is on a channel"),
    };
    let timer = times.arm(timer, &token);
    Park::Parked(Parked {
        token,
        sources: vec![source],
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
        // The queue of the arm that ended the wait has let go of the
        // token already: for a wait with one arm that is all.
        let ended_by = self.token.ended_by();
        for (arm, source) in self.sources.iter().enumerate() {
            if ended_by == Some(arm) {
                continue;
            }
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
