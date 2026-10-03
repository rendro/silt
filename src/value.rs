use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

use crate::bytecode;
use crate::typeinfo::{Tag, TypeInfo, bv, ty};
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

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Arc<Vec<Value>>),
    Range(i64, i64), // inclusive on both ends: start..end
    Map(Arc<BTreeMap<Value, Value>>),
    Set(Arc<BTreeSet<Value>>),
    Tuple(Vec<Value>),
    /// A record: its type and its fields by name.
    Record(Arc<TypeInfo>, Arc<BTreeMap<String, Value>>),
    /// A variant: which variant of which enum, and its fields.
    Variant(Tag, Vec<Value>),
    VmClosure(Arc<bytecode::VmClosure>),
    BuiltinFn(String),
    /// A function of a host module an embedder declared to the session
    /// (see `session::HostModule`), installed by the program that
    /// imports the module.
    HostFn(Arc<HostFn>),
    /// The constructor of a variant with fields, as a value.
    VariantConstructor(Tag),
    /// Runtime token for a record or enum type, or a builtin container
    /// type, passed as a `type a` argument. Keeps `type T`-style values
    /// distinct from primitives (see `PrimitiveDescriptor`).
    TypeDescriptor(Arc<TypeInfo>),
    PrimitiveDescriptor(String), // "Int", "Float", "String", "Bool" — for json.parse_map etc.
    Channel(Arc<Channel>),
    Handle(Arc<TaskHandle>),
    /// Immutable byte sequence. Structural equality and hashing — two
    /// `Bytes` values are equal iff they hold the same bytes. Forward-
    /// compatible with a future `Type::Bytes` promotion: literal syntax
    /// (`b"..."`), pattern matching, and method dispatch can be layered on
    /// top of this variant without changing semantics.
    Bytes(Arc<Vec<u8>>),
    /// TCP listener handle. Identity-based equality (id field). Created by
    /// `tcp.listen`; consumed by `tcp.accept`.
    TcpListener(Arc<TcpListenerHandle>),
    /// TCP stream handle. Identity-based equality. Wraps a trait object
    /// so plain TCP and (future) TLS streams share the same handle type
    /// transparently — the TLS layer in v0.9 PR 3 will substitute a
    /// rustls-wrapped stream behind the same `Arc<Mutex<Box<dyn ReadWrite>>>`.
    TcpStream(Arc<TcpStreamHandle>),
    Unit,
}

/// A combined Read + Write trait object used as the inner stream type for
/// `TcpStreamHandle`. Plain TCP impls it directly; the rustls wrappers in
/// `src/builtins/tcp.rs::tls` impl it manually so they can expose the
/// underlying `TcpStream`'s raw socket via `raw_socket`.
///
/// `raw_socket` exists so `tcp.close` on Windows can call `CancelIoEx`
/// on the SOCKET handle the parked `recv` is using. Winsock's
/// `shutdown(SD_BOTH)` does NOT cancel an in-progress blocking `recv`
/// on a duplicate handle (created via `WSADuplicateSocket` aka
/// `TcpStream::try_clone`), so we have to reach the actual SOCKET held
/// by the inner stream and explicitly cancel pending I/O on it.
///
/// On Unix, `raw_socket` is unused (Linux/macOS `shutdown(Both)` on the
/// cloned fd already delivers EOF to the parked reader). The default
/// returns `None` so non-socket implementors (e.g. test fakes) need not
/// override.
pub trait ReadWrite: std::io::Read + std::io::Write + Send {
    /// Underlying OS socket handle for the inner stream, if known.
    /// On Windows this is the `SOCKET` cast to `usize` (matching
    /// `std::os::windows::io::AsRawSocket::as_raw_socket() as usize`).
    /// On Unix this is the `RawFd` cast to `usize`. Used by `tcp.close`
    /// on Windows to call `CancelIoEx` on the parked reader's handle.
    fn raw_socket(&self) -> Option<usize> {
        None
    }
}

impl ReadWrite for std::net::TcpStream {
    fn raw_socket(&self) -> Option<usize> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(self.as_raw_socket() as usize)
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Some(self.as_raw_fd() as usize)
        }
        #[cfg(not(any(windows, unix)))]
        {
            None
        }
    }
}

pub struct TcpListenerHandle {
    pub id: usize,
    pub listener: std::net::TcpListener,
}

