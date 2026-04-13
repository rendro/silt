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

            // Main thread: block on a shared condvar.
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            for op in &ops {
                let pair2 = pair.clone();
                let waker = Box::new(move || {
                    let (lock, cvar) = &*pair2;
                    *lock.lock() = true;
                    cvar.notify_one();
                });
                match op {
                    SelectOp::Receive(ch) if !ch.is_closed() => {
                        ch.register_recv_waker(waker);
                    }
                    SelectOp::Send(ch, _) if !ch.is_closed() => {
                        ch.register_send_waker(waker);
                    }
                    _ => {}
                }
            }
            loop {
                if let Some(result) = try_select_sweep(&ops)? {
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
            let task_id = vm.next_task_id();
            let handle = Arc::new(TaskHandle::new(task_id));

            let child_closure = closure.clone();
            let mut child_vm = vm.spawn_child();

            #[cfg(target_arch = "wasm32")]
            {
                // WASM: run synchronously (no threads available).
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
                // M:N scheduler: submit task to the shared thread pool.
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

            // Main thread: block with condvar (safe since we're not a worker).
            match handle.join() {
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
            // scoped wall-clock deadline of `dur` from now. Equivalent
            // to `task.spawn(fn() { task.deadline(dur, fn) })` but
            // without the closure-wrapping boilerplate and without the
            // extra stack frame. Same semantics as task.deadline:
            //   - I/O operations inside the spawned task return
            //     `Err("I/O timeout (task.deadline exceeded)")` once
            //     the deadline elapses (either at entry or during
            //     an I/O block via the watchdog).
            //   - CPU-bound work is NOT interrupted.
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
            let task_id = vm.next_task_id();
            let handle = Arc::new(TaskHandle::new(task_id));

            let child_closure = closure.clone();
            let mut child_vm = vm.spawn_child();
            // Install the deadline on the child VM before scheduling
            // (or executing, on wasm). The child's I/O builtins and the
            // scheduler's watchdog both consult current_deadline.
            child_vm.current_deadline =
                Instant::now().checked_add(Duration::from_nanos(dur_ns as u64));

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
                let new_deadline = Instant::now()
                    .checked_add(Duration::from_nanos(dur_ns as u64));
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
                    // Scope ending — pop the deadline we pushed.
                    vm.current_deadline = vm.deadline_stack.pop().unwrap_or(None);
                }
            }
            result
        }
        _ => Err(VmError::new(format!("unknown task function: {name}"))),
    }
}

// ── Select helpers ────────────────────────────────────────────────

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

// ── Main-thread channel wait with watchdog ───────────────────────
//
// When `fn main()` runs on the main thread (`is_scheduled_task = false`)
// and calls `channel.send` on a full channel or `channel.receive` on an
// empty one, we cannot park via the scheduler — the main thread is
// invisible to it. Previously `send` spun with `yield_now()` (100% CPU
// forever) and `receive` blocked indefinitely via `receive_blocking`,
// so a program with no other producers/consumers would hang.
//
// These helpers now block on the channel's existing waker machinery via
// a local condvar, and periodically check the scheduler for progress.
// If no scheduled task can possibly make progress (there is no scheduler,
// `live_tasks == 0`, or `blocked_tasks >= live_tasks`), we return a
// deadlock error instead of hanging forever. The first poll happens
// immediately so the watchdog is responsive; subsequent polls use
// `wait_for` so we don't burn CPU.

/// How often the watchdog wakes up to re-check scheduler progress.
const MAIN_THREAD_WATCHDOG_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Determine whether the scheduler could still make progress on our
/// behalf. Returns `true` if there is at least one scheduled task that
/// is not currently blocked (so it could still reach the channel), or
/// if there is no scheduler at all but the channel has pending wakers
/// on the opposite side (rare edge case we treat as "still progressing").
///
/// Returns `false` when we are certain no scheduled task could unblock
/// us — the caller should treat this as a deadlock.
fn scheduler_can_make_progress(vm: &Vm) -> bool {
    match vm.current_scheduler() {
        None => false, // No scheduler exists — no task could wake us.
        Some(sched) => {
            let (live, blocked) = sched.progress_snapshot();
            // If any task is live and not currently blocked it might
            // still run and reach our channel.
            live > 0 && live > blocked
        }
    }
}

