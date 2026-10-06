//! What sits on a queue, and the wakes an operation leaves to do.
//!
//! The module is private: [`Completion`](super::Completion) names its
//! types, so only this crate can implement that trait.

use std::sync::Arc;

use super::token::{TaskId, Token, Wake};
use crate::value::Value;

/// A token on one queue: which arm of its wait this queue is, and the
/// value to send if the arm is a send.
pub struct Waiter {
    pub(super) token: Arc<Token>,
    pub(super) arm: usize,
    pub(super) payload: Option<Value>,
}

impl Waiter {
    pub(super) fn is(&self, token: &Arc<Token>) -> bool {
        Arc::ptr_eq(&self.token, token)
    }
}

/// The tasks to wake once the locks are released. Nearly every
/// operation wakes one at most.
#[derive(Default)]
pub struct Wakes {
    first: Option<TaskId>,
    more: Vec<TaskId>,
}

impl Wakes {
    pub(super) fn push(&mut self, task: TaskId) {
        match self.first {
            None => self.first = Some(task),
            Some(_) => self.more.push(task),
        }
    }

    pub(super) fn send(self, wake: &dyn Wake) {
        for task in self.first.into_iter().chain(self.more) {
            wake.wake(task);
        }
    }
}
