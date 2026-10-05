//! Concurrency builtin functions (`channel.*`, `task.*`).

use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crate::runtime::channel::{Channel, TryReceiveResult, TrySendResult};
use crate::runtime::handle::TaskHandle;
use crate::typeinfo::{bv, ty};
use crate::value::Value;
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

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
pub fn call_channel(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
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
            let val = args[1].clone();
            let ch = ch.clone();
            // Try non-blocking first.
            match ch.try_send(val.clone()) {
                TrySendResult::Sent => return Ok(Value::Unit),
                TrySendResult::Closed => {
                    return Err(closed_channel_send_err(ch.id));
                }
                TrySendResult::Full => {}
            }
            // Buffer is full -- park via scheduler or wait with a watchdog.
            if vm.is_scheduled_task {
                return Err(vm.park_with_reason(args, BlockReason::Send(ch)));
            }
            // Main thread: wait on a condvar backed by the channel's
            // send waker. A watchdog periodically checks whether any
            // scheduled task could still consume from this channel; if
            // not, we report a deadlock error rather than hanging forever.
            main_thread_wait_for_send(&ch, val, vm)
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
            // Channel is empty -- park via scheduler or wait with a watchdog.
            if vm.is_scheduled_task {
                return Err(vm.park_with_reason(args, BlockReason::Receive(ch)));
            }
            // Main thread: wait with a watchdog. The channel's receive
            // waker pokes a local condvar when a value arrives or the
            // channel closes, and the watchdog periodically checks
            // whether any scheduled task could still send to us.
            main_thread_wait_for_receive(&ch, vm)
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

            // No operation succeeded — park via scheduler or run the
            // main-thread event-driven wait (with deadlock detection).
            if vm.is_scheduled_task {
                return Err(vm.park_with_reason(args, BlockReason::Select(select_ops)));
            }

            // Main thread: same wake-graph-driven protocol as
            // `channel.receive` / `channel.send`. Returns a
            // "deadlock on main thread" error if the wake graph proves
            // no counterparty can ever make any arm ready (previously
            // this path spun on a 1s condvar poll forever).
            main_thread_wait_for_select(&ops, vm)
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

            // Resume detection. A scheduled task that parks below re-pushes
            // its args with args[1] swapped for an internal marker record
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
                fresh_dur_ns = crate::builtins::data::extract_duration(&args[1])?;
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
            if vm.is_scheduled_task {
                let select_ops: Vec<(Arc<Channel>, SelectOpKind)> = ops
                    .iter()
                    .map(|op| match op {
                        SelectOp::Receive(c) => (c.clone(), SelectOpKind::Receive),
                        SelectOp::Send(c, _) => (c.clone(), SelectOpKind::Send),
                    })
                    .collect();
                // We DO re-enter this arm on resume because CallBuiltin
                // replays its args — but the replayed args carry the resume
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
                return Err(vm.park_with_reason(&resume_args, BlockReason::Select(select_ops)));
            }
            // Main-thread path: drive the same select condvar loop that the
            // `channel.select` builtin uses. Mirrors the structure there;
            // we only differ in how we map the final Value back to a Result
            // variant.
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            let mut registrations: Vec<crate::runtime::channel::WakerRegistration> =
                Vec::with_capacity(ops.len());
            for op in &ops {
                let pair2 = pair.clone();
                let waker = Box::new(move || {
                    let (lock, cvar) = &*pair2;
                    *lock.lock() = true;
                    cvar.notify_one();
                });
                match op {
                    SelectOp::Receive(c) if !c.is_closed() => {
                        registrations.push(c.register_recv_waker_guard(waker));
                    }
                    SelectOp::Receive(_) | SelectOp::Send(_, _) => {}
                }
            }
            loop {
                // A successful sweep drops the registrations.
                if let Some(val) = try_select_sweep_registered(&ops, &mut registrations)? {
                    return Ok(map_recv_timeout_result(val, &timer_ch));
                }
                let (lock, cvar) = &*pair;
                let mut notified = lock.lock();
                if !*notified {
                    cvar.wait_for(&mut notified, std::time::Duration::from_secs(1));
                }
                *notified = false;
            }
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
            let ch = ch.clone();
            let callback = args[1].clone();
            // If we have a suspended callback from a previous yield (e.g. IO
            // inside the callback), resume it before processing new messages.
            if vm.suspended_invoke.is_some() {
                match vm.resume_suspended_invoke() {
                    Ok(_) => {
                        // Callback completed; fall through to continue the loop.
                        // Yield for round-robin if scheduled.
                        if vm.is_scheduled_task {
                            for arg in args {
                                vm.push(arg.clone());
                            }
                            return Err(VmError::yield_signal());
                        }
                    }
                    Err(e) if e.is_yield => {
                        // Still yielding — re-push our args and propagate.
                        for arg in args {
                            vm.push(arg.clone());
                        }
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
            loop {
                match ch.try_receive() {
                    TryReceiveResult::Value(val) => {
                        match vm.invoke_callable(&callback, &[val]) {
                            Ok(_) => {}
                            Err(e) if e.is_yield => {
                                // The callback yielded (e.g. IO inside the callback).
                                // Re-push channel.each args so CallBuiltin re-executes us.
                                for arg in args {
                                    vm.push(arg.clone());
                                }
                                return Err(e);
                            }
                            Err(e) => return Err(e),
                        }
                        // After each message, yield to scheduler for round-robin.
                        if vm.is_scheduled_task {
                            // Re-push args so the CallBuiltin re-executes channel.each.
                            for arg in args {
                                vm.push(arg.clone());
                            }
                            return Err(VmError::yield_signal());
                        }
                    }
                    TryReceiveResult::Closed => {
                        return Ok(Value::Unit);
                    }
                    TryReceiveResult::Empty => {
                        // Channel empty -- park via scheduler or block.
                        if vm.is_scheduled_task {
                            return Err(vm.park_with_reason(args, BlockReason::Receive(ch)));
                        }
                        // Main thread: wait through the same deadlock-aware
                        // protocol as `channel.receive` (no-scheduler fast
                        // path, wake-graph park/unpark, starvation BFS,
                        // confirm-stable gate). A bare `ch.receive_blocking()`
                        // here was an infinite condvar wait with no deadlock
                        // detection: with no counterparty that could ever
                        // send, the process hung forever where receive/send/
                        // select/join all report "deadlock on main thread"
                        // (same class as the round-2 `channel.select` fix —
                        // `each` was the arm left behind). Locked by
                        // `tests/concurrency/main_thread_each_deadlock_tests.rs`.
                        match main_thread_wait_for_receive(&ch, vm)? {
                            Value::Variant(tag, mut vals) if tag.is(bv::MESSAGE) => {
                                let val = vals.pop().unwrap_or(Value::Unit);
                                match vm.invoke_callable(&callback, &[val]) {
                                    Ok(_) => {}
                                    Err(e) if e.is_yield => {
                                        for arg in args {
                                            vm.push(arg.clone());
                                        }
                                        return Err(e);
                                    }
                                    Err(e) => return Err(e),
                                }
                            }
                            Value::Variant(tag, _) if tag.is(bv::CLOSED) => {
                                return Ok(Value::Unit);
                            }
                            _ => unreachable!(
                                "main_thread_wait_for_receive returns Message or Closed"
                            ),
                        }
                    }
                }
            }
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

    #[cfg(target_arch = "wasm32")]
    {
        use crate::vm::CallFrame;
        let child_handle = handle.clone();
        child_vm.stack = vec![Value::Unit];
        child_vm.frames = vec![CallFrame {
            closure: child_closure,
            ip: 0,
            base_slot: 1,
        }];
        match child_vm.execute() {
            Ok(val) => child_handle.complete(Ok(val)),
            Err(e) => child_handle.complete(Err(child_vm.enrich_error(e))),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::scheduler::Task;
        use crate::vm::CallFrame;
        child_vm.stack = vec![Value::Unit];
        child_vm.frames = vec![CallFrame {
            closure: child_closure,
            ip: 0,
            base_slot: 1,
        }];
        child_vm.is_scheduled_task = true;

        let scheduler = vm.get_or_create_scheduler();
        scheduler
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
pub fn call_task(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
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

            // If we're a scheduled task, park via the scheduler.
            if vm.is_scheduled_task {
                return Err(vm.park_with_reason(args, BlockReason::Join(handle)));
            }

            // Main thread: block with condvar, but wake periodically to
            // consult the scheduler's deadlock heuristic. If the joined
            // task can never finish (every scheduled task is parked on
            // an internal graph edge with no runnable counterparty), we
            // surface a `deadlock on main thread` diagnostic instead of
            // hanging forever. This is the join analogue of
            // `main_thread_wait_for_receive`.
            match main_thread_wait_for_join(&handle, vm) {
                Ok(val) => Ok(val),
                Err(mut inner) => {
                    // Distinguish "the joinee task failed" from "the main
                    // thread is starved/deadlocked while waiting to join".
                    // Only the former should be prefixed with
                    // "joined task failed:"; a main-thread deadlock is a
                    // condition of the *joiner*, not the joinee, and must
                    // surface on its own. VmError has no structured kind
                    // field (see src/vm/error.rs), so we discriminate on the
                    // stable "deadlock on main thread" marker that every
                    // such diagnostic in `main_thread_wait_for_join` carries.
                    if !inner.message.starts_with("deadlock on main thread") {
                        inner.message = format!("joined task failed: {}", inner.message);
                    }
                    Err(inner)
                }
            }
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
            let dur_ns = crate::builtins::data::extract_duration(&args[0])?;
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
        "deadline" => {
            // task.deadline(dur, fn) — runs `fn` with a scoped wall-clock
            // deadline of `dur` from now. I/O inside the callback is
            // watched by the scheduler's I/O watchdog; if the deadline
            // elapses while parked on I/O, the in-flight I/O is
            // cancelled with `Err("I/O timeout (task.deadline exceeded)")`.
            // I/O builtins also check at entry and return the same Err
            // immediately if the deadline is already past.
            //
            // Pure-CPU work inside the callback is NOT interrupted — this
            // matches Go's context.WithDeadline semantics. If you need to
            // bound CPU work, have the callback periodically yield via
            // I/O.
            //
            // Synchronously-nested task.deadline tightens the deadline
            // (earliest wins); a looser inner deadline cannot extend an
            // outer one. Resumption across yields is supported for the
            // common single-scope case.
            if args.len() != 2 {
                return Err(VmError::new(
                    "task.deadline takes 2 arguments (duration, fn)".into(),
                ));
            }
            // First entry sets up the scope; a resume (when this same
            // CallBuiltin is re-executed after the callback yielded)
            // must not push again. `suspended_invoke.is_some()` is the
            // signal that we're resuming a paused invoke_callable.
            let is_resume = vm.suspended_invoke.is_some();
            if !is_resume {
                let dur_ns = crate::builtins::data::extract_duration(&args[0])?;
                if dur_ns < 0 {
                    return Err(VmError::new(
                        "task.deadline: duration must be non-negative".into(),
                    ));
                }
                let new_deadline = vm
                    .runtime
                    .io
                    .deadline_after(Duration::from_nanos(dur_ns as u64));
                let prev = vm.current_deadline;
                vm.deadline_stack.push(prev);
                // Tighten: earliest of current and new wins.
                let effective = match (prev, new_deadline) {
                    (Some(a), Some(b)) if a <= b => Some(a),
                    (Some(_), Some(b)) => Some(b),
                    (None, x) | (x, None) => x,
                };
                vm.current_deadline = effective;
            }
            let result = vm.invoke_callable_resumable(&args[1], &[], args);
            match &result {
                Err(e) if e.is_yield => {
                    // Leave the deadline installed across the park so
                    // the scheduler's I/O watchdog registration and the
                    // I/O builtin's entry check both observe it.
                }
                _ => {
                    // Scope ending — pop the deadline we pushed. An empty
                    // stack here means push/pop got unbalanced (a bug in
                    // the scope-entry logic or is_resume detection), so
                    // surface it loudly rather than silently clearing.
                    vm.current_deadline = vm
                        .deadline_stack
                        .pop()
                        .expect("deadline_stack underflow — push/pop unbalanced in task.deadline");
                }
            }
            result
        }
        _ => Err(VmError::new(format!("unknown task function: {name}"))),
    }
}

// ── Select helpers ────────────────────────────────────────────────

// The internal `channel.recv_timeout` resume marker.
//
// When a scheduled task parks inside `recv_timeout`, the re-pushed args
// replace the user's `Duration` (args[1]) with a record of this type
// wrapping the call's private timer channel, so the re-entry after a wake
// races the ORIGINAL absolute deadline instead of arming a fresh
// full-length timer (the round-101 livelock: every timer expiry re-armed
// the timeout forever). The marker only ever exists on the VM stack
// between a park and its CallBuiltin replay — it is never user-visible,
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

// ── Main-thread channel wait with event-driven watchdog ──────────
//
// When `fn main()` runs on the main thread (`is_scheduled_task = false`)
// and calls `channel.send` on a full channel or `channel.receive` on an
// empty one, we cannot park via the scheduler — the main thread is
// invisible to it. Previously `send` spun with `yield_now()` (100% CPU
// forever) and `receive` blocked indefinitely via `receive_blocking`,
// so a program with no other producers/consumers would hang.
//
// Phase 4: these helpers block on the channel's existing waker
// machinery via a local condvar with INDEFINITE `condvar.wait` — the
// 100ms-tick polling layer + consecutive-streak escalator that lived
// here through Phase 3 are deleted. The wake graph
// (`src/scheduler/wake_graph.rs`) signals our local condvar via
// `install_main_waiter` on every park / wake / spawn / complete, so
// the only state changes that wake us are the ones that could
// plausibly unblock us. On a real deadlock the wake graph proves
// starvation atomically with the scheduler-state mutation that caused
// it (typically the last task's `on_complete`), the signal callback
// fires once, we wake and `is_main_starved` returns `true` — total
// fire latency is one signal hop, target <200ms even on heavily
// loaded CI.

// ── Stream-fed channels ──────────────────────────────────────────
//
// The output channel of a `stream.*` stage is fed by a plain OS thread
// (`src/builtins/stream.rs`), not by a scheduler task. The wake graph
// cannot see that thread, and a program that never calls `task.spawn`
// has no scheduler at all, so the main-thread waits below used to read
// an empty stream channel as "no counterparty" and reported a deadlock
// while the stage was about to deliver.
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
fn is_stream_fed(ch: &Arc<Channel>) -> bool {
    let key = Arc::as_ptr(ch) as usize;
    stream_fed_channels().lock().contains_key(&key)
}

/// True iff `target` includes a receive on a stream-fed channel: a
/// plain receive on one, or a select with at least one such receive
/// arm. Such a wait never gets a deadlock verdict. A send and a join
/// are never covered.
fn waits_on_stream_fed_channel(target: &crate::scheduler::MainTarget) -> bool {
    match target {
        crate::scheduler::MainTarget::Recv(ch) => is_stream_fed(ch),
        crate::scheduler::MainTarget::Select(edges) => edges.iter().any(|edge| match edge {
            crate::scheduler::SelectEdge::Recv(ch) => is_stream_fed(ch),
            crate::scheduler::SelectEdge::Send(_) => false,
        }),
        crate::scheduler::MainTarget::Send(_) | crate::scheduler::MainTarget::Join(_) => false,
    }
}

/// Phase 3: register the main thread with the wake graph and install
/// a callback that pokes `pair`'s condvar on every graph state-change.
/// Returns the install guard (drop deregisters the callback) so a
/// stale callback never fires into a freed local condvar. Callers
/// MUST keep the guard alive across the wait loop and call
/// `unpark_main` (via `Scheduler::unpark_main`) on exit.
fn install_main_signal(
    vm: &Vm,
    pair: &Arc<(Mutex<bool>, Condvar)>,
) -> Option<crate::scheduler::MainWaiterGuard> {
    let sched = vm.current_scheduler()?;
    sched.register_main_present();
    let pair_for_cb = pair.clone();
    let cb: crate::scheduler::MainWaiterCallback = Arc::new(move || {
        // Cheap poke: flip the flag and wake one waiter. The waiter
        // re-checks its full state on wakeup, so multiple back-to-
        // back signals just collapse into one re-check.
        let (lock, cvar) = &*pair_for_cb;
        *lock.lock() = true;
        cvar.notify_one();
    });
    Some(sched.install_main_waiter(cb))
}

/// Returns `true` iff the wake graph proves the main thread cannot
/// be driven forward on `target`. When there is no scheduler at all,
/// the only remaining wake source is the channel's own waker
/// machinery — fired by either an external `ch.close()` (e.g. the
/// `TimerManager` thread for `channel.timeout`) or, in principle,
/// some other thread holding an `Arc<Channel>`. We treat a pending
/// timer close as not-starved (the timer thread will fire); any
/// other no-scheduler park is a deadlock.
///
/// When a scheduler IS attached, defer to `Scheduler::is_main_starved`
/// — the wake graph is the SOLE deadlock signal.
///
/// Before either of those: a receive on a stream-fed channel is never
/// starved. Its counterparty is a stream thread, which neither the
/// wake graph nor the no-scheduler reasoning can see. See the
/// "Stream-fed channels" section above.
fn main_thread_is_starved(vm: &Vm, target: &crate::scheduler::MainTarget) -> bool {
    if waits_on_stream_fed_channel(target) {
        return false;
    }
    if let Some(sched) = vm.current_scheduler() {
        return sched.is_main_starved(target);
    }
    // No scheduler. Only an external timer can wake us.
    match target {
        crate::scheduler::MainTarget::Recv(ch) | crate::scheduler::MainTarget::Send(ch) => {
            !ch.has_pending_timer_close()
        }
        // Join with no scheduler: the joinee never ran, so there's
        // no result coming.
        crate::scheduler::MainTarget::Join(_) => true,
        // Select with no scheduler: same reasoning as Recv/Send, per
        // arm. If ANY arm's channel has a pending timer close, the
        // timer thread's `ch.close()` will fire that arm's waker and
        // unblock the select — not starved. This is the
        // `channel.select([Recv(ch), Recv(channel.timeout(..))])` case
        // with no spawned tasks (so no scheduler is ever created): the
        // timeout arm must still wake. Only when NO arm has an external
        // timer waiting is the select a genuine deadlock.
        crate::scheduler::MainTarget::Select(edges) => !edges.iter().any(|e| match e {
            crate::scheduler::SelectEdge::Recv(ch) | crate::scheduler::SelectEdge::Send(ch) => {
                ch.has_pending_timer_close()
            }
        }),
    }
}

/// Block the main thread until the channel accepts `val`, the channel
/// is closed, or the scheduler can no longer make progress (deadlock).
/// Per-round wait for the deadlock-confirmation gate, in milliseconds.
///
/// `main_thread_is_starved` is evaluated against a single snapshot taken under
/// the wake-graph mutex. Under heavy CPU contention that snapshot can be
/// *transiently* starved during a task state transition — e.g. the window
/// between a worker dequeuing a task and that task re-registering its park edge,
/// or between a task's last side effect and its `on_complete` propagating.
/// Acting on such a transient snapshot is a false positive, so a suspected
/// starvation is only a *candidate* deadlock: each confirmation round waits up
/// to this long for any graph mutation (every spawn/park/wake/complete calls
/// `signal_progress`) before re-checking.
const CONFIRM_MS: u64 = 100;

/// Number of consecutive starved observations required to confirm a deadlock.
///
/// A single post-wait snapshot can ITSELF land on a transient (a *different*
/// task mid-transition), so one confirmation round is not enough under extreme
/// contention. We require `CONFIRM_ROUNDS` consecutive rounds in which no
/// progress signal arrived AND the graph re-reads as starved. A real deadlock
/// is a permanent fixpoint, so it accumulates all rounds back-to-back; any
/// in-flight transition fires `signal_progress` (waking a round early) or
/// clears starvation, which resets the requirement. Worst-case added latency
/// for a genuine deadlock is `CONFIRM_ROUNDS * CONFIRM_MS` (200ms) — well
/// within the detector tests' multi-second budget, and only paid once, on a
/// program that is actually dead.
///
/// Two rounds is the floor that still gives a SECOND independent observation
/// after the initial `is_main_starved` candidate: the bug was a single
/// transient snapshot, and two clean rounds (plus the candidate check that
/// got us here) means three consecutive starved reads with no intervening
/// `signal_progress`. Raise this only if a sustained false positive ever
/// resurfaces under heavier contention than the CI Windows concurrency
/// partition.
const CONFIRM_ROUNDS: u32 = 2;

/// Confirmation gate for a suspected main-thread deadlock.
///
/// `main_thread_is_starved` returning `true` is only a *candidate* deadlock,
/// because the snapshot it reads can be transiently starved during an in-flight
/// task transition. This gate confirms the candidate is stable before the
/// caller fires: it returns `true` iff `CONFIRM_ROUNDS` consecutive rounds each
/// see no progress signal AND a fresh `still_starved()` re-read. Any round that
/// observes a progress signal or non-starved graph returns `false` immediately,
/// and the caller must `continue` its wait loop instead of firing.
///
/// Soundness:
/// - REAL deadlock: once the last runnable task completes, the wake graph is a
///   stable fixpoint — by definition nothing can mutate it. Every round clears
///   the progress flag, waits `CONFIRM_MS`, observes no `signal_progress`, and
///   re-reads starved -> all `CONFIRM_ROUNDS` pass -> confirmed. Added latency
///   <= `CONFIRM_ROUNDS * CONFIRM_MS`, paid once.
/// - FALSE positive: caused by an in-flight task transition, which calls
///   `signal_progress` when it settles (on_park/on_wake/on_complete) -> the
///   flag is set and the condvar is notified -> a round wakes early or its
///   re-read observes fuel -> the gate returns `false` and the loop re-checks.
///   Requiring CONFIRM_ROUNDS consecutive clean rounds makes a sustained false
///   positive (every 100ms window happening to catch a transient with no
///   intervening signal) astronomically unlikely for a program that is in fact
///   making progress.
///
/// IMPORTANT: the flag is cleared *before* each wait so a `signal_progress`
/// racing the gate is not lost — it either wakes the round early or leaves the
/// flag set for that round's post-wait check.
fn confirm_main_starved(
    pair: &Arc<(Mutex<bool>, Condvar)>,
    still_starved: impl Fn() -> bool,
) -> bool {
    let (lock, cvar) = &**pair;
    for _ in 0..CONFIRM_ROUNDS {
        let mut guard = lock.lock();
        *guard = false;
        let _ = cvar.wait_for(&mut guard, Duration::from_millis(CONFIRM_MS));
        let progressed = *guard;
        drop(guard);
        if progressed || !still_starved() {
            return false;
        }
    }
    true
}

/// Report the tasks that failed and that nobody joined, before a
/// main-thread wait gives its deadlock verdict.
///
/// The verdict ends the program, and a task that failed is the usual
/// reason why the counterparty of the wait is missing: a producer that
/// stopped with an error before it sent. Without the report the user
/// sees the deadlock and not its cause. Each failure is reported once,
/// so the report at the end of the program does not repeat it. Where a
/// front end collects the failures (`scheduler::collect_unjoined_failures`),
/// this reports nothing: the front end takes them when the verdict
/// reaches it and shows them before it.
fn report_unjoined_failures(vm: &Vm) {
    if let Some(sched) = vm.current_scheduler() {
        let _ = sched.report_unjoined_failures();
    }
}

fn main_thread_wait_for_send(
    ch: &Arc<crate::runtime::channel::Channel>,
    val: Value,
    vm: &Vm,
) -> Result<Value, VmError> {
    // No-scheduler + no-timer fast path: there is no scheduler to
    // pump events through `signal_progress`, AND no pending timer
    // close that would fire `wake_all_send` on the channel. Any wait
    // here would be infinite — fire deadlock immediately.
    if vm.current_scheduler().is_none() && !ch.has_pending_timer_close() {
        match ch.try_send(val) {
            TrySendResult::Sent => return Ok(Value::Unit),
            TrySendResult::Closed => {
                return Err(closed_channel_send_err(ch.id));
            }
            TrySendResult::Full => {
                return Err(VmError::new(
                    "deadlock on main thread: channel send with no counterparty".into(),
                ));
            }
        }
    }
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    // Install the wake-graph signal callback + park MAIN. See
    // `main_thread_wait_for_receive` for rationale.
    let target = crate::scheduler::MainTarget::from_send(ch);
    let _signal_guard = install_main_signal(vm, &pair);
    if let Some(sched) = vm.current_scheduler() {
        sched.park_main(&target);
    }
    let unpark_main = |vm: &Vm| {
        if let Some(sched) = vm.current_scheduler() {
            sched.unpark_main();
        }
    };
    // ROUND93-RECHECK(send): the single in-loop race-point re-check.
    // Every race window in the wait loop below (top-of-loop,
    // post-waker-registration, post-starvation-BFS, post-confirm)
    // funnels through this one closure so the copies cannot drift apart
    // (they were four byte-identical blocks hardened across audit
    // rounds 90-92; a future edit to one copy that missed the siblings
    // would silently reopen lost-wakeup / deadlock-false-positive
    // bugs). Locked by
    // `tests/concurrency/round93_concurrency_recheck_extraction_tests.rs`.
    //
    // Semantics (load-bearing, must not change):
    //   Sent   -> drop the waker-registration guard FIRST (deregisters
    //             the stale waker), THEN unpark MAIN, then Ok(Unit).
    //   Closed -> same drop/unpark ordering, canonical closed-send err.
    //   Full   -> None: fall through to the caller's wait protocol.
    // The post-`confirm_main_starved` call site consults this BEFORE
    // testing `confirmed`, so a send slot that races open during the
    // confirm window always wins over a deadlock verdict.
    //
    // NOTE: the no-scheduler fast path at the top of this function
    // looks similar but is INTENTIONALLY different (no waker or park
    // exists yet; `Full` is an immediate deadlock there) — do not unify
    // it with this closure.
    let recheck = |reg: &mut Option<crate::runtime::channel::WakerRegistration>| match ch
        .try_send(val.clone())
    {
        TrySendResult::Sent => {
            drop(reg.take());
            unpark_main(vm);
            Some(Ok(Value::Unit))
        }
        TrySendResult::Closed => {
            drop(reg.take());
            unpark_main(vm);
            Some(Err(closed_channel_send_err(ch.id)))
        }
        TrySendResult::Full => None,
    };
    // Track the most recently registered send-waker as a
    // `WakerRegistration` guard. Dropping / replacing the guard
    // deregisters the prior iteration's waker. Without this, the
    // channel only drains wakers on successful receive/close, so the
    // guard swap on every loop iteration would leave a stale waker
    // closure in the queue (unbounded growth on a channel that nobody
    // is draining).
    let mut reg: Option<crate::runtime::channel::WakerRegistration> = None;
    loop {
        // Try first so we don't miss a send slot that just opened.
        if let Some(out) = recheck(&mut reg) {
            return out;
        }
        // Explicitly take-and-drop the previous iteration's guard
        // before minting a new one so the old waker is deregistered
        // first. (If we assigned via `reg = Some(..)`, the RHS would
        // be evaluated — registering the new waker — before the old
        // value was dropped, briefly doubling the registration.)
        drop(reg.take());
        let pair2 = pair.clone();
        reg = Some(ch.register_send_waker_guard(Box::new(move || {
            let (lock, cvar) = &*pair2;
            *lock.lock() = true;
            cvar.notify_one();
        })));
        // Re-check after registering to avoid a lost wakeup race
        // between try_send above and register_send_waker.
        if let Some(out) = recheck(&mut reg) {
            return out;
        }
        // Pre-wait starvation check: see `main_thread_wait_for_receive`.
        if main_thread_is_starved(vm, &target) {
            // Candidate deadlock. First catch a send slot that raced
            // open between the re-check above and this BFS.
            if let Some(out) = recheck(&mut reg) {
                return out;
            }
            // Confirm the starvation is STABLE before firing: a single
            // snapshot can be transiently starved during a task state
            // transition under load. Wait up to CONFIRM_MS for any
            // `signal_progress`; only fire if still starved afterward.
            // NOTE: the re-check below runs BEFORE `confirmed` is
            // tested — a slot that raced open during the confirm window
            // must win over the deadlock verdict.
            let confirmed = confirm_main_starved(&pair, || main_thread_is_starved(vm, &target));
            if let Some(out) = recheck(&mut reg) {
                return out;
            }
            if confirmed {
                drop(reg);
                unpark_main(vm);
                report_unjoined_failures(vm);
                return Err(VmError::new(
                    "deadlock on main thread: channel send with no counterparty".into(),
                ));
            }
            // Progress signalled or starvation cleared: re-evaluate.
            continue;
        }
        // Indefinite wait: woken by either our send-waker firing
        // (a real progress event on the channel) or the wake-graph
        // signal callback (a scheduler state change that could make
        // the channel reachable). The 100ms-tick polling layer that
        // lived here through Phase 3 is gone; the wake graph is the
        // sole deadlock signal.
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            while !*notified {
                cvar.wait(&mut notified);
            }
            *notified = false;
        }
    }
}

/// Block the main thread until the channel yields a value, is closed,
/// or the wake graph proves no scheduled task can drive the receive
/// forward (deadlock).
///
/// Phase 4: the wake graph (`src/scheduler/wake_graph.rs`) is the
/// SOLE deadlock signal. Main parks itself in the graph (so parked
/// counterparties' BFS sees MAIN as a wake source) and waits
/// indefinitely on a local condvar; the graph's `signal_progress`
/// callback flips the condvar on every park / wake / spawn /
/// complete. On every wake we re-check the channel (lost wakeup
/// guard) and consult `is_main_starved`: a `true` return is the
/// proof of starvation — fire deadlock immediately. A `false` return
/// means the graph cannot rule out a wake from some still-runnable
/// task; loop and wait again. No 100ms tick, no consecutive-streak
/// escalator — those were Phase 3 polling-fallback artifacts.
fn main_thread_wait_for_receive(
    ch: &Arc<crate::runtime::channel::Channel>,
    vm: &Vm,
) -> Result<Value, VmError> {
    // No-scheduler + no-timer fast path: there is no scheduler to
    // pump events through `signal_progress`, AND no pending timer
    // close that would fire `wake_all_recv` on the channel. Any wait
    // here would be infinite — fire deadlock immediately. (When a
    // timer IS pending, the recv-waker we register below is woken by
    // the timer thread's `ch.close()` → `wake_all_recv()` chain, so
    // the indefinite `cvar.wait` is finite.)
    //
    // A stream-fed channel never takes this path: the stream thread
    // that feeds it is a counterparty this check cannot see. The
    // recv-waker we register below is woken by that thread's send or
    // by its `ch.close()`, neither of which needs a scheduler.
    if vm.current_scheduler().is_none() && !ch.has_pending_timer_close() && !is_stream_fed(ch) {
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                return Ok(Value::variant(bv::MESSAGE, vec![val]));
            }
            TryReceiveResult::Closed => return Ok(Value::variant(bv::CLOSED, vec![])),
            TryReceiveResult::Empty => {
                return Err(VmError::new(
                    "deadlock on main thread: channel receive with no counterparty".into(),
                ));
            }
        }
    }
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    // Install the wake-graph signal callback so any state change in
    // the scheduler pokes `pair`'s condvar. Park MAIN in the graph so
    // other tasks' BFS from `target` finds us as the destination;
    // unpark on exit so the graph stops modeling MAIN when the
    // receive resolves.
    let target = crate::scheduler::MainTarget::from_recv(ch);
    let _signal_guard = install_main_signal(vm, &pair);
    if let Some(sched) = vm.current_scheduler() {
        sched.park_main(&target);
    }
    // Track the most recently registered recv-waker as a
    // `WakerRegistration` guard so the prior iteration's waker is
    // deregistered when the guard is dropped / replaced. Without
    // this, each iteration would re-register a waker whose `WakerId`
    // is dropped — `waiting_receivers` inflates unboundedly per
    // iteration, and a later rendezvous `try_send` from another task
    // sees a phantom receiver, places a value into the handoff slot,
    // and returns `Sent` with no real receiver. Values are lost. See
    // round-26 B6.
    let mut reg: Option<crate::runtime::channel::WakerRegistration> = None;
    // Helper to consistently unpark MAIN from the wake graph on exit.
    // Called before every early-return in the loop.
    let unpark_main = |vm: &Vm| {
        if let Some(sched) = vm.current_scheduler() {
            sched.unpark_main();
        }
    };
    // ROUND93-RECHECK(recv): the single in-loop race-point re-check —
    // same rationale as ROUND93-RECHECK(send) in
    // `main_thread_wait_for_send` (four formerly byte-identical copies;
    // see that comment for the full story). Locked by
    // `tests/concurrency/round93_concurrency_recheck_extraction_tests.rs`.
    //
    // Semantics (load-bearing, must not change):
    //   Value(v) -> drop the waker-registration guard FIRST, THEN
    //               unpark MAIN, then Ok(Message(v)).
    //   Closed   -> same drop/unpark ordering, Ok(Closed).
    //   Empty    -> None: fall through to the caller's wait protocol.
    // The post-`confirm_main_starved` call site consults this BEFORE
    // testing `confirmed`, so a value that races into flight during the
    // confirm window always wins over a deadlock verdict.
    //
    // NOTE: the no-scheduler fast path at the top of this function is
    // INTENTIONALLY different (no waker or park exists yet; `Empty` is
    // an immediate deadlock there) — do not unify it with this closure.
    let recheck =
        |reg: &mut Option<crate::runtime::channel::WakerRegistration>| match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                drop(reg.take());
                unpark_main(vm);
                Some(Ok(Value::variant(bv::MESSAGE, vec![val])))
            }
            TryReceiveResult::Closed => {
                drop(reg.take());
                unpark_main(vm);
                Some(Ok(Value::variant(bv::CLOSED, vec![])))
            }
            TryReceiveResult::Empty => None,
        };
    loop {
        if let Some(out) = recheck(&mut reg) {
            return out;
        }
        // Explicitly take-and-drop the previous iteration's guard
        // before minting a new one — see `main_thread_wait_for_send`
        // for the ordering rationale.
        drop(reg.take());
        let pair2 = pair.clone();
        reg = Some(ch.register_recv_waker_guard(Box::new(move || {
            let (lock, cvar) = &*pair2;
            *lock.lock() = true;
            cvar.notify_one();
        })));
        // Re-check after registration to avoid a lost wakeup.
        if let Some(out) = recheck(&mut reg) {
            return out;
        }
        // Pre-wait starvation check: if the wake graph already proves
        // we cannot be unblocked, fire deadlock without waiting. This
        // covers the steady-state case where main parks LAST (after
        // every other task is already blocked), so no future
        // signal_progress event will fire to wake us.
        //
        // Race window: a sender's `try_send` may have just landed a
        // value AND completed (the last task) between the
        // `register_recv_waker_guard` re-check above and this BFS.
        // The graph reads as starved (no live tasks) but the channel
        // has a value waiting. Do one final `try_receive` after the
        // graph says starved — the recv-waker also fires, but a
        // racing wake might be in flight. This mirrors the pre-Phase-4
        // "give one last try" pattern.
        if main_thread_is_starved(vm, &target) {
            // Candidate deadlock. First catch a value that raced into
            // flight (a sender's `try_send` may have landed AND completed
            // between the re-check above and this BFS).
            if let Some(out) = recheck(&mut reg) {
                return out;
            }
            // Confirm the starvation is STABLE before firing: a single
            // snapshot can be transiently starved during a task state
            // transition under load. Wait up to CONFIRM_MS for any
            // `signal_progress`; only fire if still starved afterward.
            // NOTE: the re-check below runs BEFORE `confirmed` is
            // tested — a value that raced in during the confirm window
            // must win over the deadlock verdict.
            let confirmed = confirm_main_starved(&pair, || main_thread_is_starved(vm, &target));
            if let Some(out) = recheck(&mut reg) {
                return out;
            }
            if confirmed {
                drop(reg);
                unpark_main(vm);
                report_unjoined_failures(vm);
                return Err(VmError::new(
                    "deadlock on main thread: channel receive with no counterparty".into(),
                ));
            }
            // Progress signalled or starvation cleared: re-evaluate.
            continue;
        }
        // Indefinite wait — woken by the recv-waker (channel state
        // change) or the wake-graph signal callback (any scheduler
        // state change). The Phase-3 100ms tick is gone.
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            while !*notified {
                cvar.wait(&mut notified);
            }
            *notified = false;
        }
        // Re-check the channel; the loop will also re-check
        // `is_main_starved` on the next iteration before waiting.
    }
}