pub struct TcpStreamHandle {
    pub id: usize,
    pub inner: Mutex<Box<dyn ReadWrite>>,
    pub closed: std::sync::atomic::AtomicBool,
    /// Side-channel handle to the underlying `TcpStream` fd, used by
    /// `tcp.close` to call `shutdown(Both)` without having to acquire
    /// `inner`'s mutex (which a concurrent `tcp.read` on another task may
    /// be holding). Obtained via `TcpStream::try_clone` at construction
    /// time; for TLS streams this is a clone of the socket that was
    /// subsequently handed to `rustls::StreamOwned`. On Unix, a
    /// `shutdown(Both)` on any clone affects the shared fd, causing any
    /// blocked reader to return EOF promptly. On Windows, `shutdown`
    /// alone does NOT reliably unblock a parked `recv`; after the
    /// shutdown we take the cloned handle out of this slot and drop it
    /// (which calls `closesocket` on the duplicate handle) to cancel
    /// pending I/O on the underlying socket. Wrapped in a `Mutex` so
    /// `close()` can `take()` the handle out from `&self` without having
    /// to contend for `inner`'s mutex. `None` if cloning the fd failed
    /// at construction (best-effort — callers fall back to Drop
    /// semantics) or after `close()` has consumed it on Windows.
    pub shutdown_sock: Mutex<Option<std::net::TcpStream>>,
    /// Raw OS socket handle for the **inner** stream (the one a parked
    /// `tcp.read` is using). Cached at construction time so `tcp.close`
    /// can issue `CancelIoEx` on Windows WITHOUT acquiring `inner`'s
    /// mutex — which is held by the parked reader and would deadlock.
    ///
    /// On Windows: `SOCKET as usize` (matches
    /// `AsRawSocket::as_raw_socket() as usize`).
    /// On Unix: `RawFd as usize`. Currently unused on Unix because
    /// `shutdown(Both)` on the cloned fd already wakes the reader.
    /// `None` only if the underlying stream type does not expose a
    /// raw socket (e.g. test fakes).
    pub reader_socket: Option<usize>,
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

/// Handle to a spawned task. Thread-safe — shared between spawner and worker.
pub struct TaskHandle {
    pub id: usize,
    result: Mutex<Option<Result<Value, VmError>>>,
    condvar: Condvar,
    /// Wakers to call when the task completes (for scheduler-based join).
    /// Each entry carries a monotonic id so a `JoinWakerRegistration`
    /// guard can deregister exactly its own entry on drop, avoiding the
    /// leak that occurred when a `task.join(h)`-blocked task was
    /// cancelled while the joinee was still running (the closure stayed
    /// in this Vec holding `Arc<Mutex<Option<Task>>>` + `Arc<SchedulerInner>`
    /// until the joinee finally completed).
    join_wakers: Mutex<Vec<(u64, Waker)>>,
    /// Monotonic counter for minting `join_wakers` entry ids.
    next_join_waker_id: AtomicU64,
    /// Cleanup to run when a blocked task is cancelled (removes stale waker state).
    ///
    /// Lock order: this mutex is a leaf. It is held only to move a
    /// closure in or out, never while a closure runs or is dropped, and
    /// no other lock is acquired while it is held. The scheduler
    /// acquires it while holding the lock of a parked task's slot; a
    /// cleanup closure locks that slot. If the closure ran under this
    /// mutex, the two orders would meet and `task.cancel` would hang.
    cancel_cleanup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// True while the task has ended with an error of its own that no
    /// join has received, that no cancel has dismissed, and that has not
    /// been reported. Set by `fail`, cleared by `join`, `mark_joined`
    /// and `take_unjoined_failure`.
    unjoined_failure: AtomicBool,
    /// Who spawned the task, as the tag that was current where it was
    /// spawned (`crate::scheduler::set_task_owner`); 0 when nobody set
    /// one. A report of the task's failure carries it, so `silt test`
    /// can fail the test that spawned the task.
    owner: u64,
}

impl TaskHandle {
    pub fn new(id: usize) -> Self {
        Self::with_owner(id, 0)
    }

    /// A handle for a task spawned under the owner tag `owner`.
    pub fn with_owner(id: usize, owner: u64) -> Self {
        Self {
            id,
            result: Mutex::new(None),
            condvar: Condvar::new(),
            join_wakers: Mutex::new(Vec::new()),
            next_join_waker_id: AtomicU64::new(0),
            cancel_cleanup: Mutex::new(None),
            unjoined_failure: AtomicBool::new(false),
            owner,
        }
    }

    /// The owner tag the task was spawned under. See `with_owner`.
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// Register a cleanup closure to run when the task completes or is cancelled
    /// while blocked. This removes stale waker registrations from channels.
    ///
    /// A closure that was registered before is dropped, after the lock
    /// on the cleanup has been released.
    pub fn set_cancel_cleanup(&self, f: Box<dyn FnOnce() + Send>) {
        // The guard is a temporary of this statement, so the lock is
        // released before `previous` is dropped.
        let previous = self.cancel_cleanup.lock().replace(f);
        drop(previous);
    }

    /// Clear any pending cancel-cleanup closure so it won't fire when
    /// the task completes normally (prevents double-decrement of
    /// `live_tasks` and double-removal of the wake-graph node).
    ///
    /// The closure is dropped after the lock on the cleanup has been
    /// released.
    pub fn clear_cancel_cleanup(&self) {
        let previous = self.cancel_cleanup.lock().take();
        drop(previous);
    }

    /// Store the task result and notify any joiners.
    /// If the task has already completed, this is a no-op (prevents
    /// cancel from overwriting a finished task's result).
    pub fn complete(&self, result: Result<Value, VmError>) {
        self.finish(result, false);
    }

    /// Store the error that the task itself ended with, and notify any
    /// joiners. Like `complete`, and in addition the error counts as
    /// not joined until a join receives it.
    ///
    /// Returns `true` if this call stored the error, `false` if the
    /// handle already had a result (the task was cancelled before it
    /// failed); the error is dropped then.
    pub fn fail(&self, error: VmError) -> bool {
        self.finish(Err(error), true)
    }

    /// Shared body of `complete` and `fail`. Returns `true` if this
    /// call stored the result.
    fn finish(&self, result: Result<Value, VmError>, task_failed: bool) -> bool {
        {
            let mut guard = self.result.lock();
            if guard.is_some() {
                return false; // Already completed, don't overwrite
            }
            if task_failed {
                // Set before the result becomes visible: a join that
                // sees the result clears the flag after this.
                self.unjoined_failure.store(true, AtomicOrdering::Release);
            }
            *guard = Some(result);
        }
        // Fire cancel cleanup (removes stale waker state for blocked
        // tasks). The closure is taken out first and runs after the
        // lock on the cleanup has been released: see `cancel_cleanup`.
        let cleanup = self.cancel_cleanup.lock().take();
        if let Some(cleanup) = cleanup {
            cleanup();
        }
        self.condvar.notify_all();
        // Wake all tasks blocked on join.
        let wakers: Vec<(u64, Waker)> = {
            let mut guard = self.join_wakers.lock();
            std::mem::take(&mut *guard)
        };
        for (_, w) in wakers {
            w();
        }
        true
    }

