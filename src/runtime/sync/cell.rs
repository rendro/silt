//! A cell: a value that is set once, with the queue of those who wait
//! for it. The result of a task (a join waits for it) and the result of
//! an I/O operation are cells.

use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};

use super::queue::{Waiter, Wakes};
use super::token::{Fired, Outcome, Token, Wake};

pub struct Cell<T> {
    value: OnceLock<T>,
    /// Set before the queue is drained, read under the queue's lock by
    /// whoever joins the queue: a waiter is either drained or sees the
    /// value.
    waiters: Mutex<Vec<Waiter>>,
}

impl<T> Default for Cell<T> {
    fn default() -> Self {
        Cell {
            value: OnceLock::new(),
            waiters: Mutex::new(Vec::new()),
        }
    }
}

impl<T> Cell<T> {
    pub fn new() -> Arc<Cell<T>> {
        Arc::new(Cell::default())
    }

    /// Set the value and wake every waiter. When the cell is already
    /// complete the first value stands and `value` comes back.
    pub fn complete(&self, value: T, wake: &dyn Wake) -> Result<(), T> {
        self.value.set(value)?;
        let waiters = std::mem::take(&mut *self.waiters.lock());
        let mut wakes = Wakes::default();
        for waiter in waiters {
            if let Some(claim) = waiter.token.claim() {
                claim.fire(Fired::Arm(waiter.arm, Outcome::Done), &mut wakes);
            }
        }
        wakes.send(wake);
        Ok(())
    }

    /// The value, once the cell is complete.
    pub fn get(&self) -> Option<&T> {
        self.value.get()
    }
}

/// A cell as an arm of a wait, whatever its value is.
pub trait Completion: Send + Sync {
    fn is_done(&self) -> bool;

    /// How many open waits are parked here.
    fn waiting(&self) -> usize;

    #[doc(hidden)]
    fn enqueue(&self, waiter: Waiter, wakes: &mut Wakes);

    #[doc(hidden)]
    fn forget(&self, token: &Arc<Token>);
}

impl<T: Send + Sync> Completion for Cell<T> {
    fn is_done(&self) -> bool {
        self.value.get().is_some()
    }

    fn waiting(&self) -> usize {
        let waiters = self.waiters.lock();
        waiters.iter().filter(|w| w.token.is_waiting()).count()
    }

    fn enqueue(&self, waiter: Waiter, wakes: &mut Wakes) {
        let mut waiters = self.waiters.lock();
        if self.is_done() {
            drop(waiters);
            if let Some(claim) = waiter.token.claim() {
                claim.fire(Fired::Arm(waiter.arm, Outcome::Done), wakes);
            }
        } else {
            waiters.push(waiter);
        }
    }

    fn forget(&self, token: &Arc<Token>) {
        self.waiters.lock().retain(|waiter| !waiter.is(token));
    }
}
