//! Concurrency builtin functions (`channel.*`, `task.*`).

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::value::{Channel, TaskHandle, TryReceiveResult, TrySendResult, Value};
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

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
                    return Err(VmError::new(format!("send on closed channel {}", ch.id)));
                }
                TrySendResult::Full => {}
            }
            // Buffer is full -- park via scheduler or wait with a watchdog.
            if vm.is_scheduled_task {
                vm.block_reason = Some(BlockReason::Send(ch));
                // Re-push args so CallBuiltin can re-execute after wake.
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
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
                    return Ok(Value::Variant("Message".into(), vec![val]));
                }
                TryReceiveResult::Closed => {
                    return Ok(Value::Variant("Closed".into(), vec![]));
                }
                TryReceiveResult::Empty => {}
            }
            // Channel is empty -- park via scheduler or wait with a watchdog.
            if vm.is_scheduled_task {
                vm.block_reason = Some(BlockReason::Receive(ch));
                // Re-push args so CallBuiltin can re-execute after wake.
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
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
                TryReceiveResult::Value(val) => Ok(Value::Variant("Message".into(), vec![val])),
                TryReceiveResult::Empty => Ok(Value::Variant("Empty".into(), Vec::new())),
                TryReceiveResult::Closed => Ok(Value::Variant("Closed".into(), Vec::new())),
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

            // No operation succeeded — park via scheduler or spin.
            if vm.is_scheduled_task {
                vm.block_reason = Some(BlockReason::Select(select_ops));
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }

            // Main thread: block on a shared condvar. Wrap every
            // registration in a `WakerRegistration` guard so the
            // losing siblings are deregistered on return — otherwise
            // their `waiting_receivers` / `waiting_senders` counter
            // stays inflated and a later rendezvous `try_send` sees a
            // phantom peer (same bug class as the scheduled-task path
            // fix in src/scheduler.rs select arm).
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            let mut registrations: Vec<crate::value::WakerRegistration> =
                Vec::with_capacity(ops.len());
            for op in &ops {
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
                    // Closed channels: no registration needed — a
                    // subsequent `try_select_sweep` iteration observes
                    // the closed state directly.
                    SelectOp::Receive(_) | SelectOp::Send(_, _) => {}
                }
            }
            loop {
                if let Some(result) = try_select_sweep(&ops)? {
                    // Dropping `registrations` (at scope exit) runs
                    // each guard's Drop → `remove_*_waker`. Idempotent
                    // for entries whose waker already fired.
                    drop(registrations);
                    return Ok(result);
                }
                let (lock, cvar) = &*pair;
                let mut notified = lock.lock();
                if !*notified {
                    cvar.wait_for(&mut notified, std::time::Duration::from_secs(1));
                }
                *notified = false;
            }
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
            let dur_ns = crate::builtins::data::extract_duration(&args[1])?;
            if dur_ns < 0 {
                return Err(VmError::new(
                    "channel.recv_timeout: duration must be non-negative".into(),
                ));
            }

            // Always try non-blocking first — delivery beats timeout even at
            // zero duration (matches the "ready value wins" corner case).
            match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    return Ok(Value::Variant("Ok".into(), vec![val]));
                }
                TryReceiveResult::Closed => {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String("closed".into())],
                    ));
                }
                TryReceiveResult::Empty => {}
            }
            // Zero duration on an empty channel = instant timeout.
            if dur_ns == 0 {
                return Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String("timeout".into())],
                ));
            }

            // Ceil the nanosecond duration up to at least 1ms so any positive
            // sub-ms request still gets a real tick of wait. The timer wheel
            // is ms-granular.
            let ms: u64 = {
                let ns = dur_ns as u64;
                ns.div_ceil(1_000_000).max(1)
            };

            // Build the private timer channel. Reuses the shared TimerManager
            // thread — no per-call OS thread. `channel.timeout` marks the
            // channel as pending-timer-close so the main-thread deadlock
            // detector correctly treats a recv-timeout wait as "external
            // wake pending".
            let timer_id = vm.next_channel_id();
            let timer_ch = Arc::new(Channel::new(timer_id, 1));
            vm.runtime
                .timer
                .schedule(Duration::from_millis(ms), timer_ch.clone());

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
                vm.block_reason = Some(BlockReason::Select(select_ops));
                for arg in args {
                    vm.push(arg.clone());
                }
                // Stash the timer channel on the stack too? No — we rebuild
                // it from the duration on resume by calling the same code
                // path, so the duration alone is enough. Actually, we DO
                // re-enter this arm on resume because CallBuiltin replays
                // its args, and the `try_receive` above will either deliver
                // a value landed during the park (correct) or fall through
                // to a FRESH timer + select. A stale timer from this park
                // would already have closed timer_ch and been dropped here
                // — it can't keep us alive. So: rely on the `try_receive`
                // short-circuit above to observe any delivered value
                // post-wake, and otherwise re-arm a new timer. The
                // worst-case extra delay on resume is `ms * 1` (one
                // additional timer tick); for a recv_timeout that just
                // parked, that is acceptable — a task should not commonly
                // yield out of this arm mid-wait except via cancel (which
                // does not resume) or another VM-level yield signal
                // (which is not expected here because select does not
                // yield internally).
                return Err(VmError::yield_signal());
            }
            // Main-thread path: drive the same select condvar loop that the
            // `channel.select` builtin uses. Mirrors the structure there;
            // we only differ in how we map the final Value back to a Result
            // variant.
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            let mut registrations: Vec<crate::value::WakerRegistration> =
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
                if let Some(val) = try_select_sweep(&ops)? {
                    drop(registrations);
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
                .schedule(std::time::Duration::from_millis(ms), ch.clone());
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
                            vm.block_reason = Some(BlockReason::Receive(ch));
                            // Re-push args so the CallBuiltin re-executes channel.each.
                            for arg in args {
                                vm.push(arg.clone());
                            }
                            return Err(VmError::yield_signal());
                        }
                        // Main thread: block on condvar until data or close.
                        match ch.receive_blocking() {
                            TryReceiveResult::Value(val) => {
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
                            TryReceiveResult::Closed => {
                                return Ok(Value::Unit);
                            }
                            TryReceiveResult::Empty => unreachable!(),
                        }
                    }
                }
            }
        }
        _ => Err(VmError::new(format!("unknown channel function: {name}"))),
    }
}