/// Block the main thread until the channel accepts `val`, the channel
/// is closed, or the scheduler can no longer make progress (deadlock).
fn main_thread_wait_for_send(
    ch: &Arc<crate::value::Channel>,
    val: Value,
    vm: &Vm,
) -> Result<Value, VmError> {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    loop {
        // Try first so we don't miss a send slot that just opened.
        match ch.try_send(val.clone()) {
            TrySendResult::Sent => return Ok(Value::Unit),
            TrySendResult::Closed => {
                return Err(VmError::new(format!("send on closed channel {}", ch.id)));
            }
            TrySendResult::Full => {}
        }
        // Register a send waker that pokes our local condvar. The
        // channel drains wakers on successful receive/close, so we
        // must re-register each iteration.
        let pair2 = pair.clone();
        ch.register_send_waker(Box::new(move || {
            let (lock, cvar) = &*pair2;
            *lock.lock() = true;
            cvar.notify_one();
        }));
        // Re-check after registering to avoid a lost wakeup race
        // between try_send above and register_send_waker.
        match ch.try_send(val.clone()) {
            TrySendResult::Sent => return Ok(Value::Unit),
            TrySendResult::Closed => {
                return Err(VmError::new(format!("send on closed channel {}", ch.id)));
            }
            TrySendResult::Full => {}
        }
        // Wait for a notify or the watchdog tick.
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            if !*notified {
                cvar.wait_for(&mut notified, MAIN_THREAD_WATCHDOG_TICK);
            }
            *notified = false;
        }
        // If the scheduler cannot make progress, declare deadlock.
        if !scheduler_can_make_progress(vm) {
            // Give one last try in case a task completed between
            // the wait and the check.
            match ch.try_send(val.clone()) {
                TrySendResult::Sent => return Ok(Value::Unit),
                TrySendResult::Closed => {
                    return Err(VmError::new(format!("send on closed channel {}", ch.id)));
                }
                TrySendResult::Full => {}
            }
            return Err(VmError::new(
                "deadlock on main thread: channel send with no counterparty".into(),
            ));
        }
    }
}

/// Block the main thread until the channel yields a value, is closed,
/// or the scheduler can no longer make progress (deadlock).
fn main_thread_wait_for_receive(
    ch: &Arc<crate::value::Channel>,
    vm: &Vm,
) -> Result<Value, VmError> {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    loop {
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                return Ok(Value::Variant("Message".into(), vec![val]));
            }
            TryReceiveResult::Closed => {
                return Ok(Value::Variant("Closed".into(), vec![]));
            }
            TryReceiveResult::Empty => {}
        }
        let pair2 = pair.clone();
        ch.register_recv_waker(Box::new(move || {
            let (lock, cvar) = &*pair2;
            *lock.lock() = true;
            cvar.notify_one();
        }));
        // Re-check after registration to avoid a lost wakeup.
        match ch.try_receive() {
            TryReceiveResult::Value(val) => {
                return Ok(Value::Variant("Message".into(), vec![val]));
            }
            TryReceiveResult::Closed => {
                return Ok(Value::Variant("Closed".into(), vec![]));
            }
            TryReceiveResult::Empty => {}
        }
        {
            let (lock, cvar) = &*pair;
            let mut notified = lock.lock();
            if !*notified {
                cvar.wait_for(&mut notified, MAIN_THREAD_WATCHDOG_TICK);
            }
            *notified = false;
        }
        if !scheduler_can_make_progress(vm) {
            match ch.try_receive() {
                TryReceiveResult::Value(val) => {
                    return Ok(Value::Variant("Message".into(), vec![val]));
                }
                TryReceiveResult::Closed => {
                    return Ok(Value::Variant("Closed".into(), vec![]));
                }
                TryReceiveResult::Empty => {}
            }
            return Err(VmError::new(
                "deadlock on main thread: channel receive with no counterparty".into(),
            ));
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