/// Block the main thread on a `channel.select` over `ops` until one
/// arm becomes ready, or the wake graph proves no scheduled task can
/// drive ANY arm forward (deadlock).
///
/// Phase 4: same wake-graph-driven protocol as
/// `main_thread_wait_for_receive` / `_send`, generalized to a set of
/// select arms via `MainTarget::Select`. Each arm registers a
/// recv/send waker (held as a `WakerRegistration` guard so a losing
/// sibling is deregistered on return and its `waiting_*` counter does
/// not leak a phantom peer); a single shared condvar is poked by any
/// waker firing or by the wake-graph signal callback. On each wake we
/// re-run `try_select_sweep` (lost-wakeup guard) and consult
/// `main_thread_is_starved`; a confirmed-stable starvation fires a
/// "deadlock on main thread" error rather than spinning forever.
///
/// Before this path existed, this branch spun on `cvar.wait_for(1s)`
/// indefinitely — a `channel.select` over a set with no possible
/// counterparty never returned. `channel.receive` / `channel.send` on
/// the same dead set DO report a deadlock; this brings select in line
/// (and matches docs/concurrency.md, which claims select "detects a
/// deadlock and reports an error").
///
/// NOTE: `channel.recv_timeout` deliberately keeps its own inline
/// condvar loop and does NOT call this — its private timer channel is
/// `pending_timer_close`, so the timer thread's `close()` guarantees
/// termination even with no scheduler attached, where this function's
/// no-scheduler `MainTarget::Select` starvation check would (correctly,
/// for a plain select) report deadlock.
fn main_thread_wait_for_select(ops: &[SelectOp], vm: &Vm) -> Result<Value, VmError> {
    // Build the wake-graph target: one edge per arm. Closed channels
    // are still included — `is_main_starved`'s Select arm treats a
    // closed channel as fuel (not starved), and `try_select_sweep`
    // observes the closed state directly on the next pass.
    let edges: Vec<crate::scheduler::SelectEdge> = ops
        .iter()
        .map(|op| match op {
            SelectOp::Receive(ch) => crate::scheduler::SelectEdge::Recv(ch.clone()),
            SelectOp::Send(ch, _) => crate::scheduler::SelectEdge::Send(ch.clone()),
        })
        .collect();
    let target = crate::scheduler::MainTarget::Select(edges);

    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    // Install the wake-graph signal callback + park MAIN on the select
    // edge set so parked counterparties' BFS finds MAIN as a wake
    // destination. Unpark on exit (the `unpark_main` closure below).
    let _signal_guard = install_main_signal(vm, &pair);
    if let Some(sched) = vm.current_scheduler() {
        sched.park_main(&target);
    }
    let unpark_main = |vm: &Vm| {
        if let Some(sched) = vm.current_scheduler() {
            sched.unpark_main();
        }
    };

    // Per-arm waker registrations. Each iteration re-registers every
    // open arm and drops the prior guards first, so a stale waker is
    // deregistered before a fresh one is minted (no `waiting_*` leak —
    // same rationale as the receive/send single-waker paths, but here
    // the guards are a `Vec` over the arm set).
    let mut registrations: Vec<crate::runtime::channel::WakerRegistration> =
        Vec::with_capacity(ops.len());
    // Re-check helper: returns Some(result) when an arm is ready,
    // dropping the registrations FIRST then unparking MAIN — same
    // drop/unpark ordering as the receive/send recheck closures. The
    // sweep drops the registrations itself, before it passes on the
    // wake-ups of the arms that were not taken.
    let try_finish =
        |registrations: &mut Vec<crate::runtime::channel::WakerRegistration>| -> Result<Option<Value>, VmError> {
            if let Some(result) = try_select_sweep_registered(ops, registrations)? {
                unpark_main(vm);
                return Ok(Some(result));
            }
            Ok(None)
        };
    loop {
        if let Some(result) = try_finish(&mut registrations)? {
            return Ok(result);
        }
        // Drop the previous iteration's guards before minting new ones
        // so old wakers are deregistered first.
        registrations.clear();
        for op in ops {
            let pair2 = pair.clone();
            let waker = Box::new(move || {
                let (lock, cvar) = &*pair2;
                *lock.lock() = true;
                cvar.notify_one();
            });
            match op {
                SelectOp::Receive(ch) if !ch.is_closed() => {
                    registrations.push(ch.register_recv_waker_guard(waker));
                }
                SelectOp::Send(ch, _) if !ch.is_closed() => {
                    registrations.push(ch.register_send_waker_guard(waker));
                }
                // Closed channels: no registration — `try_select_sweep`
                // observes the closed state directly.
                SelectOp::Receive(_) | SelectOp::Send(_, _) => {}
            }
        }
        // Re-check after registering to close the lost-wakeup window
        // between the sweep above and the registrations.
        if let Some(result) = try_finish(&mut registrations)? {
            return Ok(result);
        }
        // Pre-wait starvation check: if the wake graph already proves
        // no arm can ever be made ready, this is a candidate deadlock.
        if main_thread_is_starved(vm, &target) {
            // Catch an arm that raced ready between the re-check above
            // and this BFS.
            if let Some(result) = try_finish(&mut registrations)? {
                return Ok(result);
            }
            // Confirm the starvation is STABLE before firing — a single
            // snapshot can be transiently starved under contention.
            let confirmed = confirm_main_starved(&pair, || main_thread_is_starved(vm, &target));
            // An arm that raced ready during the confirm window wins
            // over the deadlock verdict.
            if let Some(result) = try_finish(&mut registrations)? {
                return Ok(result);
            }
            if confirmed {
                registrations.clear();
                unpark_main(vm);
                report_unjoined_failures(vm);
                return Err(VmError::new(
                    "deadlock on main thread: channel select with no counterparty".into(),
                ));
            }
            // Progress signalled or starvation cleared: re-evaluate.
            continue;
        }
        // Indefinite wait — woken by any arm's waker (channel state
        // change) or the wake-graph signal callback. No 100ms tick.
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            while !*notified {
                cvar.wait(&mut notified);
            }
            *notified = false;
        }
    }
}

