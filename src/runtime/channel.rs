use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

use crate::value::Value;

/// A boxed callback that re-enqueues a parked task.
/// Called by Channel::try_send / Channel::close when data becomes available.
pub type Waker = Box<dyn FnOnce() + Send>;

/// Identifier returned by `register_recv_waker` / `register_send_waker` so
/// a caller (notably `channel.select`) can later deregister its sibling
/// waker entries via `remove_recv_waker` / `remove_send_waker`. Without
/// this, select on multiple channels leaks wakers into every non-firing
/// channel's waker queue and permanently inflates `waiting_receivers` /
/// `waiting_senders`, breaking rendezvous handshake semantics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WakerId(pub u64);

/// Which side of a channel a `WakerRegistration` holds — selects the
/// correct `remove_*_waker` call on drop.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakerKind {
    Recv,
    Send,
}

/// RAII guard that owns a registered waker on a `Channel` and
/// deregisters it on drop. Construct via `Channel::register_recv_waker_guard`
/// or `Channel::register_send_waker_guard`.
///
/// Previously every cancellation / select-loser / main-thread watchdog
/// iteration had to call `remove_recv_waker` / `remove_send_waker` by
/// hand, carrying the raw `WakerId` through layers of closures. Missed
/// sites leaked wakers into the channel's FIFO, which caused four
/// observable cancel-path bugs (round-27 B1–B4):
///
/// - **B1** phantom rendezvous send after cancel-during-receive:
///   cancelled receiver's waker stayed in `recv_wakers` with
///   `waiting_receivers > 0`, so a later unrelated `try_send` dropped
///   into the handoff slot with no real peer.
/// - **B2** receiver starvation: dead waker at FIFO head shadowed a
///   real receiver; `wake_recv` popped the dead entry as a no-op.
/// - **B3** / **B4** symmetric sender starvation on rendezvous and
///   buffered channels.
///
/// With a guard, "register" and "deregister" are the same lifetime.
/// Dropping the guard is the only way to release the registration, so
/// forgetting a cancel path is a compile-time (lifetime) impossibility.
///
/// The guard's `Drop` invokes `remove_recv_waker` / `remove_send_waker`,
/// both of which are idempotent: if the waker already fired (i.e. was
/// popped by `wake_recv` / `wake_send`), `remove_*_waker` returns
/// `false` without adjusting the counter, matching the accounting
/// already performed by the wake path.
pub struct WakerRegistration {
    channel: Arc<Channel>,
    id: WakerId,
    kind: WakerKind,
}

impl WakerRegistration {
    /// Expose the channel this registration is on. Useful for tests
    /// that want to query `waiting_receivers_count` / queue length
    /// without re-plumbing the channel separately.
    pub fn channel(&self) -> &Arc<Channel> {
        &self.channel
    }

    /// Expose the underlying `WakerId`. Primarily for introspection in
    /// tests; production code should not need this because the guard
    /// owns deregistration.
    pub fn id(&self) -> WakerId {
        self.id
    }

    /// Expose whether this guard is for a recv or send waker.
    pub fn kind(&self) -> WakerKind {
        self.kind
    }
}

impl Drop for WakerRegistration {
    fn drop(&mut self) {
        match self.kind {
            WakerKind::Recv => {
                self.channel.remove_recv_waker(self.id);
            }
            WakerKind::Send => {
                self.channel.remove_send_waker(self.id);
            }
        }
    }
}

