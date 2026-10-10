//! Concurrency builtin functions (`channel.*`, `task.*`).
//!
//! A function that cannot go on says what it waits for (a [`Wait`]) and
//! is resumed with how the wait ended (`Vm::park`). Whoever makes an
//! operation possible completes it, so a function that was woken has
//! its result and never tries again.

use std::sync::Arc;
use std::time::Duration;

use super::time;
use super::typed::{Arg, Called, Chan, Handle, List, builtins, unsound};
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

/// What `ChannelError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("ChannelTimeout", []) => "channel receive timed out".to_string(),
        ("ChannelClosed", []) => "channel closed with no more values".to_string(),
        _ => return None,
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

/// The time a `Duration` argument of `name` is: an error if it is
/// negative.
fn span(name: &str, duration: time::Duration) -> Result<Duration, VmError> {
    match u64::try_from(duration.0) {
        Ok(nanos) => Ok(Duration::from_nanos(nanos)),
        Err(_) => Err(VmError::new(format!(
            "{name}: duration must be non-negative"
        ))),
    }
}

fn no_timer() -> VmError {
    VmError::new("cannot start a timer: the duration is out of range".into())
}

/// The arms of a `channel.select`: each operation is `Recv(channel)`
/// or `Send(channel, value)`.
fn select_arms(ops: List) -> Result<Vec<Arm>, VmError> {
    let arm = |op: Value| {
        let Value::Variant(name, fields) = op else {
            return None;
        };
        match <[Value; 2]>::try_from(fields) {
            Ok([channel, value]) if name.is(bv::SEND) => {
                Some(Arm::Send(Chan::take(&channel)?.clone(), value))
            }
            Ok(_) => None,
            Err(fields) => match fields.as_slice() {
                [channel] if name.is(bv::RECV) => Some(Arm::Recv(Chan::take(channel)?.clone())),
                _ => None,
            },
        }
    };
    ops.iter()
        .map(|op| arm(op).ok_or_else(|| unsound("channel.select", "ops")))
        .collect()
}

/// `channel.*`
pub(crate) mod channel {
    use super::*;

    /// `channel.new()` and `channel.new(capacity)`: with or without
    /// its parameter, which the typed form has no way to say, so it is
    /// written as the call the macro would write. (The conventions step
    /// gives it one form.)
    pub(crate) fn new(vm: &mut Vm, args: &[Value]) -> Called {
        let capacity = match args {
            [] => 0,
            [capacity] => i64::take(capacity)?,
            _ => return None,
        };
        Some(match usize::try_from(capacity) {
            Ok(capacity) => {
                let id = vm.next_channel_id();
                Ok(Step::Done(Value::Channel(Channel::new(id, capacity))))
            }
            Err(_) => Err(VmError::new(
                "channel.new capacity must be a non-negative integer".into(),
            )),
        })
    }

