//! The parked tasks: who holds a task while it waits.
//!
//! [`park`] and a [`Parked`] are the wait of a task; a
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
//! "Every task waits and nothing outside can end a wait" is for the
//! scheduler to say, from what [`Parking::inspect`] shows it and what
//! it counts itself. That is exact when it keeps to one order:
//! whoever ends a wait from outside a task (the timer, an I/O thread)
//! stops counting as external only after its wake has returned, and a
//! task stops counting as live only after its last operation has
//! returned. The timer of this module does so.

use parking_lot::{Mutex, MutexGuard};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::timer::Timer;
use super::token::{Resumed, TaskId, Wake};
use super::wait::{Park, Parked, Source, Wait, park};

struct Entry<T> {
    task: T,
    parked: Parked,
    /// The worker has given the task up: a wake will come for it.
    committed: bool,
}

/// The hash of a task's name is the name, spread over the word: names
/// are numbers handed out in order.
#[derive(Default)]
struct NameHasher(u64);

impl Hasher for NameHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 << 8) | u64::from(*byte);
        }
    }

    fn write_u64(&mut self, name: u64) {
        self.0 = name.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type Tasks<T> = HashMap<TaskId, Entry<T>, BuildHasherDefault<NameHasher>>;

/// The parked tasks are kept in this many maps, each behind a lock of
/// its own, so that the workers do not all meet at one lock when they
/// park and wake.
const SHARDS: usize = 16;

/// A task that waits, as [`Parking::inspect`] shows it.
pub struct Waiting<'a, T> {
    pub task: TaskId,
    /// What the scheduler parked.
    pub sleeper: &'a T,
    /// What the task is queued on, in the order of its wait's arms.
    pub on: &'a [Source],
    /// The deadline of its wait.
    pub deadline: Option<std::time::Duration>,
}

pub struct Parking<T> {
    timer: Arc<Timer>,
    ready: Box<dyn Fn(T, Resumed) + Send + Sync>,
    shards: Vec<Mutex<Tasks<T>>>,
    /// The entries that are committed. Changed only under the lock of
    /// the entry's shard, so it stands still for whoever holds all of
    /// them ([`Parking::inspect`]).
    waiting: AtomicUsize,
    /// Set before the shards are emptied, read under a shard's lock.
    shut_down: AtomicBool,
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
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            waiting: AtomicUsize::new(0),
            shut_down: AtomicBool::new(false),
        }
    }

    pub fn timer(&self) -> &Arc<Timer> {
        &self.timer
    }

    fn shard(&self, id: TaskId) -> MutexGuard<'_, Tasks<T>> {
        self.shards[id.0 as usize % SHARDS].lock()
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
    /// whose flag is set when it gets here completes no arm. A wait
    /// that carries the flag itself ([`Wait::cancel`]) also has no arm
    /// completed for it from the moment the flag is set.
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
            Park::Cancelled => return Some((task, Resumed::Cancelled)),
            Park::Parked(parked) => parked,
        };
        let token = parked.token().clone();
        {
            let mut tasks = self.shard(id);
            if self.shut_down.load(Ordering::SeqCst) {
                drop(tasks);
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
            let before = tasks.insert(id, entry);
            assert!(before.is_none(), "task {} is parked already", id.0);
        }
        // A cancel that came before the entry was there did not find
        // it, and its flag is set by now. The token is not committed,
        // so this cancel wakes nobody.
        if cancelled() {
            token.cancel(self);
        }
        let mut tasks = self.shard(id);
        if token.commit() {
            // No wake can take the entry while the lock is held; only
            // a shutdown may have taken it before.
            if let Some(entry) = tasks.get_mut(&id) {
                entry.committed = true;
                self.waiting.fetch_add(1, Ordering::SeqCst);
            }
            return None;
        }
        // The wait ended while the task was being parked: no wake
        // comes, and the task goes on here.
        let entry = tasks.remove(&id);
        drop(tasks);
        entry.map(|entry| (entry.task, finish(entry.parked)))
    }

    /// End the wait of the parked task `id` because the task is
    /// cancelled. `false` when the task is not parked or its wait has
    /// already ended: what it took or sent stands.
    pub fn cancel(&self, id: TaskId) -> bool {
        let token = {
            let tasks = self.shard(id);
            tasks.get(&id).map(|entry| entry.parked.token().clone())
        };
        token.is_some_and(|token| token.cancel(self))
    }

    /// How many tasks wait.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    pub fn is_parked(&self, id: TaskId) -> bool {
        let tasks = self.shard(id);
        tasks.get(&id).is_some_and(|entry| entry.committed)
    }

    /// Look at the tasks that wait, by id, while none parks and none
    /// wakes: every lock of the registry is held for the time of
    /// `look`. What the scheduler counts outside (its live tasks, what
    /// is pending) is to be read inside `look`, so that it belongs to
    /// the same moment (see the [module documentation](self)).
    pub fn inspect<R>(&self, look: impl FnOnce(&[Waiting<'_, T>]) -> R) -> R {
        let shards: Vec<_> = self.shards.iter().map(|shard| shard.lock()).collect();
        let mut waiting: Vec<Waiting<'_, T>> = shards
            .iter()
            .flat_map(|tasks| tasks.iter())
            .filter(|(_, entry)| entry.committed)
            .map(|(&task, entry)| Waiting {
                task,
                sleeper: &entry.task,
                on: entry.parked.sources(),
                deadline: entry.parked.deadline(),
            })
            .collect();
        waiting.sort_by_key(|waiting| waiting.task);
        look(&waiting)
    }

    /// The runtime ends: every parked task comes back, by id, off every
    /// queue and out of the timer, to be dropped. A task that parks
    /// from now on is handed back at once as cancelled.
    pub fn shutdown(&self) -> Vec<(TaskId, T)> {
        self.shut_down.store(true, Ordering::SeqCst);
        let mut entries = Vec::new();
        for shard in &self.shards {
            let mut tasks = shard.lock();
            let committed = tasks.values().filter(|entry| entry.committed).count();
            self.waiting.fetch_sub(committed, Ordering::SeqCst);
            entries.extend(tasks.drain());
        }
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
            let mut tasks = self.shard(task);
            // Not there: a shutdown took the task.
            let Some(entry) = tasks.remove(&task) else {
                return;
            };
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            entry
        };
        (self.ready)(entry.task, finish(entry.parked));
    }
}