/// A thread-safe channel with support for both buffered and rendezvous semantics.
///
/// - **Capacity 0**: True rendezvous — sender blocks until a receiver is ready
///   and vice versa. Value is transferred via a handoff slot, never buffered.
/// - **Capacity N > 0**: Buffered — up to N values can be queued before the
///   sender blocks.
pub struct Channel {
    pub id: usize,
    buffer: Mutex<VecDeque<Value>>,
    pub capacity: usize,
    closed: AtomicBool,
    /// Notified when a value is sent or the channel is closed.
    condvar: Condvar,
    /// Wakers to call when a value is sent or the channel is closed
    /// (wakes tasks blocked on receive/select/each). Each waker carries
    /// a `WakerId` so that `channel.select` can deregister siblings on
    /// the channels that did NOT fire, avoiding leaked waker closures
    /// and a permanently-inflated `waiting_receivers` counter.
    recv_wakers: Mutex<VecDeque<(WakerId, Waker)>>,
    /// Wakers to call when buffer space becomes available (wakes tasks
    /// blocked on send). One is taken out for every value a receiver
    /// takes out of the channel.
    send_wakers: Mutex<VecDeque<(WakerId, Waker)>>,
    /// Monotonic counter for minting `WakerId`s on this channel. A `u64`
    /// at 1 ns per increment overflows in ~585 years, so overflow is
    /// not a practical concern.
    next_waker_id: AtomicU64,
    /// For rendezvous (capacity == 0): a parked sender places its value here.
    /// The receiver takes it directly, completing the handshake.
    handoff: Mutex<Option<Value>>,
    /// Number of receivers currently waiting (waker-based + condvar-based).
    /// Used by rendezvous try_send to detect if a direct handoff is possible.
    waiting_receivers: AtomicUsize,
    /// Set when `TimerManager::schedule` has registered this channel for
    /// a pending close. Cleared when `close()` runs. The main-thread
    /// wait loop consults this flag to avoid declaring deadlock while a
    /// timer is legitimately pending: `channel.timeout(50)` with no
    /// other scheduled tasks is not a deadlock — the timer thread will
    /// close the channel on schedule.
    pending_timer_close: AtomicBool,
    /// Set once a thread has waited in `receive_blocking`: a stream
    /// stage reads the channel, on a thread the scheduler does not
    /// count, so a send that waits on it is no deadlock.
    read_by_stream: AtomicBool,
}

/// Result of attempting to send on a channel.
pub enum TrySendResult {
    Sent,
    Full,
    Closed,
}

/// Result of attempting to receive from a channel.
pub enum TryReceiveResult {
    Value(Value),
    Empty,
    Closed,
}

impl Channel {
    pub fn new(id: usize, capacity: usize) -> Self {
        Self {
            id,
            buffer: Mutex::new(VecDeque::new()),
            capacity,
            closed: AtomicBool::new(false),
            condvar: Condvar::new(),
            recv_wakers: Mutex::new(VecDeque::new()),
            send_wakers: Mutex::new(VecDeque::new()),
            next_waker_id: AtomicU64::new(0),
            handoff: Mutex::new(None),
            waiting_receivers: AtomicUsize::new(0),
            pending_timer_close: AtomicBool::new(false),
            read_by_stream: AtomicBool::new(false),
        }
    }

    /// Mark this channel as having a pending timer-driven close. The
    /// main-thread wait loop uses this to distinguish a timer-parked
    /// wait from a real deadlock.
    pub fn mark_pending_timer_close(&self) {
        self.pending_timer_close
            .store(true, AtomicOrdering::Release);
    }

    /// True while a timer is scheduled to close this channel and has
    /// not yet fired. Read by `main_thread_wait_for_receive` before
    /// declaring deadlock.
    pub fn has_pending_timer_close(&self) -> bool {
        self.pending_timer_close.load(AtomicOrdering::Acquire)
    }

    /// Mint a fresh `WakerId` for a new registration.
    fn mint_waker_id(&self) -> WakerId {
        WakerId(self.next_waker_id.fetch_add(1, AtomicOrdering::Relaxed))
    }

    /// Test/introspection accessor: number of receive-side waiters
    /// currently counted toward the `waiting_receivers` atomic. Used by
    /// regression tests that verify `channel.select` deregisters stale
    /// wakers from sibling channels.
    pub fn waiting_receivers_count(&self) -> usize {
        self.waiting_receivers.load(AtomicOrdering::Acquire)
    }

