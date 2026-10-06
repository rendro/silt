//! Concurrency builtin functions (`channel.*`, `task.*`).
//!
//! A function that cannot go on says what it waits for (a [`Wait`]) and
//! is resumed with how the wait ended (`Vm::park`). Whoever makes an
//! operation possible completes it, so a function that was woken has
//! its result and never tries again.

use std::sync::Arc;
use std::time::Duration;

use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Native, Step, Vm, VmError};

/// Build the canonical closed-channel-send VmError (message wording is
/// pinned by tests in `tests/lang/error_tests.rs` and `tests/heavy/integration.rs`).
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

fn message(value: Value) -> Value {
    Value::variant(bv::MESSAGE, vec![value])
}

fn closed() -> Value {
    Value::variant(bv::CLOSED, vec![])
}

fn channel_err(tag: crate::typeinfo::BuiltinVariant) -> Value {
    Value::variant(bv::ERR, vec![Value::variant(tag, vec![])])
}

/// Dispatch `channel.<name>(args)`.
pub(crate) fn call_channel(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
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
            Ok(Step::Done(Value::Channel(Channel::new(id, capacity))))
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
            let id = ch.id();
            let value = match ch.try_send(args[1].clone(), vm.scheduler().wake()) {
                TrySend::Sent => return Ok(Step::Done(Value::Unit)),
                TrySend::Closed(_) => return Err(closed_channel_send_err(id)),
                TrySend::Full(value) => value,
            };
            // No receiver and no room: the task waits until its value
            // is taken.
            let wait = Wait::new(vec![Arm::Send(ch.clone(), value)]);
            Ok(vm.park("channel.send", wait, move |_, fired| match fired {
                Fired::Arm(_, Outcome::Closed(_)) => Err(closed_channel_send_err(id)),
                _ => Ok(Step::Done(Value::Unit)),
            }))
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
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Ok(Step::Done(message(value))),
                TryReceive::Closed(_) => return Ok(Step::Done(closed())),
                TryReceive::Empty => {}
            }
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]);
            Ok(vm.park("channel.receive", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => message(value),
                    _ => closed(),
                }))
            }))
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
            ch.close(Close::default(), vm.scheduler().wake());
            Ok(Step::Done(Value::Unit))
        }
        "try_send" => {
            if args.len() != 2 {
                return Err(VmError::new("channel.try_send takes 2 arguments".into()));
            }
            let Value::Channel(ch) = &args[0] else {
                return Err(VmError::new("channel.try_send requires a channel".into()));
            };
            let sent = ch.try_send(args[1].clone(), vm.scheduler().wake());
            Ok(Step::Done(Value::Bool(matches!(sent, TrySend::Sent))))
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
            Ok(Step::Done(match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => message(value),
                TryReceive::Empty => Value::variant(bv::EMPTY, Vec::new()),
                TryReceive::Closed(_) => closed(),
            }))
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
            let arms = parse_select_ops(ops_list)?;
            if arms.is_empty() {
                return Err(VmError::new(
                    "channel.select requires at least one operation".into(),
                ));
            }
            let channels: Vec<Arc<Channel>> = arms
                .iter()
                .map(|arm| match arm {
                    Arm::Recv(ch) | Arm::Send(ch, _) => ch.clone(),
                    Arm::Cell(_) => unreachable!("a select has channel arms"),
                })
                .collect();
            // The wait completes the first arm that is possible, from
            // an arm picked at random, or the first that becomes so.
            let first = select_start_index(arms.len());
            let wait = Wait::new(arms).first(first);
            Ok(vm.park("channel.select", wait, move |_, fired| {
                let Fired::Arm(arm, outcome) = fired else {
                    return Err(VmError::new(
                        "internal VM error: a select ended without an arm".into(),
                    ));
                };
                let outcome = match outcome {
                    Outcome::Received(value) => message(value),
                    Outcome::Sent => Value::variant(bv::SENT, vec![]),
                    Outcome::Closed(_) | Outcome::Done => closed(),
                };
                Ok(Step::Done(Value::Tuple(vec![
                    Value::Channel(channels[arm].clone()),
                    outcome,
                ])))
            }))
        }
        "recv_timeout" => {
            // channel.recv_timeout(ch, dur) -> Result(a, ChannelError)
            //
            //   * Ok(value)            — a value was delivered within the
            //                            timeout. A value that is there wins
            //                            over a timeout that has passed, even
            //                            at duration zero.
            //   * Err(ChannelClosed)   — the channel is closed and empty.
            //   * Err(ChannelTimeout)  — the timeout elapsed with no value
            //                            and no close. A positive duration
            //                            below a millisecond waits one.
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
            // A negative duration is an error even when a value is
            // there.
            let nanos = crate::builtins::time::extract_duration(&args[1])?;
            if nanos < 0 {
                return Err(VmError::new(
                    "channel.recv_timeout: duration must be non-negative".into(),
                ));
            }
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => {
                    return Ok(Step::Done(Value::variant(bv::OK, vec![value])));
                }
                TryReceive::Closed(_) => return Ok(Step::Done(channel_err(bv::CHANNEL_CLOSED))),
                TryReceive::Empty => {}
            }
            if nanos == 0 {
                return Ok(Step::Done(channel_err(bv::CHANNEL_TIMEOUT)));
            }
            let millis = (nanos as u64).div_ceil(1_000_000).max(1);
            let deadline = vm
                .runtime
                .io
                .deadline_after(Duration::from_millis(millis))
                .ok_or_else(|| {
                    VmError::new("cannot start a timer: the duration is out of range".into())
                })?;
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]).deadline(Some(deadline));
            Ok(vm.park("channel.recv_timeout", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => Value::variant(bv::OK, vec![value]),
                    Fired::Arm(..) => channel_err(bv::CHANNEL_CLOSED),
                    Fired::Deadline => channel_err(bv::CHANNEL_TIMEOUT),
                }))
            }))
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
            let deadline = vm
                .runtime
                .io
                .deadline_after(Duration::from_millis(*ms as u64))
                .ok_or_else(|| {
                    VmError::new("cannot start a timer: the duration is out of range".into())
                })?;
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::new(failure));
            }
            // Nothing is ever sent on it: the timer closes it.
            let ch = Channel::new(vm.next_channel_id(), 1);
            vm.scheduler().close_at(deadline, ch.clone());
            Ok(Step::Done(Value::Channel(ch)))
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
            Ok(Step::Run(Box::new(Each {
                ch: ch.clone(),
                callback: args[1].clone(),
                state: EachState::Take,
            })))
        }
        _ => Err(VmError::new(format!("unknown channel function: {name}"))),
    }
}

