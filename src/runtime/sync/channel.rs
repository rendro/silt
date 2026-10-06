//! A channel: a buffer, a queue of parked receivers and a queue of
//! parked senders, behind one lock.
//!
//! The party that makes an operation possible completes it under the
//! lock. A receiver that finds a parked sender takes the sender's value
//! out of its waiter; a sender that finds a parked receiver puts the
//! value into the receiver's token. The peer is woken with the
//! operation done: it never looks at the channel again.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use super::queue::{Waiter, Wakes};
use super::token::{Fired, Outcome, Token, Wake};
use crate::value::Value;
use crate::vm::VmError;

/// How a channel was closed.
#[derive(Clone, Debug, Default)]
pub struct Close {
    /// Why, when the producer failed: every receiver that finds the
    /// channel closed and empty gets it.
    pub failure: Option<Arc<VmError>>,
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
    /// How many waiters whose wait has ended were left on the queues
    /// since they were last swept ([`Channel::forget`]).
    stale: usize,
}

impl State {
    pub(super) fn send(&mut self, value: Value, wakes: &mut Wakes) -> TrySend {
        if let Some(close) = &self.closed {
            return TrySend::Closed(close.clone());
        }
        while let Some(receiver) = self.recvq.pop_front() {
            if let Some(claim) = receiver.token.claim() {
                claim.fire(Fired::Arm(receiver.arm, Outcome::Received(value)), wakes);
                return TrySend::Sent;
            }
            // That wait ended on another arm, by its deadline or by a
            // cancel: the value goes to the next receiver.
        }
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
            if let Some(value) = self.take_from_sender(wakes) {
                self.buf.push_back(value);
            }
            return TryReceive::Value(value);
        }
        if let Some(value) = self.take_from_sender(wakes) {
            return TryReceive::Value(value);
        }
        match &self.closed {
            Some(close) => TryReceive::Closed(close.clone()),
            None => TryReceive::Empty,
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
        // only when the buffer is empty.
        for waiter in self.sendq.drain(..).chain(self.recvq.drain(..)) {
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
    pub fn new(id: usize, capacity: usize) -> Arc<Channel> {
        Arc::new(Channel {
            id,
            capacity,
            on_abandon: OnceLock::new(),
            state: Mutex::new(State {
                capacity,
                buf: VecDeque::new(),
                closed: None,
                recvq: VecDeque::new(),
                sendq: VecDeque::new(),
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
        let queued = state.recvq.len() + state.sendq.len();
        if queued <= SHORT {
            state.recvq.retain(|waiter| !waiter.is(token));
            state.sendq.retain(|waiter| !waiter.is(token));
            return;
        }
        state.stale += 1;
        if state.stale * 2 >= queued {
            state.recvq.retain(|waiter| waiter.token.is_waiting());
            state.sendq.retain(|waiter| waiter.token.is_waiting());
            state.stale = 0;
        }
    }
}