    /// Test/introspection accessor: length of the pending `recv_wakers`
    /// queue. Complements `waiting_receivers_count` when testing that
    /// select's sibling deregistration removed the waker closures
    /// themselves (not just the counter decrement).
    pub fn recv_waker_queue_len(&self) -> usize {
        self.recv_wakers.lock().len()
    }

    /// Test/introspection accessor: length of the pending `send_wakers`
    /// queue. See `recv_waker_queue_len`.
    pub fn send_waker_queue_len(&self) -> usize {
        self.send_wakers.lock().len()
    }

    /// True if this is a rendezvous (unbuffered) channel.
    pub fn is_rendezvous(&self) -> bool {
        self.capacity == 0
    }

    pub fn try_send(&self, val: Value) -> TrySendResult {
        // `closed` is read under the lock that guards the channel's
        // values (`handoff` for rendezvous, `buffer` otherwise), and
        // `close` sets it under the same lock. A send therefore either
        // stores its value before the close, or is refused: no value is
        // accepted after a receiver has been told `Closed`.
        if self.is_rendezvous() {
            // Rendezvous: only succeed if a receiver is already waiting AND
            // the handoff slot is empty (no other sender already parked).
            let has_receiver = self.waiting_receivers.load(AtomicOrdering::Acquire) > 0;
            let mut slot = self.handoff.lock();
            if self.closed.load(AtomicOrdering::Acquire) {
                return TrySendResult::Closed;
            }
            if has_receiver && slot.is_none() {
                *slot = Some(val);
                drop(slot);
                self.condvar.notify_one();
                self.wake_recv();
                TrySendResult::Sent
            } else {
                TrySendResult::Full
            }
        } else {
            // Buffered: succeed if there's room in the buffer.
            let mut buf = self.buffer.lock();
            if self.closed.load(AtomicOrdering::Acquire) {
                return TrySendResult::Closed;
            }
            if buf.len() < self.capacity {
                buf.push_back(val);
                drop(buf);
                self.condvar.notify_one();
                self.wake_recv();
                TrySendResult::Sent
            } else {
                TrySendResult::Full
            }
        }
    }

    pub fn try_receive(&self) -> TryReceiveResult {
        if self.is_rendezvous() {
            // Rendezvous: check the handoff slot for a parked sender's value.
            let mut slot = self.handoff.lock();
            if let Some(val) = slot.take() {
                drop(slot);
                // Sender completed the handshake — wake it.
                self.wake_send();
                TryReceiveResult::Value(val)
            } else if self.closed.load(AtomicOrdering::Acquire) {
                TryReceiveResult::Closed
            } else {
                TryReceiveResult::Empty
            }
        } else {
            // Buffered: pop from the buffer.
            let mut buf = self.buffer.lock();
            if let Some(val) = buf.pop_front() {
                drop(buf);
                // Every value taken out frees one place, so every one
                // wakes one parked sender. Waking only when the buffer
                // had been full leaves senders parked for good: several
                // receives in a row free several places and wake one
                // sender.
                self.wake_send();
                TryReceiveResult::Value(val)
            } else if self.closed.load(AtomicOrdering::Acquire) {
                TryReceiveResult::Closed
            } else {
                TryReceiveResult::Empty
            }
        }
    }

    /// Whether a stream stage reads the channel.
    pub fn is_read_by_stream(&self) -> bool {
        self.read_by_stream.load(AtomicOrdering::Acquire)
    }

    /// Blocking receive — waits until a value is available or the channel closes.
    pub fn receive_blocking(&self) -> TryReceiveResult {
        self.read_by_stream.store(true, AtomicOrdering::Release);
        if self.is_rendezvous() {
            // Signal that a receiver is waiting so rendezvous senders can proceed.
            self.waiting_receivers.fetch_add(1, AtomicOrdering::Release);
            // Wake any parked sender now that a receiver is available.
            self.wake_send();
            let mut slot = self.handoff.lock();
            loop {
                if let Some(val) = slot.take() {
                    self.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
                    drop(slot);
                    self.wake_send();
                    return TryReceiveResult::Value(val);
                }
                if self.closed.load(AtomicOrdering::Acquire) {
                    self.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
                    return TryReceiveResult::Closed;
                }
                self.condvar.wait(&mut slot);
            }
        } else {
            let mut buf = self.buffer.lock();
            loop {
                if let Some(val) = buf.pop_front() {
                    drop(buf);
                    // One freed place, one woken sender: see `try_receive`.
                    self.wake_send();
                    return TryReceiveResult::Value(val);
                }
                if self.closed.load(AtomicOrdering::Acquire) {
                    return TryReceiveResult::Closed;
                }
                self.condvar.wait(&mut buf);
            }
        }
    }

