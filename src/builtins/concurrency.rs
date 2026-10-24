//! Concurrency builtin functions (`channel.*`, `task.*`).
//!
//! A function that cannot go on says what it waits for (a [`Wait`]) and
//! is resumed with how the wait ended (`Vm::park`). Whoever makes an
//! operation possible completes it, so a function that was woken has
//! its result and never tries again.

use std::sync::Arc;
use std::time::Duration;

use super::time;
use super::typed::{Arg, Chan, Handle, List, builtins, unsound};
use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Native, Step, Vm, VmError};

/// A send on a closed channel is a runtime error, whichever function
/// was to send: `channel.send`, `channel.try_send`, or a
/// `channel.select` with a `Send` arm on the channel. This is the
/// error, which names the channel.
fn closed_channel_send_err(id: usize) -> VmError {
    VmError::new(format!("send on closed channel {id}"))
}

fn message(value: Value) -> Value {
    Value::variant(bv::MESSAGE, [value])
}

/// What `channel.receive` and a `Recv` arm of `channel.select` give:
/// `Some(value)`, or `None` once the channel is closed and empty.
fn received(value: Option<Value>) -> Value {
    match value {
        Some(value) => Value::variant(bv::SOME, [value]),
        None => Value::variant(bv::NONE, []),
    }
}

fn closed() -> Value {
    Value::variant(bv::CLOSED, [])
}

