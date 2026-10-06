//! Concurrency builtin functions (`channel.*`, `task.*`).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crate::runtime::channel::{Channel, TryReceiveResult, TrySendResult};
use crate::runtime::handle::TaskHandle;
use crate::typeinfo::{bv, ty};
use crate::value::Value;
use crate::vm::{BlockReason, Native, SelectOpKind, Step, Vm, VmError};

/// Build the canonical closed-channel-send VmError (message wording is
/// pinned by tests in `tests/lang/error_tests.rs` and `tests/heavy/integration.rs`,
/// and the round86 regression-lock test asserts the `format!` literal for
/// it appears in exactly one place in this file). Route every
/// closed-channel try_send branch through this helper so the message
/// cannot drift across the 5 sites.
fn closed_channel_send_err(id: usize) -> VmError {
    VmError::new(format!("send on closed channel {id}"))
}

/// Dispatch the builtin `trait Error for ChannelError` method table.
/// Scaffolding lives in `super::dispatch_error_trait`; this site just
/// supplies the variant → message rendering.
pub fn call_channel_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("ChannelError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("ChannelTimeout", []) => "channel receive timed out".to_string(),
            ("ChannelClosed", []) => "channel closed with no more values".to_string(),
            _ => return None,
        })
    })
}

/// Dispatch `channel.<name>(args)`.
pub(crate) fn call_channel(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "channel.each takes 2 arguments (channel, function)".into(),
                ));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.each requires a channel as first argument".into(),
                ));
            };
            Ok(Step::Run(Box::new(Each {
                ch: ch.clone(),
                callback: args[1].clone(),
                called: false,
            })))
        }
        _ => channel_plain(vm, name, args).map(Step::Done),
    }
}

/// `channel.each(ch, f)`: `f` is called with each message of `ch` until
/// the channel is closed.
struct Each {
    ch: Arc<Channel>,
    callback: Value,
    /// The input is the value of a call of the callback.
    called: bool,
}

impl Each {
    fn call(&mut self, vm: &mut Vm, message: Value) -> Step {
        self.called = true;
        vm.call(self.callback.clone(), [message])
    }
}

impl Native for Each {
    fn name(&self) -> &str {
        "channel.each"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        if std::mem::take(&mut self.called) {
            // After each message, give way to the other tasks.
            return Ok(Step::Park);
        }
        match self.ch.try_receive() {
            TryReceiveResult::Value(message) => Ok(self.call(vm, message)),
            TryReceiveResult::Closed => Ok(Step::Done(Value::Unit)),
            TryReceiveResult::Empty => {
                vm.block_reason = Some(BlockReason::Receive(self.ch.clone()));
                Ok(Step::Park)
            }
        }
    }
}