/// Spawn a child task with an optional scoped wall-clock deadline.
/// Shared by `task.spawn` (deadline = None) and `task.spawn_until`
/// (deadline = Some(now + dur)). Returns the Handle wrapping the
/// spawned task; propagates scheduler.submit errors unchanged.
fn spawn_with_deadline(
    vm: &mut Vm,
    closure: &Arc<crate::bytecode::VmClosure>,
    deadline: Option<Instant>,
) -> Result<Value, VmError> {
    let task_id = vm.next_task_id();
    let handle = Arc::new(TaskHandle::new(task_id));

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

            // If already complete, return immediately.
            if let Some(result) = handle.try_get() {
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
                vm.block_reason = Some(BlockReason::Join(handle));
                // Re-push args so CallBuiltin can re-execute after wake.
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
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
                    inner.message = format!("joined task failed: {}", inner.message);
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
            Ok(Value::Unit)
        }
        "spawn_until" => {
            // task.spawn_until(dur, fn) — spawn a task that runs with a
            // scoped wall-clock deadline. Equivalent to
            // `task.spawn(fn() { task.deadline(dur, fn) })` minus the
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
            let deadline = Instant::now().checked_add(Duration::from_nanos(dur_ns as u64));
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
                let new_deadline = Instant::now().checked_add(Duration::from_nanos(dur_ns as u64));
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

/// Translate a `try_select_sweep` result (a `(Channel, Variant)` tuple) into
/// the `Result(a, String)` shape expected by `channel.recv_timeout`:
///
///   * (timer_ch, _) → `Err("timeout")` — the timer channel fired, regardless
///     of whether as `Message` (never sent to) or `Closed` (timer expired).
///   * (user_ch, Message(v)) → `Ok(v)`.
///   * (user_ch, Closed) → `Err("closed")`.
///
/// `tuple` is expected to be `Value::Tuple(vec![Channel, Variant])` per the
/// shape returned by `try_select_sweep`; anything else is a programming bug.
fn map_recv_timeout_result(tuple: Value, timer_ch: &Arc<Channel>) -> Value {
    let Value::Tuple(parts) = tuple else {
        debug_assert!(false, "recv_timeout: select result not a tuple");
        return Value::Variant(
            "Err".into(),
            vec![Value::String("recv_timeout: internal shape error".into())],
        );
    };
    let Some(Value::Channel(src)) = parts.first() else {
        debug_assert!(false, "recv_timeout: select result missing channel");
        return Value::Variant(
            "Err".into(),
            vec![Value::String("recv_timeout: internal channel error".into())],
        );
    };
    if Arc::ptr_eq(src, timer_ch) {
        return Value::Variant("Err".into(), vec![Value::String("timeout".into())]);
    }
    match parts.get(1) {
        Some(Value::Variant(name, fields)) if name.as_str() == "Message" => {
            let val = fields.first().cloned().unwrap_or(Value::Unit);
            Value::Variant("Ok".into(), vec![val])
        }
        Some(Value::Variant(name, _)) if name.as_str() == "Closed" => {
            Value::Variant("Err".into(), vec![Value::String("closed".into())])
        }
        _ => {
            debug_assert!(false, "recv_timeout: unexpected select variant");
            Value::Variant(
                "Err".into(),
                vec![Value::String("recv_timeout: internal variant error".into())],
            )
        }
    }
}

/// A parsed select operation: receive from a channel or send to a channel.
enum SelectOp {
    Receive(Arc<Channel>),
    Send(Arc<Channel>, Value),
}

/// Parse the select operations list.
/// - Bare `Channel` → receive
/// - `(Channel, value)` tuple → send
fn parse_select_ops(ops_list: &[Value]) -> Result<Vec<SelectOp>, VmError> {
    let mut ops = Vec::with_capacity(ops_list.len());
    for item in ops_list {
        match item {
            Value::Channel(ch) => {
                ops.push(SelectOp::Receive(ch.clone()));
            }
            Value::Tuple(pair) if pair.len() == 2 => {
                let Value::Channel(ch) = &pair[0] else {
                    return Err(VmError::new(
                        "channel.select send operation must be (channel, value)".into(),
                    ));
                };
                ops.push(SelectOp::Send(ch.clone(), pair[1].clone()));
            }
            _ => {
                return Err(VmError::new(
                    "channel.select list items must be channels or (channel, value) tuples".into(),
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
    use std::time::SystemTime;
    thread_local! {
        static SELECT_RNG: Cell<u64> = Cell::new({
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xA5A5_A5A5_5A5A_5A5A)
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
fn try_select_sweep(ops: &[SelectOp]) -> Result<Option<Value>, VmError> {
    let n = ops.len();
    if n == 0 {
        return Ok(None);
    }
    let start = select_start_index(n);
    for i in 0..n {
        let op = &ops[(start + i) % n];
        match op {
            SelectOp::Receive(ch) => match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    return Ok(Some(Value::Tuple(vec![
                        Value::Channel(ch.clone()),
                        Value::Variant("Message".into(), vec![val]),
                    ])));
                }
                TryReceiveResult::Closed => {
                    return Ok(Some(Value::Tuple(vec![
                        Value::Channel(ch.clone()),
                        Value::Variant("Closed".into(), vec![]),
                    ])));
                }
                TryReceiveResult::Empty => {}
            },
            SelectOp::Send(ch, val) => match ch.try_send(val.clone()) {
                TrySendResult::Sent => {
                    return Ok(Some(Value::Tuple(vec![
                        Value::Channel(ch.clone()),
                        Value::Variant("Sent".into(), vec![]),
                    ])));
                }
                TrySendResult::Closed => {
                    return Ok(Some(Value::Tuple(vec![
                        Value::Channel(ch.clone()),
                        Value::Variant("Closed".into(), vec![]),
                    ])));
                }
                TrySendResult::Full => {}
            },
        }
    }

    Ok(None)
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
fn main_thread_is_starved(vm: &Vm, target: &crate::scheduler::MainTarget) -> bool {
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
        // Select with no scheduler: no channel timers tracked through
        // SelectEdge ids; deadlock.
        crate::scheduler::MainTarget::Select(_) => true,
    }
}

/// Block the main thread until the channel accepts `val`, the channel
/// is closed, or the scheduler can no longer make progress (deadlock).
fn main_thread_wait_for_send(
    ch: &Arc<crate::value::Channel>,
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
                return Err(VmError::new(format!("send on closed channel {}", ch.id)));
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
    // Track the most recently registered send-waker as a
    // `WakerRegistration` guard. Dropping / replacing the guard
    // deregisters the prior iteration's waker. Without this, the
    // channel only drains wakers on successful receive/close, so the
    // guard swap on every loop iteration would leave a stale waker
    // closure in the queue (unbounded growth on a channel that nobody
    // is draining).
    let mut reg: Option<crate::value::WakerRegistration> = None;
    loop {
        // Try first so we don't miss a send slot that just opened.
        match ch.try_send(val.clone()) {
            TrySendResult::Sent => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Unit);
            }
            TrySendResult::Closed => {
                drop(reg);
                unpark_main(vm);
                return Err(VmError::new(format!("send on closed channel {}", ch.id)));
            }
            TrySendResult::Full => {}
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
        match ch.try_send(val.clone()) {
            TrySendResult::Sent => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Unit);
            }
            TrySendResult::Closed => {
                drop(reg);
                unpark_main(vm);
                return Err(VmError::new(format!("send on closed channel {}", ch.id)));
            }
            TrySendResult::Full => {}
        }
        // Pre-wait starvation check: see `main_thread_wait_for_receive`.
        if main_thread_is_starved(vm, &target) {
            match ch.try_send(val.clone()) {
                TrySendResult::Sent => {
                    drop(reg);
                    unpark_main(vm);
                    return Ok(Value::Unit);
                }
                TrySendResult::Closed => {
                    drop(reg);
                    unpark_main(vm);
                    return Err(VmError::new(format!("send on closed channel {}", ch.id)));
                }
                TrySendResult::Full => {}
            }
            drop(reg);
            unpark_main(vm);
            return Err(VmError::new(
                "deadlock on main thread: channel send with no counterparty".into(),
            ));
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
    ch: &Arc<crate::value::Channel>,
    vm: &Vm,
) -> Result<Value, VmError> {
    // No-scheduler + no-timer fast path: there is no scheduler to
    // pump events through `signal_progress`, AND no pending timer
    // close that would fire `wake_all_recv` on the channel. Any wait
    // here would be infinite — fire deadlock immediately. (When a
    // timer IS pending, the recv-waker we register below is woken by
    // the timer thread's `ch.close()` → `wake_all_recv()` chain, so
    // the indefinite `cvar.wait` is finite.)
    if vm.current_scheduler().is_none() && !ch.has_pending_timer_close() {
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                return Ok(Value::Variant("Message".into(), vec![val]));
            }
            TryReceiveResult::Closed => return Ok(Value::Variant("Closed".into(), vec![])),
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
    let mut reg: Option<crate::value::WakerRegistration> = None;
    // Helper to consistently unpark MAIN from the wake graph on exit.
    // Called before every early-return in the loop.
    let unpark_main = |vm: &Vm| {
        if let Some(sched) = vm.current_scheduler() {
            sched.unpark_main();
        }
    };
    loop {
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Variant("Message".into(), vec![val]));
            }
            TryReceiveResult::Closed => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Variant("Closed".into(), vec![]));
            }
            TryReceiveResult::Empty => {}
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
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Variant("Message".into(), vec![val]));
            }
            TryReceiveResult::Closed => {
                drop(reg);
                unpark_main(vm);
                return Ok(Value::Variant("Closed".into(), vec![]));
            }
            TryReceiveResult::Empty => {}
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
            match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    drop(reg);
                    unpark_main(vm);
                    return Ok(Value::Variant("Message".into(), vec![val]));
                }
                TryReceiveResult::Closed => {
                    drop(reg);
                    unpark_main(vm);
                    return Ok(Value::Variant("Closed".into(), vec![]));
                }
                TryReceiveResult::Empty => {}
            }
            drop(reg);
            unpark_main(vm);
            return Err(VmError::new(
                "deadlock on main thread: channel receive with no counterparty".into(),
            ));
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
    handle: &Arc<crate::value::TaskHandle>,
    vm: &Vm,
) -> Result<Value, VmError> {
    // Fast path: no scheduler exists. The joinee can only have run
    // and completed if a scheduler exists, so absent one, either the
    // handle already has its result or the join is unsatisfiable.
    if vm.current_scheduler().is_none() {
        if let Some(result) = handle.try_get() {
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
        if let Some(result) = handle.try_get() {
            unpark_main(vm);
            return result;
        }
        // Pre-wait starvation check: see `main_thread_wait_for_receive`.
        // If the graph says starved, do one final `try_get` — the
        // join-waker may have fired between the try above and the BFS,
        // racing the `on_complete` that flipped the graph empty.
        if main_thread_is_starved(vm, &target) {
            if let Some(result) = handle.try_get() {
                unpark_main(vm);
                return result;
            }
            unpark_main(vm);
            return Err(VmError::new(
                "deadlock on main thread: task.join with no progress possible".into(),
            ));
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
    use crate::value::Channel;
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