fn empty() -> Value {
    Value::variant(bv::EMPTY, [])
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

/// The time a timer of `channel.*` runs for a duration: whole
/// milliseconds, and one for a positive duration below a millisecond.
fn timer_span(dur: Duration) -> Duration {
    match dur.as_nanos() as u64 {
        0 => Duration::ZERO,
        nanos => Duration::from_millis(nanos.div_ceil(1_000_000)),
    }
}

/// The arms of a `channel.select`: each operation is `Recv(channel)`
/// or `Send(channel, value)`.
fn select_arms(ops: List) -> Result<Vec<Arm>, VmError> {
    let arm = |op: Value| {
        let Value::Variant(variant) = op else {
            return None;
        };
        match variant.fields() {
            [channel, value] if variant.is(bv::SEND) => {
                Some(Arm::Send(Chan::take(channel)?.clone(), value.clone()))
            }
            [channel] if variant.is(bv::RECV) => Some(Arm::Recv(Chan::take(channel)?.clone())),
            _ => None,
        }
    };
    ops.iter()
        .map(|op| arm(op).ok_or_else(|| unsound("channel.select", "ops")))
        .collect()
}

/// `channel.*`
pub(crate) mod channel {
    use super::*;

    builtins! {
        fn new(vm, capacity: i64) -> Result<Value, VmError> {
            match usize::try_from(capacity) {
                // The buffer is a collection: it holds no more than a
                // list may.
                Ok(capacity) if capacity > crate::value::MAX_COLLECTION_LEN => {
                    Err(VmError::new(format!(
                        "channel.new: capacity exceeds maximum size of {} elements",
                        crate::value::MAX_COLLECTION_LEN
                    )))
                }
                Ok(capacity) => Ok(Value::Channel(Channel::new(vm.next_channel_id(), capacity))),
                Err(_) => Err(VmError::new(
                    "channel.new capacity must be a non-negative integer".into(),
                )),
            }
        }

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
            // A task that was cancelled, or whose time is over, takes
            // nothing new: not what is there either.
            if vm.takes_nothing_new() {
                return vm.end_here("channel.receive");
            }
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Step::Done(received(Some(value))),
                TryReceive::Closed(_) => return Step::Done(received(None)),
                TryReceive::Empty => {}
            }
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]);
            vm.park("channel.receive", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => received(Some(value)),
                    _ => received(None),
                }))
            })
        }

        fn close(vm, ch: Chan) {
            ch.close(Close::default(), vm.scheduler().wake());
        }

        // `true` if the value was sent, `false` if there is no room and
        // no receiver for it now.
        fn try_send(vm, ch: Chan, value: &Value) -> Result<bool, VmError> {
            match ch.try_send(value.clone(), vm.scheduler().wake()) {
                TrySend::Sent => Ok(true),
                TrySend::Full(_) => Ok(false),
                TrySend::Closed(_) => Err(closed_channel_send_err(ch.id())),
            }
        }

        fn try_receive(vm, ch: Chan) -> Step {
            if vm.takes_nothing_new() {
                return vm.end_here("channel.try_receive");
            }
            Step::Done(match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => message(value),
                TryReceive::Empty => empty(),
                TryReceive::Closed(_) => closed(),
            })
        }

        fn select(vm, ops: List) -> Result<Step, VmError> {
            let arms = select_arms(ops)?;
            if arms.is_empty() {
                return Err(VmError::new(
                    "channel.select requires at least one operation".into(),
                ));
            }
            // A `Send` arm on a closed channel is the error of a send
            // on it, whatever the other arms could do.
            if let Some(closed) = arms.iter().find_map(|arm| match arm {
                Arm::Send(ch, _) if ch.is_closed() => Some(ch.id()),
                _ => None,
            }) {
                return Err(closed_channel_send_err(closed));
            }
            // Each arm's channel, and whether the arm sends.
            let channels: Vec<(Arc<Channel>, bool)> = arms
                .iter()
                .map(|arm| match arm {
                    Arm::Recv(ch) => (ch.clone(), false),
                    Arm::Send(ch, _) => (ch.clone(), true),
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
                let (channel, sends) = &channels[arm];
                let ch = Value::Channel(channel.clone());
                Ok(Step::Done(match outcome {
                    Outcome::Received(value) => {
                        Value::variant(bv::RECEIVED, [ch, received(Some(value))])
                    }
                    Outcome::Sent => Value::variant(bv::SENT, [ch]),
                    // The channel was closed while the arm waited to send.
                    Outcome::Closed(_) if *sends => {
                        return Err(closed_channel_send_err(channel.id()));
                    }
                    Outcome::Closed(_) | Outcome::Done => {
                        Value::variant(bv::RECEIVED, [ch, received(None)])
                    }
                }))
            }))
        }

        // `Message(value)` if a value is there or comes within the
        // duration: a value that is there wins over a duration that
        // has passed, even at duration zero. `Closed` if the channel
        // is closed and empty. `Empty` if the duration passed with no
        // value and no close.
        fn receive_timeout(vm, ch: Chan, dur: time::Duration) -> Result<Step, VmError> {
            // A negative duration is an error even when a value is
            // there.
            let dur = span("channel.receive_timeout", dur)?;
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("channel.receive_timeout"));
            }
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Ok(Step::Done(message(value))),
                TryReceive::Closed(_) => return Ok(Step::Done(closed())),
                TryReceive::Empty => {}
            }
            if dur.is_zero() {
                return Ok(Step::Done(empty()));
            }
            let deadline = vm
                .runtime
                .io
                .deadline_after(timer_span(dur))
                .ok_or_else(no_timer)?;
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]).deadline(Some(deadline));
            Ok(vm.park("channel.receive_timeout", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => message(value),
                    Fired::Arm(..) => closed(),
                    Fired::Deadline => empty(),
                }))
            }))
        }

        fn timeout(vm, dur: time::Duration) -> Result<Value, VmError> {
            let dur = span("channel.timeout", dur)?;
            let deadline = vm
                .runtime
                .io
                .deadline_after(timer_span(dur))
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
        // The first take and every later one. A task that was
        // cancelled, or whose time is over, takes nothing new: the
        // message it was called with last is the last.
        if vm.takes_nothing_new() {
            self.state = EachState::Waited;
            return Ok(Step::Park(Vm::no_wait()));
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

/// Start `f` as a task of whoever spawns it: the owner of the task
/// that runs this, or the owner the front end set for the program.
/// With `deadline` the task is over at that reading of the clock
/// (`TaskHandle::deadline`). The handle of the task; an error of
/// `scheduler.submit` as it is.
fn start(vm: &mut Vm, f: &Value, deadline: Option<Duration>) -> Result<Arc<TaskHandle>, VmError> {
    let task_id = vm.next_task_id();
    let handle = TaskHandle::with_owner(task_id, vm.scheduler().current_owner());
    let handle = Arc::new(match deadline {
        // A task whose time is its caller's goes on with the caller's
        // count of the steps run past that time: the work past a
        // deadline is bounded in total, however many deadlines the
        // function calls when its time is over.
        Some(deadline) => {
            let callers = vm.deadline.filter(|own| deadline >= *own);
            handle.with_deadline(deadline, callers.and(vm.late.clone()))
        }
        None => handle,
    });

    let mut child_vm = vm.spawn_child();
    match f {
        Value::VmClosure(closure) => child_vm.start_task(closure.clone()),
        // Any other function (a builtin, a host function): the task
        // is a frame that calls it.
        other => child_vm.push_native_frame(Box::new(Body(Some(other.clone())))),
    }
    child_vm.spawned = true;
    vm.scheduler()
        .submit(task_id, child_vm, handle.clone())
        .map_err(VmError::new)?;

    Ok(handle)
}

/// The body of a task that is no function of silt's own code
/// (`task.spawn(uuid.v4)`): the function, until it is called. Its value
/// is the task's.
struct Body(Option<Value>);

impl Native for Body {
    fn name(&self) -> &str {
        "task.spawn"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        match self.0.take() {
            Some(callee) => Ok(vm.call(callee, [])),
            None => Ok(Step::Done(input)),
        }
    }
}

/// What `TaskError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn task_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Timeout", []) => "the function was ended by its deadline".to_string(),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

/// `task.deadline(dur, f)`: `Ok(f())`, or `Err(Timeout)` if `f` was
/// ended because `dur` had passed.
///
/// The deadline is the function's task's, and it is decided in one
/// place. `f` runs as a task of its own that is over at a set reading
/// of the clock (`TaskHandle::deadline`), and the scheduler ends it
/// there, never anybody from outside:
///
/// - Every wait of the task has that time beside its arms, in one
///   claim (`runtime::sync::Wait::task_deadline`): the wait ends by an
///   arm (the value is taken, the send is done, the operation's result
///   is the task's) or by the time (nothing is taken, nothing is sent,
///   an operation in flight is given up), never by both. A wait that
///   the time ends, or that begins when the time is over, ends the
///   task there.
/// - A task past its time takes nothing new (`Vm::takes_nothing_new`,
///   as a cancelled task): an operation that would take ends it, and
///   what it holds it may still give.
/// - The work of a task past its time is bounded: every step it runs
///   in a slice that began when the time was over is counted, and
///   when a standard slice of them is used up the task is ended, after
///   that step or at the entry of the wait it began (no arm is tried).
///
/// So this frame only waits for the task to end, however long after
/// the time that is (a slice and those steps at most), and reports how
/// it did: it returned, it was ended by its deadline, or it failed. A
/// function that took a value in time has the steps to return it, and
/// its `Ok` is never thrown away for the clock's sake.
///
/// The task's time is no later than the caller's own, if the caller
/// has one (a deadline inside a deadline): the outer time ends the
/// inner function by the same rule, with no cancel. A task that has
/// its caller's time goes on with its caller's count of steps
/// (`TaskHandle::late`), so that the work past a deadline is bounded
/// in total: a caller that gets its function's `Ok` when the time of
/// both is over has what the function left of the steps. (A caller
/// whose time is over when it comes to the call starts no function:
/// the call ends it.)
///
/// The frame owns the task: if the frame is dropped while it waits
/// (the caller was cancelled by `task.cancel`, or the program ended),
/// the task is cancelled too. That is the rule of a cancel, and the
/// only cancel there is here.
struct Deadline {
    task: Arc<TaskHandle>,
    state: DeadlineState,
}

enum DeadlineState {
    /// The task is started and the frame has not parked.
    Started,
    /// The frame waits for the task to end.
    Waiting,
    /// The task was ended at a time that is the caller's own too: the
    /// caller's task is over, and ends in the wait it has entered.
    Over,
}

impl Native for Deadline {
    fn name(&self) -> &str {
        "task.deadline"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        let timeout = || {
            let timeout = Value::variant(bv::TASK_TIMEOUT, []);
            Ok(Step::Done(Value::variant(bv::ERR, [timeout])))
        };
        match self.state {
            DeadlineState::Started => {
                self.state = DeadlineState::Waiting;
                // Not ended by the caller's own deadline: the task's
                // time is no later, and it ends by itself. Were this
                // wait ended first, a value the function took in time
                // would be lost with its `Ok`.
                let wait = Wait::new(vec![Arm::Cell(self.task.done())]).unbounded();
                return Ok(Step::Park(wait));
            }
            DeadlineState::Waiting => {}
            DeadlineState::Over => return timeout(),
        }
        vm.woken()?;
        if self.task.is_expired() {
            // A clock that has panicked ended every wait that was
            // pending on it: that is the clock's failure, not a time
            // that passed.
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::new(failure));
            }
            // The time that ended the function is the caller's own
            // (an outer deadline's): the caller is over as well, and a
            // wait that begins now ends its task.
            let inherited = self.task.deadline().zip(vm.deadline);
            if inherited.is_some_and(|(its, own)| its >= own) {
                self.state = DeadlineState::Over;
                return Ok(Step::Park(Wait::new(vec![])));
            }
            return timeout();
        }
        let result = self.task.try_get().ok_or_else(|| {
            VmError::new("internal VM error: a deadline's wait ended before its task".into())
        })?;
        // The function's failure is the caller's, as if the caller had
        // called the function itself: where it happened, and below the
        // function's own calls the calls that led to this
        // `task.deadline`. It is raised whatever the clock says.
        self.task.mark_joined();
        match result {
            Ok(value) => Ok(Step::Done(Value::variant(bv::OK, [value]))),
            Err(mut failure) => {
                let here = vm.enrich_error(VmError::new(String::new()));
                failure.call_stack.extend(here.call_stack);
                Err(failure)
            }
        }
    }

    fn abandon(&mut self, vm: &mut Vm) {
        vm.scheduler().cancel(&self.task);
    }
}

