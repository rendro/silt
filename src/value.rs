use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

use crate::bytecode;
use crate::vm::VmError;

/// Maximum number of elements that may be materialized from a range into a
/// list, JSON array, or similar eager collection.  Prevents accidental OOM
/// when a user writes something like `(1..1_000_000_000) |> list.reverse`.
pub(crate) const MAX_RANGE_MATERIALIZE: usize = 10_000_000;

/// Return the number of elements in the inclusive range `lo..=hi`, or an error
/// string if the count exceeds [`MAX_RANGE_MATERIALIZE`].
pub(crate) fn checked_range_len(lo: i64, hi: i64) -> Result<usize, String> {
    if lo > hi {
        return Ok(0);
    }
    let len = (hi as i128 - lo as i128 + 1) as u128;
    if len > MAX_RANGE_MATERIALIZE as u128 {
        Err(format!(
            "range {}..{} has {} elements; materializing more than {} is not allowed",
            lo, hi, len, MAX_RANGE_MATERIALIZE,
        ))
    } else {
        Ok(len as usize)
    }
}

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

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    ExtFloat(f64),
    Bool(bool),
    String(String),
    List(Arc<Vec<Value>>),
    Range(i64, i64), // inclusive on both ends: start..end
    Map(Arc<BTreeMap<Value, Value>>),
    Set(Arc<BTreeSet<Value>>),
    Tuple(Vec<Value>),
    Record(String, Arc<BTreeMap<String, Value>>),
    Variant(String, Vec<Value>),
    VmClosure(Arc<bytecode::VmClosure>),
    BuiltinFn(String),
    VariantConstructor(String, usize), // name, arity
    RecordDescriptor(String),          // record type name
    PrimitiveDescriptor(String),       // "Int", "Float", "String", "Bool" — for json.parse_map etc.
    Channel(Arc<Channel>),
    Handle(Arc<TaskHandle>),
    Unit,
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
    /// Wakers to call when buffer space becomes available
    /// (wakes tasks blocked on send when buffer was full).
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
        if self.closed.load(AtomicOrdering::Acquire) {
            return TrySendResult::Closed;
        }
        if self.is_rendezvous() {
            // Rendezvous: only succeed if a receiver is already waiting AND
            // the handoff slot is empty (no other sender already parked).
            let has_receiver = self.waiting_receivers.load(AtomicOrdering::Acquire) > 0;
            let mut slot = self.handoff.lock();
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
                let was_full = buf.len() + 1 >= self.capacity;
                drop(buf);
                if was_full {
                    self.wake_send();
                }
                TryReceiveResult::Value(val)
            } else if self.closed.load(AtomicOrdering::Acquire) {
                TryReceiveResult::Closed
            } else {
                TryReceiveResult::Empty
            }
        }
    }

    /// Blocking receive — waits until a value is available or the channel closes.
    pub fn receive_blocking(&self) -> TryReceiveResult {
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
                    let was_full = buf.len() + 1 >= self.capacity;
                    drop(buf);
                    if was_full {
                        self.wake_send();
                    }
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
        // B1 fix: do NOT clear the handoff slot. A rendezvous `try_send`
        // succeeds by placing a value in the handoff slot, but the receiver
        // may not have taken it yet. Clearing the slot here silently drops
        // that final message. Instead, leave the slot alone — `try_receive`
        // and `receive_blocking` drain the slot BEFORE checking `closed`,
        // so the last value is still observed after close.
        //
        // We still acquire + release the handoff lock here to act as a
        // memory barrier synchronising with the rendezvous `receive_blocking`
        // loop (which holds that lock while checking `closed`). Without this,
        // the receiver could see `closed == false` in the loop, enter
        // `condvar.wait` just after we set `closed = true` and fired
        // `notify_all`, and then miss the wakeup.
        self.closed.store(true, AtomicOrdering::Release);
        // Timer-driven close has landed; clear the pending flag so the
        // main-thread wait loop falls through to its normal path.
        self.pending_timer_close
            .store(false, AtomicOrdering::Release);
        drop(self.handoff.lock());
        // Acquire + release the buffer lock so that any thread in the
        // buffered receive_blocking path that already checked `closed`
        // (saw false) but hasn't entered condvar.wait yet will finish
        // entering the wait before we signal. Without this, notify_all
        // can fire between the check and the wait — a classic lost-wakeup.
        drop(self.buffer.lock());
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
}

