//! A channel: a buffer, a queue of parked receivers and a queue of
//! parked senders, behind one lock.
//!
//! The party that makes an operation possible completes it under the
//! lock. A receiver that finds a parked sender takes the sender's value
//! out of its waiter; a sender that finds a parked receiver puts the
//! value into the receiver's token. The peer is woken with the
//! operation done: it never looks at the channel again.
//!
//! A third queue holds the feeders that wait to be asked for a value
//! ([`Arm::Demand`](super::Arm)): a stream stage takes from its input
//! only when its reader asks for a value.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::queue::{Waiter, Wakes};
use super::token::{Fired, Outcome, Token, Wake};
use crate::runtime::handle::TaskHandle;
use crate::value::Value;
use crate::vm::VmError;

/// How a channel was closed.
#[derive(Clone, Default)]
pub struct Close {
    /// Why, when the producer failed: every receiver that finds the
    /// channel closed and empty gets it.
    pub failure: Option<Arc<VmError>>,
    /// The task of the stream stage whose failure it is. A read that
    /// raises the failure handles it ([`Close::raise`]); a failure that
    /// no read raised is reported when the program ends.
    pub stage: Option<Arc<TaskHandle>>,
}

impl std::fmt::Debug for Close {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Close")
            .field("failure", &self.failure)
            .field("stage", &self.stage.as_ref().map(|stage| stage.id))
            .finish()
    }
}

impl Close {
    /// A receive found the channel closed and empty: the failure of the
    /// stage that fed it, if it failed, which counts as read from now on.
    pub fn raise(self) -> Result<(), VmError> {
        if let Some(stage) = &self.stage {
            stage.mark_joined();
        }
        match self.failure {
            Some(failure) => Err((*failure).clone()),
            None => Ok(()),
        }
    }
}

/// The result of a send that does not wait.
#[derive(Debug)]
pub enum TrySend {
    Sent,
    /// No receiver is parked and the buffer has no room; the value
    /// comes back.
    Full(Value),
    /// The channel is closed; the value is dropped.
    Closed(Close),
}

/// The result of a receive that does not wait.
#[derive(Debug)]
pub enum TryReceive {
    Value(Value),
    /// The buffer is empty and no sender is parked.
    Empty,
    /// The channel is closed and empty.
    Closed(Close),
}

pub struct Channel {
    id: usize,
    capacity: usize,
    /// What its only reader does when it gives the channel up
    /// ([`Channel::abandon`]): tell whoever feeds it to stop.
    on_abandon: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// What its reader does when it drains the channel
    /// ([`Channel::drain`]): drain what feeds it.
    on_drain: OnceLock<Box<dyn Fn(&dyn Wake) + Send + Sync>>,
    /// Set when a timer was told to close it (`channel.timeout`).
    closes_by_timer: AtomicBool,
    pub(super) state: Mutex<State>,
}

pub(super) struct State {
    capacity: usize,
    buf: VecDeque<Value>,
    closed: Option<Close>,
    /// Parked receivers, oldest first. One whose wait is open is here
    /// only while the buffer is empty.
    pub(super) recvq: VecDeque<Waiter>,
    /// Parked senders with their values, oldest first. One whose wait
    /// is open is here only while the buffer is full (always, for
    /// capacity 0).
    pub(super) sendq: VecDeque<Waiter>,
    /// Parked feeders that wait to be asked for a value, oldest first.
    /// One whose wait is open is here only while nobody asks.
    pub(super) demandq: VecDeque<Waiter>,
    /// Whether a reader asks for a value that no send has given yet: a
    /// receive found the channel empty, and the value it asked for has
    /// not been sent since.
    asked: bool,
    /// Whether its reader reads it to the end ([`Channel::drain`]): a
    /// feeder may fill the buffer without being asked.
    draining: bool,
    /// How many values a feeder may hold in the buffer without being
    /// asked, when the reader does not drain the channel: the capacity
    /// for `stream.buffered`, none for any other stream stage.
    ahead: usize,
    /// How many waiters whose wait has ended were left on the queues
    /// since they were last swept ([`Channel::forget`]).
    stale: usize,
}

impl Drop for State {
    fn drop(&mut self) {
        // On no native stack, however deep the values that wait nest:
        // a channel may hold a channel that holds a channel.
        if !self.buf.is_empty() {
            crate::value::drop_held(Vec::from(std::mem::take(&mut self.buf)));
        }
    }
}

impl State {
    pub(super) fn send(&mut self, value: Value, wakes: &mut Wakes) -> TrySend {
        if let Some(close) = &self.closed {
            return TrySend::Closed(close.clone());
        }
        while let Some(receiver) = self.recvq.pop_front() {
            if let Some(claim) = receiver.token.claim() {
                claim.fire(Fired::Arm(receiver.arm, Outcome::Received(value)), wakes);
                // Another receiver that is parked still asks.
                self.asked = !self.recvq.is_empty();
                return TrySend::Sent;
            }
            // That wait ended on another arm, by its deadline or by a
            // cancel: the value goes to the next receiver.
        }
        // The value is for whoever asked last: it waits for them here.
        self.asked = false;
        if self.buf.len() < self.capacity {
            self.buf.push_back(value);
            TrySend::Sent
        } else {
            TrySend::Full(value)
        }
    }