/// The `channel` functions that call no function.
fn channel_plain(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "new" => {
            let capacity = match args.len() {
                0 => 0,
                1 => match &args[0] {
                    Value::Int(n) if *n >= 0 => *n as usize,
                    _ => {
                        return Err(VmError::new(
                            "channel.new capacity must be a non-negative integer".into(),
                        ));
                    }
                },
                _ => return Err(VmError::new("channel.new takes 0 or 1 arguments".into())),
            };
            let id = vm.next_channel_id();
            Ok(Value::Channel(Arc::new(Channel::new(id, capacity))))
        }
        "send" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "channel.send takes 2 arguments (channel, value)".into(),
                ));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.send requires a channel as first argument".into(),
                ));
            };
            let ch = ch.clone();
            // Try non-blocking first.
            match ch.try_send(args[1].clone()) {
                TrySendResult::Sent => return Ok(Value::Unit),
                TrySendResult::Closed => {
                    return Err(closed_channel_send_err(ch.id));
                }
                TrySendResult::Full => {}
            }
            // The buffer is full: the task waits.
            Err(vm.park_with_reason(args, BlockReason::Send(ch)))
        }
        "receive" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "channel.receive takes 1 argument (channel)".into(),
                ));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.receive requires a channel argument".into(),
                ));
            };
            let ch = ch.clone();
            // Try non-blocking first.
            match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    return Ok(Value::variant(bv::MESSAGE, vec![val]));
                }
                TryReceiveResult::Closed => {
                    return Ok(Value::variant(bv::CLOSED, vec![]));
                }
                TryReceiveResult::Empty => {}
            }
            // The channel is empty: the task waits.
            Err(vm.park_with_reason(args, BlockReason::Receive(ch)))
        }
        "close" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "channel.close takes 1 argument (channel)".into(),
                ));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.close requires a channel argument".into(),
                ));
            };
            ch.close();
            Ok(Value::Unit)
        }
        "try_send" => {
            if args.len() != 2 {
                return Err(VmError::new("channel.try_send takes 2 arguments".into()));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new("channel.try_send requires a channel".into()));
            };
            match ch.try_send(args[1].clone()) {
                TrySendResult::Sent => Ok(Value::Bool(true)),
                TrySendResult::Full | TrySendResult::Closed => Ok(Value::Bool(false)),
            }
        }
        "try_receive" => {
            if args.len() != 1 {
                return Err(VmError::new("channel.try_receive takes 1 argument".into()));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.try_receive requires a channel".into(),
                ));
            };
            match ch.try_receive() {
                TryReceiveResult::Value(val) => Ok(Value::variant(bv::MESSAGE, vec![val])),
                TryReceiveResult::Empty => Ok(Value::variant(bv::EMPTY, Vec::new())),
                TryReceiveResult::Closed => Ok(Value::variant(bv::CLOSED, Vec::new())),
            }
        }
        "select" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "channel.select takes 1 argument (list of operations)".into(),
                ));
            }
            let Value::List(ops_list) = &args[0] else {
                return Err(VmError::new(
                    "channel.select argument must be a list".into(),
                ));
            };

            // Parse operations: bare Channel = receive, (Channel, value) = send.
            let ops = parse_select_ops(ops_list)?;
            if ops.is_empty() {
                return Err(VmError::new(
                    "channel.select requires at least one operation".into(),
                ));
            }

            // Try all operations non-blocking first.
            if let Some(result) = try_select_sweep(&ops)? {
                return Ok(result);
            }

            // Build op descriptors for the scheduler.
            let select_ops: Vec<(Arc<Channel>, SelectOpKind)> = ops
                .iter()
                .map(|op| match op {
                    SelectOp::Receive(ch) => (ch.clone(), SelectOpKind::Receive),
                    SelectOp::Send(ch, _) => (ch.clone(), SelectOpKind::Send),
                })
                .collect();

            // No operation is possible: the task waits.
            Err(vm.park_with_reason(args, BlockReason::Select(select_ops)))
        }
        "recv_timeout" => {
            // channel.recv_timeout(ch, dur) -> Result(a, String)
            //
            // Blocking receive with a scoped timeout. Semantics:
            //
            //   * Ok(value)       — a value was delivered within the timeout.
            //                       (A value already buffered or a rendezvous
            //                       sender already parked wins over an expired
            //                       timer: try_receive is always attempted
            //                       first, even at duration == 0.)
            //   * Err("closed")   — the channel is closed and empty.
            //   * Err("timeout")  — the timeout elapsed with no value and no
            //                       close. A `ceil-to-ms` rounding is applied
            //                       so any positive sub-ms duration waits at
            //                       least one timer tick.
            //
            // Negative duration → construction error. Zero duration → try_recv
            // semantics (no timer is scheduled).
            //
            // Cancellation: the inner select's per-arm `WakerRegistration`
            // guards deregister the channel-side waker on task.cancel. The
            // timer registration cannot be cancelled mid-flight — the timer
            // thread fires `ch.close()` later — but the timer channel is
            // private to this call, dropped on return, and
            // `IoCompletion::complete` / `Channel::close` are first-writer-
            // wins, so the stale wake is a harmless no-op.
            //
            // Implementation: reuse the channel.select machinery. An internal
            // timer channel (`channel.timeout`) is built alongside the user's
            // channel, select races the two, and the winning arm is mapped
            // back into a `Result` variant. This keeps all the parking,
            // wake-graph bookkeeping, and cancel-cleanup guarantees from the
            // existing select path.
            if args.len() != 2 {
                return Err(VmError::new(
                    "channel.recv_timeout takes 2 arguments (channel, duration)".into(),
                ));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new(
                    "channel.recv_timeout requires a channel as first argument".into(),
                ));
            };
            let ch = ch.clone();

            // Resume detection. A scheduled task that parks below is called
            // again with args[1] swapped for an internal marker record
            // carrying the ORIGINAL private timer channel (see the park
            // site). Recovering that channel preserves the original absolute
            // deadline across parks: the timer thread will close (or has
            // already closed) that exact channel at the originally scheduled
            // instant. Re-deriving a fresh timer from the user's Duration on
            // every re-entry (the pre-round-101 behavior) livelocked — a
            // wake caused by the timer itself re-armed a brand-new
            // full-length timer whose closed predecessor was no longer in
            // the select ops, so a recv_timeout on a quiet channel inside a
            // spawned task never timed out.
            let resume_timer: Option<Arc<Channel>> = match &args[1] {
                Value::Record(ty, fields) if ty.id == ty::RECV_TIMEOUT => {
                    match fields.get(RECV_TIMEOUT_RESUME_TIMER_FIELD) {
                        Some(Value::Channel(t)) => Some(t.clone()),
                        _ => {
                            return Err(VmError::new(
                                "channel.recv_timeout: malformed internal resume marker".into(),
                            ));
                        }
                    }
                }
                _ => None,
            };

            // Fresh entry: validate the duration up front. Negative duration
            // is a construction error even when a value is already buffered
            // (pinned by tests/concurrency/channel_timeout_tests.rs).
            let mut fresh_dur_ns: i64 = 0;
            if resume_timer.is_none() {
                fresh_dur_ns = crate::builtins::time::extract_duration(&args[1])?;
                if fresh_dur_ns < 0 {
                    return Err(VmError::new(
                        "channel.recv_timeout: duration must be non-negative".into(),
                    ));
                }
            }

            // Always try non-blocking first — delivery beats timeout even at
            // zero duration and on a resume whose wake was the timer expiring
            // (matches the "ready value wins" corner case).
            match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    return Ok(Value::variant(bv::OK, vec![val]));
                }
                TryReceiveResult::Closed => {
                    return Ok(Value::variant(
                        bv::ERR,
                        vec![Value::variant(bv::CHANNEL_CLOSED, vec![])],
                    ));
                }
                TryReceiveResult::Empty => {}
            }
            // Zero duration on an empty channel = instant timeout.
            if resume_timer.is_none() && fresh_dur_ns == 0 {
                return Ok(Value::variant(
                    bv::ERR,
                    vec![Value::variant(bv::CHANNEL_TIMEOUT, vec![])],
                ));
            }

            let timer_ch = match resume_timer {
                Some(t) => t,
                None => {
                    // Ceil the nanosecond duration up to at least 1ms so any
                    // positive sub-ms request still gets a real tick of wait.
                    // The timer wheel is ms-granular.
                    let ms: u64 = {
                        let ns = fresh_dur_ns as u64;
                        ns.div_ceil(1_000_000).max(1)
                    };

                    // Build the private timer channel. Reuses the shared
                    // TimerManager thread — no per-call OS thread.
                    // `TimerManager::schedule` marks the channel as
                    // pending-timer-close so the main-thread deadlock
                    // detector correctly treats a recv-timeout wait as
                    // "external wake pending".
                    let timer_id = vm.next_channel_id();
                    let t = Arc::new(Channel::new(timer_id, 1));
                    vm.runtime
                        .timer
                        .schedule(Duration::from_millis(ms), t.clone())?;
                    t
                }
            };

            // Race ch vs timer_ch via a two-op select. Dropping timer_ch on
            // return deallocates the private channel; any straggling wake
            // from the timer thread into it becomes a no-op.
            let ops = vec![
                SelectOp::Receive(ch.clone()),
                SelectOp::Receive(timer_ch.clone()),
            ];
            // Try non-blocking first so we don't needlessly park on a race
            // that already resolved (e.g. the value landed between the
            // `try_receive` above and here).
            if let Some(val) = try_select_sweep(&ops)? {
                return Ok(map_recv_timeout_result(val, &timer_ch));
            }
            let select_ops: Vec<(Arc<Channel>, SelectOpKind)> = ops
                .iter()
                .map(|op| match op {
                    SelectOp::Receive(c) => (c.clone(), SelectOpKind::Receive),
                    SelectOp::Send(c, _) => (c.clone(), SelectOpKind::Send),
                })
                .collect();
            // We DO re-enter this arm on resume because the parked call
            // is made again — but with arguments that carry the resume
            // marker (SAME timer channel) instead of the user's
            // Duration, so the re-entry races the ORIGINAL absolute
            // deadline rather than arming a fresh full-length timer.
            // Wake causes and their re-entry outcomes:
            //   * value landed during the park → `try_receive` at entry
            //     returns it (delivery beats an expired timer);
            //   * user channel closed → `try_receive` maps to
            //     Err(ChannelClosed);
            //   * timer expired → timer_ch is closed, the
            //     `try_select_sweep` above sees it and
            //     `map_recv_timeout_result` yields Err(ChannelTimeout);
            //   * spurious wake (e.g. a racing sibling consumed the
            //     value) → nothing ready, re-park on the same pair.
            // If the timer fires between the sweep and the waker
            // registration below, `register_recv_waker`'s closed-state
            // double-check fires the waker inline — no lost wakeup.
            let resume_args = vec![
                Value::Channel(ch.clone()),
                make_recv_timeout_resume_marker(&timer_ch),
            ];
            Err(vm.park_with_reason(&resume_args, BlockReason::Select(select_ops)))
        }
        "timeout" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "channel.timeout takes 1 argument (milliseconds)".into(),
                ));
            }
            let Value::Int(ms) = &args[0] else {
                return Err(VmError::new(
                    "channel.timeout requires an Int argument".into(),
                ));
            };
            if *ms < 0 {
                return Err(VmError::new(
                    "channel.timeout duration must be non-negative".into(),
                ));
            }
            let ms = *ms as u64;
            let id = vm.next_channel_id();
            // Use capacity 1 so the timeout channel itself is buffered
            // (we close it, not send to it, so capacity doesn't matter much).
            let ch = Arc::new(Channel::new(id, 1));
            vm.runtime
                .timer
                .schedule(std::time::Duration::from_millis(ms), ch.clone())?;
            Ok(Value::Channel(ch))
        }
        _ => Err(VmError::new(format!("unknown channel function: {name}"))),
    }
}