/// Handle to a spawned task. Thread-safe — shared between spawner and worker.
pub struct TaskHandle {
    pub id: usize,
    result: Mutex<Option<Result<Value, VmError>>>,
    condvar: Condvar,
    /// Wakers to call when the task completes (for scheduler-based join).
    join_wakers: Mutex<Vec<Waker>>,
    /// Cleanup to run when a blocked task is cancelled (removes stale waker state).
    cancel_cleanup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl TaskHandle {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            result: Mutex::new(None),
            condvar: Condvar::new(),
            join_wakers: Mutex::new(Vec::new()),
            cancel_cleanup: Mutex::new(None),
        }
    }

    /// Register a cleanup closure to run when the task completes or is cancelled
    /// while blocked. This removes stale waker registrations from channels.
    pub fn set_cancel_cleanup(&self, f: Box<dyn FnOnce() + Send>) {
        *self.cancel_cleanup.lock() = Some(f);
    }

    /// Clear any pending cancel-cleanup closure so it won't fire when the
    /// task completes normally (prevents double-decrement of blocked_tasks).
    pub fn clear_cancel_cleanup(&self) {
        *self.cancel_cleanup.lock() = None;
    }

    /// Store the task result and notify any joiners.
    /// If the task has already completed, this is a no-op (prevents
    /// cancel from overwriting a finished task's result).
    pub fn complete(&self, result: Result<Value, VmError>) {
        {
            let mut guard = self.result.lock();
            if guard.is_some() {
                return; // Already completed, don't overwrite
            }
            *guard = Some(result);
        }
        // Fire cancel cleanup (removes stale waker state for blocked tasks).
        if let Some(cleanup) = self.cancel_cleanup.lock().take() {
            cleanup();
        }
        self.condvar.notify_all();
        // Wake all tasks blocked on join.
        let wakers: Vec<Waker> = {
            let mut guard = self.join_wakers.lock();
            std::mem::take(&mut *guard)
        };
        for w in wakers {
            w();
        }
    }

    /// Block until the task produces a result.
    pub fn join(&self) -> Result<Value, VmError> {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.clone() {
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Result<Value, VmError>> {
        self.result.lock().clone()
    }

    /// Register a waker to be called when the task completes.
    pub fn register_join_waker(&self, waker: Waker) {
        // Check if already complete to avoid missed wakeups.
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.join_wakers.lock().push(waker);
            // Double-check to avoid race: if result was set between our check and push.
            if self.result.lock().is_some() {
                // It completed in the meantime; drain and fire.
                let wakers: Vec<Waker> = {
                    let mut guard = self.join_wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for w in wakers {
                    w();
                }
            }
        }
    }
}

/// Completion handle for async I/O operations.
pub struct IoCompletion {
    result: Mutex<Option<Value>>,
    condvar: Condvar,
    wakers: Mutex<Vec<Waker>>,
}

impl IoCompletion {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            condvar: Condvar::new(),
            wakers: Mutex::new(Vec::new()),
        })
    }

    /// Store the I/O result and notify all waiters. First-writer-wins:
    /// once a result is stored, subsequent calls are no-ops. Returns
    /// `true` if this call stored the result, `false` if a previous
    /// caller already did. This lets the scheduler watchdog set a
    /// timeout error without racing against a late-arriving real result.
    pub fn complete(&self, value: Value) -> bool {
        {
            let mut guard = self.result.lock();
            if guard.is_some() {
                return false;
            }
            *guard = Some(value);
        }
        self.condvar.notify_all();
        let wakers: Vec<Waker> = {
            let mut guard = self.wakers.lock();
            std::mem::take(&mut *guard)
        };
        for w in wakers {
            w();
        }
        true
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Value> {
        self.result.lock().clone()
    }

    /// Blocking wait (for main thread). Clones the result rather than
    /// taking it, so the first-writer-wins invariant on `complete` is
    /// preserved: a subsequent `try_get` still observes the same value.
    pub fn wait(&self) -> Value {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.clone() {
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Register a waker with double-check pattern (prevents missed wakeups).
    pub fn register_waker(&self, waker: Waker) {
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.wakers.lock().push(waker);
            // Double-check: result may have arrived between check and push
            if self.result.lock().is_some() {
                let wakers: Vec<Waker> = {
                    let mut guard = self.wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for w in wakers {
                    w();
                }
            }
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::ExtFloat(n) => write!(f, "ExtFloat({n})"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::List(xs) => f.debug_list().entries(xs.iter()).finish(),
            Value::Range(lo, hi) => write!(f, "{lo}..{hi}"),
            Value::Map(m) => f.debug_map().entries(m.iter()).finish(),
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v:?}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                let mut t = f.debug_tuple("");
                for v in vs {
                    t.field(v);
                }
                t.finish()
            }
            Value::Record(name, fields) => {
                write!(f, "{name} {{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v:?}")?;
                }
                write!(f, "}}")
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v:?}")?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name),
            Value::BuiltinFn(name) => write!(f, "<builtin:{name}>"),
            Value::VariantConstructor(name, _) => write!(f, "<constructor:{name}>"),
            Value::RecordDescriptor(name) => write!(f, "<type:{name}>"),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

impl Value {
    /// Format a value in silt syntax, suitable for `io.inspect`.
    ///
    /// Unlike `Display` (which prints bare strings for user output) or `Debug`
    /// (which leaks Rust internals), this produces the silt-source representation:
    /// strings are quoted, collections use silt syntax, etc.
    pub fn format_silt(&self) -> String {
        match self {
            Value::Int(n) => format!("{n}"),
            Value::Float(n) => format!("{n}"),
            Value::ExtFloat(n) => format!("{n}"),
            Value::Bool(b) => format!("{b}"),
            Value::String(s) => format!("\"{s}\""),
            Value::List(xs) => {
                let items: Vec<String> = xs.iter().map(|v| v.format_silt()).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Range(lo, hi) => format!("{lo}..{hi}"),
            Value::Map(m) => {
                let items: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.format_silt(), v.format_silt()))
                    .collect();
                format!("#{{{}}}", items.join(", "))
            }
            Value::Set(s) => {
                let items: Vec<String> = s.iter().map(|v| v.format_silt()).collect();
                format!("#[{}]", items.join(", "))
            }
            Value::Tuple(vs) => {
                let items: Vec<String> = vs.iter().map(|v| v.format_silt()).collect();
                format!("({})", items.join(", "))
            }
            Value::Record(name, fields) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.format_silt()))
                    .collect();
                format!("{name} {{{}}}", items.join(", "))
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    name.clone()
                } else {
                    let items: Vec<String> = fields.iter().map(|v| v.format_silt()).collect();
                    format!("{name}({})", items.join(", "))
                }
            }
            Value::VmClosure(_) => "<fn>".to_string(),
            Value::BuiltinFn(_) => "<fn>".to_string(),
            Value::VariantConstructor(name, _) => format!("<constructor:{name}>"),
            Value::RecordDescriptor(name) => format!("<type:{name}>"),
            Value::PrimitiveDescriptor(name) => format!("<type:{name}>"),
            Value::Channel(ch) => format!("<channel:{}>", ch.id),
            Value::Handle(h) => format!("<handle:{}>", h.id),
            Value::Unit => "()".to_string(),
        }
    }
}