    pub(super) fn receive(&mut self, wakes: &mut Wakes) -> TryReceive {
        if let Some(value) = self.buf.pop_front() {
            // The place that came free goes to the oldest parked sender.
            match self.take_from_sender(wakes) {
                Some(value) => self.buf.push_back(value),
                // Otherwise there is room now, which a feeder may use.
                None if self.demand().is_some() => self.wake_feeders(wakes),
                None => {}
            }
            return TryReceive::Value(value);
        }
        if let Some(value) = self.take_from_sender(wakes) {
            return TryReceive::Value(value);
        }
        match &self.closed {
            Some(close) => TryReceive::Closed(close.clone()),
            None => {
                // A receive that finds nothing asks for a value, whether
                // it waits for one or not: a later read gets it.
                self.asked = true;
                self.wake_feeders(wakes);
                TryReceive::Empty
            }
        }
    }

    /// What a feeder that waits to be asked for a value finds:
    /// [`Outcome::Asked`] when a reader asks, or the buffer has room
    /// that the feeder may fill unasked (its reader drains the channel,
    /// or it may hold values ahead), [`Outcome::Closed`] when nobody
    /// will ask any more.
    pub(super) fn demand(&self) -> Option<Outcome> {
        if let Some(close) = &self.closed {
            return Some(Outcome::Closed(close.clone()));
        }
        let room = match self.draining {
            true => self.capacity,
            false => self.ahead,
        };
        (self.asked || self.buf.len() < room).then_some(Outcome::Asked)
    }

    /// A reader asks, or room came free: every feeder that waits for
    /// that goes on.
    fn wake_feeders(&mut self, wakes: &mut Wakes) {
        while let Some(feeder) = self.demandq.pop_front() {
            if let Some(claim) = feeder.token.claim() {
                claim.fire(Fired::Arm(feeder.arm, Outcome::Asked), wakes);
            }
        }
    }

    /// Complete the send of the oldest parked sender whose wait is
    /// open, and give its value.
    fn take_from_sender(&mut self, wakes: &mut Wakes) -> Option<Value> {
        while let Some(mut sender) = self.sendq.pop_front() {
            if let Some(claim) = sender.token.claim() {
                claim.fire(Fired::Arm(sender.arm, Outcome::Sent), wakes);
                return sender.payload.take();
            }
        }
        None
    }

    fn close(&mut self, close: Close, wakes: &mut Wakes) -> bool {
        if self.closed.is_some() {
            return false;
        }
        // The values of parked senders were never sent and are dropped;
        // the buffer stays for later receives. A receiver is parked
        // only when the buffer is empty. A feeder that waits to be
        // asked learns that nobody will.
        let queues = self.sendq.drain(..).chain(self.recvq.drain(..));
        for waiter in queues.chain(self.demandq.drain(..)) {
            if let Some(claim) = waiter.token.claim() {
                claim.fire(
                    Fired::Arm(waiter.arm, Outcome::Closed(close.clone())),
                    wakes,
                );
            }
        }
        self.closed = Some(close);
        true
    }
}

impl Channel {
    /// A channel that buffers up to `capacity` values. With capacity 0
    /// a send completes only when a receiver takes the value.
    /// A timer will close the channel.
    pub(super) fn set_closes_by_timer(&self) {
        self.closes_by_timer.store(true, Ordering::SeqCst);
    }

    /// Whether a timer will close the channel, or has: a wait on it
    /// ends without anybody's doing. (Nobody waits on it once it is
    /// closed, so the answer need not change then.)
    pub fn closes_by_timer(&self) -> bool {
        self.closes_by_timer.load(Ordering::SeqCst)
    }

    pub fn new(id: usize, capacity: usize) -> Arc<Channel> {
        Channel::fed(id, capacity, capacity)
    }

