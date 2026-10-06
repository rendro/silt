//! The parked tasks: who holds a task while it waits.
//!
//! [`park`](super::park) and a [`Parked`] are the wait of a task; a
//! [`Parking`] is the scheduler's side of it, done once. It takes the
//! task (whatever the scheduler's task is: `T`) for the time of the
//! wait, is the [`Wake`] that every operation of this module is given,
//! and hands the task back, with what its wait ended on, to the
//! function it was made with: the scheduler's run queue.
//!
//! A task leaves in one of three ways, and each takes it off every
//! queue and out of the timer:
//!
//! - its wait ends (an arm is completed, or the deadline passes): the
//!   task is handed back with [`Resumed::Fired`];
//! - it is cancelled ([`Parking::cancel`]): handed back with
//!   [`Resumed::Cancelled`], having taken and sent nothing;
//! - the runtime shuts down ([`Parking::shutdown`]): the caller gets
//!   every task to drop.
//!
//! # Deadlock
//!
//! [`Parking::stuck`] is the rule "every task waits and nothing outside
//! can end a wait". It is exact when the scheduler keeps to one order:
//! whoever ends a wait from outside a task (the timer, an I/O thread)
//! stops counting as external only after its wake has returned, and a
//! task stops counting as live only after its last operation has
//! returned. The timer of this module does so.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use super::timer::Timer;
use super::token::{Resumed, TaskId, Wake};
use super::wait::{Park, Parked, Source, Wait, park};

struct Entry<T> {
    task: T,
    parked: Parked,
    /// The worker has given the task up: a wake will come for it.
    committed: bool,
}

struct State<T> {
    tasks: HashMap<TaskId, Entry<T>>,
    /// The entries that are committed.
    waiting: usize,
    shut_down: bool,
}

/// A task that waits when no wait can end any more, and what it waits
/// on.
pub struct Stuck {
    pub task: TaskId,
    pub on: Vec<Source>,
}

pub struct Parking<T> {
    timer: Arc<Timer>,
    ready: Box<dyn Fn(T, Resumed) + Send + Sync>,
    state: Mutex<State<T>>,
}

impl<T: Send> Parking<T> {
    /// A registry whose waits have their deadlines in `timer`. `ready`
    /// gets a task whose wait has ended, from whichever thread ended
    /// it, with no lock of this module held: it counts the task as
    /// runnable and queues it.
    pub fn new(timer: Arc<Timer>, ready: impl Fn(T, Resumed) + Send + Sync + 'static) -> Self {
        Parking {
            timer,
            ready: Box::new(ready),
            state: Mutex::new(State {
                tasks: HashMap::new(),
                waiting: 0,
                shut_down: false,
            }),
        }
    }

    pub fn timer(&self) -> &Arc<Timer> {
        &self.timer
    }

    /// Run the task `task`, named `id`, into `wait`.
    ///
    /// `Some`: the wait is over and the caller still has the task,
    /// which goes on with the result. `None`: the registry has the
    /// task; it comes back through `ready`, or from
    /// [`Parking::shutdown`].
    ///
    /// `cancelled` reads the task's cancel flag. Whoever cancels a task
    /// sets the flag first and then calls [`Parking::cancel`]; with
    /// that order a cancel is never lost between the two, and a task
    /// whose flag is set when it gets here completes no arm.
    pub fn park(
        &self,
        id: TaskId,
        task: T,
        wait: Wait,
        cancelled: impl Fn() -> bool,
    ) -> Option<(T, Resumed)> {
        if cancelled() {
            return Some((task, Resumed::Cancelled));
        }
        let parked = match park(id, wait, &self.timer, self) {
            Park::Ready(fired) => return Some((task, Resumed::Fired(fired))),
            Park::Parked(parked) => parked,
        };
        let token = parked.token().clone();
        {
            let mut state = self.state.lock();
            if state.shut_down {
                drop(state);
                // The runtime is ending: the task waits for nothing.
                drop(parked);
                return Some((task, Resumed::Cancelled));
            }
            let committed = false;
            let entry = Entry {
                task,
                parked,
                committed,
            };
            let before = state.tasks.insert(id, entry);
            assert!(before.is_none(), "task {} is parked already", id.0);
        }
        // A cancel that came before the entry was there did not find
        // it, and its flag is set by now. The token is not committed,
        // so this cancel wakes nobody.
        if cancelled() {
            token.cancel(self);
        }
        let mut state = self.state.lock();
        if token.commit() {
            // No wake can take the entry while the lock is held; only
            // a shutdown may have taken it before.
            if let Some(entry) = state.tasks.get_mut(&id) {
                entry.committed = true;
                state.waiting += 1;
            }
            return None;
        }
        // The wait ended while the task was being parked: no wake
        // comes, and the task goes on here.
        let entry = state.tasks.remove(&id);
        drop(state);
        entry.map(|entry| (entry.task, finish(entry.parked)))
    }