impl Value {
    /// Materialize a Range into a List. Returns self unchanged for non-Range values.
    /// Returns an error if the range exceeds [`MAX_RANGE_MATERIALIZE`] elements.
    pub fn materialize_range(&self) -> Result<Value, String> {
        match self {
            Value::Range(lo, hi) => {
                checked_range_len(*lo, *hi)?;
                let items: Vec<Value> = (*lo..=*hi).map(Value::Int).collect();
                Ok(Value::List(Arc::new(items)))
            }
            other => Ok(other.clone()),
        }
    }

    /// Get the length of a list or range, if applicable.
    pub fn collection_len(&self) -> Option<usize> {
        match self {
            Value::List(xs) => Some(xs.len()),
            Value::Range(lo, hi) => {
                if hi >= lo {
                    (*hi as i128 - *lo as i128 + 1).try_into().ok()
                } else {
                    Some(0)
                }
            }
            _ => None,
        }
    }
}

/// Extract an i64 from an optional Value reference.
fn val_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

/// Compare a named field in two record field maps.
fn cmp_record_field(
    a: &BTreeMap<String, Value>,
    b: &BTreeMap<String, Value>,
    key: &str,
) -> Ordering {
    match (a.get(key), b.get(key)) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

/// Map Weekday variant name to ordinal for comparison (Monday=1..Sunday=7).
fn weekday_ordinal(name: &str) -> Option<u8> {
    match name {
        "Monday" => Some(1),
        "Tuesday" => Some(2),
        "Wednesday" => Some(3),
        "Thursday" => Some(4),
        "Friday" => Some(5),
        "Saturday" => Some(6),
        "Sunday" => Some(7),
        _ => None,
    }
}

/// Format a duration in nanoseconds as a human-readable string.
fn fmt_duration(f: &mut fmt::Formatter<'_>, total_ns: i64) -> fmt::Result {
    if total_ns < 0 {
        write!(f, "-")?;
    }
    let ns = total_ns.unsigned_abs();
    if ns == 0 {
        write!(f, "0s")
    } else if ns >= 3_600_000_000_000 {
        let h = ns / 3_600_000_000_000;
        let m = (ns % 3_600_000_000_000) / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if m > 0 && s > 0 {
            write!(f, "{h}h{m}m{s}s")
        } else if m > 0 {
            write!(f, "{h}h{m}m")
        } else {
            write!(f, "{h}h")
        }
    } else if ns >= 60_000_000_000 {
        let m = ns / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if s > 0 {
            write!(f, "{m}m{s}s")
        } else {
            write!(f, "{m}m")
        }
    } else if ns >= 1_000_000_000 {
        let s = ns / 1_000_000_000;
        let ms = (ns % 1_000_000_000) / 1_000_000;
        if ms > 0 {
            write!(f, "{s}.{ms:03}s")
        } else {
            write!(f, "{s}s")
        }
    } else if ns >= 1_000_000 {
        write!(f, "{}ms", ns / 1_000_000)
    } else if ns >= 1_000 {
        write!(f, "{}us", ns / 1_000)
    } else {
        write!(f, "{ns}ns")
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::ExtFloat(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s}"),
            Value::List(xs) => {
                write!(f, "[")?;
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Range(lo, hi) => write!(f, "{lo}..{hi}"),
            Value::Map(m) => {
                write!(f, "#{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Value::String(s) = k {
                        write!(f, "\"{s}\": {v}")?;
                    } else {
                        write!(f, "{k}: {v}")?;
                    }
                }
                write!(f, "}}")
            }
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            Value::Record(name, fields) => match name.as_str() {
                "Date" => {
                    let y = val_i64(fields.get("year"));
                    let m = val_i64(fields.get("month"));
                    let d = val_i64(fields.get("day"));
                    write!(f, "{y:04}-{m:02}-{d:02}")
                }
                "Time" => {
                    let h = val_i64(fields.get("hour"));
                    let m = val_i64(fields.get("minute"));
                    let s = val_i64(fields.get("second"));
                    let ns = val_i64(fields.get("ns"));
                    if ns > 0 {
                        write!(f, "{h:02}:{m:02}:{s:02}.{ns:09}")
                    } else {
                        write!(f, "{h:02}:{m:02}:{s:02}")
                    }
                }
                "DateTime" => {
                    if let (Some(date), Some(time)) = (fields.get("date"), fields.get("time")) {
                        write!(f, "{date}T{time}")
                    } else {
                        write!(f, "DateTime {{}}")
                    }
                }
                "Duration" => fmt_duration(f, val_i64(fields.get("ns"))),
                _ => {
                    write!(f, "{name} {{")?;
                    for (i, (k, v)) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{k}: {v}")?;
                    }
                    write!(f, "}}")
                }
            },
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v}")?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name),
            Value::BuiltinFn(name) => write!(f, "<builtin:{name}>"),
            Value::VariantConstructor(name, _) => write!(f, "<constructor:{name}>"),
            Value::RecordDescriptor(name) => write!(f, "<type:{name}>"),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

/// Materialized length of the inclusive range `lo..=hi`, clamped to 0 when
/// empty and saturating to `i64::MAX` for ranges larger than `i64::MAX`
/// elements (e.g. `i64::MIN..=i64::MAX`). Computed via `i128` to avoid
/// overflow on the subtraction/addition. L2 fix: the old implementation
/// computed `hi - lo + 1` directly in i64, which panicked in debug and
/// wrapped in release builds for extreme ranges.
fn range_len(lo: i64, hi: i64) -> i64 {
    if lo > hi {
        return 0;
    }
    let len = (hi as i128) - (lo as i128) + 1;
    if len > i64::MAX as i128 {
        i64::MAX
    } else {
        len as i64
    }
}

/// Compare a `List` and a `Range` for equality. Returns `true` when the list
/// has exactly the same materialized elements as the range (all `Int`s in
/// ascending order from `lo` to `hi` inclusive).
fn list_eq_range(list: &[Value], lo: i64, hi: i64) -> bool {
    let len = range_len(lo, hi);
    if list.len() as i64 != len {
        return false;
    }
    if len == 0 {
        return true;
    }
    let mut cur = lo;
    for item in list.iter() {
        match item {
            Value::Int(n) if *n == cur => {}
            _ => return false,
        }
        // Avoid overflow on the final iteration: only increment while cur < hi.
        if cur == hi {
            break;
        }
        cur += 1;
    }
    true
}

/// Lexicographically compare a `List` and a `Range`.
///
/// Treats the range as its materialized sequence of `Int`s from `lo` to `hi`
/// inclusive. When `list_first` is true, `list` is the left-hand side; when
/// false, the range is the left-hand side and the resulting ordering is
/// reversed accordingly.
pub(crate) fn cmp_list_range(list: &[Value], lo: i64, hi: i64, list_first: bool) -> Ordering {
    let range_len = range_len(lo, hi);
    let common = (list.len() as i64).min(range_len);
    for i in 0..common {
        let range_val = Value::Int(lo + i);
        let list_item = &list[i as usize];
        let ord = if list_first {
            list_item.cmp(&range_val)
        } else {
            range_val.cmp(list_item)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Shared prefix is equal — the shorter side is less.
    let list_len = list.len() as i64;
    let len_ord = list_len.cmp(&range_len);
    if list_first {
        len_ord
    } else {
        len_ord.reverse()
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // SURPRISE: `ExtFloat` uses bitwise equality here so that
            // `NaN == NaN`. This is deliberate and REQUIRED for
            // `Ord`/`Eq` consistency, because `Value` is used as a key
            // in `BTreeMap`, `BTreeSet`, and deduplication paths
            // (`Map`, `Set`, `list.dedup`, `==` on `Map`/`Set`). Those
            // containers rely on reflexivity: a NaN key must always
            // compare equal to itself. The language-level `==`
            // operator does NOT use this path — see `language_eq` in
            // `src/vm/execute.rs`, which overrides `ExtFloat` to follow
            // IEEE-754 semantics (`NaN != NaN`). Do not "fix" the
            // bitwise check here without also fixing every
            // container/dedup path that depends on it.
            (Value::ExtFloat(a), Value::ExtFloat(b)) => a.to_bits() == b.to_bits(),
            // Mixed Float/ExtFloat: the typechecker permits this pair for
            // `==`/`!=`, so the VM must honor it rather than returning
            // `false` via the fallback arm. We widen the `Float` side to
            // `f64` and use the standard `PartialEq` for `f64`, which
            // matches `Float`-vs-`Float` semantics (so `1.0 == 1.0`, and
            // `NaN` from `ExtFloat` is never equal to a finite `Float`).
            (Value::Float(a), Value::ExtFloat(b)) | (Value::ExtFloat(b), Value::Float(a)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => na == nb && fa == fb,
            (Value::Unit, Value::Unit) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Range(a1, a2), Value::Range(b1, b2)) => {
                // Two ranges are equal iff they materialize to the same
                // sequence. Empty ranges (`lo > hi`) are all equal to each
                // other regardless of their endpoints.
                let (a_lo, a_hi) = (*a1, *a2);
                let (b_lo, b_hi) = (*b1, *b2);
                let a_empty = a_lo > a_hi;
                let b_empty = b_lo > b_hi;
                if a_empty || b_empty {
                    a_empty && b_empty
                } else {
                    a_lo == b_lo && a_hi == b_hi
                }
            }
            // Range vs List: the typechecker gives `Range(..)` the type
            // `List(Int)`, so the two sides share a Silt type and must have
            // a defined equality. Walk the range and list element-wise.
            (Value::List(list), Value::Range(lo, hi)) => list_eq_range(list.as_ref(), *lo, *hi),
            (Value::Range(lo, hi), Value::List(list)) => list_eq_range(list.as_ref(), *lo, *hi),
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Record(na, fa), Value::Record(nb, fb)) => na == nb && fa == fb,
            (Value::RecordDescriptor(a), Value::RecordDescriptor(b)) => a == b,
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a == b,
            (Value::Channel(a), Value::Channel(b)) => a.id == b.id,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let disc = |v: &Value| -> u8 {
            match v {
                Value::Unit => 0,
                Value::Bool(_) => 1,
                Value::Int(_) => 2,
                Value::Float(_) => 3,
                Value::ExtFloat(_) => 4,
                Value::String(_) => 5,
                Value::List(_) => 6,
                Value::Range(..) => 6, // same discriminant as List for ordering
                Value::Tuple(_) => 7,
                Value::Map(_) => 8,
                Value::Set(_) => 9,
                Value::Record(..) => 10,
                Value::Variant(..) => 11,
                Value::Channel(_) => 12,
                Value::Handle(_) => 13,
                Value::VmClosure(_) => 14,
                Value::BuiltinFn(_) => 15,
                Value::VariantConstructor(..) => 16,
                Value::RecordDescriptor(_) => 17,
                Value::PrimitiveDescriptor(_) => 18,
            }
        };
        let d1 = disc(self);
        let d2 = disc(other);
        if d1 != d2 {
            return d1.cmp(&d2);
        }
        match (self, other) {
            (Value::Unit, Value::Unit) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => {
                // Float values are guaranteed finite, so partial_cmp always
                // returns Some. The fallback to Equal is a safety net that
                // keeps Eq/Ord consistent (NaN == NaN) if a non-finite value
                // ever appears.
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (Value::ExtFloat(a), Value::ExtFloat(b)) => a.to_bits().cmp(&b.to_bits()),
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.as_slice().cmp(b.as_slice()),
            (Value::Range(a1, a2), Value::Range(b1, b2)) => {
                // Lexicographically compare materialized ranges. Empty ranges
                // (lo > hi) are all equal regardless of endpoints.
                let (al, ah) = (*a1, *a2);
                let (bl, bh) = (*b1, *b2);
                let a_len = range_len(al, ah);
                let b_len = range_len(bl, bh);
                if a_len == 0 || b_len == 0 {
                    a_len.cmp(&b_len)
                } else {
                    al.cmp(&bl).then_with(|| a_len.cmp(&b_len))
                }
            }
            // Range vs List: walk element-wise. Required for Ord/PartialOrd
            // consistency with PartialEq when the typechecker hands both
            // sides the same `List(Int)` type.
            (Value::List(list), Value::Range(lo, hi)) => {
                cmp_list_range(list.as_ref(), *lo, *hi, true)
            }
            (Value::Range(lo, hi), Value::List(list)) => {
                cmp_list_range(list.as_ref(), *lo, *hi, false)
            }
            (Value::Tuple(a), Value::Tuple(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.iter().cmp(b.iter()),
            (Value::Set(a), Value::Set(b)) => a.iter().cmp(b.iter()),
            (Value::Record(na, fa), Value::Record(nb, fb)) => {
                na.cmp(nb).then_with(|| match na.as_str() {
                    "Date" => cmp_record_field(fa, fb, "year")
                        .then_with(|| cmp_record_field(fa, fb, "month"))
                        .then_with(|| cmp_record_field(fa, fb, "day")),
                    "Time" => cmp_record_field(fa, fb, "hour")
                        .then_with(|| cmp_record_field(fa, fb, "minute"))
                        .then_with(|| cmp_record_field(fa, fb, "second"))
                        .then_with(|| cmp_record_field(fa, fb, "ns")),
                    "DateTime" => cmp_record_field(fa, fb, "date")
                        .then_with(|| cmp_record_field(fa, fb, "time")),
                    _ => fa.iter().cmp(fb.iter()),
                })
            }
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => {
                if let (Some(a), Some(b)) = (weekday_ordinal(na), weekday_ordinal(nb)) {
                    a.cmp(&b)
                } else {
                    na.cmp(nb).then_with(|| fa.cmp(fb))
                }
            }
            (Value::RecordDescriptor(a), Value::RecordDescriptor(b)) => a.cmp(b),
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a.cmp(b),
            (Value::Channel(a), Value::Channel(b)) => a.id.cmp(&b.id),
            // Handle / VmClosure / BuiltinFn / VariantConstructor: PartialEq
            // returns `false` for every pair (catch-all `_ => false` arm at
            // ~line 1028), so Ord must never return `Equal` for distinct
            // instances either — otherwise BTreeSet / BTreeMap silently drop
            // what they see as duplicates (Ord contract: a == b ⇒ cmp == Equal,
            // contrapositively a != b ⇒ cmp != Equal). We order by identity
            // (`id` field for TaskHandle, Arc pointer address for VmClosure)
            // and by contents (name / arity) for the name-carrying variants.
            (Value::Handle(a), Value::Handle(b)) => a.id.cmp(&b.id),
            (Value::VmClosure(a), Value::VmClosure(b)) => {
                (Arc::as_ptr(a) as usize).cmp(&(Arc::as_ptr(b) as usize))
            }
            (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a.cmp(b),
            (Value::VariantConstructor(na, aa), Value::VariantConstructor(nb, ab)) => {
                na.cmp(nb).then_with(|| aa.cmp(ab))
            }
            _ => Ordering::Equal,
        }
    }
}

// ── FFI conversion traits ──────────────────────────────────────────

/// Convert a `Value` into a Rust type.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

/// Convert a Rust type into a `Value`.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(value.clone())
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Int(n) => Ok(*n),
            other => Err(format!("expected Int, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Int(self)
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Float(n) => Ok(*n),
            Value::ExtFloat(n) => Ok(*n),
            Value::Int(n) => Ok(*n as f64),
            other => Err(format!("expected Float, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Float(self)
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => Err(format!("expected Bool, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            other => Err(format!("expected String, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl FromValue for () {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Unit => Ok(()),
            other => Err(format!("expected Unit, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for () {
    fn into_value(self) -> Value {
        Value::Unit
    }
}

impl FromValue for Vec<Value> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(xs) => Ok(xs.as_ref().clone()),
            Value::Range(lo, hi) => {
                checked_range_len(*lo, *hi)?;
                Ok((*lo..=*hi).map(Value::Int).collect())
            }
            other => Err(format!("expected List, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for Vec<Value> {
    fn into_value(self) -> Value {
        Value::List(Arc::new(self))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        match self {
            Some(v) => Value::Variant("Some".into(), vec![v.into_value()]),
            None => Value::Variant("None".into(), vec![]),
        }
    }
}

impl<T: IntoValue> IntoValue for Result<T, String> {
    fn into_value(self) -> Value {
        match self {
            Ok(v) => Value::Variant("Ok".into(), vec![v.into_value()]),
            Err(e) => Value::Variant("Err".into(), vec![Value::String(e)]),
        }
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::ExtFloat(_) => "ExtFloat",
        Value::Bool(_) => "Bool",
        Value::String(_) => "String",
        Value::List(_) => "List",
        Value::Range(..) => "Range",
        Value::Map(_) => "Map",
        Value::Set(_) => "Set",
        Value::Tuple(_) => "Tuple",
        Value::Record(..) => "Record",
        Value::Variant(..) => "Variant",
        Value::VmClosure(_) | Value::BuiltinFn(_) => "Function",
        Value::VariantConstructor(..) => "Constructor",
        Value::RecordDescriptor(_) | Value::PrimitiveDescriptor(_) => "Type",
        Value::Channel(_) => "Channel",
        Value::Handle(_) => "Handle",
        Value::Unit => "Unit",
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Unit => {}
            Value::Bool(b) => b.hash(state),
            Value::Int(n) => n.hash(state),
            Value::Float(f) => {
                // Canonicalize -0.0 to 0.0 for consistent hashing
                let bits = if *f == 0.0 {
                    0.0_f64.to_bits()
                } else {
                    f.to_bits()
                };
                bits.hash(state);
            }
            Value::ExtFloat(f) => {
                // Canonicalize -0.0 to 0.0 for consistent hashing
                let bits = if *f == 0.0 {
                    0.0_f64.to_bits()
                } else {
                    f.to_bits()
                };
                bits.hash(state);
            }
            Value::String(s) => s.hash(state),
            Value::List(xs) => {
                xs.len().hash(state);
                for x in xs.iter() {
                    x.hash(state);
                }
            }
            Value::Range(lo, hi) => {
                lo.hash(state);
                hi.hash(state);
            }
            Value::Tuple(vs) => {
                vs.len().hash(state);
                for v in vs {
                    v.hash(state);
                }
            }
            Value::Map(m) => {
                m.len().hash(state);
                for (k, v) in m.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Set(s) => {
                s.len().hash(state);
                for v in s.iter() {
                    v.hash(state);
                }
            }
            Value::Record(name, fields) => {
                name.hash(state);
                for (k, v) in fields.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Variant(name, fields) => {
                name.hash(state);
                fields.len().hash(state);
                for f in fields {
                    f.hash(state);
                }
            }
            Value::Channel(ch) => ch.id.hash(state),
            Value::Handle(h) => h.id.hash(state),
            Value::VmClosure(_) => {} // not meaningfully hashable
            Value::BuiltinFn(name) => name.hash(state),
            Value::VariantConstructor(name, arity) => {
                name.hash(state);
                arity.hash(state);
            }
            Value::RecordDescriptor(name) => name.hash(state),
            Value::PrimitiveDescriptor(name) => name.hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_of(v: &Value) -> u64 {
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    fn make_date(year: i64, month: i64, day: i64) -> Value {
        let mut fields = BTreeMap::new();
        fields.insert("year".to_string(), Value::Int(year));
        fields.insert("month".to_string(), Value::Int(month));
        fields.insert("day".to_string(), Value::Int(day));
        Value::Record("Date".to_string(), Arc::new(fields))
    }

    fn make_time(hour: i64, minute: i64, second: i64, ns: i64) -> Value {
        let mut fields = BTreeMap::new();
        fields.insert("hour".to_string(), Value::Int(hour));
        fields.insert("minute".to_string(), Value::Int(minute));
        fields.insert("second".to_string(), Value::Int(second));
        fields.insert("ns".to_string(), Value::Int(ns));
        Value::Record("Time".to_string(), Arc::new(fields))
    }

    // ── Hash/Eq consistency ────────────────────────────────────────

    #[test]
    fn hash_eq_float_zero_and_neg_zero() {
        let pos = Value::Float(0.0);
        let neg = Value::Float(-0.0);
        assert_eq!(pos, neg, "0.0 and -0.0 should be equal");
        assert_eq!(hash_of(&pos), hash_of(&neg), "0.0 and -0.0 must hash equal");
    }

    #[test]
    fn hash_eq_extfloat_nan() {
        let nan1 = Value::ExtFloat(f64::NAN);
        let nan2 = Value::ExtFloat(f64::NAN);
        assert_eq!(nan1, nan2, "ExtFloat NaN should equal itself via to_bits");
        assert_eq!(
            hash_of(&nan1),
            hash_of(&nan2),
            "ExtFloat NaN must hash consistently"
        );
    }

    #[test]
    fn hash_extfloat_zero_and_neg_zero() {
        let pos = Value::ExtFloat(0.0);
        let neg = Value::ExtFloat(-0.0);
        // ExtFloat uses to_bits() for PartialEq, so 0.0 != -0.0 (different bits).
        assert_ne!(pos, neg, "ExtFloat 0.0 and -0.0 differ by to_bits");
        // Hash canonicalizes -0.0 to 0.0, so they hash the same.
        // (This is a known Hash/Eq tension for ExtFloat -0.0.)
        assert_eq!(
            hash_of(&pos),
            hash_of(&neg),
            "ExtFloat 0.0 and -0.0 hash the same due to canonicalization"
        );
    }

    #[test]
    fn hash_eq_int_values() {
        let a = Value::Int(42);
        let b = Value::Int(42);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn hash_eq_string_values() {
        let a = Value::String("hello".into());
        let b = Value::String("hello".into());
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn hash_eq_list_values() {
        let a = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn hash_eq_record_values() {
        let a = make_date(2025, 1, 15);
        let b = make_date(2025, 1, 15);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn hash_eq_variant_values() {
        let a = Value::Variant("Ok".into(), vec![Value::Int(42)]);
        let b = Value::Variant("Ok".into(), vec![Value::Int(42)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    // ── PartialEq edge cases ───────────────────────────────────────

    #[test]
    fn float_nan_not_equal() {
        let nan1 = Value::Float(f64::NAN);
        let nan2 = Value::Float(f64::NAN);
        assert_ne!(nan1, nan2, "Float NaN should NOT equal NaN (IEEE 754)");
    }

    #[test]
    fn extfloat_nan_equal() {
        let nan1 = Value::ExtFloat(f64::NAN);
        let nan2 = Value::ExtFloat(f64::NAN);
        assert_eq!(nan1, nan2, "ExtFloat NaN should equal NaN via to_bits");
    }

    #[test]
    fn empty_list_eq() {
        let a = Value::List(Arc::new(vec![]));
        let b = Value::List(Arc::new(vec![]));
        assert_eq!(a, b);
    }

    #[test]
    fn nested_list_eq() {
        let inner1 = Value::List(Arc::new(vec![Value::Int(1)]));
        let inner2 = Value::List(Arc::new(vec![Value::Int(1)]));
        let a = Value::List(Arc::new(vec![inner1]));
        let b = Value::List(Arc::new(vec![inner2]));
        assert_eq!(a, b);
    }

    #[test]
    fn different_variant_types_not_equal() {
        assert_ne!(Value::Int(1), Value::Float(1.0));
        assert_ne!(Value::Int(0), Value::Bool(false));
        assert_ne!(Value::String("1".into()), Value::Int(1));
    }

    #[test]
    fn unit_eq() {
        assert_eq!(Value::Unit, Value::Unit);
    }

    #[test]
    fn tuple_eq() {
        let a = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
        let b = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
        assert_eq!(a, b);
    }

    #[test]
    fn tuple_neq_different_lengths() {
        let a = Value::Tuple(vec![Value::Int(1)]);
        let b = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        assert_ne!(a, b);
    }

    // ── Ord correctness ────────────────────────────────────────────

    #[test]
    fn ord_int_ordering() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Int(-5) < Value::Int(0));
        assert_eq!(Value::Int(42).cmp(&Value::Int(42)), Ordering::Equal);
    }

    #[test]
    fn ord_string_ordering() {
        assert!(Value::String("apple".into()) < Value::String("banana".into()));
        assert!(Value::String("a".into()) < Value::String("b".into()));
    }

    #[test]
    fn ord_float_normal() {
        assert!(Value::Float(1.0) < Value::Float(2.0));
        assert!(Value::Float(-1.0) < Value::Float(0.0));
    }

    #[test]
    fn ord_float_nan_fallback() {
        let nan = Value::Float(f64::NAN);
        let _ = nan.cmp(&Value::Float(0.0));
        let _ = nan.cmp(&nan);
    }

    #[test]
    fn ord_date_records() {
        let earlier = make_date(2024, 6, 15);
        let later = make_date(2024, 7, 1);
        let same_year_month = make_date(2024, 6, 20);
        assert!(earlier < later, "June 15 < July 1");
        assert!(earlier < same_year_month, "June 15 < June 20");
        assert_eq!(
            make_date(2024, 6, 15).cmp(&make_date(2024, 6, 15)),
            Ordering::Equal
        );
    }

    #[test]
    fn ord_date_year_takes_priority() {
        let d2023 = make_date(2023, 12, 31);
        let d2024 = make_date(2024, 1, 1);
        assert!(d2023 < d2024, "2023-12-31 < 2024-01-01");
    }

    #[test]
    fn ord_time_records() {
        let earlier = make_time(10, 30, 0, 0);
        let later = make_time(10, 31, 0, 0);
        assert!(earlier < later, "10:30:00 < 10:31:00");
        let by_hour = make_time(9, 59, 59, 0);
        assert!(by_hour < earlier, "09:59:59 < 10:30:00");
    }

    #[test]
    fn ord_time_ns_tiebreaker() {
        let a = make_time(12, 0, 0, 100);
        let b = make_time(12, 0, 0, 200);
        assert!(a < b, "ns should break ties in time ordering");
    }

    #[test]
    fn ord_weekday_variants() {
        let monday = Value::Variant("Monday".into(), vec![]);
        let tuesday = Value::Variant("Tuesday".into(), vec![]);
        let friday = Value::Variant("Friday".into(), vec![]);
        let sunday = Value::Variant("Sunday".into(), vec![]);
        assert!(monday < tuesday, "Monday < Tuesday");
        assert!(tuesday < friday, "Tuesday < Friday");
        assert!(friday < sunday, "Friday < Sunday");
        assert_eq!(
            Value::Variant("Wednesday".into(), vec![])
                .cmp(&Value::Variant("Wednesday".into(), vec![])),
            Ordering::Equal,
        );
    }

    #[test]
    fn ord_non_weekday_variants_lexicographic() {
        let ok = Value::Variant("Ok".into(), vec![Value::Int(1)]);
        let err = Value::Variant("Err".into(), vec![Value::String("e".into())]);
        assert!(err < ok);
    }

    #[test]
    fn ord_cross_type_by_discriminant() {
        assert!(Value::Unit < Value::Bool(true));
        assert!(Value::Bool(false) < Value::Int(0));
        assert!(Value::Int(0) < Value::Float(0.0));
    }

    // ── Display formatting ─────────────────────────────────────────

    #[test]
    fn display_int() {
        assert_eq!(format!("{}", Value::Int(42)), "42");
        assert_eq!(format!("{}", Value::Int(-1)), "-1");
    }

    #[test]
    fn display_float() {
        assert_eq!(format!("{}", Value::Float(4.25)), "4.25");
    }

    #[test]
    fn display_bool() {
        assert_eq!(format!("{}", Value::Bool(true)), "true");
        assert_eq!(format!("{}", Value::Bool(false)), "false");
    }

    #[test]
    fn display_string() {
        assert_eq!(format!("{}", Value::String("hello".into())), "hello");
    }

    #[test]
    fn display_unit() {
        assert_eq!(format!("{}", Value::Unit), "()");
    }

    #[test]
    fn display_list() {
        let list = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(format!("{}", list), "[1, 2, 3]");
    }

    #[test]
    fn display_empty_list() {
        let list = Value::List(Arc::new(vec![]));
        assert_eq!(format!("{}", list), "[]");
    }

    #[test]
    fn display_range() {
        assert_eq!(format!("{}", Value::Range(1, 10)), "1..10");
    }

    #[test]
    fn display_tuple() {
        let tuple = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
        assert_eq!(format!("{}", tuple), "(1, x)");
    }

    #[test]
    fn display_variant_no_fields() {
        assert_eq!(format!("{}", Value::Variant("None".into(), vec![])), "None");
    }

    #[test]
    fn display_variant_with_fields() {
        let v = Value::Variant("Some".into(), vec![Value::Int(42)]);
        assert_eq!(format!("{}", v), "Some(42)");
    }

    #[test]
    fn display_date_record() {
        assert_eq!(format!("{}", make_date(2024, 3, 5)), "2024-03-05");
    }

    #[test]
    fn display_time_record() {
        assert_eq!(format!("{}", make_time(9, 5, 0, 0)), "09:05:00");
    }

    #[test]
    fn display_time_record_with_ns() {
        assert_eq!(
            format!("{}", make_time(14, 30, 0, 123000000)),
            "14:30:00.123000000"
        );
    }

    #[test]
    fn display_generic_record() {
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(10));
        fields.insert("y".to_string(), Value::Int(20));
        let rec = Value::Record("Point".to_string(), Arc::new(fields));
        assert_eq!(format!("{}", rec), "Point {x: 10, y: 20}");
    }

    #[test]
    fn display_set() {
        let mut s = BTreeSet::new();
        s.insert(Value::Int(1));
        s.insert(Value::Int(2));
        let set = Value::Set(Arc::new(s));
        assert_eq!(format!("{}", set), "#[1, 2]");
    }

    #[test]
    fn display_builtin_fn() {
        assert_eq!(
            format!("{}", Value::BuiltinFn("println".into())),
            "<builtin:println>"
        );
    }

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

    // ── range_len overflow regression (L2) ──────────────────────────

    /// L2: `range_len(i64::MIN, i64::MAX)` used to overflow at
    /// `hi - lo + 1`. After fix it must saturate to `i64::MAX`.
    #[test]
    fn range_len_no_overflow_on_full_i64_range() {
        assert_eq!(range_len(i64::MIN, i64::MAX), i64::MAX);
        assert_eq!(range_len(0, i64::MAX), i64::MAX);
        assert_eq!(range_len(i64::MIN, 0), i64::MAX);
        assert_eq!(range_len(i64::MIN, -1), i64::MAX);
        // Normal small ranges still work.
        assert_eq!(range_len(1, 5), 5);
        assert_eq!(range_len(0, 0), 1);
        assert_eq!(range_len(10, 5), 0); // empty
    }
}