/// Spawn a child task with an optional scoped deadline, a reading of
/// the host clock. Shared by `task.spawn` (deadline = None) and
/// `task.spawn_until` (deadline = Some(now + dur)). Returns the Handle wrapping the
/// spawned task; propagates scheduler.submit errors unchanged.
fn spawn_with_deadline(
    vm: &mut Vm,
    closure: &Arc<crate::bytecode::VmClosure>,
    deadline: Option<Duration>,
) -> Result<Value, VmError> {
    let task_id = vm.next_task_id();
    // The task belongs to whoever spawns it: the owner of the task that
    // runs this, or the owner the front end set for the program.
    let handle = Arc::new(TaskHandle::with_owner(
        task_id,
        crate::scheduler::current_task_owner(),
    ));

    let child_closure = closure.clone();
    let mut child_vm = vm.spawn_child();
    child_vm.current_deadline = deadline;

    // A target without threads runs the task to its end here.
    #[cfg(target_arch = "wasm32")]
    handle.complete(child_vm.call_blocking(&Value::VmClosure(child_closure), &[]));

    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::scheduler::Task;
        child_vm.start_task(child_closure);
        child_vm.spawned = true;

        vm.scheduler()
            .submit(Task {
                id: task_id,
                vm: child_vm,
                handle: handle.clone(),
            })
            .map_err(VmError::new)?;
    }

    Ok(Value::Handle(handle))
}