    /// Block until the task produces a result.
    pub fn join(&self) -> Result<Value, VmError> {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.clone() {
                self.mark_joined();
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Result<Value, VmError>> {
        self.result.lock().clone()
    }

    /// Note that the program has handled the task: a join has received
    /// its result, or `task.cancel` was called on it. A failure of the
    /// task is the program's to handle from here on, and is not reported
    /// as unjoined.
    pub fn mark_joined(&self) {
        self.unjoined_failure.store(false, AtomicOrdering::Release);
    }

    /// True iff the task failed and no join has received the error.
    pub fn has_unjoined_failure(&self) -> bool {
        self.unjoined_failure.load(AtomicOrdering::Acquire)
    }

    /// The error of a task that failed and that no join has received.
    /// Returns it once: after this call the failure counts as reported.
    pub fn take_unjoined_failure(&self) -> Option<VmError> {
        if !self.unjoined_failure.swap(false, AtomicOrdering::AcqRel) {
            return None;
        }
        match self.result.lock().as_ref() {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        }
    }

    /// Mint a fresh id for a new join-waker registration.
    fn mint_join_waker_id(&self) -> u64 {
        self.next_join_waker_id
            .fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Register a waker to be called when the task completes.
    ///
    /// Legacy entry point: callers that need RAII deregistration on
    /// cancel should prefer [`register_join_waker_guard`](Self::register_join_waker_guard).
    /// This non-guard variant remains for stable call sites that join
    /// with no cancellation pressure (e.g. `task.join(h)` from main in
    /// `concurrency::main_thread_wait_for_join`).
    pub fn register_join_waker(&self, waker: Waker) {
        // Allocate an id even on the non-guard path so the storage
        // shape stays uniform — drop(_id) is a no-op once the closure
        // has been fired or drained.
        let id = self.mint_join_waker_id();
        // Check if already complete to avoid missed wakeups.
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.join_wakers.lock().push((id, waker));
            // Double-check to avoid race: if result was set between our check and push.
            if self.result.lock().is_some() {
                // It completed in the meantime; drain and fire.
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.join_wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
    }

    /// Register a join waker and return a `JoinWakerRegistration` RAII
    /// guard that deregisters the entry on drop. Required for cancel-
    /// path correctness: a `task.join(h)`-blocked task that is cancelled
    /// before the joinee completes must NOT leave its waker closure in
    /// `join_wakers`, because the closure holds `Arc<Mutex<Option<Task>>>`
    /// (with the Task already taken, so it would be inert) plus
    /// `Arc<SchedulerInner>`. Without the guard, the entry persists
    /// until the joinee finally completes — N cancelled joiners means N
    /// leaked closures on a long-running joinee.
    ///
    /// If the joinee is already complete, this fires the waker inline
    /// and returns a guard whose `id` does not appear in the Vec; the
    /// guard's Drop is a no-op deregister in that case.
    pub fn register_join_waker_guard(self: &Arc<Self>, waker: Waker) -> JoinWakerRegistration {
        let id = self.mint_join_waker_id();
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.join_wakers.lock().push((id, waker));
            // Double-check to avoid race: if result was set between our check and push.
            if self.result.lock().is_some() {
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.join_wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
        JoinWakerRegistration {
            handle: self.clone(),
            id,
        }
    }

    /// Remove a previously-registered join waker by id. Returns `true`
    /// if the entry was found and removed, `false` if it had already
    /// been drained (e.g. by `complete()` firing all pending wakers).
    pub fn remove_join_waker(&self, id: u64) -> bool {
        let mut guard = self.join_wakers.lock();
        if let Some(pos) = guard.iter().position(|(wid, _)| *wid == id) {
            // Drop the (id, Waker) tuple — we are intentionally
            // discarding the closure without firing it; cancellation
            // of a parked task means its waker should never run.
            let _ = guard.remove(pos);
            true
        } else {
            false
        }
    }

    /// Test/introspection accessor: number of join-waker entries
    /// currently registered. Used by regression tests that verify
    /// cancelled `task.join(h)` blocks do not leak waker closures.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn join_waker_count(&self) -> usize {
        self.join_wakers.lock().len()
    }
}

/// RAII guard that owns a registered join-waker entry on a
/// `TaskHandle` and deregisters it on drop. Construct via
/// [`TaskHandle::register_join_waker_guard`].
///
/// Ensures the cancel path for `task.join(h)`-blocked tasks does not
/// leak waker closures into `TaskHandle::join_wakers`. The guard's
/// Drop calls `remove_join_waker`, which is idempotent: if the waker
/// already fired (drained by `complete()`), Drop returns `false`
/// without further action.
pub struct JoinWakerRegistration {
    handle: Arc<TaskHandle>,
    id: u64,
}

impl JoinWakerRegistration {
    /// Expose the underlying entry id. Primarily for tests; production
    /// code should not need this because the guard owns deregistration.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Expose the handle this registration is on. Useful for tests
    /// that want to query `join_waker_count` without re-plumbing the
    /// handle separately.
    pub fn handle(&self) -> &Arc<TaskHandle> {
        &self.handle
    }
}

impl Drop for JoinWakerRegistration {
    fn drop(&mut self) {
        self.handle.remove_join_waker(self.id);
    }
}

/// Per-completion factory that builds the typed `Err` variant the
/// scheduler watchdog (or an already-elapsed entry guard) should
/// surface when the task's deadline cancels this I/O op. Each builtin
/// provides its own factory so a timed-out `tcp.read` produces
/// `Err(TcpTimeout)`, a timed-out `io.read_file` produces
/// `Err(IoUnknown(msg))`, etc.
///
/// Stored on `IoCompletion` so the scheduler thread can construct the
/// shape without knowing which module's submit it was built for.
pub type TimeoutErrFactory = std::sync::Arc<dyn Fn(&str) -> Value + Send + Sync>;

/// Default factory: wraps `msg` in `Err(IoUnknown(msg))`. Used by
/// `IoCompletion::new()` and by builtins on the io/fs family, which
/// still use `IoError` as their error type. Any builtin whose
/// signature declares a different error enum MUST construct its own
/// factory and pass it via `with_timeout_err` + `submit_with`.
pub fn io_unknown_timeout_err(msg: &str) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::IO_UNKNOWN,
            vec![Value::String(msg.to_string())],
        )],
    )
}

/// Completion handle for async I/O operations.
pub struct IoCompletion {
    result: Mutex<Option<Value>>,
    condvar: Condvar,
    /// Wakers to call when the I/O op completes. Each entry carries a
    /// monotonic id so an `IoWakerRegistration` guard can deregister
    /// exactly its own entry on drop, avoiding the leak that occurred
    /// when an I/O-blocked task was cancelled (or its deadline elapsed)
    /// before the completion fired — the closure stayed in this Vec
    /// holding `Arc<Mutex<Option<Task>>>` + `Arc<SchedulerInner>` until
    /// the underlying I/O finally produced a value.
    wakers: Mutex<Vec<(u64, Waker)>>,
    /// Monotonic counter for minting `wakers` entry ids.
    next_waker_id: AtomicU64,
    timeout_err: TimeoutErrFactory,
}

impl IoCompletion {
    /// Default constructor: deadline-cancellation surfaces as
    /// `Err(IoUnknown(msg))`. This is the standard entry point and is
    /// used by the io/fs family of builtins (and by any builtin that
    /// has not declared a typed error enum). Builtins whose signature
    /// declares a different error enum should call
    /// [`with_timeout_err`](Self::with_timeout_err) and pass a
    /// module-specific factory so a deadline-cancelled `tcp.read`
    /// produces `Err(TcpTimeout)` rather than `Err(IoUnknown(_))`.
    pub fn new() -> Arc<Self> {
        Self::with_timeout_err(std::sync::Arc::new(io_unknown_timeout_err))
    }