    pub fn close(&self) {
        // The handoff slot is NOT cleared. A rendezvous `try_send`
        // succeeds by placing a value in the handoff slot, but the receiver
        // may not have taken it yet. Clearing the slot here silently drops
        // that final message. Instead, leave the slot alone — `try_receive`
        // and `receive_blocking` drain the slot BEFORE checking `closed`,
        // so the last value is still observed after close.
        //
        // `closed` is set while holding the lock that guards the
        // channel's values. Two things depend on that:
        //   * `try_send` reads `closed` under the same lock, so a send
        //     and a close are ordered: no value is stored after a
        //     receiver has seen `Closed`.
        //   * `receive_blocking` checks `closed` and enters
        //     `condvar.wait` under the same lock, so the `notify_all`
        //     below cannot fall between its check and its wait.
        if self.is_rendezvous() {
            let _slot = self.handoff.lock();
            self.closed.store(true, AtomicOrdering::Release);
        } else {
            let _buf = self.buffer.lock();
            self.closed.store(true, AtomicOrdering::Release);
        }
        // Timer-driven close has landed; clear the pending flag so the
        // main-thread wait loop falls through to its normal path. This
        // comes after `closed` is set, so a reader that checks the
        // pending flag first and `closed` second never sees both false
        // for a channel whose timer was scheduled.
        self.pending_timer_close
            .store(false, AtomicOrdering::Release);
        self.condvar.notify_all();
        // Wake ALL tasks blocked on receive or send — channel is done.
        self.wake_all_recv();
        self.wake_all_send();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(AtomicOrdering::Acquire)
    }

    /// Register a waker to be called when a value is sent or the channel is closed.
    ///
    /// Uses a double-check pattern to avoid lost wakeups: after registering,
    /// re-checks data availability (or closed state). If the channel became
    /// readable between the caller's `try_receive` and this registration,
    /// the waker fires immediately.
    ///
    /// Returns a `WakerId` so callers (notably `channel.select`) can later
    /// deregister this entry via `remove_recv_waker` when a sibling
    /// channel fires first. Callers that never deregister (simple
    /// `channel.receive`, `channel.each`) may ignore the returned id.
    pub fn register_recv_waker(&self, waker: Waker) -> WakerId {
        let id = self.mint_waker_id();
        self.waiting_receivers.fetch_add(1, AtomicOrdering::Release);
        self.recv_wakers.lock().push_back((id, waker));
        // Double-check: if data is now available or channel closed, wake immediately.
        let has_data_or_closed = if self.is_rendezvous() {
            self.handoff.lock().is_some() || self.closed.load(AtomicOrdering::Acquire)
        } else {
            let buf = self.buffer.lock();
            !buf.is_empty() || self.closed.load(AtomicOrdering::Acquire)
        };
        if has_data_or_closed {
            // Drain and fire all recv wakers — the channel state changed.
            let wakers: VecDeque<(WakerId, Waker)> = {
                let mut guard = self.recv_wakers.lock();
                std::mem::take(&mut *guard)
            };
            let count = wakers.len();
            for (_, w) in wakers {
                w();
            }
            self.waiting_receivers
                .fetch_sub(count, AtomicOrdering::Release);
        }
        // For rendezvous channels, a receiver arriving means a parked sender
        // can now proceed with the handshake. Wake one sender.
        if self.is_rendezvous() {
            self.wake_send();
        }
        id
    }