/// Dispatch `task.<name>(args)`.
pub(crate) fn call_task(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        "deadline" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "task.deadline takes 2 arguments (duration, fn)".into(),
                ));
            }
            let dur_ns = crate::builtins::time::extract_duration(&args[0])?;
            if dur_ns < 0 {
                return Err(VmError::new(
                    "task.deadline: duration must be non-negative".into(),
                ));
            }
            Ok(Step::Run(Box::new(Deadline {
                after: Duration::from_nanos(dur_ns as u64),
                callback: Some(args[1].clone()),
                outer: None,
            })))
        }
        _ => task_plain(vm, name, args).map(Step::Done),
    }
}

/// `task.deadline(dur, fn)`: runs `fn` with a scoped wall-clock
/// deadline of `dur` from now. I/O inside the callback is watched by
/// the scheduler's I/O watchdog; if the deadline elapses while parked
/// on I/O, the in-flight I/O is cancelled with `Err("I/O timeout
/// (task.deadline exceeded)")`. I/O builtins also check at entry and
/// return the same Err immediately if the deadline is already past.
///
/// Pure-CPU work inside the callback is NOT interrupted — this matches
/// Go's context.WithDeadline semantics.
///
/// A nested task.deadline tightens the deadline (earliest wins); a
/// looser inner deadline cannot extend an outer one. The deadline stays
/// in effect while the task is parked inside the callback.
struct Deadline {
    after: Duration,
    /// The function, until it is called.
    callback: Option<Value>,
    /// The deadline that was in effect outside, while the callback
    /// runs.
    outer: Option<Option<Duration>>,
}

impl Native for Deadline {
    fn name(&self) -> &str {
        "task.deadline"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(callee) = self.callback.take() {
            let outer = vm.current_deadline;
            self.outer = Some(outer);
            // Tighten: earliest of current and new wins.
            vm.current_deadline = match (outer, vm.runtime.io.deadline_after(self.after)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (None, x) | (x, None) => x,
            };
            return Ok(vm.call(callee, []));
        }
        self.abandon(vm);
        Ok(Step::Done(input))
    }

    fn abandon(&mut self, vm: &mut Vm) {
        if let Some(outer) = self.outer.take() {
            vm.current_deadline = outer;
        }
    }
}

/// The `task` functions that call no function.
fn task_plain(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "spawn" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "task.spawn takes 1 argument (a function)".into(),
                ));
            }
            let Value::VmClosure(closure) = &args[0] else {
                return Err(VmError::new(
                    "task.spawn requires a function argument".into(),
                ));
            };
            spawn_with_deadline(vm, closure, None)
        }
        "join" => {
            if args.len() != 1 {
                return Err(VmError::new("task.join takes 1 argument (handle)".into()));
            }
            let Value::Handle(handle) = &args[0] else {
                return Err(VmError::new("task.join requires a handle argument".into()));
            };
            let handle = handle.clone();

            // If already complete, return immediately. This is also
            // where a scheduled task that parked on the join arrives
            // when it runs again.
            if let Some(result) = handle.try_get() {
                // The joiner has the result now. If the task failed,
                // the error is the joiner's to handle and is not
                // reported as an unjoined failure.
                handle.mark_joined();
                return match result {
                    Ok(val) => Ok(val),
                    Err(mut inner) => {
                        inner.message = format!("joined task failed: {}", inner.message);
                        Err(inner)
                    }
                };
            }

            // The task is not finished: this one waits.
            Err(vm.park_with_reason(args, BlockReason::Join(handle)))
        }
        "cancel" => {
            if args.len() != 1 {
                return Err(VmError::new("task.cancel takes 1 argument (handle)".into()));
            }
            let Value::Handle(handle) = &args[0] else {
                return Err(VmError::new(
                    "task.cancel requires a handle argument".into(),
                ));
            };
            handle.complete(Err(VmError::new("cancelled".to_string())));
            // Cancelling a task handles it. If the task had failed
            // before, its failure is dismissed: it is not reported as
            // an unjoined failure. A join still raises that failure,
            // because the handle keeps the result that came first.
            handle.mark_joined();
            Ok(Value::Unit)
        }
        "spawn_until" => {
            // task.spawn_until(dur, fn) — spawn a task that runs with a
            // scoped wall-clock deadline. Equivalent to
            // `task.spawn { -> task.deadline(dur, fn) }` minus the
            // closure-wrapping boilerplate.
            if args.len() != 2 {
                return Err(VmError::new(
                    "task.spawn_until takes 2 arguments (duration, fn)".into(),
                ));
            }
            let dur_ns = crate::builtins::time::extract_duration(&args[0])?;
            if dur_ns < 0 {
                return Err(VmError::new(
                    "task.spawn_until: duration must be non-negative".into(),
                ));
            }
            let Value::VmClosure(closure) = &args[1] else {
                return Err(VmError::new(
                    "task.spawn_until requires a function argument".into(),
                ));
            };
            let deadline = vm
                .runtime
                .io
                .deadline_after(Duration::from_nanos(dur_ns as u64));
            spawn_with_deadline(vm, closure, deadline)
        }
        _ => Err(VmError::new(format!("unknown task function: {name}"))),
    }
}