    /// Build a completion with a caller-supplied timeout-error factory.
    pub fn with_timeout_err(timeout_err: TimeoutErrFactory) -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            condvar: Condvar::new(),
            wakers: Mutex::new(Vec::new()),
            next_waker_id: AtomicU64::new(0),
            timeout_err,
        })
    }

    /// Construct the typed `Err` variant this completion should surface
    /// when its task's deadline elapses. Called by the scheduler
    /// watchdog and by `deadline_exceeded_err_value` at entry-guard.
    pub fn build_timeout_err(&self, msg: &str) -> Value {
        (self.timeout_err)(msg)
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
        let wakers: Vec<(u64, Waker)> = {
            let mut guard = self.wakers.lock();
            std::mem::take(&mut *guard)
        };
        for (_, w) in wakers {
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

    /// Mint a fresh id for a new waker registration.
    fn mint_waker_id(&self) -> u64 {
        self.next_waker_id.fetch_add(1, AtomicOrdering::Relaxed)
    }

    // Round 80 dead-code removal (DEAD-FN): the non-guard
    // `register_waker` had zero production callers — every I/O entry
    // guard now uses `register_waker_guard` for cancel-path
    // correctness. The only remaining caller was the
    // `legacy_register_io_waker_still_works` test asserting the
    // function existed; both have been removed. The waker-id minting
    // and double-check pattern survive verbatim inside
    // `register_waker_guard` below.

    /// Register a waker and return an `IoWakerRegistration` RAII guard
    /// that deregisters the entry on drop. Required for cancel-path
    /// correctness: an I/O-blocked task that is cancelled (or whose
    /// deadline elapses) before the I/O completes must NOT leave its
    /// waker closure in `wakers`, because the closure holds
    /// `Arc<Mutex<Option<Task>>>` plus `Arc<SchedulerInner>`. Without
    /// the guard, the entry persists until the I/O finally produces a
    /// value — N cancelled waiters on a slow I/O op means N leaked
    /// closures on the completion handle.
    pub fn register_waker_guard(self: &Arc<Self>, waker: Waker) -> IoWakerRegistration {
        let id = self.mint_waker_id();
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.wakers.lock().push((id, waker));
            // Double-check: result may have arrived between check and push
            if self.result.lock().is_some() {
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
        IoWakerRegistration {
            completion: self.clone(),
            id,
        }
    }

    /// Remove a previously-registered waker by id. Returns `true` if
    /// the entry was found and removed, `false` if it had already been
    /// drained (e.g. by `complete()` firing all pending wakers).
    pub fn remove_waker(&self, id: u64) -> bool {
        let mut guard = self.wakers.lock();
        if let Some(pos) = guard.iter().position(|(wid, _)| *wid == id) {
            // Drop the (id, Waker) tuple — we are intentionally
            // discarding the closure without firing it; cancellation
            // of a parked task means its waker should never run.
            let _ = guard.remove(pos);
            true
        } else {
            false
        }
    }

    /// Test/introspection accessor: number of waker entries currently
    /// registered. Used by regression tests that verify cancelled
    /// I/O-blocked tasks do not leak waker closures.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn waker_count(&self) -> usize {
        self.wakers.lock().len()
    }
}

/// RAII guard that owns a registered waker entry on an `IoCompletion`
/// and deregisters it on drop. Construct via
/// [`IoCompletion::register_waker_guard`].
///
/// Ensures the cancel path for I/O-blocked tasks does not leak waker
/// closures into `IoCompletion::wakers`. The guard's Drop calls
/// `remove_waker`, which is idempotent: if the waker already fired
/// (drained by `complete()`), Drop returns `false` without further
/// action.
pub struct IoWakerRegistration {
    completion: Arc<IoCompletion>,
    id: u64,
}

impl IoWakerRegistration {
    /// Expose the underlying entry id. Primarily for tests.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Expose the completion this registration is on. Useful for
    /// tests that want to query `waker_count` without re-plumbing the
    /// completion separately.
    pub fn completion(&self) -> &Arc<IoCompletion> {
        &self.completion
    }
}

impl Drop for IoWakerRegistration {
    fn drop(&mut self) {
        self.completion.remove_waker(self.id);
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
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
            Value::Record(ty, fields) => {
                write!(f, "{} {{", ty.name)?;
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
            Value::HostFn(h) => write!(f, "<host:{}>", h.name),
            Value::VariantConstructor(tag) => write!(f, "<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => write!(f, "<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Bytes(b) => write!(f, "{}", format_bytes_preview(b)),
            Value::TcpListener(t) => write!(f, "<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => write!(f, "<tcp-stream:{}>", t.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

impl Value {
    /// The variant `tag` (a [`Tag`], or a builtin variant of
    /// [`crate::typeinfo::bv`]) with the fields `fields`.
    pub fn variant(tag: impl Into<Tag>, fields: Vec<Value>) -> Value {
        Value::Variant(tag.into(), fields)
    }

    /// A record of the builtin record type `ty` (`ty::DATE`).
    pub fn builtin_record(ty: crate::defs::TypeId, fields: BTreeMap<String, Value>) -> Value {
        Value::Record(crate::typeinfo::builtin_type(ty).clone(), Arc::new(fields))
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
            Value::Record(ty, fields) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.format_silt()))
                    .collect();
                format!("{} {{{}}}", ty.name, items.join(", "))
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    name.name().to_string()
                } else {
                    let items: Vec<String> = fields.iter().map(|v| v.format_silt()).collect();
                    format!("{name}({})", items.join(", "))
                }
            }
            Value::VmClosure(_) => "<fn>".to_string(),
            Value::BuiltinFn(_) | Value::HostFn(_) => "<fn>".to_string(),
            Value::VariantConstructor(tag) => format!("<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => format!("<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => format!("<type:{name}>"),
            Value::Channel(ch) => format!("<channel:{}>", ch.id),
            Value::Handle(h) => format!("<handle:{}>", h.id),
            Value::Bytes(b) => format_bytes_preview(b),
            Value::TcpListener(t) => format!("<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => format!("<tcp-stream:{}>", t.id),
            Value::Unit => "()".to_string(),
        }
    }
}

/// Shared rendering for `Bytes` values: short hex preview + length.
/// Truncates to the first 32 bytes with an ellipsis to keep output
/// readable for large buffers (e.g. tcp.read(conn, 4096)).
fn format_bytes_preview(b: &[u8]) -> String {
    const PREVIEW: usize = 32;
    if b.len() <= PREVIEW {
        let hex: Vec<String> = b.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("bytes({}, length: {})", hex.join(" "), b.len())
    } else {
        let hex: Vec<String> = b[..PREVIEW]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("bytes({} …, length: {})", hex.join(" "), b.len())
    }
}

impl Value {
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
            Value::Record(ty, fields) => match ty.id {
                ty::DATE => {
                    let y = val_i64(fields.get("year"));
                    let m = val_i64(fields.get("month"));
                    let d = val_i64(fields.get("day"));
                    write!(f, "{y:04}-{m:02}-{d:02}")
                }
                ty::TIME => {
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
                ty::DATE_TIME => {
                    if let (Some(date), Some(time)) = (fields.get("date"), fields.get("time")) {
                        write!(f, "{date}T{time}")
                    } else {
                        write!(f, "DateTime {{}}")
                    }
                }
                ty::DURATION => fmt_duration(f, val_i64(fields.get("ns"))),
                _ => {
                    write!(f, "{} {{", ty.name)?;
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
                // Stdlib error variants render via
                // their `Error::message()` implementation so that
                // `format!("{e}")` and `e.message()` produce the same
                // text — the "one way" principle. User enums are
                // unaffected (the registry only covers stdlib errors).
                if let Some(msg) =
                    crate::vm::dispatch::render_stdlib_error_message(name, fields.as_slice())
                {
                    return write!(f, "{msg}");
                }
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
            Value::HostFn(h) => write!(f, "<host:{}>", h.name),
            Value::VariantConstructor(tag) => write!(f, "<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => write!(f, "<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Bytes(b) => write!(f, "{}", format_bytes_preview(b)),
            Value::TcpListener(t) => write!(f, "<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => write!(f, "<tcp-stream:{}>", t.id),
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
    // Zip the list against an increasing counter so the intent (one
    // list item per integer in [lo, hi]) is structural.
    for (cur, item) in (lo..=hi).zip(list.iter()) {
        match item {
            Value::Int(n) if *n == cur => {}
            _ => return false,
        }
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
            // The typechecker lets a nominal record and an anonymous
            // record of the same shape meet (`unify_anon_nominal`) with
            // no change to the value, so when either side is anonymous
            // the fields alone decide. Two nominal types (`Person{x:1}`
            // vs `Car{x:1}`) are never unified and compare unequal.
            (Value::Record(ta, fa), Value::Record(tb, fb)) => {
                if ta.is_anon() || tb.is_anon() {
                    fa == fb
                } else {
                    ta.id == tb.id && fa == fb
                }
            }
            (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a.id == b.id,
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a == b,
            (Value::Channel(a), Value::Channel(b)) => a.id == b.id,
            // Structural equality — same content, regardless of Arc identity.
            // This is the load-bearing forward-compat decision for the
            // future native `Type::Bytes`: equality semantics must already
            // match what a value-type byte array would do.
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            // Tcp handles: identity-based, like Channel/Handle.
            (Value::TcpListener(a), Value::TcpListener(b)) => a.id == b.id,
            (Value::TcpStream(a), Value::TcpStream(b)) => a.id == b.id,
            // Handle / VmClosure / BuiltinFn / VariantConstructor: without
            // explicit arms here, the catch-all `_ => false` would violate
            // reflexivity (`h == h` returning false) and break the Eq/Ord
            // contract — `impl Ord` (below) returns `Equal` for identical
            // instances while `PartialEq` returned `false`, so BTreeSet /
            // BTreeMap silently dropped duplicates. Mirror the identity
            // rules from `impl Ord`:
            //   - Handle: id equality (TaskHandle is heap-allocated, id is unique).
            //   - VmClosure: Arc pointer equality (closures carry captured
            //     upvalues; two closures of the same function with different
            //     upvalues must NOT compare equal).
            //   - BuiltinFn: string equality (builtins are by name).
            //   - VariantConstructor: variant equality.
            // Cross-kind pairs still fall through to `_ => false`.
            (Value::Handle(a), Value::Handle(b)) => a.id == b.id,
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a == b,
            (Value::HostFn(a), Value::HostFn(b)) => a.name == b.name,
            (Value::VariantConstructor(a), Value::VariantConstructor(b)) => a == b,
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
                Value::HostFn(_) => 22,
                Value::VariantConstructor(..) => 16,
                Value::TypeDescriptor(_) => 17,
                Value::PrimitiveDescriptor(_) => 18,
                Value::Bytes(_) => 19,
                Value::TcpListener(_) => 20,
                Value::TcpStream(_) => 21,
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
            (Value::Record(ta, fa), Value::Record(tb, fb)) => {
                // Mirror the `<anon>` wildcard of `PartialEq`: when either
                // side is an anonymous record, the typechecker has
                // already decided these are the same type, and Ord must
                // agree so that `a == b ⇒ cmp(a, b) == Equal`; otherwise
                // BTreeSet / BTreeMap would treat equal values as
                // distinct. Records of one builtin time type order by
                // their fields from the largest unit down; any other
                // record by its fields in name order.
                if ta.is_anon() || tb.is_anon() {
                    fa.iter().cmp(fb.iter())
                } else {
                    ta.id.cmp(&tb.id).then_with(|| match ta.id {
                        ty::DATE => cmp_record_field(fa, fb, "year")
                            .then_with(|| cmp_record_field(fa, fb, "month"))
                            .then_with(|| cmp_record_field(fa, fb, "day")),
                        ty::TIME => cmp_record_field(fa, fb, "hour")
                            .then_with(|| cmp_record_field(fa, fb, "minute"))
                            .then_with(|| cmp_record_field(fa, fb, "second"))
                            .then_with(|| cmp_record_field(fa, fb, "ns")),
                        ty::DATE_TIME => cmp_record_field(fa, fb, "date")
                            .then_with(|| cmp_record_field(fa, fb, "time")),
                        _ => fa.iter().cmp(fb.iter()),
                    })
                }
            }
            // Variants of one enum order by declaration, then by their
            // fields (see `Tag`'s `Ord`).
            (Value::Variant(ta, fa), Value::Variant(tb, fb)) => ta.cmp(tb).then_with(|| fa.cmp(fb)),
            (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a.id.cmp(&b.id),
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a.cmp(b),
            (Value::Channel(a), Value::Channel(b)) => a.id.cmp(&b.id),
            // Structural lex comparison on bytes — required for Eq/Ord
            // consistency with structural PartialEq above. BTreeMap/BTreeSet
            // key contracts depend on this.
            (Value::Bytes(a), Value::Bytes(b)) => a.as_slice().cmp(b.as_slice()),
            // Tcp handles ordered by id (identity), matching PartialEq.
            (Value::TcpListener(a), Value::TcpListener(b)) => a.id.cmp(&b.id),
            (Value::TcpStream(a), Value::TcpStream(b)) => a.id.cmp(&b.id),
            // Handle / VmClosure / BuiltinFn / VariantConstructor: PartialEq
            // returns `false` for every pair (catch-all `_ => false` arm at
            // ~line 1028), so Ord must never return `Equal` for distinct
            // instances either — otherwise BTreeSet / BTreeMap silently drop
            // what they see as duplicates (Ord contract: a == b ⇒ cmp == Equal,
            // contrapositively a != b ⇒ cmp != Equal). We order by identity
            // (`id` field for TaskHandle, Arc pointer address for VmClosure)
            // and by contents for the name-carrying variants.
            (Value::Handle(a), Value::Handle(b)) => a.id.cmp(&b.id),
            (Value::VmClosure(a), Value::VmClosure(b)) => {
                (Arc::as_ptr(a) as usize).cmp(&(Arc::as_ptr(b) as usize))
            }
            (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a.cmp(b),
            (Value::HostFn(a), Value::HostFn(b)) => a.name.cmp(&b.name),
            (Value::VariantConstructor(a), Value::VariantConstructor(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

// ── Host functions ─────────────────────────────────────────────────

/// The Rust side of a host function: it takes the call's arguments and
/// gives its result.
pub type HostImpl = Arc<dyn Fn(&[Value]) -> Result<Value, VmError> + Send + Sync>;

/// A function of a host module, as the VM calls it.
pub struct HostFn {
    /// The function's name, qualified by its module (`mylib.double`).
    pub name: String,
    pub call: HostImpl,
    /// What its signature says it returns: each result is checked
    /// against it.
    pub returns: HostShape,
}

/// The shape of the values of a type a host function's signature
/// names, as far as a value shows it: a type variable, or a type whose
/// values are not told apart here, admits anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostShape {
    Any,
    Int,
    Float,
    Bool,
    String,
    Bytes,
    Unit,
    List(Box<HostShape>),
    Set(Box<HostShape>),
    Map(Box<HostShape>, Box<HostShape>),
    Option(Box<HostShape>),
    Result(Box<HostShape>, Box<HostShape>),
    Tuple(Vec<HostShape>),
}

impl HostShape {
    /// Whether `value` is a value of the shape.
    pub fn admits(&self, value: &Value) -> bool {
        match (self, value) {
            (HostShape::Any, _)
            | (HostShape::Int, Value::Int(_))
            | (HostShape::Float, Value::Float(_))
            | (HostShape::Bool, Value::Bool(_))
            | (HostShape::String, Value::String(_))
            | (HostShape::Bytes, Value::Bytes(_))
            | (HostShape::Unit, Value::Unit) => true,
            (HostShape::List(item), Value::List(items)) => items.iter().all(|v| item.admits(v)),
            (HostShape::List(item), Value::Range(..)) => item.admits(&Value::Int(0)),
            (HostShape::Set(item), Value::Set(items)) => items.iter().all(|v| item.admits(v)),
            (HostShape::Map(k, v), Value::Map(entries)) => entries
                .iter()
                .all(|(key, value)| k.admits(key) && v.admits(value)),
            (HostShape::Option(item), Value::Variant(tag, payload)) => match payload.as_slice() {
                [v] if tag.is(bv::SOME) => item.admits(v),
                [] => tag.is(bv::NONE),
                _ => false,
            },
            (HostShape::Result(ok, err), Value::Variant(tag, payload)) => {
                match payload.as_slice() {
                    [v] if tag.is(bv::OK) => ok.admits(v),
                    [v] if tag.is(bv::ERR) => err.admits(v),
                    _ => false,
                }
            }
            (HostShape::Tuple(items), Value::Tuple(values)) => {
                items.len() == values.len()
                    && items.iter().zip(values).all(|(item, v)| item.admits(v))
            }
            _ => false,
        }
    }
}

impl fmt::Display for HostShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, name: &str, items: &[&HostShape]| {
            write!(f, "{name}(")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{item}")?;
            }
            write!(f, ")")
        };
        match self {
            HostShape::Any => write!(f, "_"),
            HostShape::Int => write!(f, "Int"),
            HostShape::Float => write!(f, "Float"),
            HostShape::Bool => write!(f, "Bool"),
            HostShape::String => write!(f, "String"),
            HostShape::Bytes => write!(f, "Bytes"),
            HostShape::Unit => write!(f, "()"),
            HostShape::List(item) => list(f, "List", &[item]),
            HostShape::Set(item) => list(f, "Set", &[item]),
            HostShape::Map(k, v) => list(f, "Map", &[k, v]),
            HostShape::Option(item) => list(f, "Option", &[item]),
            HostShape::Result(ok, err) => list(f, "Result", &[ok, err]),
            HostShape::Tuple(items) => list(f, "", &items.iter().collect::<Vec<_>>()),
        }
    }
}

impl fmt::Debug for HostFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFn({})", self.name)
    }
}

// ── Host function conversion traits ────────────────────────────────

/// Convert a `Value` into a Rust type.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

/// Convert a Rust type into a `Value`. The conversion fails when the
/// Rust value has no silt counterpart (a NaN or infinite `f64`: a silt
/// `Float` is always finite); a host function whose result fails to
/// convert raises a runtime error.
pub trait IntoValue {
    fn into_value(self) -> Result<Value, String>;
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(value.clone())
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Result<Value, String> {
        Ok(self)
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Int(self))
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Float(n) => Ok(*n),
            Value::Int(n) => Ok(*n as f64),
            other => Err(format!("expected Float, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Result<Value, String> {
        // A silt `Float` is always finite and never `-0.0`: a NaN or
        // infinite result has no silt value, so it is an error rather
        // than a `Float` that every comparison, hash and container path
        // would mishandle.
        if !self.is_finite() {
            return Err(format!("non-finite float result: {self}"));
        }
        Ok(Value::Float(if self == 0.0 { 0.0 } else { self }))
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Bool(self))
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self))
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self.to_string()))
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Unit)
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::List(Arc::new(self)))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Some(v) => Value::variant(bv::SOME, vec![v.into_value()?]),
            None => Value::variant(bv::NONE, vec![]),
        })
    }
}

impl<T: IntoValue> IntoValue for Result<T, String> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Ok(v) => Value::variant(bv::OK, vec![v.into_value()?]),
            Err(e) => Value::variant(bv::ERR, vec![Value::String(e)]),
        })
    }
}

