//! The tasks of one `task.join`, `task.background`, `task.each`,
//! `task.map` or `task.first_ok`: what their frame waits for, counted,
//! so that a call of a thousand pieces wakes its caller twice and not a
//! thousand times.
//!
//! Each task of the call has the call's [`Together`] on its handle
//! (`TaskHandle::in_together`). The scheduler tells it of a failure of
//! the task (`Together::failed`, in place of the record of failures
//! that nobody joined: no task of such a call is ever one of them) and
//! of the task's end (`TaskHandle::stop`). The frame waits first for
//! the call to be settled (a failure, the end of the body of a
//! `task.background`, the first `Ok` of a `task.first_ok`, or the end
//! of every task), stops what still
//! runs, then waits for the end of every task, and only then returns
//! or raises.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;

use super::sync::{Cell, Completion, Wake};
use crate::vm::VmError;

/// What a task is to its call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// A function of `task.join`.
    Piece,
    /// A helper of `task.background`.
    Helper,
    /// The body of `task.background`.
    Body,
    /// One of the `at_once` tasks of `task.each`.
    EachWorker,
    /// One of the `at_once` tasks of `task.map`.
    MapWorker,
    /// A function of `task.first_ok`.
    Racer,
}

impl Role {
    /// The task, as a line of the deadlock report names it before
    /// `at {at}`.
    pub(crate) fn what(self) -> &'static str {
        match self {
            Role::Piece => "a piece of `task.join`",
            Role::Helper => "a helper of `task.background`",
            Role::Body => "the body of `task.background`",
            Role::EachWorker => "a worker of `task.each`",
            Role::MapWorker => "a worker of `task.map`",
            Role::Racer => "a piece of `task.first_ok`",
        }
    }

    /// Who failed as well, in a note under the first failure.
    fn too(self) -> &'static str {
        match self {
            Role::Piece | Role::Racer => "another piece",
            Role::EachWorker | Role::MapWorker => "another worker",
            Role::Helper => "a helper",
            Role::Body => "the body",
        }
    }
}

/// How many failures after the first are named in its notes; the rest
/// are counted in one more.
const NOTED: usize = 4;

/// The failures of the tasks of one call.
#[derive(Default)]
struct Failures {
    /// The first: the call raises it.
    first: Option<VmError>,
    /// Those that came after it, while the others were being stopped
    /// or before the frame had woken: notes under it.
    also: Vec<(Role, VmError)>,
    /// Those after the first [`NOTED`] of `also`.
    more: usize,
}

pub(crate) struct Together {
    /// The tasks that have not ended.
    left: AtomicUsize,
    /// Complete when the frame is to wake the first time: a task
    /// failed, the body ended, a task gave the call's value
    /// ([`Together::settle`]), or every task ended.
    settled: Arc<Cell<()>>,
    /// Complete when every task has ended.
    ended: Arc<Cell<()>>,
    failures: Mutex<Failures>,
}

impl Together {
    /// The record of a call `call` (`task.join`, `task.background`)
    /// of `count` tasks; `what` names them in the deadlock report's
    /// line of a task that waits for them (`the pieces of its
    /// \`task.join\``).
    pub(crate) fn new(call: &'static str, what: &str, count: usize) -> Arc<Together> {
        Arc::new(Together {
            left: AtomicUsize::new(count),
            settled: Cell::of_call(what.to_string(), call),
            ended: Cell::of_call(what.to_string(), call),
            failures: Mutex::default(),
        })
    }

    /// The task of `role` failed with `error`. The first failure is
    /// the call's; one after it is a note. Whoever waits is told.
    pub(crate) fn failed(&self, role: Role, error: VmError, wake: &dyn Wake) {
        {
            let mut failures = self.failures.lock();
            if failures.first.is_none() {
                failures.first = Some(error);
            } else if failures.also.len() < NOTED {
                failures.also.push((role, error));
            } else {
                failures.more += 1;
            }
        }
        let _ = self.settled.complete((), wake);
    }

    /// A task of `role` has ended: its last step has run and its frames
    /// are gone.
    pub(crate) fn stopped(&self, role: Role, wake: &dyn Wake) {
        let last = self.left.fetch_sub(1, Ordering::SeqCst) == 1;
        if role == Role::Body || last {
            let _ = self.settled.complete((), wake);
        }
        if last {
            let _ = self.ended.complete((), wake);
        }
    }