// ── Select helpers ────────────────────────────────────────────────

// The internal `channel.recv_timeout` resume marker.
//
// When a scheduled task parks inside `recv_timeout`, the arguments it is
// called again with (`Vm::park_with_reason`) have a record of this type
// in place of the user's `Duration` (args[1]),
// wrapping the call's private timer channel, so the re-entry after a wake
// races the ORIGINAL absolute deadline instead of arming a fresh
// full-length timer (the round-101 livelock: every timer expiry re-armed
// the timeout forever). The marker only ever exists in the frame of the
// parked call, between a park and the next call — it is never user-visible,
// and its type (`ty::RECV_TIMEOUT`) is no type a program can name, so
// it cannot be mistaken for a typechecked `Duration` argument.

/// Field of the resume marker record holding the private timer channel.
const RECV_TIMEOUT_RESUME_TIMER_FIELD: &str = "timer";

/// Build the internal resume marker for `channel.recv_timeout` parks. See
/// [`ty::RECV_TIMEOUT`].
fn make_recv_timeout_resume_marker(timer_ch: &Arc<Channel>) -> Value {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        RECV_TIMEOUT_RESUME_TIMER_FIELD.to_string(),
        Value::Channel(timer_ch.clone()),
    );
    Value::builtin_record(ty::RECV_TIMEOUT, fields)
}

/// Translate a `try_select_sweep` result (a `(Channel, Variant)` tuple) into
/// the `Result(a, ChannelError)` shape expected by `channel.recv_timeout`:
///
///   * (timer_ch, _) → `Err(ChannelTimeout)` — the timer channel fired,
///     regardless of whether as `Message` (never sent to) or `Closed`.
///   * (user_ch, Message(v)) → `Ok(v)`.
///   * (user_ch, Closed) → `Err(ChannelClosed)`.
///
/// `tuple` is expected to be `Value::Tuple(vec![Channel, Variant])` per the
/// shape returned by `try_select_sweep`; anything else is a programming bug.
fn map_recv_timeout_result(tuple: Value, timer_ch: &Arc<Channel>) -> Value {
    let timeout_err = || Value::variant(bv::ERR, vec![Value::variant(bv::CHANNEL_TIMEOUT, vec![])]);
    let closed_err = || Value::variant(bv::ERR, vec![Value::variant(bv::CHANNEL_CLOSED, vec![])]);
    let Value::Tuple(parts) = tuple else {
        debug_assert!(false, "recv_timeout: select result not a tuple");
        return closed_err();
    };
    let Some(Value::Channel(src)) = parts.first() else {
        debug_assert!(false, "recv_timeout: select result missing channel");
        return closed_err();
    };
    if Arc::ptr_eq(src, timer_ch) {
        return timeout_err();
    }
    match parts.get(1) {
        Some(Value::Variant(name, fields)) if name.is(bv::MESSAGE) => {
            let val = fields.first().cloned().unwrap_or(Value::Unit);
            Value::variant(bv::OK, vec![val])
        }
        Some(Value::Variant(name, _)) if name.is(bv::CLOSED) => closed_err(),
        _ => {
            debug_assert!(false, "recv_timeout: unexpected select variant");
            closed_err()
        }
    }
}

/// A parsed select operation: receive from a channel or send to a channel.
enum SelectOp {
    Receive(Arc<Channel>),
    Send(Arc<Channel>, Value),
}

