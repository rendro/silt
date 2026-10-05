//! A stand-in for the scheduler, for tests of the concurrency core.
//!
//! It keeps the tasks that are parked and the order in which they were
//! woken, drives the timer from a clock the test sets, and fails the
//! test when a wake breaks the contract of [`Wake`]: a wake for a task
//! that is not parked, or a second wake for one park.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::{Fired, Park, Parked, Resumed, TaskId, Timer, Wait, Wake, park};
use crate::vm::{Buffer, Clock, HostIo};

/// A clock that moves only when the test moves it.
#[derive(Clone, Default)]
pub(crate) struct ManualClock(Arc<Mutex<Duration>>);

impl ManualClock {
    pub(crate) fn advance(&self, by: Duration) {
        *self.0.lock() += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.0.lock()
    }

    fn monotonic(&self) -> Duration {
        *self.0.lock()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

struct Task {
    parked: Parked,
    woken: bool,
}

#[derive(Default)]
struct State {
    parked: HashMap<TaskId, Task>,
    /// The wakes since the last [`Double::woken`], in order.
    woken: Vec<u64>,
}

pub(crate) struct Double {
    pub(crate) clock: ManualClock,
    pub(crate) timer: Arc<Timer>,
    state: Mutex<State>,
}

impl Wake for Double {
    fn wake(&self, task: TaskId) {
        let mut state = self.state.lock();
        let entry = state
            .parked
            .get_mut(&task)
            .unwrap_or_else(|| panic!("wake of task {} which is not parked", task.0));
        assert!(!entry.woken, "task {} woken twice for one park", task.0);
        entry.woken = true;
        state.woken.push(task.0);
    }
}

impl Double {
    pub(crate) fn new() -> Double {
        let clock = ManualClock::default();
        let io = HostIo::buffer(&Buffer::new()).clock(clock.clone());
        Double {
            clock,
            timer: Timer::new(io),
            state: Mutex::default(),
        }
    }

    /// Run `task` into `wait` as a worker does: park, record the task,
    /// commit. `None` when the task is parked.
    pub(crate) fn block(&self, task: u64, wait: Wait) -> Option<Fired> {
        let task = TaskId(task);
        match park(task, wait, &self.timer, self) {
            Park::Ready(fired) => Some(fired),
            Park::Parked(parked) => {
                let token = parked.token().clone();
                let woken = false;
                let before = self
                    .state
                    .lock()
                    .parked
                    .insert(task, Task { parked, woken });
                assert!(before.is_none(), "task {} is parked already", task.0);
                assert!(token.commit(), "nothing ran between park and commit");
                None
            }
        }
    }

    /// The tasks woken since the last call, in the order of the wakes.
    pub(crate) fn woken(&self) -> Vec<u64> {
        std::mem::take(&mut self.state.lock().woken)
    }

    pub(crate) fn is_parked(&self, task: u64) -> bool {
        let state = self.state.lock();
        state.parked.get(&TaskId(task)).is_some_and(|t| !t.woken)
    }

    /// Run a woken task again: what its wait ended with.
    pub(crate) fn resume(&self, task: u64) -> Resumed {
        let entry = self.state.lock().parked.remove(&TaskId(task));
        let entry = entry.unwrap_or_else(|| panic!("task {task} is not parked"));
        assert!(entry.woken, "task {task} was not woken");
        match entry.parked.finish() {
            Ok(resumed) => resumed,
            Err(_) => panic!("task {task} was woken with its wait open"),
        }
    }

    /// Cancel a parked task, as `task.cancel` does.
    pub(crate) fn cancel(&self, task: u64) -> bool {
        let token = {
            let state = self.state.lock();
            state.parked[&TaskId(task)].parked.token().clone()
        };
        token.cancel(self)
    }

    /// Drop a parked task without running it again.
    pub(crate) fn drop_task(&self, task: u64) {
        let entry = self.state.lock().parked.remove(&TaskId(task));
        drop(entry.unwrap_or_else(|| panic!("task {task} is not parked")));
    }

    /// Move the clock and let the timer fire what is due.
    pub(crate) fn advance(&self, by: Duration) -> usize {
        self.clock.advance(by);
        self.timer.fire_due(self)
    }
}