    /// A task gave what the call gives before every task has ended
    /// (the first `Ok` of a `task.first_ok`): the call is settled.
    pub(crate) fn settle(&self, wake: &dyn Wake) {
        let _ = self.settled.complete((), wake);
    }

    /// What the frame waits for first.
    pub(crate) fn settled(&self) -> Arc<dyn Completion> {
        self.settled.clone()
    }

    /// What the frame waits for then: the end of every task.
    pub(crate) fn ended(&self) -> Arc<dyn Completion> {
        self.ended.clone()
    }

    /// The failure the call raises, if a task failed: the first, with
    /// a note for each that came after it.
    pub(crate) fn take_failure(&self) -> Option<VmError> {
        let mut failures = self.failures.lock();
        let mut first = failures.first.take()?;
        for (role, also) in std::mem::take(&mut failures.also) {
            let message = also.message.lines().next().unwrap_or_default();
            first = match also.span {
                Some(at) => first.with_note_at(
                    format!("{} failed too, at {{at}}: {message}", role.too()),
                    at,
                ),
                None => first.with_note(format!("{} failed too: {message}", role.too())),
            };
        }
        match failures.more {
            0 => {}
            1 => first = first.with_note("1 more failed too"),
            more => first = first.with_note(format!("{more} more failed too")),
        }
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scheduler's waker, for the cells the tests complete.
    fn wake() -> Arc<crate::scheduler::Scheduler> {
        crate::vm::Vm::new(crate::HostIo::process())
            .scheduler()
            .clone()
    }

    fn failure(message: &str, line: u32) -> VmError {
        let mut error = VmError::new(message.to_string());
        error.span = Some(crate::source::Span::point(Default::default(), line));
        error
    }

    /// The first failure is the call's; each after it is a note with its
    /// place, up to four, and the rest are counted in one note.
    #[test]
    fn the_failures_after_the_first_are_notes() {
        let scheduler = wake();
        let wake: &dyn Wake = scheduler.wake();
        let together = Together::new("task.join", "the pieces", 7);
        assert!(!together.settled().is_done());
        for line in 1..=7 {
            together.failed(Role::Piece, failure(&format!("panic: {line}"), line), wake);
        }
        assert!(together.settled().is_done());
        let first = together.take_failure().expect("a failure");
        assert_eq!(first.message, "panic: 1");
        let more = first.more.expect("notes");
        let notes: Vec<&str> = more.notes.iter().map(|(note, _)| note.as_str()).collect();
        assert_eq!(
            notes,
            [
                "another piece failed too, at {at}: panic: 2",
                "another piece failed too, at {at}: panic: 3",
                "another piece failed too, at {at}: panic: 4",
                "another piece failed too, at {at}: panic: 5",
                "2 more failed too",
            ]
        );
        assert!(more.notes[..4].iter().all(|(_, at)| at.is_some()));
    }

    /// A join is settled when its last piece ends, a background when its
    /// body ends; both have ended when every task has.
    #[test]
    fn the_end_of_the_body_or_of_every_task_settles_the_call() {
        let scheduler = wake();
        let wake: &dyn Wake = scheduler.wake();
        let join = Together::new("task.join", "the pieces", 2);
        join.stopped(Role::Piece, wake);
        assert!(!join.settled().is_done());
        join.stopped(Role::Piece, wake);
        assert!(join.settled().is_done() && join.ended().is_done());
        assert!(join.take_failure().is_none());

        let background = Together::new("task.background", "the body and helpers", 2);
        background.stopped(Role::Body, wake);
        assert!(background.settled().is_done());
        assert!(!background.ended().is_done());
        background.stopped(Role::Helper, wake);
        assert!(background.ended().is_done());
    }

    /// A first `Ok` settles a `task.first_ok` before its pieces end; it
    /// has ended when every piece has.
    #[test]
    fn a_first_ok_settles_the_call_before_its_pieces_end() {
        let scheduler = wake();
        let wake: &dyn Wake = scheduler.wake();
        let race = Together::new("task.first_ok", "the pieces", 2);
        race.settle(wake);
        assert!(race.settled().is_done());
        assert!(!race.ended().is_done());
        race.stopped(Role::Racer, wake);
        race.stopped(Role::Racer, wake);
        assert!(race.ended().is_done());
        assert!(race.take_failure().is_none());
    }
}
