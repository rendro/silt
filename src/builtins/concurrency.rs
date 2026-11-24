//! Concurrency builtin functions (`channel.*`, `task.*`).
//!
//! A function that cannot go on says what it waits for (a [`Wait`]) and
//! is resumed with how the wait ended (`Vm::park`). Whoever makes an
//! operation possible completes it, so a function that was woken has
//! its result and never tries again.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use super::time;
use super::typed::{Arg, Chan, List, builtins, unsound};
use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::runtime::together::{Role, Together};
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

/// The time a `Duration` argument is: a negative one is no time, as
/// for `time.sleep` and `task.deadline`.
fn span(duration: time::Duration) -> Duration {
    Duration::from_nanos(u64::try_from(duration.0).unwrap_or(0))
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

/// A send into the output of a stream's stage is refused: a stream's
/// channel is only read. (It went ahead of what the stage gives, or,
/// where the stage had ended, was refused as a send on a closed
/// channel.)
fn refuse_a_stream(ch: &Channel) -> Result<(), VmError> {
    match ch.fed_by() {
        Some(name) => Err(VmError::new(format!(
            "cannot send into a stream: channel {} is the output of {name}",
            ch.id()
        ))),
        None => Ok(()),
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
            refuse_a_stream(ch)?;
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

        fn receive(vm, ch: Chan) -> Result<Step, VmError> {
            // A task that was cancelled, or whose time is over, takes
            // nothing new: not what is there either.
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("channel.receive"));
            }
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Ok(Step::Done(received(Some(value)))),
                TryReceive::Closed(close) => {
                    raise_failure(close)?;
                    return Ok(Step::Done(received(None)));
                }
                TryReceive::Empty => {}
            }
            let wait = Wait::new(vec![Arm::Recv(ch.clone())]);
            Ok(vm.park("channel.receive", wait, |_, fired| {
                Ok(Step::Done(match fired {
                    Fired::Arm(_, Outcome::Received(value)) => received(Some(value)),
                    Fired::Arm(_, Outcome::Closed(close)) => {
                        raise_failure(close)?;
                        received(None)
                    }
                    _ => received(None),
                }))
            }))
        }

        // A stream that its reader closes is given up as every other
        // cut gives it up: the stages that feed it stop at once, and
        // take nothing that comes after. A channel the program made is
        // only closed: its other readers get what is buffered.
        fn close(vm, ch: Chan) {
            let wake = vm.scheduler().wake();
            ch.close(Close::default(), wake);
            ch.abandon(wake);
        }

        // `true` if the value was sent, `false` if there is no room and
        // no receiver for it now.
        fn try_send(vm, ch: Chan, value: &Value) -> Result<bool, VmError> {
            refuse_a_stream(ch)?;
            match ch.try_send(value.clone(), vm.scheduler().wake()) {
                TrySend::Sent => Ok(true),
                TrySend::Full(_) => Ok(false),
                TrySend::Closed(_) => Err(closed_channel_send_err(ch.id())),
            }
        }

        fn try_receive(vm, ch: Chan) -> Result<Step, VmError> {
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("channel.try_receive"));
            }
            Ok(Step::Done(match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => message(value),
                TryReceive::Empty => empty(),
                TryReceive::Closed(close) => {
                    raise_failure(close)?;
                    closed()
                }
            }))
        }

        fn select(vm, ops: List) -> Result<Step, VmError> {
            let arms = select_arms(ops)?;
            if arms.is_empty() {
                return Err(VmError::new(
                    "channel.select requires at least one operation".into(),
                ));
            }
            // A `Send` arm on a stream's channel, or on a closed one, is
            // the error of a send on it, whatever the other arms could do.
            for arm in &arms {
                if let Arm::Send(ch, _) = arm {
                    refuse_a_stream(ch)?;
                }
            }
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
                    Arm::Cell(_) | Arm::Demand(_) => unreachable!("a select has channel arms"),
                })
                .collect();
            // The wait completes the first arm that is possible, from
            // an arm picked at random, or the first that becomes so.
            let first = select_start_index(arms.len());
            let wait = Wait::new(arms).first(first);
            Ok(vm.park("channel.select", wait, move |_, fired| {
                let Fired::Arm(arm, outcome) = fired else {
                    return Err(VmError::internal("a select ended without an arm"));
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
                    Outcome::Closed(close) => {
                        raise_failure(close)?;
                        Value::variant(bv::RECEIVED, [ch, received(None)])
                    }
                    Outcome::Done | Outcome::Asked => Value::variant(bv::RECEIVED, [ch, received(None)]),
                }))
            }))
        }

        // `Message(value)` if a value is there or comes within the
        // duration: a value that is there wins over a duration that
        // has passed, even at duration zero. `Closed` if the channel
        // is closed and empty. `Empty` if the duration passed with no
        // value and no close.
        fn receive_timeout(vm, ch: Chan, dur: time::Duration) -> Result<Step, VmError> {
            let dur = span(dur);
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("channel.receive_timeout"));
            }
            match ch.try_receive(vm.scheduler().wake()) {
                TryReceive::Value(value) => return Ok(Step::Done(message(value))),
                TryReceive::Closed(close) => {
                    raise_failure(close)?;
                    return Ok(Step::Done(closed()));
                }
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
                    Fired::Arm(_, Outcome::Closed(close)) => {
                        raise_failure(close)?;
                        closed()
                    }
                    Fired::Arm(..) => closed(),
                    Fired::Deadline => empty(),
                }))
            }))
        }

        fn timeout(vm, dur: time::Duration) -> Result<Value, VmError> {
            let dur = span(dur);
            let deadline = vm
                .runtime
                .io
                .deadline_after(timer_span(dur))
                .ok_or_else(no_timer)?;
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::ending(failure));
            }
            // Nothing is ever sent on it: the timer closes it.
            let ch = Channel::new(vm.next_channel_id(), 1);
            vm.scheduler().close_at(deadline, &ch);
            Ok(Value::Channel(ch))
        }

        fn each(ch: Chan, f: &Value) -> Step {
            Step::Run(Box::new(Each {
                ch: ch.clone(),
                callback: f.clone(),
                state: EachState::Start,
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
    /// Nothing: the first message is to be taken.
    Start,
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

/// A receive found the channel closed and empty. If a stream stage
/// fed it and failed, every read of it raises that failure.
fn raise_failure(close: Close) -> Result<(), VmError> {
    close.raise()
}

/// The end of a `channel.each`: the channel is closed and empty.
fn ended(close: Close) -> Result<Step, VmError> {
    raise_failure(close)?;
    Ok(Step::Done(Value::Unit))
}

impl Native for Each {
    fn name(&self) -> &str {
        "channel.each"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        let first = matches!(self.state, EachState::Start);
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
            EachState::Start | EachState::Take => {}
        }
        // The first take and every later one. A task that was
        // cancelled, or whose time is over, takes nothing new: the
        // message it was called with last is the last.
        if vm.takes_nothing_new() {
            self.state = EachState::Waited;
            return Ok(Step::Park(Vm::no_wait()));
        }
        // It reads the channel to the end: what feeds it works ahead.
        if first {
            self.ch.drain(vm.scheduler().wake());
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

    /// It reads no further (its task is cancelled, its time is over,
    /// or an error passes through it): as a sink of `stream.*` does,
    /// it stops the stage that feeds the channel, if one does. A
    /// channel the program made is left to its other readers.
    fn abandon(&mut self, vm: &mut Vm) {
        self.ch.abandon(vm.scheduler().wake());
    }
}


/// What a task runs: a function, called with no argument, or a frame
/// of the runtime's own (a worker of `task.each` or `task.map`, a
/// piece of `task.first_ok`).
enum Work {
    Call(Value),
    Frame(Box<dyn Native>),
}

/// Start `work` as a task of whoever starts it, for the call `call`:
/// the owner of the task that runs this, or the owner the front end set
/// for the program. With `deadline` the task is over at that reading of
/// the clock (`TaskHandle::deadline`). A task that is `role` in the
/// call of `together`, if it is one. The handle of the task; an error
/// of `scheduler.submit` as it is.
fn start_in(
    vm: &mut Vm,
    call: &'static str,
    work: Work,
    deadline: Option<Duration>,
    together: Option<(&Arc<Together>, Role)>,
) -> Result<Arc<TaskHandle>, VmError> {
    let task_id = vm.next_task_id();
    let mut handle = TaskHandle::with_owner(task_id, vm.scheduler().current_owner())
        .started_at(vm.place_here())
        .for_call(call);
    if let Some((together, role)) = together {
        handle = handle.in_together(together.clone(), role);
    }
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
    match work {
        Work::Call(Value::VmClosure(closure)) => child_vm.start_task(closure),
        // Any other function (a builtin, a host function): the task
        // is a frame that calls it.
        Work::Call(other) => child_vm.push_native_frame(Box::new(Body(call, Some(other)))),
        Work::Frame(frame) => child_vm.push_native_frame(frame),
    }
    child_vm.spawned = true;
    vm.scheduler().submit(task_id, child_vm, handle.clone())?;

    Ok(handle)
}

/// The body of a task that is no function of silt's own code (a piece
/// `uuid.v4` of a `task.join`): the call that started it, and the
/// function, until it is called. Its value is the task's.
struct Body(&'static str, Option<Value>);

impl Native for Body {
    fn name(&self) -> &str {
        self.0
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        match self.1.take() {
            Some(callee) => Ok(vm.call(callee, [])),
            None => Ok(Step::Done(input)),
        }
    }
}

/// The body of a `task.background` with no helpers, called in place:
/// the function, until it is called. Its value is the call's.
struct InPlace(Option<Value>);

impl Native for InPlace {
    fn name(&self) -> &str {
        "task.background"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        match self.0.take() {
            Some(body) => Ok(vm.call(body, [])),
            None => Ok(Step::Done(input)),
        }
    }
}

/// `task.deadline(limit, work)`: `Some(work())`, or `None` if `work`
/// was ended because `limit` had passed. A failure of `work` is the
/// caller's.
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
/// its `Some` is never thrown away for the clock's sake.
///
/// The task's time is no later than the caller's own, if the caller
/// has one (a deadline inside a deadline): the outer time ends the
/// inner function by the same rule, with no cancel. A task that has
/// its caller's time goes on with its caller's count of steps
/// (`TaskHandle::late`), so that the work past a deadline is bounded
/// in total: a caller that gets its function's `Some` when the time of
/// both is over has what the function left of the steps. (A caller
/// whose time is over when it comes to the call starts no function:
/// the call ends it.)
///
/// The frame owns the task: if the frame is dropped while it waits
/// (the caller was stopped by a call around it, or the program ended),
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
    /// The caller was cancelled, and the cancel reached the task: the
    /// frame waits until the task's last step has run.
    Stopping,
    /// The task was ended at a time that is the caller's own too: the
    /// caller's task is over, and ends in the wait it has entered.
    Over,
}

impl Native for Deadline {
    fn name(&self) -> &str {
        "task.deadline"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        let timeout = || Ok(Step::Done(Value::variant(bv::NONE, [])));
        match self.state {
            DeadlineState::Started => {
                self.state = DeadlineState::Waiting;
                // Not ended by the caller's own deadline: the task's
                // time is no later, and it ends by itself. Were this
                // wait ended first, a value the function took in time
                // would be lost with its `Some`.
                // Nor by a cancel of the caller, which reaches the task.
                // And it ends when the task has ended without a result
                // too: one dropped with its owner's tasks (a test that
                // ended).
                let wait = Wait::new(vec![
                    Arm::Cell(self.task.done()),
                    Arm::Cell(self.task.stopped()),
                ])
                .unbounded()
                .stays();
                return Ok(Step::Park(wait));
            }
            DeadlineState::Waiting => {
                vm.woken()?;
                if !self.task.has_result() {
                    self.state = DeadlineState::Over;
                    return Ok(Step::Park(Wait::new(vec![])));
                }
                // A cancel of the caller gave the task its result at
                // once; the caller ends after the task's last step.
                let cancelled = vm
                    .cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst));
                if cancelled && !self.task.has_stopped() {
                    self.state = DeadlineState::Stopping;
                    let wait = Wait::new(vec![Arm::Cell(self.task.stopped())])
                        .unbounded()
                        .stays();
                    return Ok(Step::Park(wait));
                }
            }
            DeadlineState::Stopping => {
                vm.woken()?;
            }
            DeadlineState::Over => return timeout(),
        }
        // The task was stopped by the cancel that stopped the caller:
        // the caller ends here.
        if self.task.was_ended_by_cancel() && self.task.has_stopped() && vm.takes_nothing_new() {
            self.state = DeadlineState::Over;
            return Ok(Step::Park(Wait::new(vec![])));
        }
        if self.task.is_expired() {
            // A clock that has panicked ended every wait that was
            // pending on it: that is the clock's failure, not a time
            // that passed.
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::ending(failure));
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
        let result = self
            .task
            .try_get()
            .ok_or_else(|| VmError::internal("a deadline's wait ended before its task"))?;
        // The function's failure is the caller's, as if the caller had
        // called the function itself: where it happened, and below the
        // function's own calls the calls that led to this
        // `task.deadline`. It is raised whatever the clock says.
        self.task.mark_read();
        match result {
            Ok(value) => Ok(Step::Done(Value::variant(bv::SOME, [value]))),
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

/// `task.join(pieces)`, `task.background(helpers, body)`,
/// `task.each(work, at_once, f)`, `task.map(xs, at_once, f)` and
/// `task.first_ok(works)`: every function (a piece, a helper, the body,
/// a worker) runs as a task of its own, and the caller waits in this
/// frame until all of them have ended. The frame owns them.
///
/// - The first failure of one of them stops the others (as a cancel of
///   a task above them would) and is raised in the caller, as if the
///   caller had called the function itself: where it happened, and
///   below the function's own calls the calls that led to the call.
///   Failures that come while the others are stopped are notes under
///   it (`Together::take_failure`). No task of the call is ever
///   reported as a failure that nothing read.
/// - The end of the body of a `task.background` stops the helpers
///   that still run; its value is the call's. The first `Ok` of a
///   piece of `task.first_ok` stops the other pieces in the same way.
/// - The call returns, or raises, only when every task of it has
///   ended: no task of the call runs on.
/// - The tasks have the caller's time, if it has one (a deadline
///   around the call), and its count of late steps: that time ends
///   them by the rule that ends the caller, and then the caller, in
///   the wait it enters. A cancel of the caller, or its end, stops
///   them (`Joined::abandon`; a cancel reaches them as tasks it
///   spawned besides).
///
/// The frame wakes twice, whatever the number of tasks: when the call
/// is settled (a failure, the body's end, a first `Ok`, or the end of
/// every task), and when every task has ended (`runtime::together`).
struct Joined {
    /// `task.join`, `task.background`, `task.each`, `task.map` or
    /// `task.first_ok`.
    call: &'static str,
    together: Arc<Together>,
    /// Every task of the call: the pieces, the helpers and the body, or
    /// the workers.
    tasks: Vec<Arc<TaskHandle>>,
    gives: Gives,
    state: JoinedState,
}

/// What a call gives when every task of it has ended.
enum Gives {
    /// `()`: `task.join`, `task.each`.
    Unit,
    /// The value of the body: `task.background`. (Its task is known
    /// once it is started: `Joined::start` sets it.)
    Body(Option<Arc<TaskHandle>>),
    /// The values of `task.map`.
    Mapped(Arc<Mapping>),
    /// The first `Ok`, or every `Err`: `task.first_ok`.
    FirstOk(Arc<Race>),
}

impl Gives {
    /// The call's value, if its tasks gave it.
    fn value(&self) -> Option<Value> {
        match self {
            Gives::Unit => Some(Value::Unit),
            Gives::Body(body) => match body.as_ref()?.try_get() {
                Some(Ok(value)) => Some(value),
                _ => None,
            },
            Gives::Mapped(mapping) => mapping.values(),
            Gives::FirstOk(race) => race.result(),
        }
    }
}

enum JoinedState {
    /// The tasks are started and the frame has not parked.
    Started,
    /// The frame waits for the call to be settled.
    Settling,
    /// The frame waits for every task to end.
    Ending,
    /// The tasks were ended by a time that is the caller's own: the
    /// caller's task is over, and ends in the wait it has entered.
    Over,
}

impl Joined {
    /// Start a task for each of `works`, each what its role says, for
    /// the call `call` whose record is `together` (made for as many
    /// tasks); what the call gives when they have ended is `gives`.
    fn start(
        vm: &mut Vm,
        call: &'static str,
        together: Arc<Together>,
        works: Vec<(Work, Role)>,
        mut gives: Gives,
    ) -> Result<Step, VmError> {
        // The caller's own time, if it has one: a task of the call
        // goes on with the caller's count of late steps then.
        let deadline = vm.deadline;
        let mut tasks = Vec::with_capacity(works.len());
        for (work, role) in works {
            match start_in(vm, call, work, deadline, Some((&together, role))) {
                Ok(task) => {
                    if role == Role::Body {
                        gives = Gives::Body(Some(task.clone()));
                    }
                    tasks.push(task);
                }
                // (The task limit.) What was started is stopped.
                Err(error) => {
                    for task in &tasks {
                        vm.scheduler().stop_task(task);
                    }
                    return Err(error);
                }
            }
        }
        Ok(Step::Run(Box::new(Joined {
            call,
            together,
            tasks,
            gives,
            state: JoinedState::Started,
        })))
    }

    /// Stop every task that has not ended.
    fn stop_the_rest(&self, vm: &mut Vm) {
        for task in &self.tasks {
            if !task.has_stopped() {
                vm.scheduler().stop_task(task);
            }
        }
    }

    /// Every task has ended: the call's value, or its failure.
    fn finish(&mut self, vm: &mut Vm) -> Result<Step, VmError> {
        if let Some(mut failure) = self.together.take_failure() {
            let here = vm.enrich_error(VmError::new(String::new()));
            failure.call_stack.extend(here.call_stack);
            return Err(failure);
        }
        let expired = self.tasks.iter().any(|task| task.is_expired());
        // A clock that has panicked ended every wait that was pending
        // on it: that is the clock's failure, not a time that passed.
        if expired && let Some(failure) = vm.runtime.io.clock_failure() {
            return Err(VmError::ending(failure));
        }
        match &self.gives {
            // Work that ended by the time gives nothing.
            Gives::Unit if expired => {}
            // A value given in time is the call's, whatever ended the
            // other tasks: the body's, the values of every call of a
            // map's function, a first `Ok`.
            gives => match gives.value() {
                Some(value) => return Ok(Step::Done(value)),
                None if expired => {}
                // The caller was stopped (a cancel reached it and its
                // tasks): it ends here, now that they have ended.
                None if vm.takes_nothing_new() => {}
                None => {
                    return Err(VmError::internal(format!(
                        "the tasks of a {} ended without its value",
                        self.call
                    )));
                }
            },
        }
        // The tasks had the caller's time, and it is over, or the
        // caller was stopped with them: a wait that begins now ends
        // its task.
        self.state = JoinedState::Over;
        Ok(Step::Park(Wait::new(vec![])))
    }
}

impl Native for Joined {
    fn name(&self) -> &str {
        self.call
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        // The waits are not ended by the caller's own deadline, nor by
        // a cancel of the caller: the tasks have that time, and a
        // cancel that reaches the caller reaches them (they are tasks
        // it spawned), so they end by it themselves. The caller ends
        // after them: a stop waits for every task it reaches.
        match self.state {
            JoinedState::Started => {
                self.state = JoinedState::Settling;
                let wait = Wait::new(vec![Arm::Cell(self.together.settled())])
                    .unbounded()
                    .stays();
                Ok(Step::Park(wait))
            }
            JoinedState::Settling => {
                vm.woken()?;
                self.stop_the_rest(vm);
                self.state = JoinedState::Ending;
                let wait = Wait::new(vec![Arm::Cell(self.together.ended())])
                    .unbounded()
                    .stays();
                Ok(Step::Park(wait))
            }
            JoinedState::Ending => {
                vm.woken()?;
                self.finish(vm)
            }
            JoinedState::Over => Err(VmError::internal("a task went on whose time was over")),
        }
    }

    /// The caller was cancelled, or ended, or an error passes through
    /// the frame: what still runs of the call is stopped.
    fn abandon(&mut self, vm: &mut Vm) {
        self.stop_the_rest(vm);
    }
}

/// The number of workers `at_once` asks `call` for: 1 or more.
fn workers(call: &str, at_once: i64) -> Result<usize, VmError> {
    match usize::try_from(at_once) {
        Ok(workers) if workers >= 1 => Ok(workers),
        _ => Err(VmError::new(format!(
            "{call}: at_once is {at_once}, and must be 1 or more"
        ))),
    }
}

/// The list of a `task.map` and what its workers made of it.
struct Mapping {
    xs: Vec<Value>,
    f: Value,
    /// The place in `xs` of the next item a worker takes.
    next: AtomicUsize,
    /// The value of `f` for each item, once it has returned.
    values: Mutex<Vec<Option<Value>>>,
}

impl Mapping {
    /// The list of the values, in the order of the items, if every
    /// call of `f` has returned.
    fn values(&self) -> Option<Value> {
        let values = self
            .values
            .lock()
            .iter()
            .cloned()
            .collect::<Option<Vec<_>>>()?;
        Some(Value::list(values))
    }
}

/// One of the tasks of a `task.map`: it takes the next item of the
/// list and calls the function with it, until no item is left.
struct MapWorker {
    mapping: Arc<Mapping>,
    /// The place of the item whose call returns next.
    at: Option<usize>,
    /// The worker was stopped: it took no further item.
    ended: bool,
}

impl Native for MapWorker {
    fn name(&self) -> &str {
        "task.map"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.ended {
            return Ok(Step::Done(Value::Unit));
        }
        if let Some(at) = self.at.take() {
            self.mapping.values.lock()[at] = Some(input);
        }
        // A worker that was stopped (a failure of another, a stop from
        // outside) starts no further call: it ends here.
        if vm.takes_nothing_new() {
            self.ended = true;
            return Ok(Step::Park(Vm::no_wait()));
        }
        let at = self.mapping.next.fetch_add(1, Ordering::SeqCst);
        let Some(item) = self.mapping.xs.get(at) else {
            return Ok(Step::Done(Value::Unit));
        };
        self.at = Some(at);
        Ok(vm.call(self.mapping.f.clone(), [item.clone()]))
    }
}

/// The pieces of a `task.first_ok` and what they gave.
struct Race {
    /// The value of the first `Ok`.
    won: Mutex<Option<Value>>,
    /// The error of each piece that gave `Err`, in the order of the
    /// pieces.
    errs: Mutex<Vec<Option<Value>>>,
}

impl Race {
    /// `Ok` of the first `Ok`, or `Err` of every error if every piece
    /// gave `Err`.
    fn result(&self) -> Option<Value> {
        if let Some(won) = self.won.lock().clone() {
            return Some(Value::variant(bv::OK, [won]));
        }
        let errs = self
            .errs
            .lock()
            .iter()
            .cloned()
            .collect::<Option<Vec<_>>>()?;
        Some(Value::variant(bv::ERR, [Value::list(errs)]))
    }
}

/// A piece of a `task.first_ok`: it calls its function and keeps what
/// it gave. The first `Ok` settles the call, which stops the others.
struct Racer {
    /// The function, until it is called.
    work: Option<Value>,
    /// The piece's place in the list.
    at: usize,
    race: Arc<Race>,
    together: Arc<Together>,
}

impl Native for Racer {
    fn name(&self) -> &str {
        "task.first_ok"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(work) = self.work.take() {
            return Ok(vm.call(work, []));
        }
        match &input {
            Value::Variant(result) if result.is(bv::OK) => {
                let [value] = result.fields() else {
                    return Err(unsound("task.first_ok", "works"));
                };
                let mut won = self.race.won.lock();
                if won.is_none() {
                    *won = Some(value.clone());
                    drop(won);
                    self.together.settle(vm.scheduler().wake());
                }
            }
            Value::Variant(result) if result.is(bv::ERR) => {
                let [error] = result.fields() else {
                    return Err(unsound("task.first_ok", "works"));
                };
                self.race.errs.lock()[self.at] = Some(error.clone());
            }
            _ => return Err(unsound("task.first_ok", "works")),
        }
        Ok(Step::Done(Value::Unit))
    }
}

/// `task.*`
pub(crate) mod task {
    use super::*;

    builtins! {
        fn deadline(vm, limit: time::Duration, work: &Value) -> Result<Step, VmError> {
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
            let after = Duration::from_nanos(u64::try_from(limit.0).unwrap_or(0));
            let until = vm.runtime.io.deadline_after(after).ok_or_else(no_timer)?;
            if let Some(failure) = vm.runtime.io.clock_failure() {
                return Err(VmError::ending(failure));
            }
            // No later than the caller's own time.
            let until = vm.deadline.map_or(until, |own| own.min(until));
            let task = start_in(vm, "task.deadline", Work::Call(work.clone()), Some(until), None)?;
            Ok(Step::Run(Box::new(Deadline { task, state: DeadlineState::Started })))
        }

        fn background(vm, helpers: List, body: &Value) -> Result<Step, VmError> {
            // With no helper it is the body, called in place.
            if helpers.is_empty() {
                return Ok(Step::Run(Box::new(InPlace(Some(body.clone())))));
            }
            // A task that was cancelled or whose time is over starts
            // none: it is ended here.
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.background"));
            }
            let mut works: Vec<(Work, Role)> =
                helpers.iter().map(|helper| (Work::Call(helper), Role::Helper)).collect();
            works.push((Work::Call(body.clone()), Role::Body));
            let together = Together::new(
                "task.background",
                "the body and helpers of its `task.background`",
                works.len(),
            );
            Joined::start(vm, "task.background", together, works, Gives::Body(None))
        }

        fn each(vm, work: Chan, at_once: i64, f: &Value) -> Result<Step, VmError> {
            let workers = workers("task.each", at_once)?;
            if workers > crate::scheduler::MAX_TASKS {
                return Err(VmError::new(format!(
                    "task.each: at_once is {at_once}, more than the {} tasks that can run at once",
                    crate::scheduler::MAX_TASKS
                )));
            }
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.each"));
            }
            // Each worker is a `channel.each` of the channel.
            let works = (0..workers)
                .map(|_| {
                    let worker = Each {
                        ch: work.clone(),
                        callback: f.clone(),
                        state: EachState::Start,
                    };
                    (Work::Frame(Box::new(worker)), Role::EachWorker)
                })
                .collect();
            let together = Together::new("task.each", "the workers of its `task.each`", workers);
            Joined::start(vm, "task.each", together, works, Gives::Unit)
        }

        fn first_ok(vm, works: List) -> Result<Step, VmError> {
            if works.is_empty() {
                return Ok(Step::Done(Value::variant(bv::ERR, [Value::list(Vec::new())])));
            }
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.first_ok"));
            }
            let together = Together::new(
                "task.first_ok",
                "the pieces of its `task.first_ok`",
                works.len(),
            );
            let race = Arc::new(Race {
                won: Mutex::new(None),
                errs: Mutex::new(vec![None; works.len()]),
            });
            let pieces = works
                .iter()
                .enumerate()
                .map(|(at, work)| {
                    let racer = Racer {
                        work: Some(work),
                        at,
                        race: race.clone(),
                        together: together.clone(),
                    };
                    (Work::Frame(Box::new(racer)), Role::Racer)
                })
                .collect();
            Joined::start(vm, "task.first_ok", together, pieces, Gives::FirstOk(race))
        }

        fn join(vm, pieces: List) -> Result<Step, VmError> {
            if pieces.is_empty() {
                return Ok(Step::Done(Value::Unit));
            }
            // As `task.background`.
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.join"));
            }
            let works: Vec<(Work, Role)> =
                pieces.iter().map(|piece| (Work::Call(piece), Role::Piece)).collect();
            let together = Together::new("task.join", "the pieces of its `task.join`", works.len());
            Joined::start(vm, "task.join", together, works, Gives::Unit)
        }

        fn map(vm, xs: List, at_once: i64, f: &Value) -> Result<Step, VmError> {
            let workers = workers("task.map", at_once)?.min(xs.len());
            if workers == 0 {
                return Ok(Step::Done(Value::list(Vec::new())));
            }
            if vm.takes_nothing_new() {
                return Ok(vm.end_here("task.map"));
            }
            let xs: Vec<Value> = xs.iter().collect();
            let mapping = Arc::new(Mapping {
                values: Mutex::new(vec![None; xs.len()]),
                xs,
                f: f.clone(),
                next: AtomicUsize::new(0),
            });
            let works = (0..workers)
                .map(|_| {
                    let worker = MapWorker {
                        mapping: mapping.clone(),
                        at: None,
                        ended: false,
                    };
                    (Work::Frame(Box::new(worker)), Role::MapWorker)
                })
                .collect();
            let together = Together::new("task.map", "the workers of its `task.map`", workers);
            Joined::start(vm, "task.map", together, works, Gives::Mapped(mapping))
        }

        fn guard(vm, work: &Value) -> Result<Step, VmError> {
            Ok(crate::builtins::guard::Guard::enter(vm, work.clone()))
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
