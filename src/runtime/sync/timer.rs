//! Deadlines on the VM's clock.
//!
//! The timer keeps what is to happen at which reading of the clock's
//! monotonic time ([`crate::Clock::monotonic`]) and does it when the
//! scheduler calls [`Timer::fire_due`]. It never reads the system
//! clock: with an embedder's clock, time passes as that clock says.

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::channel::{Channel, Close};
use super::queue::Wakes;
use super::token::{Fired, Token, Wake};
use crate::vm::HostIo;

/// An armed entry: its deadline and a number that tells entries of one
/// deadline apart.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimerId(Duration, u64);

impl TimerId {
    pub fn deadline(self) -> Duration {
        self.0
    }
}

enum Action {
    /// The deadline of a wait.
    Fire(Arc<Token>),
    /// A channel that closes at a set time (`channel.timeout`).
    Close(Arc<Channel>),
}

#[derive(Default)]
struct Entries {
    next: u64,
    by_deadline: BTreeMap<TimerId, Action>,
}

pub struct Timer {
    io: HostIo,
    entries: Mutex<Entries>,
    /// The entries that are armed or that `fire_due` is carrying out.
    pending: AtomicUsize,
}

impl Timer {
    /// A timer on the clock of `io`.
    pub fn new(io: HostIo) -> Arc<Timer> {
        Arc::new(Timer {
            io,
            entries: Mutex::default(),
            pending: AtomicUsize::new(0),
        })
    }

    /// The clock's monotonic reading.
    pub fn now(&self) -> Duration {
        self.io.monotonic()
    }

    /// The reading at which a wait of `duration` that starts now ends;
    /// `None` when that is out of the clock's range, which is never.
    pub fn deadline_after(&self, duration: Duration) -> Option<Duration> {
        self.io.deadline_after(duration)
    }

    fn arm(&self, deadline: Duration, action: Action) -> TimerId {
        let mut entries = self.entries.lock();
        let id = TimerId(deadline, entries.next);
        entries.next += 1;
        entries.by_deadline.insert(id, action);
        self.pending.fetch_add(1, Ordering::SeqCst);
        id
    }

    /// End the wait of `token` with [`Fired::Deadline`] at `deadline`.
    pub(super) fn fire_at(&self, deadline: Duration, token: Arc<Token>) -> TimerId {
        self.arm(deadline, Action::Fire(token))
    }

    /// Close `channel` at `deadline`.
    pub fn close_at(&self, deadline: Duration, channel: Arc<Channel>) -> TimerId {
        self.arm(deadline, Action::Close(channel))
    }

    /// Take an entry out before its time. `false` when it has fired.
    pub fn disarm(&self, id: TimerId) -> bool {
        let removed = self.entries.lock().by_deadline.remove(&id).is_some();
        if removed {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
        removed
    }

    /// How many entries are still to happen. An entry counts until what
    /// it does is done and its task is woken, so a scheduler that sees
    /// nothing runnable and nothing pending here has not missed a wake
    /// that is on its way.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// The earliest deadline.
    pub fn next_deadline(&self) -> Option<Duration> {
        let entries = self.entries.lock();
        entries.by_deadline.keys().next().map(|id| id.0)
    }

    /// How long the thread that drives the timer may wait, in real
    /// time, before it calls [`Timer::fire_due`] again; `None` when
    /// nothing is armed. On an embedder's clock that is a short poll,
    /// since only the clock knows when its time has passed.
    pub fn real_wait(&self) -> Option<Duration> {
        let deadline = self.next_deadline()?;
        Some(self.io.real_wait(deadline.saturating_sub(self.now())))
    }

    /// Carry out every entry whose deadline the clock has reached, and
    /// give their number. If the clock has panicked
    /// ([`HostIo::clock_failure`]) its time no longer passes, and every
    /// entry is carried out: no wait is left that would never end.
    pub fn fire_due(&self, wake: &dyn Wake) -> usize {
        let now = self.now();
        let failed = self.io.clock_failure().is_some();
        let due = {
            let mut entries = self.entries.lock();
            if failed {
                std::mem::take(&mut entries.by_deadline)
            } else {
                // Every id above (now, MAX) has a later deadline.
                let later = entries.by_deadline.split_off(&TimerId(now, u64::MAX));
                std::mem::replace(&mut entries.by_deadline, later)
            }
        };
        let count = due.len();
        let mut wakes = Wakes::default();
        for action in due.into_values() {
            match action {
                Action::Fire(token) => {
                    if let Some(claim) = token.claim() {
                        claim.fire(Fired::Deadline, &mut wakes);
                    }
                }
                Action::Close(channel) => {
                    channel.close(Close::default(), wake);
                }
            }
        }
        wakes.send(wake);
        self.pending.fetch_sub(count, Ordering::SeqCst);
        count
    }
}