/// Block the main thread until `handle` produces a result or the wake
/// graph proves no scheduled task can drive the joinee forward
/// (deadlock).
///
/// Phase 4: same shape as `main_thread_wait_for_receive` — indefinite
/// `condvar.wait` woken by the join-waker (joinee completion) or the
/// wake-graph signal callback. The graph's BFS walks the joinee's
/// Join chain looking for a runnable / I/O / pending-counterparty
/// node; if it finds none, fire deadlock immediately. The Phase-3
/// `is_handle_blocked` carve-out + 100ms-tick streak escalator are
/// gone — the BFS subsumes them.
fn main_thread_wait_for_join(
    handle: &Arc<crate::runtime::handle::TaskHandle>,
    vm: &Vm,
) -> Result<Value, VmError> {
    // Fast path: no scheduler exists. The joinee can only have run
    // and completed if a scheduler exists, so absent one, either the
    // handle already has its result or the join is unsatisfiable.
    if vm.current_scheduler().is_none() {
        if let Some(result) = handle.try_get() {
            handle.mark_joined();
            return result;
        }
        return Err(VmError::new(
            "deadlock on main thread: task.join with no progress possible".into(),
        ));
    }
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    // Install the wake-graph signal callback + park MAIN on the join
    // target so the joinee BFS sees us as the destination.
    let target = crate::scheduler::MainTarget::from_join(handle);
    let _signal_guard = install_main_signal(vm, &pair);
    if let Some(sched) = vm.current_scheduler() {
        sched.park_main(&target);
    }
    let unpark_main = |vm: &Vm| {
        if let Some(sched) = vm.current_scheduler() {
            sched.unpark_main();
        }
    };
    // ROUND93-RECHECK(join): the single in-loop race-point re-check —
    // same rationale as ROUND93-RECHECK(send) in
    // `main_thread_wait_for_send` (three formerly identical copies).
    // Locked by `tests/concurrency/round93_concurrency_recheck_extraction_tests.rs`.
    //
    // Semantics (load-bearing, must not change): if the joinee's result
    // is available, unpark MAIN and return it; otherwise None and the
    // caller continues its wait protocol. There is no waker-registration
    // guard here — `register_join_waker` is one-shot, so the join family
    // has no `reg` to drop. The post-`confirm_main_starved` call site
    // consults this BEFORE testing `confirmed`, so a result that races
    // in during the confirm window always wins over a deadlock verdict.
    //
    // NOTE: the no-scheduler fast path at the top of this function is
    // INTENTIONALLY different (nothing is parked; a missing result is an
    // immediate deadlock) — do not unify it with this closure.
    let recheck = || match handle.try_get() {
        Some(result) => {
            // The join has the result: a failure of the task is not an
            // unjoined failure any more.
            handle.mark_joined();
            unpark_main(vm);
            Some(result)
        }
        None => None,
    };
    // Register a one-shot waker that flips the local condvar when the
    // task completes. `register_join_waker` fires the closure inline if
    // the task has already completed, which short-circuits the loop.
    let pair2 = pair.clone();
    handle.register_join_waker(Box::new(move || {
        let (lock, cvar) = &*pair2;
        *lock.lock() = true;
        cvar.notify_one();
    }));
    loop {
        if let Some(out) = recheck() {
            return out;
        }
        // Pre-wait starvation check: see `main_thread_wait_for_receive`.
        // If the graph says starved, do one final `try_get` — the
        // join-waker may have fired between the try above and the BFS,
        // racing the `on_complete` that flipped the graph empty.
        if main_thread_is_starved(vm, &target) {
            // Candidate deadlock. First catch a result that raced into
            // flight (the join-waker may have fired between the try
            // above and the BFS, racing the `on_complete`).
            if let Some(out) = recheck() {
                return out;
            }
            // Confirm the starvation is STABLE before firing: a single
            // snapshot can be transiently starved during a task state
            // transition under load. Wait up to CONFIRM_MS for any
            // `signal_progress`; only fire if still starved afterward.
            // NOTE: the re-check below runs BEFORE `confirmed` is
            // tested — a result that raced in during the confirm window
            // must win over the deadlock verdict.
            let confirmed = confirm_main_starved(&pair, || main_thread_is_starved(vm, &target));
            if let Some(out) = recheck() {
                return out;
            }
            if confirmed {
                unpark_main(vm);
                report_unjoined_failures(vm);
                return Err(VmError::new(
                    "deadlock on main thread: task.join with no progress possible".into(),
                ));
            }
            // Progress signalled or starvation cleared: re-evaluate.
            continue;
        }
        // Indefinite wait — woken by the join-waker (joinee completed)
        // or the wake-graph signal callback.
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            while !*notified {
                cvar.wait(&mut notified);
            }
            *notified = false;
        }
    }
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