    builtins! {
        fn send(vm, ch: Chan, value: &Value) -> Result<Step, VmError> {
            let id = ch.id();
            let value = match ch.try_send(value.clone(), vm.scheduler().wake()) {
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

        fn receive(vm, ch: Chan) -> Step {
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Step::Done(message(value)),
                TryReceive::Closed(_) => return Step::Done(closed()),
                TryReceive::Empty => {}
            }
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]);
            vm.park("channel.receive", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => message(value),
                    _ => closed(),
                }))
            })
        }

        fn close(vm, ch: Chan) {
            ch.close(Close::default(), vm.scheduler().wake());
        }

        fn try_send(vm, ch: Chan, value: &Value) -> bool {
            matches!(ch.try_send(value.clone(), vm.scheduler().wake()), TrySend::Sent)
        }

        fn try_receive(vm, ch: Chan) -> Value {
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => message(value),
                TryReceive::Empty => Value::variant(bv::EMPTY, Vec::new()),
                TryReceive::Closed(_) => closed(),
            }
        }

        fn select(vm, ops: List) -> Result<Step, VmError> {
            let arms = select_arms(ops)?;
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

        //   * Ok(value)            — a value was delivered within the
        //                            timeout. A value that is there wins
        //                            over a timeout that has passed, even
        //                            at duration zero.
        //   * Err(ChannelClosed)   — the channel is closed and empty.
        //   * Err(ChannelTimeout)  — the timeout elapsed with no value
        //                            and no close. A positive duration
        //                            below a millisecond waits one.
        fn recv_timeout(vm, ch: Chan, dur: time::Duration) -> Result<Step, VmError> {
            // A negative duration is an error even when a value is
            // there.
            let dur = span("channel.recv_timeout", dur)?;
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => {
                    return Ok(Step::Done(Value::variant(bv::OK, vec![value])));
                }
                TryReceive::Closed(_) => return Ok(Step::Done(channel_err(bv::CHANNEL_CLOSED))),
                TryReceive::Empty => {}
            }
            if dur.is_zero() {
                return Ok(Step::Done(channel_err(bv::CHANNEL_TIMEOUT)));
            }
            let millis = (dur.as_nanos() as u64).div_ceil(1_000_000).max(1);
            let deadline = vm
                .runtime
                .io
                .deadline_after(Duration::from_millis(millis))
                .ok_or_else(no_timer)?;
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]).deadline(Some(deadline));
            Ok(vm.park("channel.recv_timeout", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => Value::variant(bv::OK, vec![value]),
                    Fired::Arm(..) => channel_err(bv::CHANNEL_CLOSED),
                    Fired::Deadline => channel_err(bv::CHANNEL_TIMEOUT),
                }))
            }))
        }

        fn timeout(vm, ms: i64) -> Result<Value, VmError> {
            let Ok(ms) = u64::try_from(ms) else {
                return Err(VmError::new(
                    "channel.timeout duration must be non-negative".into(),
                ));
            };
            let deadline = vm
                .runtime
                .io
                .deadline_after(Duration::from_millis(ms))
                .ok_or_else(no_timer)?;
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::new(failure));
            }
            // Nothing is ever sent on it: the timer closes it.
            let ch = Channel::new(vm.next_channel_id(), 1);
            vm.scheduler().close_at(deadline, ch.clone());
            Ok(Value::Channel(ch))
        }

        fn each(ch: Chan, f: &Value) -> Step {
            Step::Run(Box::new(Each {
                ch: ch.clone(),
                callback: f.clone(),
                state: EachState::Take,
            }))
        }
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
        vm.scheduler().current_owner(),
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

/// The function a task is to run, the argument of `name`: a function
/// of silt.
fn task_fn<'a>(name: &str, f: &'a Value) -> Result<&'a Arc<crate::bytecode::VmClosure>, VmError> {
    match f {
        Value::VmClosure(closure) => Ok(closure),
        _ => Err(VmError::new(format!("{name} requires a function argument"))),
    }
}

/// `task.*`
pub(crate) mod task {
    use super::*;

    builtins! {
        fn deadline(dur: time::Duration, f: &Value) -> Result<Step, VmError> {
            Ok(Step::Run(Box::new(Deadline {
                after: span("task.deadline", dur)?,
                callback: Some(f.clone()),
                outer: None,
            })))
        }

        fn spawn(vm, f: &Value) -> Result<Value, VmError> {
            spawn_with_deadline(vm, task_fn("task.spawn", f)?, None)
        }

        fn join(vm, handle: Handle) -> Result<Step, VmError> {
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

        // The task runs no further slice, and a wait of it ends
        // without taking or sending anything. Cancelling a task
        // handles it: if it had failed before, its failure is
        // dismissed and not reported as an unjoined failure. A join
        // still raises that failure, because the handle keeps the
        // result that came first.
        fn cancel(vm, handle: Handle) {
            vm.scheduler().cancel(handle);
        }

        // A task that runs with a scoped wall-clock deadline. Equivalent
        // to `task.spawn { -> task.deadline(dur, fn) }` minus the
        // closure-wrapping boilerplate.
        fn spawn_until(vm, dur: time::Duration, f: &Value) -> Result<Value, VmError> {
            let after = span("task.spawn_until", dur)?;
            let closure = task_fn("task.spawn_until", f)?;
            let deadline = vm.runtime.io.deadline_after(after);
            spawn_with_deadline(vm, closure, deadline)
        }
    }
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
