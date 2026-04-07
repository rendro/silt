//! Concurrency builtin functions (`channel.*`, `task.*`).

use std::sync::{Arc, Condvar, Mutex};

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
            // Buffer is full -- park via scheduler or spin.
            if vm.is_scheduled_task {
                vm.block_reason = Some(BlockReason::Send(ch));
                // Re-push args so CallBuiltin can re-execute after wake.
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }
            // Main thread: spin with OS yield until buffer has space.
            loop {
                match ch.try_send(val.clone()) {
                    TrySendResult::Sent => return Ok(Value::Unit),
                    TrySendResult::Closed => {
                        return Err(VmError::new(format!("send on closed channel {}", ch.id)));
                    }
                    TrySendResult::Full => {}
                }
                std::thread::yield_now();
            }
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
            // Channel is empty -- park via scheduler or block.
            if vm.is_scheduled_task {
                vm.block_reason = Some(BlockReason::Receive(ch));
                // Re-push args so CallBuiltin can re-execute after wake.
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }
            // Main thread: fall back to condvar-based blocking.
            match ch.receive_blocking() {
                TryReceiveResult::Value(val) => Ok(Value::Variant("Message".into(), vec![val])),
                TryReceiveResult::Closed => Ok(Value::Variant("Closed".into(), vec![])),
                TryReceiveResult::Empty => {
                    unreachable!("receive_blocking should not return Empty")
                }
            }
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
                    *lock.lock().unwrap() = true;
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
                let mut notified = lock.lock().unwrap();
                if !*notified {
                    let result = cvar
                        .wait_timeout(notified, std::time::Duration::from_secs(1))
                        .unwrap();
                    notified = result.0;
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
            let ch_clone = ch.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                ch_clone.close();
            });
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
            loop {
                match ch.try_receive() {
                    TryReceiveResult::Value(val) => {
                        vm.invoke_callable(&callback, &[val])?;
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
                                vm.invoke_callable(&callback, &[val])?;
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
                    Err(e) => child_handle.complete(Err(e.message)),
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
                scheduler.submit(Task {
                    id: task_id,
                    vm: child_vm,
                    handle: handle.clone(),
                });
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
                    Err(msg) => Err(VmError::new(format!("joined task failed: {msg}"))),
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
                Err(msg) => Err(VmError::new(format!("joined task failed: {msg}"))),
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
            handle.complete(Err("cancelled".to_string()));
            Ok(Value::Unit)
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

/// Try all select operations non-blocking. Returns the first that succeeds.
/// A closed channel counts as a successful receive (returns Closed).
fn try_select_sweep(ops: &[SelectOp]) -> Result<Option<Value>, VmError> {
    for op in ops {
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
