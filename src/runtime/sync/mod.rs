//! The concurrency core: what a task waits on, and how a wait ends.
//!
//! Nothing here knows the scheduler or the VM. A task that cannot go on
//! describes what it waits for as a [`Wait`]: some arms (receive from a
//! channel, send to a channel, a cell's completion) and maybe a
//! deadline. [`park`] either completes one arm at once or leaves one
//! [`Token`] on the queue of every arm. Whoever later makes one of the
//! arms possible completes it under that queue's lock, fires the token,
//! and tells the scheduler (a [`Wake`]) to run the task again. A token
//! fires once, so one arm of a `select` wins and no operation is ever
//! retried.
//!
//! - [`token`]: the token, its states, and the scheduler's side of a
//!   wake.
//! - [`channel`]: a buffer with a queue of parked receivers and one of
//!   parked senders, behind one lock.
//! - [`cell`]: a value that is set once, with a queue of waiters: a
//!   join, an I/O completion.
//! - [`timer`]: deadlines on the VM's clock ([`crate::HostIo`]); an
//!   entry fires a token or closes a channel.
//! - [`wait`]: `Wait`, `park` and what a parked task holds.
//! - [`parking`]: the registry of parked tasks, which does the
//!   scheduler's part below once and is the `Wake` of a runtime.
//!
//! # What the holder of a parked task must do
//!
//! ([`Parking`] is that holder; this is its contract with the rest.)
//! A worker that runs a task into a wait calls [`park`]. On
//! [`Park::Ready`] the task goes on. On [`Park::Parked`] the worker
//! puts the task where a wake can find it, counts it as not runnable,
//! and then calls [`Parked::commit`]. `true` means the task is parked
//! and exactly one [`Wake::wake`] will name it, when its token fires or
//! is cancelled. `false` means the token was fired or cancelled in the
//! meantime: no wake comes, and the worker takes the task back. Either
//! way [`Parked::finish`] gives what happened. Dropping a [`Parked`]
//! takes the task off every queue and disarms its deadline, so a task
//! that is dropped while it waits leaves nothing behind.
//!
//! `Wake::wake` is never called under a lock of this module.
//!
//! # Locks
//!
//! `park` holds the locks of all channels of one wait together, taken
//! in address order. Every other operation holds one channel lock, one
//! cell lock or the timer lock. A token's lock is a leaf below all of
//! them.

pub mod cell;
pub mod channel;
pub mod parking;
mod queue;
pub mod timer;
pub mod token;
pub mod wait;

#[cfg(test)]
pub(crate) mod double;
#[cfg(test)]
mod tests;

pub use cell::{Cell, Completion};
pub use channel::{Channel, Close, TryReceive, TrySend};
pub use parking::{Parking, Stuck};
pub use timer::{Timer, TimerId};
pub use token::{Fired, Outcome, Resumed, TaskId, Token, Wake};
pub use wait::{Arm, Park, Parked, Source, Wait, park};