    /// Register a waker to be called when buffer space becomes available.
    ///
    /// Uses a double-check pattern to avoid lost wakeups: after registering,
    /// re-checks buffer space availability. If space opened up between the
    /// caller's `try_send` and this registration, the waker fires immediately.
    ///
    /// Returns a `WakerId` (see `register_recv_waker` for rationale).
    pub fn register_send_waker(&self, waker: Waker) -> WakerId {
        let id = self.mint_waker_id();
        self.send_wakers.lock().push_back((id, waker));
        // Double-check: if we can now proceed or channel closed, wake immediately.
        let has_space_or_closed = if self.is_rendezvous() {
            // For rendezvous, sender can proceed if a receiver is waiting and
            // the handoff slot is empty, or if the channel is closed.
            let has_receiver = self.waiting_receivers.load(AtomicOrdering::Acquire) > 0;
            let slot_empty = self.handoff.lock().is_none();
            (has_receiver && slot_empty) || self.closed.load(AtomicOrdering::Acquire)
        } else {
            let buf = self.buffer.lock();
            buf.len() < self.capacity || self.closed.load(AtomicOrdering::Acquire)
        };
        if has_space_or_closed {
            // Drain and fire all send wakers — the channel state changed.
            let wakers: VecDeque<(WakerId, Waker)> = {
                let mut guard = self.send_wakers.lock();
                std::mem::take(&mut *guard)
            };
            for (_, w) in wakers {
                w();
            }
        }
        id
    }

    /// Register a recv waker and return a `WakerRegistration` RAII
    /// guard that deregisters the entry on drop. This is the preferred
    /// entry point for anything that needs to manage waker lifetime
    /// (scheduler BlockReason arms, main-thread watchdog loops,
    /// `channel.select` siblings). See [`WakerRegistration`] for why a
    /// guard is required — bare `register_recv_waker` + manual
    /// `remove_recv_waker` has leaked repeatedly through the cancel
    /// path (round-27 B1–B4).
    pub fn register_recv_waker_guard(self: &Arc<Self>, waker: Waker) -> WakerRegistration {
        let id = self.register_recv_waker(waker);
        WakerRegistration {
            channel: self.clone(),
            id,
            kind: WakerKind::Recv,
        }
    }

    /// Register a send waker and return a `WakerRegistration` RAII
    /// guard. See [`register_recv_waker_guard`](Self::register_recv_waker_guard).
    pub fn register_send_waker_guard(self: &Arc<Self>, waker: Waker) -> WakerRegistration {
        let id = self.register_send_waker(waker);
        WakerRegistration {
            channel: self.clone(),
            id,
            kind: WakerKind::Send,
        }
    }

    /// Remove a previously-registered recv waker by id, decrementing
    /// `waiting_receivers` if the entry was still pending. Returns
    /// `true` if the entry was found and removed. Used by
    /// `channel.select` to clean up sibling registrations when one
    /// branch fires first — without this, the counter permanently
    /// inflates and rendezvous `try_send` falsely sees a phantom
    /// receiver, placing a value in the handoff slot and returning
    /// `Sent` with no real counterparty.
    ///
    /// If the entry has already been drained (e.g. the waker already
    /// fired via `wake_recv`), this is a no-op returning `false`.
    /// That matches the existing accounting: `wake_recv` /
    /// `wake_all_recv` already decremented the counter when they
    /// popped the entry.
    pub fn remove_recv_waker(&self, id: WakerId) -> bool {
        let mut guard = self.recv_wakers.lock();
        if let Some(pos) = guard.iter().position(|(wid, _)| *wid == id) {
            guard.remove(pos);
            drop(guard);
            self.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
            true
        } else {
            false
        }
    }

    /// Remove a previously-registered send waker by id. Returns `true`
    /// if the entry was found and removed, `false` if it had already
    /// been drained. See `remove_recv_waker` for rationale.
    pub fn remove_send_waker(&self, id: WakerId) -> bool {
        let mut guard = self.send_wakers.lock();
        if let Some(pos) = guard.iter().position(|(wid, _)| *wid == id) {
            guard.remove(pos);
            true
        } else {
            false
        }
    }