/// Parse the select operations list. Every element must be a
/// `ChannelOp` variant:
/// - `Recv(channel)` → receive
/// - `Send(channel, value)` → send
fn parse_select_ops(ops_list: &[Value]) -> Result<Vec<SelectOp>, VmError> {
    let mut ops = Vec::with_capacity(ops_list.len());
    for item in ops_list {
        match item {
            Value::Variant(name, fields) if name.is(bv::RECV) && fields.len() == 1 => {
                let Value::Channel(ch) = &fields[0] else {
                    return Err(VmError::new(
                        "channel.select Recv operation must wrap a Channel".into(),
                    ));
                };
                ops.push(SelectOp::Receive(ch.clone()));
            }
            Value::Variant(name, fields) if name.is(bv::SEND) && fields.len() == 2 => {
                let Value::Channel(ch) = &fields[0] else {
                    return Err(VmError::new(
                        "channel.select Send operation must take (Channel, value)".into(),
                    ));
                };
                ops.push(SelectOp::Send(ch.clone(), fields[1].clone()));
            }
            _ => {
                return Err(VmError::new(
                    "channel.select list items must be `channel.Recv(ch)` or \
                     `channel.Send(ch, value)` ChannelOp values"
                        .into(),
                ));
            }
        }
    }
    Ok(ops)
}

/// Pick a pseudo-random starting index in [0, n) to give `try_select_sweep`
/// fair semantics: when multiple select ops are simultaneously ready, each
/// one has a chance of being chosen instead of always the earliest.
///
/// Uses the same thread-local xorshift64 pattern as `math.random` in
/// `src/builtins/numeric.rs` — no extra dependency.
fn select_start_index(n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    use std::cell::Cell;
    use std::hash::{BuildHasher, Hasher};
    thread_local! {
        // Seeded from the standard library's hasher keys, not from a
        // clock: the sweep has no VM at hand to read the host clock
        // from, and fairness needs no better seed.
        static SELECT_RNG: Cell<u64> = Cell::new({
            std::collections::hash_map::RandomState::new().build_hasher().finish()
                | 1 // xorshift64 must not be seeded with 0
        });
    }
    SELECT_RNG.with(|state| {
        let mut s = state.get();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        state.set(s);
        (s as usize) % n
    })
}

/// Try all select operations non-blocking. Returns the first that succeeds,
/// iterating circularly from a pseudo-random start index so that readiness
/// races between channels are resolved fairly rather than always in list order.
/// A closed channel counts as a successful receive (returns Closed).
///
/// When an arm succeeds, the wake-ups that the select may have received
/// for its other arms are passed on: see `pass_on_wake_ups`.
///
/// For a caller that holds no waker registration on the channels of
/// `ops`. A caller that does uses `try_select_sweep_registered`.
fn try_select_sweep(ops: &[SelectOp]) -> Result<Option<Value>, VmError> {
    try_select_sweep_registered(ops, &mut Vec::new())
}

/// `try_select_sweep` for a caller that waits on the channels of `ops`
/// with the wakers in `registrations` (the main-thread waits). When an
/// arm succeeds, the registrations are dropped BEFORE the wake-ups are
/// passed on. Otherwise a channel would hand the passed-on wake-up to
/// the caller's own waker, which is about to go away, instead of to
/// the next waiter.
fn try_select_sweep_registered(
    ops: &[SelectOp],
    registrations: &mut Vec<crate::runtime::channel::WakerRegistration>,
) -> Result<Option<Value>, VmError> {
    let n = ops.len();
    if n == 0 {
        return Ok(None);
    }
    let start = select_start_index(n);
    for i in 0..n {
        let index = (start + i) % n;
        let (ch, outcome) = match &ops[index] {
            SelectOp::Receive(ch) => match ch.try_receive() {
                TryReceiveResult::Value(val) => (ch, Some(Value::variant(bv::MESSAGE, vec![val]))),
                TryReceiveResult::Closed => (ch, Some(Value::variant(bv::CLOSED, vec![]))),
                TryReceiveResult::Empty => (ch, None),
            },
            SelectOp::Send(ch, val) => match ch.try_send(val.clone()) {
                TrySendResult::Sent => (ch, Some(Value::variant(bv::SENT, vec![]))),
                TrySendResult::Closed => (ch, Some(Value::variant(bv::CLOSED, vec![]))),
                TrySendResult::Full => (ch, None),
            },
        };
        if let Some(outcome) = outcome {
            registrations.clear();
            pass_on_wake_ups(ops, index);
            return Ok(Some(Value::Tuple(vec![
                Value::Channel(ch.clone()),
                outcome,
            ])));
        }
    }

    Ok(None)
}

/// A select that completes arm `taken` performs none of its other
/// arms. If the select was woken because one of those became possible,
/// it has used up a wake-up that a waiter on that channel needs: the
/// channel wakes one waiter for each value and each free place. So
/// every other arm's channel is asked to wake its next waiter, if its
/// state allows an operation. A channel on which nothing is possible,
/// or nobody waits, does nothing.
///
/// This covers every caller of `try_select_sweep`: `channel.select`
/// and `channel.recv_timeout`, in a task and on the main thread.
fn pass_on_wake_ups(ops: &[SelectOp], taken: usize) {
    for (index, op) in ops.iter().enumerate() {
        if index == taken {
            continue;
        }
        match op {
            SelectOp::Receive(ch) | SelectOp::Send(ch, _) => ch.rewake_waiters(),
        }
    }
}