/// `channel.each(ch, f)`: `f` is called with each message of `ch` until
/// the channel is closed.
struct Each {
    ch: Arc<Channel>,
    callback: Value,
    state: EachState,
}

/// What the input of an `Each` frame is.
enum EachState {
    /// Nothing: the next message is to be taken.
    Take,
    /// The value of a call of the callback.
    Called,
    /// Nothing: the frame waited for a message.
    Waited,
}

impl Each {
    fn call(&mut self, vm: &mut Vm, message: Value) -> Step {
        self.state = EachState::Called;
        vm.call(self.callback.clone(), [message])
    }
}

/// The end of a `channel.each`: the channel is closed and empty. If a
/// stream stage fed it and failed, that failure is raised here.
fn ended(close: Close) -> Result<Step, VmError> {
    match close.failure {
        Some(failure) => Err((*failure).clone()),
        None => Ok(Step::Done(Value::Unit)),
    }
}

impl Native for Each {
    fn name(&self) -> &str {
        "channel.each"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        match std::mem::replace(&mut self.state, EachState::Take) {
            // After each message, give way to the other tasks.
            EachState::Called => return Ok(Step::Yield),
            EachState::Waited => {
                return match vm.woken()? {
                    Fired::Arm(_, Outcome::Received(message)) => Ok(self.call(vm, message)),
                    Fired::Arm(_, Outcome::Closed(close)) => ended(close),
                    _ => Ok(Step::Done(Value::Unit)),
                };
            }
            EachState::Take => {}
        }
        match self.ch.try_receive(vm.scheduler().wake()) {
            TryReceive::Value(message) => Ok(self.call(vm, message)),
            TryReceive::Closed(close) => ended(close),
            TryReceive::Empty => {
                self.state = EachState::Waited;
                Ok(Step::Park(Wait::new(vec![Arm::Recv(self.ch.clone())])))
            }
        }
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

    let mut child_vm = vm.spawn_child();
    child_vm.current_deadline = deadline;

    child_vm.start_task(closure.clone());
    child_vm.spawned = true;
    vm.scheduler()
        .submit(task_id, child_vm, handle.clone())
        .map_err(VmError::new)?;

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
        _ => task_plain(vm, name, args),
    }
}