    /// A channel that buffers up to `capacity` values, of which its
    /// feeder may hold `ahead` without being asked for them (see
    /// [`Arm::Demand`](super::Arm)) unless its reader drains it.
    pub fn fed(id: usize, capacity: usize, ahead: usize) -> Arc<Channel> {
        Arc::new(Channel {
            id,
            capacity,
            on_abandon: OnceLock::new(),
            on_drain: OnceLock::new(),
            closes_by_timer: AtomicBool::new(false),
            state: Mutex::new(State {
                capacity,
                buf: VecDeque::new(),
                closed: None,
                recvq: VecDeque::new(),
                sendq: VecDeque::new(),
                demandq: VecDeque::new(),
                asked: false,
                draining: false,
                ahead,
                stale: 0,
            }),
        })
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Send `value` if a receiver is parked or the buffer has room.
    pub fn try_send(&self, value: Value, wake: &dyn Wake) -> TrySend {
        let mut wakes = Wakes::default();
        let result = self.state.lock().send(value, &mut wakes);
        wakes.send(wake);
        result
    }

    /// Take the oldest value: from the buffer, or from a parked sender.
    pub fn try_receive(&self, wake: &dyn Wake) -> TryReceive {
        let mut wakes = Wakes::default();
        let result = self.state.lock().receive(&mut wakes);
        wakes.send(wake);
        result
    }

    /// Close the channel. Every parked sender and receiver is woken
    /// with [`Outcome::Closed`]; buffered values stay to be received.
    /// `false` when the channel was closed already: the first close
    /// stands.
    pub fn close(&self, close: Close, wake: &dyn Wake) -> bool {
        let mut wakes = Wakes::default();
        let closed = self.state.lock().close(close, &mut wakes);
        wakes.send(wake);
        closed
    }

    /// The channel is the output of something that is to stop when
    /// nobody reads the output any more: `stop` tells it to. Set once,
    /// when the channel is made.
    pub fn stop_feeder_with(&self, stop: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_abandon.set(Box::new(stop));
    }

    /// The reader of the channel reads no more. If the channel is the
    /// output of a feeder ([`Channel::stop_feeder_with`]), it is closed
    /// and the feeder is told to stop; any other channel is left as it
    /// is, for its other readers.
    pub fn abandon(&self, wake: &dyn Wake) {
        if let Some(stop) = self.on_abandon.get() {
            self.close(Close::default(), wake);
            stop();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed.is_some()
    }

    /// The reader of the channel reads it to the end: its feeder may
    /// fill the buffer without waiting to be asked for each value.
    /// The first drain of a channel that a feeder set up with
    /// [`Channel::drain_feeder_with`] drains what feeds it.
    pub fn drain(&self, wake: &dyn Wake) {
        let mut wakes = Wakes::default();
        {
            let mut state = self.state.lock();
            if std::mem::replace(&mut state.draining, true) {
                return;
            }
            if state.demand().is_some() {
                state.wake_feeders(&mut wakes);
            }
        }
        wakes.send(wake);
        if let Some(drain) = self.on_drain.get() {
            drain(wake);
        }
    }

    /// The channel is the output of something that reads its own
    /// inputs to the end when its reader reads the channel to the
    /// end: `drain` drains them. Set once, when the channel is made.
    pub fn drain_feeder_with(&self, drain: impl Fn(&dyn Wake) + Send + Sync + 'static) {
        let _ = self.on_drain.set(Box::new(drain));
    }

    /// Whether the reader of the channel reads it to the end.
    pub fn is_draining(&self) -> bool {
        self.state.lock().draining
    }

    /// Whether a feeder may take a value for the channel's reader now:
    /// a reader asks, or the buffer has room, and the channel is open.
    /// (A wait on [`Arm::Demand`](super::Arm) waits for it.)
    pub fn is_asked(&self) -> bool {
        matches!(self.state.lock().demand(), Some(Outcome::Asked))
    }

    /// The number of buffered values.
    pub fn len(&self) -> usize {
        self.state.lock().buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many open waits are parked here, as (receivers, senders).
    pub fn waiting(&self) -> (usize, usize) {
        let state = self.state.lock();
        let open = |queue: &VecDeque<Waiter>| queue.iter().filter(|w| w.token.is_waiting()).count();
        (open(&state.recvq), open(&state.sendq))
    }

    /// How many feeders wait here to be asked for a value.
    #[cfg(test)]
    pub(super) fn feeders(&self) -> usize {
        self.state.lock().demandq.len()
    }

    /// The length of the two queues, with waiters whose wait has ended
    /// and that nobody has taken off yet.
    #[cfg(test)]
    pub(super) fn queued(&self) -> (usize, usize) {
        let state = self.state.lock();
        (state.recvq.len(), state.sendq.len())
    }

    /// The wait of `token` has ended otherwise than by an operation on
    /// this channel (its task was cancelled or dropped, its deadline
    /// passed, another arm of its select was completed): its waiters
    /// here are of no use any more.
    ///
    /// On short queues they are taken off at once. On long ones that
    /// would cost the length of the queue each time, so they stay,
    /// where every operation passes over them, and are swept out
    /// together once they are half of what is queued: leaving a queue
    /// costs a constant on average, however many wait.
    pub(super) fn forget(&self, token: &Arc<Token>) {
        /// Up to this many waiters, a queue is searched.
        const SHORT: usize = 32;
        let mut state = self.state.lock();
        let queued = state.recvq.len() + state.sendq.len() + state.demandq.len();
        if queued <= SHORT {
            state.recvq.retain(|waiter| !waiter.is(token));
            state.sendq.retain(|waiter| !waiter.is(token));
            state.demandq.retain(|waiter| !waiter.is(token));
            return;
        }
        state.stale += 1;
        if state.stale * 2 >= queued {
            state.recvq.retain(|waiter| waiter.token.is_waiting());
            state.sendq.retain(|waiter| waiter.token.is_waiting());
            state.demandq.retain(|waiter| waiter.token.is_waiting());
            state.stale = 0;
        }
    }
}