// ── Stream-fed channels ──────────────────────────────────────────
//
// The output channel of a `stream.*` stage is fed by a plain OS thread
// (`src/builtins/stream.rs`), not by a task. The scheduler does not
// count that thread, so a wait on an empty stream channel would read
// as a deadlock while the stage was about to deliver.
//
// `stream.rs` therefore records the output channel of every stage
// here, and a receive on a recorded channel never gets a deadlock
// verdict: it waits for a value or for `Closed`. A stage closes its
// output when it ends, so the wait ends when the stage does.
//
// The mark is on the channel, not on the thread. A wait on any other
// channel gets the same verdict as before, whether or not stream
// threads are alive. That matters because stage threads commonly
// outlive their pipeline: a truncating stage such as `stream.take`
// leaves its upstream stages blocked on a full buffer for the rest of
// the program, and they must not switch the detection off.
//
// Channels are keyed by address, not by `Channel::id`. Ids restart at
// 0 for every top-level VM, and one process can hold several of those
// (`silt test` creates one per file). Each entry holds a `Weak` to its
// channel. That keeps the allocation, not the channel, alive, so the
// address cannot be reused by another channel while the entry exists.
//
// Known limit: a receive on the output of a stage that never delivers
// and never closes (its own input stays open and silent forever, or
// its thread died without closing) waits forever instead of being
// reported.

fn stream_fed_channels() -> &'static Mutex<HashMap<usize, Weak<Channel>>> {
    static CHANNELS: OnceLock<Mutex<HashMap<usize, Weak<Channel>>>> = OnceLock::new();
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record `ch` as the output channel of a stream stage. `stream.rs`
/// calls this when it creates the stage, before the builtin returns,
/// so the mark is in place before anyone can wait on the channel.
pub(crate) fn mark_stream_fed(ch: &Arc<Channel>) {
    let mut channels = stream_fed_channels().lock();
    // Forget the channels that no longer exist, so the registry stays
    // as small as the set of stage outputs that are still in use.
    channels.retain(|_, fed| fed.strong_count() > 0);
    channels.insert(Arc::as_ptr(ch) as usize, Arc::downgrade(ch));
}

/// True iff `ch` is the output channel of a stream stage.
pub(crate) fn is_stream_fed(ch: &Arc<Channel>) -> bool {
    let key = Arc::as_ptr(ch) as usize;
    stream_fed_channels().lock().contains_key(&key)
}

#[cfg(test)]
mod select_fairness_tests {
    use super::*;
    use crate::runtime::channel::Channel;
    use std::sync::Arc;

    #[test]
    fn try_select_sweep_is_fair_between_ready_channels() {
        let ch1 = Arc::new(Channel::new(1, 4));
        let ch2 = Arc::new(Channel::new(2, 4));
        // Both channels always have data available.
        for _ in 0..4 {
            let _ = ch1.try_send(Value::Int(1));
            let _ = ch2.try_send(Value::Int(2));
        }
        // Refill on every iteration so both stay ready.
        let mut ch1_wins = 0u32;
        let mut ch2_wins = 0u32;
        let iters = 4000u32;
        for _ in 0..iters {
            while !matches!(ch1.try_send(Value::Int(1)), TrySendResult::Full) {}
            while !matches!(ch2.try_send(Value::Int(2)), TrySendResult::Full) {}
            let ops = vec![
                SelectOp::Receive(ch1.clone()),
                SelectOp::Receive(ch2.clone()),
            ];
            let result = try_select_sweep(&ops).unwrap().unwrap();
            if let Value::Tuple(parts) = result
                && let Value::Channel(c) = &parts[0]
            {
                if Arc::ptr_eq(c, &ch1) {
                    ch1_wins += 1;
                } else if Arc::ptr_eq(c, &ch2) {
                    ch2_wins += 1;
                }
            }
        }
        let min_share = iters / 5; // require each ≥ 20%
        assert!(
            ch1_wins >= min_share,
            "ch1 under-selected: {ch1_wins}/{iters}"
        );
        assert!(
            ch2_wins >= min_share,
            "ch2 under-selected: {ch2_wins}/{iters}"
        );
    }
}