/// `task.deadline(dur, fn)`: runs `fn` with a scoped wall-clock
/// deadline of `dur` from now. A wait for I/O inside the callback
/// ends at the deadline, with `Err("I/O timeout (task.deadline
/// exceeded)")` in the module's typed shape. I/O builtins also check at entry and
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

/// The result of a task for the one that joins it.
fn joined(handle: &TaskHandle, result: Result<Value, VmError>) -> Result<Step, VmError> {
    // The joiner has the result now. If the task failed, the error is
    // the joiner's to handle and is not reported as an unjoined
    // failure.
    handle.mark_joined();
    match result {
        Ok(value) => Ok(Step::Done(value)),
        Err(mut inner) => {
            inner.message = format!("joined task failed: {}", inner.message);
            Err(inner)
        }
    }
}

/// The `task` functions that call no function.
fn task_plain(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
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
            spawn_with_deadline(vm, closure, None).map(Step::Done)
        }
        "join" => {
            if args.len() != 1 {
                return Err(VmError::new("task.join takes 1 argument (handle)".into()));
            }
            let Value::Handle(handle) = &args[0] else {
                return Err(VmError::new("task.join requires a handle argument".into()));
            };
            if let Some(result) = handle.try_get() {
                return joined(handle, result);
            }
            let handle = handle.clone();
            let wait = Wait::new(vec![Arm::Cell(handle.done())]);
            Ok(vm.park("task.join", wait, move |_, _| {
                let result = handle.try_get().ok_or_else(|| {
                    VmError::new("internal VM error: a join ended before its task".into())
                })?;
                joined(&handle, result)
            }))
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
            // The task runs no further slice, and a wait of it ends
            // without taking or sending anything. Cancelling a task
            // handles it: if it had failed before, its failure is
            // dismissed and not reported as an unjoined failure. A join
            // still raises that failure, because the handle keeps the
            // result that came first.
            vm.scheduler().cancel(handle);
            Ok(Step::Done(Value::Unit))
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
            spawn_with_deadline(vm, closure, deadline).map(Step::Done)
        }
        _ => Err(VmError::new(format!("unknown task function: {name}"))),
    }
}

// ── Select helpers ────────────────────────────────────────────────

/// Parse the select operations list. Every element must be a
/// `ChannelOp` variant:
/// - `Recv(channel)` → receive
/// - `Send(channel, value)` → send
fn parse_select_ops(ops_list: &[Value]) -> Result<Vec<Arm>, VmError> {
    let mut ops = Vec::with_capacity(ops_list.len());
    for item in ops_list {
        match item {
            Value::Variant(name, fields) if name.is(bv::RECV) && fields.len() == 1 => {
                let Value::Channel(ch) = &fields[0] else {
                    return Err(VmError::new(
                        "channel.select Recv operation must wrap a Channel".into(),
                    ));
                };
                ops.push(Arm::Recv(ch.clone()));
            }
            Value::Variant(name, fields) if name.is(bv::SEND) && fields.len() == 2 => {
                let Value::Channel(ch) = &fields[0] else {
                    return Err(VmError::new(
                        "channel.select Send operation must take (Channel, value)".into(),
                    ));
                };
                ops.push(Arm::Send(ch.clone(), fields[1].clone()));
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

/// Pick a pseudo-random index in [0, n): the arm a `channel.select`
/// tries first. When several arms are ready, each one has a chance of
/// being chosen instead of always the earliest.
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
        // clock: fairness needs no better seed.
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