    /// End the wait of the parked task `id` because the task is
    /// cancelled. `false` when the task is not parked or its wait has
    /// already ended: what it took or sent stands.
    pub fn cancel(&self, id: TaskId) -> bool {
        let token = {
            let state = self.state.lock();
            state
                .tasks
                .get(&id)
                .map(|entry| entry.parked.token().clone())
        };
        token.is_some_and(|token| token.cancel(self))
    }

    /// How many tasks wait.
    pub fn waiting(&self) -> usize {
        self.state.lock().waiting
    }

    pub fn is_parked(&self, id: TaskId) -> bool {
        let state = self.state.lock();
        state.tasks.get(&id).is_some_and(|entry| entry.committed)
    }

    /// The parked tasks, by id, if no wait can end any more: at least
    /// one task waits, every live task waits, and nothing is pending in
    /// the timer or outside. `live` and `external` are the scheduler's
    /// counts (see the [module documentation](self)); they are read
    /// under this registry's lock. Once `Some`, it stays so.
    pub fn stuck(
        &self,
        live: impl FnOnce() -> usize,
        external: impl FnOnce() -> usize,
    ) -> Option<Vec<Stuck>> {
        let state = self.state.lock();
        let stuck = state.waiting > 0
            && state.waiting == live()
            && self.timer.pending() == 0
            && external() == 0;
        if !stuck {
            return None;
        }
        let mut tasks: Vec<Stuck> = state
            .tasks
            .iter()
            .filter(|(_, entry)| entry.committed)
            .map(|(&task, entry)| Stuck {
                task,
                on: entry.parked.sources().to_vec(),
            })
            .collect();
        tasks.sort_by_key(|stuck| stuck.task);
        Some(tasks)
    }

    /// The runtime ends: every parked task comes back, by id, off every
    /// queue and out of the timer, to be dropped. A task that parks
    /// from now on is handed back at once as cancelled.
    pub fn shutdown(&self) -> Vec<(TaskId, T)> {
        let entries: Vec<_> = {
            let mut state = self.state.lock();
            state.shut_down = true;
            state.waiting = 0;
            state.tasks.drain().collect()
        };
        let mut tasks: Vec<_> = entries
            .into_iter()
            .map(|(id, entry)| (id, entry.task))
            .collect();
        tasks.sort_by_key(|&(id, _)| id);
        tasks
    }
}

fn finish(parked: Parked) -> Resumed {
    match parked.finish() {
        Ok(resumed) => resumed,
        Err(_) => unreachable!("the wait of a task that is taken back has ended"),
    }
}

impl<T: Send> Wake for Parking<T> {
    fn wake(&self, task: TaskId) {
        let entry = {
            let mut state = self.state.lock();
            // Not there: a shutdown took the task.
            let Some(entry) = state.tasks.remove(&task) else {
                return;
            };
            state.waiting -= 1;
            entry
        };
        (self.ready)(entry.task, finish(entry.parked));
    }
}