    /// Wake one task blocked on receive (FIFO — oldest waiter first).
    fn wake_recv(&self) {
        let waker = self.recv_wakers.lock().pop_front();
        if let Some((_, w)) = waker {
            self.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
            w();
        }
    }

    /// Wake all tasks blocked on receive (used when channel is closed).
    fn wake_all_recv(&self) {
        let wakers: VecDeque<(WakerId, Waker)> = {
            let mut guard = self.recv_wakers.lock();
            std::mem::take(&mut *guard)
        };
        let count = wakers.len();
        for (_, w) in wakers {
            w();
        }
        self.waiting_receivers
            .fetch_sub(count, AtomicOrdering::Release);
    }

    /// Wake one task blocked on send (FIFO — oldest waiter first).
    fn wake_send(&self) {
        let waker = self.send_wakers.lock().pop_front();
        if let Some((_, w)) = waker {
            w();
        }
    }

    /// Wake all tasks blocked on send (used when channel is closed).
    fn wake_all_send(&self) {
        let wakers: VecDeque<(WakerId, Waker)> = {
            let mut guard = self.send_wakers.lock();
            std::mem::take(&mut *guard)
        };
        for (_, w) in wakers {
            w();
        }
    }

    /// Pass a wake-up on: wake one parked receiver if a receive could
    /// complete now, and one parked sender if a send could complete now.
    ///
    /// A wake-up is a one-shot hint. The channel takes one waker out of
    /// its queue for each value that arrives and each place that frees
    /// up, and the woken party retries its operation. A party that was
    /// chosen and then does not perform the operation has used up a
    /// hint that another waiter needed: a `select` that completes a
    /// different arm, or a task that was cancelled after its waker had
    /// been taken out of the queue. Such a party calls this, so the
    /// hint reaches the next waiter.
    ///
    /// The wake-ups depend on the state of the channel at the time of
    /// the call, so a call that was not needed wakes nobody, or wakes a
    /// waiter that retries and parks again. It never loses a value.
    ///
    /// Must be called without any lock of this channel held.
    pub fn rewake_waiters(&self) {
        let (receive_possible, send_possible) = if self.is_rendezvous() {
            let slot = self.handoff.lock();
            let closed = self.closed.load(AtomicOrdering::Acquire);
            let has_receiver = self.waiting_receivers.load(AtomicOrdering::Acquire) > 0;
            (
                slot.is_some() || closed,
                (slot.is_none() && has_receiver) || closed,
            )
        } else {
            let buf = self.buffer.lock();
            let closed = self.closed.load(AtomicOrdering::Acquire);
            (
                !buf.is_empty() || closed,
                buf.len() < self.capacity || closed,
            )
        };
        if receive_possible {
            self.wake_recv();
        }
        if send_possible {
            self.wake_send();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Channel: close-drops-data regression tests (B1) ─────────────

    /// B1: On a rendezvous channel, `send(v); close()` must leave `v`
    /// observable to the receiver rather than silently dropping it.
    #[test]
    fn channel_rendezvous_send_then_close_preserves_value() {
        let ch = Channel::new(0, 0);
        // Simulate a receiver being ready (as rendezvous try_send requires).
        ch.waiting_receivers.fetch_add(1, AtomicOrdering::Release);
        match ch.try_send(Value::Int(42)) {
            TrySendResult::Sent => {}
            _ => panic!("expected Sent"),
        }
        ch.close();
        match ch.try_receive() {
            TryReceiveResult::Value(Value::Int(42)) => {}
            other => panic!(
                "expected Value(Int(42)) after close, got {}",
                match other {
                    TryReceiveResult::Value(v) => format!("Value({v:?})"),
                    TryReceiveResult::Empty => "Empty".into(),
                    TryReceiveResult::Closed => "Closed".into(),
                }
            ),
        }
        ch.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
        // After draining, next receive sees Closed.
        match ch.try_receive() {
            TryReceiveResult::Closed => {}
            _ => panic!("expected Closed after draining last value"),
        }
    }

    /// B1: On a buffered channel, all messages sent before close must be
    /// observable in order, with Closed reported only after the last one.
    #[test]
    fn channel_buffered_send_then_close_preserves_values() {
        let ch = Channel::new(0, 4);
        for i in 1..=3 {
            match ch.try_send(Value::Int(i)) {
                TrySendResult::Sent => {}
                _ => panic!("expected Sent for {i}"),
            }
        }
        ch.close();
        for expected in 1..=3 {
            match ch.try_receive() {
                TryReceiveResult::Value(Value::Int(n)) if n == expected => {}
                other => panic!(
                    "expected Int({expected}), got {}",
                    match other {
                        TryReceiveResult::Value(v) => format!("Value({v:?})"),
                        TryReceiveResult::Empty => "Empty".into(),
                        TryReceiveResult::Closed => "Closed".into(),
                    }
                ),
            }
        }
        match ch.try_receive() {
            TryReceiveResult::Closed => {}
            _ => panic!("expected Closed after draining buffer"),
        }
    }

    /// B1: Sending to an already-closed channel must report Closed, not panic.
    #[test]
    fn channel_send_after_close_reports_closed() {
        let ch = Channel::new(0, 2);
        ch.close();
        match ch.try_send(Value::Int(1)) {
            TrySendResult::Closed => {}
            _ => panic!("expected TrySendResult::Closed for send after close"),
        }
    }

    /// B1: A receiver already blocked in receive_blocking must see the
    /// final rendezvous value before seeing Closed.
    #[test]
    fn channel_rendezvous_blocking_receive_sees_final_value() {
        let ch = Arc::new(Channel::new(0, 0));
        let ch_producer = ch.clone();
        // Spawn a producer that sends then closes after the receiver
        // has started blocking.
        let producer = std::thread::spawn(move || {
            // Spin until a receiver registers so try_send succeeds.
            while ch_producer.waiting_receivers.load(AtomicOrdering::Acquire) == 0 {
                std::thread::yield_now();
            }
            match ch_producer.try_send(Value::Int(42)) {
                TrySendResult::Sent => {}
                _ => panic!("expected Sent"),
            }
            ch_producer.close();
        });
        let first = ch.receive_blocking();
        match first {
            TryReceiveResult::Value(Value::Int(42)) => {}
            other => panic!(
                "expected Value(Int(42)) as final rendezvous value, got {}",
                match other {
                    TryReceiveResult::Value(v) => format!("Value({v:?})"),
                    TryReceiveResult::Empty => "Empty".into(),
                    TryReceiveResult::Closed => "Closed".into(),
                }
            ),
        }
        // Join the producer so we know close() has completed before we
        // call receive again (otherwise we could block indefinitely).
        producer.join().expect("producer thread panicked");
        // Next receive returns Closed.
        match ch.receive_blocking() {
            TryReceiveResult::Closed => {}
            _ => panic!("expected Closed after final rendezvous value"),
        }
    }

    /// B1: Buffered close-then-drain — receiver waiting in receive_blocking
    /// should get queued data after close.
    #[test]
    fn channel_buffered_close_then_drain() {
        let ch = Arc::new(Channel::new(0, 4));
        match ch.try_send(Value::Int(1)) {
            TrySendResult::Sent => {}
            _ => panic!("expected Sent for 1"),
        }
        match ch.try_send(Value::Int(2)) {
            TrySendResult::Sent => {}
            _ => panic!("expected Sent for 2"),
        }
        ch.close();
        match ch.receive_blocking() {
            TryReceiveResult::Value(Value::Int(1)) => {}
            _ => panic!("expected Int(1)"),
        }
        match ch.receive_blocking() {
            TryReceiveResult::Value(Value::Int(2)) => {}
            _ => panic!("expected Int(2)"),
        }
        match ch.receive_blocking() {
            TryReceiveResult::Closed => {}
            _ => panic!("expected Closed after draining buffer"),
        }
    }
}
