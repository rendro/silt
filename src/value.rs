use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use crate::bytecode;

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
    /// (wakes tasks blocked on receive/select/each).
    recv_wakers: Mutex<VecDeque<Waker>>,
    /// Wakers to call when buffer space becomes available
    /// (wakes tasks blocked on send when buffer was full).
    send_wakers: Mutex<VecDeque<Waker>>,
    /// For rendezvous (capacity == 0): a parked sender places its value here.
    /// The receiver takes it directly, completing the handshake.
    handoff: Mutex<Option<Value>>,
    /// Number of receivers currently waiting (waker-based + condvar-based).
    /// Used by rendezvous try_send to detect if a direct handoff is possible.
    waiting_receivers: AtomicUsize,
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
            handoff: Mutex::new(None),
            waiting_receivers: AtomicUsize::new(0),
        }
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
        self.closed.store(true, AtomicOrdering::Release);
        // Clear any pending handoff value.
        *self.handoff.lock() = None;
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
    pub fn register_recv_waker(&self, waker: Waker) {
        self.waiting_receivers.fetch_add(1, AtomicOrdering::Release);
        self.recv_wakers.lock().push_back(waker);
        // Double-check: if data is now available or channel closed, wake immediately.
        let has_data_or_closed = if self.is_rendezvous() {
            self.handoff.lock().is_some() || self.closed.load(AtomicOrdering::Acquire)
        } else {
            let buf = self.buffer.lock();
            !buf.is_empty() || self.closed.load(AtomicOrdering::Acquire)
        };
        if has_data_or_closed {
            // Drain and fire all recv wakers — the channel state changed.
            let wakers: VecDeque<Waker> = {
                let mut guard = self.recv_wakers.lock();
                std::mem::take(&mut *guard)
            };
            let count = wakers.len();
            for w in wakers {
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
    }

    /// Register a waker to be called when buffer space becomes available.
    ///
    /// Uses a double-check pattern to avoid lost wakeups: after registering,
    /// re-checks buffer space availability. If space opened up between the
    /// caller's `try_send` and this registration, the waker fires immediately.
    pub fn register_send_waker(&self, waker: Waker) {
        self.send_wakers.lock().push_back(waker);
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
            let wakers: VecDeque<Waker> = {
                let mut guard = self.send_wakers.lock();
                std::mem::take(&mut *guard)
            };
            for w in wakers {
                w();
            }
        }
    }

    /// Wake one task blocked on receive (FIFO — oldest waiter first).
    fn wake_recv(&self) {
        let waker = self.recv_wakers.lock().pop_front();
        if let Some(w) = waker {
            self.waiting_receivers.fetch_sub(1, AtomicOrdering::Release);
            w();
        }
    }

    /// Wake all tasks blocked on receive (used when channel is closed).
    fn wake_all_recv(&self) {
        let wakers: VecDeque<Waker> = {
            let mut guard = self.recv_wakers.lock();
            std::mem::take(&mut *guard)
        };
        let count = wakers.len();
        for w in wakers {
            w();
        }
        self.waiting_receivers
            .fetch_sub(count, AtomicOrdering::Release);
    }

    /// Wake one task blocked on send (FIFO — oldest waiter first).
    fn wake_send(&self) {
        let waker = self.send_wakers.lock().pop_front();
        if let Some(w) = waker {
            w();
        }
    }

    /// Wake all tasks blocked on send (used when channel is closed).
    fn wake_all_send(&self) {
        let wakers: VecDeque<Waker> = {
            let mut guard = self.send_wakers.lock();
            std::mem::take(&mut *guard)
        };
        for w in wakers {
            w();
        }
    }
}

/// Handle to a spawned task. Thread-safe — shared between spawner and worker.
pub struct TaskHandle {
    pub id: usize,
    result: Mutex<Option<Result<Value, String>>>,
    condvar: Condvar,
    /// Wakers to call when the task completes (for scheduler-based join).
    join_wakers: Mutex<Vec<Waker>>,
}

impl TaskHandle {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            result: Mutex::new(None),
            condvar: Condvar::new(),
            join_wakers: Mutex::new(Vec::new()),
        }
    }

    /// Store the task result and notify any joiners.
    pub fn complete(&self, result: Result<Value, String>) {
        {
            let mut guard = self.result.lock();
            *guard = Some(result);
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
    pub fn join(&self) -> Result<Value, String> {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.take() {
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Result<Value, String>> {
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

    /// Store the I/O result and notify all waiters.
    pub fn complete(&self, value: Value) {
        {
            let mut guard = self.result.lock();
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
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Value> {
        self.result.lock().clone()
    }

    /// Blocking wait (for main thread).
    pub fn wait(&self) -> Value {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.take() {
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

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::ExtFloat(a), Value::ExtFloat(b)) => a.to_bits() == b.to_bits(),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => na == nb && fa == fb,
            (Value::Unit, Value::Unit) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Range(a1, a2), Value::Range(b1, b2)) => a1 == b1 && a2 == b2,
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
                a.partial_cmp(b).unwrap_or_else(|| {
                    // NaN handling: treat as equal for ordering purposes
                    a.to_bits().cmp(&b.to_bits())
                })
            }
            (Value::ExtFloat(a), Value::ExtFloat(b)) => a.to_bits().cmp(&b.to_bits()),
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.as_slice().cmp(b.as_slice()),
            (Value::Range(a1, a2), Value::Range(b1, b2)) => a1.cmp(b1).then_with(|| a2.cmp(b2)),
            // Range vs List: compare by start element
            (Value::Range(..), Value::List(..)) | (Value::List(..), Value::Range(..)) => {
                Ordering::Equal
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