#[cfg(test)]
mod wake_up_tests {
    use super::*;
    use crate::runtime::channel::{Channel, Waker};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A waker that counts how often it is called.
    fn counting_waker(calls: &Arc<AtomicUsize>) -> Waker {
        let calls = calls.clone();
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
        })
    }

    /// Every value taken out of a buffered channel wakes one parked
    /// sender, also when the buffer was not full before the receive.
    #[test]
    fn every_receive_wakes_one_parked_sender() {
        let ch = Arc::new(Channel::new(1, 3));
        for i in 0..3 {
            assert!(matches!(ch.try_send(Value::Int(i)), TrySendResult::Sent));
        }
        let woken = Arc::new(AtomicUsize::new(0));
        let _first = ch.register_send_waker_guard(counting_waker(&woken));
        let _second = ch.register_send_waker_guard(counting_waker(&woken));
        let _third = ch.register_send_waker_guard(counting_waker(&woken));
        assert_eq!(
            woken.load(Ordering::SeqCst),
            0,
            "the buffer is full: registering must not wake a sender"
        );
        for expected in 1..=3 {
            assert!(matches!(ch.try_receive(), TryReceiveResult::Value(_)));
            assert_eq!(
                woken.load(Ordering::SeqCst),
                expected,
                "receive number {expected} must wake one more sender"
            );
        }
    }

    /// The same for the blocking receive that the stream stages use.
    #[test]
    fn every_blocking_receive_wakes_one_parked_sender() {
        let ch = Arc::new(Channel::new(1, 3));
        for i in 0..3 {
            assert!(matches!(ch.try_send(Value::Int(i)), TrySendResult::Sent));
        }
        let woken = Arc::new(AtomicUsize::new(0));
        let _first = ch.register_send_waker_guard(counting_waker(&woken));
        let _second = ch.register_send_waker_guard(counting_waker(&woken));
        for expected in 1..=2 {
            assert!(matches!(ch.receive_blocking(), TryReceiveResult::Value(_)));
            assert_eq!(woken.load(Ordering::SeqCst), expected);
        }
    }

    /// A select that completes one arm passes the wake-up of the other
    /// arm on: the receiver that waits on that channel is woken, because
    /// the channel holds a value for it.
    #[test]
    fn select_passes_the_wake_up_of_the_arm_it_did_not_take_on() {
        let a = Arc::new(Channel::new(1, 4));
        let b = Arc::new(Channel::new(2, 4));
        let select_woken = Arc::new(AtomicUsize::new(0));
        let receiver_woken = Arc::new(AtomicUsize::new(0));
        // The select waits on `a` in front of the plain receiver.
        let select_on_a = a.register_recv_waker_guard(counting_waker(&select_woken));
        let _receiver_on_a = a.register_recv_waker_guard(counting_waker(&receiver_woken));
        assert!(matches!(a.try_send(Value::Int(1)), TrySendResult::Sent));
        assert!(matches!(b.try_send(Value::Int(2)), TrySendResult::Sent));
        assert_eq!(select_woken.load(Ordering::SeqCst), 1);
        assert_eq!(
            receiver_woken.load(Ordering::SeqCst),
            0,
            "one value in `a` wakes one waiter: the select"
        );
        drop(select_on_a);
        // The select takes arm 1 (`b`) and leaves the value in `a`.
        let ops = vec![SelectOp::Receive(a.clone()), SelectOp::Receive(b.clone())];
        assert!(matches!(b.try_receive(), TryReceiveResult::Value(_)));
        pass_on_wake_ups(&ops, 1);
        assert_eq!(
            receiver_woken.load(Ordering::SeqCst),
            1,
            "the value in `a` is for the receiver that still waits on it"
        );
    }

    /// Passing a wake-up on does nothing on a channel whose state allows
    /// no operation.
    #[test]
    fn passing_on_wakes_nobody_when_nothing_is_possible() {
        let empty = Arc::new(Channel::new(1, 2));
        let receiver_woken = Arc::new(AtomicUsize::new(0));
        let _receiver = empty.register_recv_waker_guard(counting_waker(&receiver_woken));
        empty.rewake_waiters();
        assert_eq!(receiver_woken.load(Ordering::SeqCst), 0);

        let full = Arc::new(Channel::new(2, 1));
        assert!(matches!(full.try_send(Value::Int(1)), TrySendResult::Sent));
        let sender_woken = Arc::new(AtomicUsize::new(0));
        let _sender = full.register_send_waker_guard(counting_waker(&sender_woken));
        full.rewake_waiters();
        assert_eq!(sender_woken.load(Ordering::SeqCst), 0);
    }

    /// A channel that was closed refuses a send, and a receiver gets the
    /// values that were sent before the close, then `Closed`.
    #[test]
    fn close_orders_sends_and_receives() {
        let ch = Arc::new(Channel::new(1, 2));
        assert!(matches!(ch.try_send(Value::Int(1)), TrySendResult::Sent));
        ch.close();
        assert!(matches!(ch.try_send(Value::Int(2)), TrySendResult::Closed));
        assert!(matches!(ch.try_receive(), TryReceiveResult::Value(_)));
        assert!(matches!(ch.try_receive(), TryReceiveResult::Closed));

        let rendezvous = Arc::new(Channel::new(2, 0));
        rendezvous.close();
        assert!(matches!(
            rendezvous.try_send(Value::Int(1)),
            TrySendResult::Closed
        ));
        assert!(matches!(rendezvous.try_receive(), TryReceiveResult::Closed));
    }
}