/// The result of a task for the one that joins it.
fn joined(vm: &Vm, handle: &TaskHandle, result: Result<Value, VmError>) -> Result<Step, VmError> {
    // The joiner has the result now. If the task failed, the error is
    // the joiner's to handle and is not reported as an unjoined
    // failure.
    handle.mark_joined();
    match result {
        Ok(value) => Ok(Step::Done(value)),
        // As for `task.deadline`: where the task failed, and below
        // the task's own calls the calls that led to this
        // `task.join`. A failure that has no place (a cancelled task)
        // is placed at the join.
        Err(mut inner) => {
            inner.message = format!("joined task failed: {}", inner.message);
            let here = vm.enrich_error(VmError::new(String::new()));
            if inner.span.is_none() {
                inner.span = here.span;
            }
            inner.call_stack.extend(here.call_stack);
            Err(inner)
        }
    }
}

/// `task.*`
pub(crate) mod task {
    use super::*;

    builtins! {
        fn deadline(vm, dur: time::Duration, f: &Value) -> Result<Step, VmError> {
            // The call is a wait for the function, and a task that
            // was cancelled or whose time is over begins none: it is
            // ended here, before a task is started that would take in
            // its place.
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.deadline"));
            }
            // A duration of zero or less is a time that is over now:
            // the function runs to its first wait that has to wait,
            // or until the steps of a late task are used up.
            let after = Duration::from_nanos(u64::try_from(dur.0).unwrap_or(0));
            let until = vm.runtime.io.deadline_after(after).ok_or_else(no_timer)?;
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::new(failure));
            }
            // No later than the caller's own time.
            let until = vm.deadline.map_or(until, |own| own.min(until));
            let task = start(vm, f, Some(until))?;
            Ok(Step::Run(Box::new(Deadline { task, state: DeadlineState::Started })))
        }

        fn spawn(vm, f: &Value) -> Result<Value, VmError> {
            Ok(Value::Handle(start(vm, f, None)?))
        }

        fn join(vm, handle: Handle) -> Result<Step, VmError> {
            if let Some(result) = handle.try_get() {
                return joined(vm, handle, result);
            }
            let handle = handle.clone();
            let wait = Wait::new(vec![Arm::Cell(handle.done())]);
            Ok(vm.park("task.join", wait, move |vm, _| {
                let result = handle.try_get().ok_or_else(|| {
                    VmError::new("internal VM error: a join ended before its task".into())
                })?;
                joined(vm, &handle, result)
            }))
        }

        // A wait of the task ends without taking or sending
        // anything, and the task with it; a task that is not waiting
        // goes on for a bounded number of steps, to its next wait or
        // its next take at the latest (`Scheduler::cancel`).
        // Cancelling a task
        // handles it: if it had failed before, its failure is
        // dismissed and not reported as an unjoined failure. A join
        // still raises that failure, because the handle keeps the
        // result that came first.
        fn cancel(vm, handle: Handle) {
            vm.scheduler().cancel(handle);
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