/// Surface kind name used by the `FromValue` impls' diagnostic shape
/// (`"expected <Kind>, got <kind>"`). Round 76 ERR-1 GAP collapse: this
/// previously hand-rolled its own match arms that drifted from the two
/// canonical kind oracles (`builtins::common::value_kind` and
/// `vm::Vm::type_name`) on four variants — `BuiltinFn` ("Fn" vs
/// "BuiltinFn"), `VariantConstructor` ("Constructor" vs
/// "VariantConstructor"), `TypeDescriptor` ("Type" vs "TypeDescriptor"),
/// `PrimitiveDescriptor` ("Type" vs "PrimitiveDescriptor"). Round 75
/// ERR-1 GAP unified `value_kind` with `Vm::type_name` for all 22
/// variants; this helper now delegates to that canonical source so a
/// single edit to one match arm propagates to every conversion error message.
/// Per the project's "one way to do things" convention.
///
/// Locked by `tests/meta/round76_value_type_name_parity_tests.rs`.
fn value_type_name(v: &Value) -> &'static str {
    crate::builtins::value_kind(v)
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Explicit per-category tags (NOT `std::mem::discriminant(self)`):
        // the Hash/Eq contract requires that `a == b ⇒ hash(a) == hash(b)`,
        // and `PartialEq` admits a cross-discriminant equal pair:
        // `List(xs) == Range(lo, hi)` when xs materializes the range.
        // Therefore List and Range MUST share a tag AND hash the same
        // materialized sequence.
        match self {
            Value::Unit => {
                state.write_u8(0);
            }
            Value::Bool(b) => {
                state.write_u8(1);
                b.hash(state);
            }
            Value::Int(n) => {
                state.write_u8(2);
                n.hash(state);
            }
            // Canonicalize -0.0 → 0.0 so that 0.0/-0.0 (which compare
            // equal) hash equally.
            Value::Float(f) => {
                state.write_u8(3);
                let bits = if *f == 0.0 {
                    0.0_f64.to_bits()
                } else {
                    f.to_bits()
                };
                bits.hash(state);
            }
            Value::String(s) => {
                state.write_u8(4);
                s.hash(state);
            }
            // List and Range share tag 5 (any-list-shape). Range hashes its
            // materialized `[Int(lo), Int(lo+1), ..., Int(hi)]` sequence so
            // the contract holds: a `List` and a `Range` that compare equal
            // produce the same byte stream into the hasher.
            Value::List(xs) => {
                state.write_u8(5);
                xs.len().hash(state);
                for x in xs.iter() {
                    x.hash(state);
                }
            }
            // Round 92 (BROKEN): the element-by-element walk below used to be
            // unconditional, so `(0..4_000_000_000).hash()` spun ~4e9
            // iterations inside a single opcode — uninterruptible by the
            // time-slice scheduler — and `0..i64::MAX` hung forever. The walk
            // is now capped at `MAX_RANGE_MATERIALIZE`; over-cap ranges hash a
            // closed form of their endpoints instead. This preserves the
            // Hash/Eq contract (`a == b ⇒ hash(a) == hash(b)`, round 74):
            //   - within the cap: identical byte stream to the equal `List`
            //     (`tag 5, len, Int(lo) .. Int(hi)`), so List ↔ Range equal
            //     pairs still hash equal;
            //   - over the cap: every list-producing site enforces
            //     `MAX_RANGE_MATERIALIZE` (vm/execute.rs, builtins/*), so no
            //     `Value::List` can ever have > cap elements and no List can
            //     compare equal to an over-cap Range. The only values equal
            //     to such a Range are Ranges, and non-empty equal Ranges have
            //     identical endpoints (see `PartialEq` ~line 1693), so
            //     hashing `(len, lo, hi)` is contract-safe. (Empty ranges all
            //     compare equal regardless of endpoints; they take the
            //     `len == 0` path and hash only `(tag, 0)`, as before.)
            // Doing the cap inside `impl Hash` (rather than erroring in the
            // dispatch arm) also bounds nested walks for free: auto-derived
            // `.hash()` on records/variants/tuples/lists recurses into this
            // arm for embedded range fields.
            Value::Range(lo, hi) => {
                state.write_u8(5);
                let len = range_len(*lo, *hi);
                (len as usize).hash(state);
                if len > 0 {
                    if len as u128 <= MAX_RANGE_MATERIALIZE as u128 {
                        for n in *lo..=*hi {
                            Value::Int(n).hash(state);
                        }
                    } else {
                        lo.hash(state);
                        hi.hash(state);
                    }
                }
            }
            Value::Tuple(vs) => {
                state.write_u8(6);
                vs.len().hash(state);
                for v in vs {
                    v.hash(state);
                }
            }
            Value::Map(m) => {
                state.write_u8(7);
                m.len().hash(state);
                for (k, v) in m.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Set(s) => {
                state.write_u8(8);
                s.len().hash(state);
                for v in s.iter() {
                    v.hash(state);
                }
            }
            Value::Record(_, fields) => {
                state.write_u8(9);
                // Do NOT hash the type. `PartialEq` treats an anonymous
                // record as equal to a nominal one with the same fields
                // (the `<anon>` wildcard), so the Hash contract `a == b ⇒
                // hash(a) == hash(b)` requires the same fields to hash to
                // the same value whatever the type. Two distinct nominal
                // types with the same fields (`Person{x:1}` vs
                // `Car{x:1}`) still compare unequal, so they just
                // hash-collide and are told apart by Eq.
                for (k, v) in fields.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Variant(tag, fields) => {
                state.write_u8(10);
                tag.hash(state);
                fields.len().hash(state);
                for f in fields {
                    f.hash(state);
                }
            }
            Value::Channel(ch) => {
                state.write_u8(11);
                ch.id.hash(state);
            }
            Value::Handle(h) => {
                state.write_u8(12);
                h.id.hash(state);
            }
            // Content-hash bytes — Eq/Hash contract requires the same
            // structural treatment as PartialEq.
            Value::Bytes(b) => {
                state.write_u8(13);
                b.len().hash(state);
                state.write(b.as_slice());
            }
            Value::TcpListener(t) => {
                state.write_u8(14);
                t.id.hash(state);
            }
            Value::TcpStream(t) => {
                state.write_u8(15);
                t.id.hash(state);
            }
            Value::VmClosure(_) => {
                state.write_u8(16);
                // not meaningfully hashable
            }
            Value::BuiltinFn(name) => {
                state.write_u8(17);
                name.hash(state);
            }
            Value::HostFn(h) => {
                state.write_u8(21);
                h.name.hash(state);
            }
            Value::VariantConstructor(tag) => {
                state.write_u8(18);
                tag.hash(state);
            }
            Value::TypeDescriptor(ty) => {
                state.write_u8(19);
                ty.id.hash(state);
            }
            Value::PrimitiveDescriptor(name) => {
                state.write_u8(20);
                name.hash(state);
            }
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
        Value::builtin_record(ty::DATE, fields)
    }

    fn make_time(hour: i64, minute: i64, second: i64, ns: i64) -> Value {
        let mut fields = BTreeMap::new();
        fields.insert("hour".to_string(), Value::Int(hour));
        fields.insert("minute".to_string(), Value::Int(minute));
        fields.insert("second".to_string(), Value::Int(second));
        fields.insert("ns".to_string(), Value::Int(ns));
        Value::builtin_record(ty::TIME, fields)
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
        let a = Value::variant(bv::OK, vec![Value::Int(42)]);
        let b = Value::variant(bv::OK, vec![Value::Int(42)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    // ── PartialEq edge cases ───────────────────────────────────────

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
        let monday = Value::variant(bv::MONDAY, vec![]);
        let tuesday = Value::variant(bv::TUESDAY, vec![]);
        let friday = Value::variant(bv::FRIDAY, vec![]);
        let sunday = Value::variant(bv::SUNDAY, vec![]);
        assert!(monday < tuesday, "Monday < Tuesday");
        assert!(tuesday < friday, "Tuesday < Friday");
        assert!(friday < sunday, "Friday < Sunday");
        assert_eq!(
            Value::variant(bv::WEDNESDAY, vec![]).cmp(&Value::variant(bv::WEDNESDAY, vec![])),
            Ordering::Equal,
        );
    }

    #[test]
    fn ord_result_variants_decl_order() {
        // Result is declared as `type Result(a, e) { Ok(a), Err(e) }`,
        // so Ok has ordinal 0, Err ordinal 1, and `Ok < Err`.
        let ok = Value::variant(bv::OK, vec![Value::Int(1)]);
        let err = Value::variant(bv::ERR, vec![Value::String("e".into())]);
        assert!(ok < err, "Ok declared before Err → Ok < Err");
    }

    /// Two enums with variants of one name each order by their own
    /// declaration, and their variants are not equal.
    #[test]
    fn variants_of_one_name_in_two_enums_stay_apart() {
        use crate::defs::{DefId, TypeId};
        let a = TypeInfo::new_enum(TypeId(DefId(9000)), "A", &[("Red", 0), ("Blue", 0)]);
        let b = TypeInfo::new_enum(TypeId(DefId(9001)), "B", &[("Blue", 0), ("Red", 0)]);
        let value = |ty: &Arc<TypeInfo>, name: &str| {
            Value::Variant(Tag::named(ty, name).expect("a variant"), vec![])
        };
        assert!(value(&a, "Red") < value(&a, "Blue"));
        assert!(value(&b, "Blue") < value(&b, "Red"));
        assert_ne!(value(&a, "Red"), value(&b, "Red"));
        assert_eq!(hash_of(&value(&a, "Red")), hash_of(&value(&a, "Red")));
    }

    /// A program's record type named like a builtin one prints as a
    /// record: Display is keyed by the builtin type's id, not its name.
    #[test]
    fn a_program_type_named_time_is_not_the_builtin_time() {
        use crate::defs::{DefId, TypeId};
        let ty = TypeInfo::new_record(TypeId(DefId(9002)), "Time", Vec::new());
        let mut fields = BTreeMap::new();
        fields.insert("h".to_string(), Value::Int(1));
        let rec = Value::Record(ty, Arc::new(fields));
        assert_eq!(format!("{rec}"), "Time {h: 1}");
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
        assert_eq!(format!("{}", Value::variant(bv::NONE, vec![])), "None");
    }

    #[test]
    fn display_variant_with_fields() {
        let v = Value::variant(bv::SOME, vec![Value::Int(42)]);
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
        let ty = TypeInfo::new_record(
            crate::defs::TypeId(crate::defs::DefId(9003)),
            "Point",
            Vec::new(),
        );
        let rec = Value::Record(ty, Arc::new(fields));
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
