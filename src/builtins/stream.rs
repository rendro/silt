//! `stream.*` builtin functions: a library of channel-backed sources,
//! transforms, and sinks. The underlying primitive is `Value::Channel(_)`
//! — there is no separate stream value type.
//!
//! A source or a transform is a stage: a task of the scheduler that
//! reads its input channels, calls the user's function, and writes to
//! its output channel. It waits where any task waits (for a value, for
//! its reader to ask for one, for an I/O operation), so a thousand
//! stages cost no thread. A transform takes a value from its input
//! only when its reader asks for one; a source works ahead, up to the
//! capacity of its output; `stream.buffered(n)` takes up to `n` values
//! before they are asked for.
//!
//! A sink (collect, fold, count, ...) runs in the caller's task and
//! waits there for each value.
//!
//! How a pipeline ends:
//!
//! - A stage whose input is closed closes its output.
//! - A stage that fails (its function raises an error) closes its
//!   output with the failure. The next stage fails with it in turn,
//!   and the sink at the end raises it.
//! - A stage or a sink that reads no further (`take`, `first`, an
//!   error downstream) stops the stage that feeds it, which stops the
//!   one before it. A channel that no stage feeds is left alone.
//! - A stage whose output every reader has let go of is stopped the
//!   same way: the stage holds its output weakly, and so does its wait
//!   to be asked (`Source::Demand`), so the output is dropped with its
//!   last reader (`Channel`'s `Drop`).
//!
//! Every stage and every sink is one `Pipe`: a frame that does the
//! waiting, around a function (`Logic`) that says what comes next.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use super::collections::{Kind, fits};
use super::common::{READ_AT_ONCE, ok};
use super::io::{io_result_err, path_err};
#[cfg(feature = "tcp")]
use super::typed::TcpStream;
use super::typed::{Arg, Chan, List, builtins, unsound};
use crate::runtime::handle::{StageOf, TaskHandle};
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{IoOp, Leave, Limit, Native, Step, Vm, VmError};

/// The output of a stage: how many values it buffers, how many of
/// them the stage may hold without being asked for them, and whether a
/// reader that drains it (reads it to the end) drains the stage's
/// inputs as well.
#[derive(Clone, Copy)]
struct Output {
    capacity: usize,
    ahead: usize,
    drains: bool,
}

/// How many values a stage holds ahead of a reader that drains its
/// output, and a source ahead of any reader.
const AHEAD: usize = 16;

/// A source works ahead of its reader: it takes nothing that the
/// program holds, so it does not wait to be asked.
const SOURCE: Output = Output {
    capacity: AHEAD,
    ahead: AHEAD,
    drains: false,
};

/// A transform takes a value from its input only when its reader asks
/// for one, or fills its output while its reader drains it. This one
/// reads its whole input when its whole output is read, so a reader
/// that drains it drains its inputs.
const DRAINS: Output = Output {
    capacity: AHEAD,
    ahead: 0,
    drains: true,
};

/// A transform that may stop before its input ends (`take`,
/// `take_while`, `zip`): it asks its input for one value at a time,
/// whoever reads it, so its input loses nothing when it stops.
const STOPS: Output = Output {
    capacity: AHEAD,
    ahead: 0,
    drains: false,
};

// ── The pipe ───────────────────────────────────────────────────────────

/// What happened since a stage or a sink said what it does next.
enum Got {
    /// Nothing yet.
    Start,
    /// The input at this index gave a value.
    Value(usize, Value),
    /// An input is closed and empty.
    End,
    /// The value was taken by the output.
    Emitted,
    /// The function returned this.
    Returned(Value),
    /// The I/O operation gave this.
    Io(Value),
}

/// What a stage or a sink does next.
enum Next {
    /// Wait for a value from the input at this index.
    Take(usize),
    /// Wait for a value from whichever input has one first.
    TakeAny,
    /// Send a value on the output, waiting for room. If the output is
    /// closed nobody reads it, and the stage ends.
    Emit(Value),
    /// Call a function with arguments.
    Call(Value, Vec<Value>),
    /// Run a blocking operation on the I/O pool, on what `On` says,
    /// until `SILT_IO_TIMEOUT` at most.
    Io(Box<dyn FnOnce() -> Value + Send>, On),
    /// It is finished. A sink's value; a stage's is ignored.
    Done(Value),
}

/// The part of a stage or sink that is its own: given what happened,
/// what comes next.
/// What makes the blocking operations of a source or a sink return
/// when nobody waits for them any more (a source on a connection lets go
/// of it, a sink shuts it down): `true` if what is in flight ends so
/// (see `Leave::Closes`).
type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

/// What the blocking operations of a source or a sink work on: the
/// typed error of its module, for an operation that could not run, and
/// what makes it return when nobody waits for it any more (`None`: it
/// runs out).
#[derive(Clone)]
struct On {
    failure: crate::vm::ErrFactory,
    stop: Option<Stop>,
    /// What a wait for I/O that ended at `SILT_IO_TIMEOUT` does first:
    /// on a connection, shut it down, as every tcp operation that times
    /// out does. (A source that is stopped lets go of its connection;
    /// one whose read timed out shuts it.)
    timed_out: Option<Stop>,
}

/// What a source or a sink of a file works on.
const ON_A_FILE: On = On {
    failure: io_failure,
    stop: None,
    timed_out: None,
};

type Logic = Box<dyn FnMut(Got) -> Result<Next, VmError> + Send>;

/// Where a pipe stands between two resumptions.
enum State {
    /// It goes on with this.
    Go(Got),
    /// It waits for a value; the wait's arms are these inputs.
    Taking(Vec<usize>),
    Calling,
    Io(IoOp),
    /// It waits to be asked for a value, or for room in its output, and
    /// then does this: a take, or the emit of a value.
    Asking(Next),
    /// It gave way to the other tasks; it goes on with this.
    Then(Next),
}

/// A stage or a sink: the frame that waits for what its [`Logic`]
/// asks for.
struct Pipe {
    name: &'static str,
    inputs: Vec<Arc<Channel>>,
    /// Which inputs have not ended.
    open: Vec<bool>,
    /// The output of a stage; a sink has none. Held weakly: the stage
    /// does not keep its output. When every reader has let it go, it is
    /// dropped, and that stops the stage (`Channel`'s `Drop`); the stage
    /// that finds it gone is over.
    out: Option<Weak<Channel>>,
    /// The handle of a stage's own task: where its failure is read
    /// when the task ends by one.
    handle: Option<Arc<TaskHandle>>,
    logic: Logic,
    state: State,
    /// Which input a wait for any of them tries first: each in turn.
    turn: usize,
    finished: bool,
    /// A sink that reads its input to the end, and has not yet said so.
    drains: bool,
    /// What ends its blocking operations, for a pipe on a connection:
    /// called when the pipe is stopped before its end, whether an
    /// operation is in flight then or not.
    stop: Option<Stop>,
    /// What its operation does when its wait ends at `SILT_IO_TIMEOUT`
    /// ([`On::timed_out`]).
    timed_out: Option<Stop>,
    /// For a source on a connection: its stop (it lets go of the
    /// connection, [`Held::let_go`]) that its output's reader runs when
    /// it stops reading, also before the source has run. It is disarmed
    /// when the source ends by itself.
    armed: Option<Arc<Armed>>,
}

/// The stop that a stage on a connection is stopped with: run
/// once, by whoever stops the stage first, and not after the stage
/// ended by itself.
struct Armed {
    stop: Stop,
    armed: AtomicBool,
}

impl Armed {
    fn new(stop: Stop) -> Armed {
        Armed {
            stop,
            armed: AtomicBool::new(true),
        }
    }

    fn fire(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            let _ = (self.stop)();
        }
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }
}

/// How many steps a pipe takes without waiting before it gives way to
/// the other tasks.
const BURST: usize = 256;

/// The error of an I/O operation of a pipe that could not run.
fn io_failure(failure: crate::vm::IoFailure<'_>) -> Value {
    err_io_unknown(failure.text())
}

fn internal(what: &str) -> VmError {
    VmError::new(format!("internal VM error: a stream stage {what}"))
}

impl Pipe {
    fn new(
        name: &'static str,
        inputs: Vec<Arc<Channel>>,
        logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
    ) -> Pipe {
        Pipe {
            name,
            open: vec![true; inputs.len()],
            inputs,
            out: None,
            handle: None,
            logic: Box::new(logic),
            state: State::Go(Got::Start),
            turn: 0,
            drains: false,
            finished: false,
            stop: None,
            timed_out: None,
            armed: None,
        }
    }

    /// The input `index` is closed and empty. If the stage that fed it
    /// failed, this one fails with the same error.
    fn ended(&mut self, index: usize, close: Close) -> Result<Got, VmError> {
        self.open[index] = false;
        if let Err(failure) = close.raise() {
            if let Some(handle) = &self.handle {
                handle.fails_with_its_input();
            }
            return Err(failure);
        }
        Ok(Got::End)
    }

    /// The pipe is over: its output closes, with the failure if it
    /// ends by one, and the stages that feed its inputs are told that
    /// nobody reads them any more.
    fn finish(&mut self, vm: &Vm, failure: Option<Arc<VmError>>) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let wake = vm.scheduler().wake();
        if let Some(out) = self.output() {
            let stage = failure.as_ref().and(self.handle.clone());
            out.close(Close { failure, stage }, wake);
        }
        for (input, open) in self.inputs.iter().zip(&self.open) {
            if *open {
                input.abandon(wake);
            }
        }
    }

    /// The output of a stage, while a reader has it.
    fn output(&self) -> Option<Arc<Channel>> {
        self.out.as_ref().and_then(Weak::upgrade)
    }

    /// Nobody reads the output any more, and the pipe has a value for
    /// it: the pipe is over before its end. One on a connection is
    /// stopped (a source lets go of it, a sink shuts it down), as one
    /// that is stopped in a wait is (`abandon`), at whatever moment its
    /// reader went.
    fn cut_short(&mut self, vm: &Vm) {
        if !self.finished {
            if let Some(stop) = self.stop.take() {
                let _ = stop();
            }
            if let Some(armed) = &self.armed {
                armed.fire();
            }
        }
        self.finish(vm, None);
    }

    /// Do `next`, and what the logic says after each step, until the
    /// pipe waits or ends, or for [`BURST`] steps.
    fn run(&mut self, vm: &mut Vm, mut next: Next) -> Result<Step, VmError> {
        for _ in 0..BURST {
            let got = match next {
                // A task that was cancelled, or whose time is over,
                // takes nothing new, though it still runs for some
                // steps: a stage that was
                // stopped (nobody reads its output any more: its task
                // is cancelled) is over, and a sink ends the task that
                // called it.
                Next::Take(_) | Next::TakeAny if vm.takes_nothing_new() => {
                    self.cut_short(vm);
                    return Ok(match self.handle {
                        Some(_) => Step::Done(Value::Unit),
                        None => Step::Park(Vm::no_wait()),
                    });
                }
                // A sink that reads its input to the end says so before
                // its first take.
                Next::Take(index) if std::mem::take(&mut self.drains) => {
                    self.inputs[index].drain(vm.scheduler().wake());
                    next = Next::Take(index);
                    continue;
                }
                // A stage takes a value only when its reader asks for
                // one, or while it may fill its output (its reader
                // drains it; `stream.buffered` holds values ahead):
                // until then it waits, with nothing taken.
                take @ (Next::Take(_) | Next::TakeAny)
                    if self.out.is_some() && self.output().is_none_or(|out| !out.is_asked()) =>
                {
                    // Its readers have let the output go: nobody asks.
                    let Some(out) = self.output() else {
                        self.cut_short(vm);
                        return Ok(Step::Done(Value::Unit));
                    };
                    let wait = Wait::new(vec![Arm::Demand(out)]);
                    self.state = State::Asking(take);
                    return Ok(Step::Park(wait));
                }
                Next::Take(index) => {
                    let input = &self.inputs[index];
                    match input.try_receive(vm.scheduler().wake()) {
                        TryReceive::Value(value) => Got::Value(index, value),
                        TryReceive::Closed(close) => self.ended(index, close)?,
                        TryReceive::Empty => {
                            let wait = Wait::new(vec![Arm::Recv(input.clone())]);
                            self.state = State::Taking(vec![index]);
                            return Ok(Step::Park(wait));
                        }
                    }
                }
                Next::TakeAny => {
                    let arms: Vec<usize> = (0..self.inputs.len())
                        .filter(|index| self.open[*index])
                        .collect();
                    if arms.is_empty() {
                        return Err(internal("waits for a value with no input left"));
                    }
                    let recv = |index: &usize| Arm::Recv(self.inputs[*index].clone());
                    let wait = Wait::new(arms.iter().map(recv).collect()).first(self.turn);
                    self.turn = self.turn.wrapping_add(1);
                    self.state = State::Taking(arms);
                    return Ok(Step::Park(wait));
                }
                Next::Emit(value) => {
                    if self.out.is_none() {
                        return Err(internal("has no output"));
                    }
                    // Nobody reads the output: its readers let it go.
                    let Some(out) = self.output() else {
                        self.cut_short(vm);
                        return Ok(Step::Done(Value::Unit));
                    };
                    match out.try_send(value, vm.scheduler().wake()) {
                        TrySend::Sent => Got::Emitted,
                        // Nobody reads the output.
                        TrySend::Closed(_) => {
                            self.cut_short(vm);
                            return Ok(Step::Done(Value::Unit));
                        }
                        // It waits for room as for being asked, which
                        // does not keep the output, and sends again.
                        TrySend::Full(value) => {
                            let wait = Wait::new(vec![Arm::Demand(out)]);
                            self.state = State::Asking(Next::Emit(value));
                            return Ok(Step::Park(wait));
                        }
                    }
                }
                Next::Call(callee, args) => {
                    self.state = State::Calling;
                    return Ok(vm.call(callee, args));
                }
                Next::Io(
                    operation,
                    On {
                        failure,
                        stop,
                        timed_out,
                    },
                ) => {
                    self.timed_out = timed_out;
                    // An operation on a connection that ends without its
                    // value reaching the pipe ends the connection, as a
                    // `tcp.read` does.
                    let leave = match stop {
                        Some(stop) => {
                            self.stop = Some(stop.clone());
                            Leave::Closes(Box::new(move || stop()))
                        }
                        None => Leave::RunsOut,
                    };
                    let op = vm.io_op(failure, leave, move |_| operation());
                    let wait = op.wait(vm, Limit::Io);
                    self.state = State::Io(op);
                    return Ok(Step::Park(wait));
                }
                Next::Done(value) => {
                    // A source that ended by itself leaves its
                    // connection to the program.
                    if let Some(armed) = &self.armed {
                        armed.disarm();
                    }
                    self.finish(vm, None);
                    return Ok(Step::Done(value));
                }
            };
            next = (self.logic)(got)?;
        }
        self.state = State::Then(next);
        Ok(Step::Yield)
    }
}

impl Native for Pipe {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        let got = match std::mem::replace(&mut self.state, State::Calling) {
            State::Go(got) => got,
            State::Then(next) => return self.run(vm, next),
            State::Calling => Got::Returned(input),
            State::Taking(arms) => match vm.woken()? {
                Fired::Arm(arm, Outcome::Received(value)) => Got::Value(arms[arm], value),
                Fired::Arm(arm, Outcome::Closed(close)) => self.ended(arms[arm], close)?,
                _ => return Err(internal("was woken without a value")),
            },
            // The value, or the module's error at `SILT_IO_TIMEOUT`.
            State::Io(op) => {
                let fired = vm.woken()?;
                if matches!(fired, Fired::Deadline)
                    && let Some(timed_out) = self.timed_out.take()
                {
                    let _ = timed_out();
                }
                Got::Io(op.value(&fired))
            }
            State::Asking(take) => match vm.woken()? {
                Fired::Arm(_, Outcome::Asked) => return self.run(vm, take),
                // Nobody reads the output.
                _ => {
                    self.cut_short(vm);
                    return Ok(Step::Done(Value::Unit));
                }
            },
        };
        let next = (self.logic)(got)?;
        self.run(vm, next)
    }

    fn abandon(&mut self, vm: &mut Vm) {
        // A pipe on a connection that is stopped before its end is
        // stopped on it (a source lets go of it, a sink shuts it down),
        // whatever it was doing: not only when a read was in flight.
        if !self.finished {
            if let Some(stop) = self.stop.take() {
                let _ = stop();
            }
            if let Some(armed) = &self.armed {
                armed.fire();
            }
        }
        // A stage that failed hands its failure to whoever reads its
        // output: the first read that raises it handles it. One that no
        // read raises is reported when the program ends, unless the
        // reader stopped reading (that cancels the stage, which
        // dismisses it). A stage that was stopped closes its output
        // plainly.
        let failure = self.handle.as_ref().and_then(|handle| {
            if handle.is_cancelled() {
                return None;
            }
            Some(Arc::new(handle.try_get()?.err()?))
        });
        self.finish(vm, failure);
    }
}

/// Start a stage: a task that runs `logic` over `inputs` and feeds the
/// channel that is returned, as `output` says. Every source and every
/// transform starts its task here.
///
/// A task that takes nothing new (`Vm::takes_nothing_new`: it was
/// cancelled, or its time is over) starts no stage: the stage's task would take from its
/// inputs, or from outside, for a reader that ends at its own next
/// take. The task ends here, with nothing started.
fn stage(
    vm: &mut Vm,
    name: &'static str,
    inputs: Vec<Arc<Channel>>,
    output: Output,
    logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    stage_on(vm, name, inputs, output, None, logic)
}

/// Start a stage, as [`stage`] does, that works on a connection: `stop`
/// is run when the stage is stopped, whenever that is: in a read,
/// between two, or before the stage has run at all.
fn stage_on(
    vm: &mut Vm,
    name: &'static str,
    inputs: Vec<Arc<Channel>>,
    output: Output,
    stop: Option<Stop>,
    logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    if vm.takes_nothing_new() {
        return Ok(vm.end_here(name));
    }
    let Output {
        capacity,
        ahead,
        drains,
    } = output;
    let out = Channel::fed(vm.next_channel_id(), capacity, ahead);
    let id = vm.next_task_id();
    let owner = vm.scheduler().current_owner();
    let stage = StageOf {
        name,
        source: inputs.is_empty(),
        at: vm.place_here(),
        inputs: inputs.iter().map(|input| input.id()).collect(),
        output: out.id(),
    };
    let handle = Arc::new(TaskHandle::with_owner(id, owner).of_stage(stage));
    // Whoever reads the output and stops reading stops the stage.
    let scheduler = Arc::downgrade(vm.scheduler());
    let stopped = handle.clone();
    let armed = stop.map(|stop| Arc::new(Armed::new(stop)));
    let shut = armed.clone();
    out.stop_feeder_with(move || {
        // The connection first: a stage that is cancelled before it has
        // run may be ended by a worker at once, and nothing of it runs
        // then.
        if let Some(shut) = &shut {
            shut.fire();
        }
        if let Some(scheduler) = scheduler.upgrade() {
            scheduler.cancel(&stopped);
        }
    });
    let mut pipe = Pipe::new(name, inputs, logic);
    pipe.armed = armed;
    pipe.out = Some(Arc::downgrade(&out));
    pipe.handle = Some(handle.clone());
    if drains {
        // A reader that drains the output drains every input: the
        // stage reads them to the end for it.
        let inputs = pipe.inputs.clone();
        out.drain_feeder_with(move |wake| {
            for input in &inputs {
                input.drain(wake);
            }
        });
    }
    let mut child = vm.spawn_child();
    child.spawned = true;
    child.push_native_frame(Box::new(pipe));
    vm.scheduler()
        .submit(id, child, handle)
        .map_err(VmError::new)?;
    Ok(Step::Done(Value::Channel(out)))
}

/// A stream whose one item is `item` (an `Err`), and then its end: a
/// source that could not start says why.
#[cfg(feature = "tcp")]
fn failed_stream(vm: &mut Vm, item: Value) -> Step {
    let out = Channel::new(vm.next_channel_id(), 1);
    let _ = out.try_send(item, vm.scheduler().wake());
    out.close(Close::default(), vm.scheduler().wake());
    Step::Done(Value::Channel(out))
}

/// A channel that is closed and empty: the output of a stage that has
/// nothing to do (`stream.take(s, 0)`).
fn closed_channel(vm: &mut Vm) -> Step {
    let out = Channel::new(vm.next_channel_id(), 1);
    out.close(Close::default(), vm.scheduler().wake());
    Step::Done(Value::Channel(out))
}

/// Run a sink in the caller's task: its value is the call's. A sink
/// that `drains` reads its input to the end, and says so: the stages
/// before it work ahead while it reads.
fn sink(
    name: &'static str,
    input: Arc<Channel>,
    drains: bool,
    logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    let mut pipe = Pipe::new(name, vec![input], logic);
    pipe.drains = drains;
    Ok(Step::Run(Box::new(pipe)))
}

/// What a logic says to an event it never asked for.
fn unexpected() -> Result<Next, VmError> {
    Err(internal("was told of something it did not wait for"))
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Build an `Err(Other(msg))` for string-only failures.
fn err_io_unknown(s: impl Into<Arc<str>>) -> Value {
    Value::variant(bv::ERR, [Value::variant(bv::IO_OTHER, [Value::string(s)])])
}

/// What a sink on `stream` works on: its errors are `TcpError`s, and
/// its operations return when the connection is shut down.
#[cfg(feature = "tcp")]
fn on_tcp(stream: &Arc<crate::runtime::handle::TcpStreamHandle>) -> On {
    let stream = stream.clone();
    let shut: Stop = Arc::new(move || {
        stream.shut_down();
        true
    });
    On {
        failure: super::tcp::tcp_timeout_err,
        stop: Some(shut.clone()),
        timed_out: Some(shut),
    }
}

/// The connection of a `stream.tcp_*` source, which the stream holds as
/// any other name does: while the stream is read, the connection stays.
/// When the stream is let go of (nothing can read it any more, or
/// `channel.close`), the source stops reading and lets go of the
/// connection ([`Held::let_go`]): it is closed only when nothing else
/// holds it, as any value is. A source that ends by itself (the peer
/// closed, an error) lets go of it too.
#[cfg(feature = "tcp")]
struct Held {
    slot: Mutex<Option<Arc<crate::runtime::handle::TcpStreamHandle>>>,
    conn: Weak<crate::runtime::handle::TcpStreamHandle>,
    /// A read of the source is in flight, with its own hold of the
    /// connection.
    reading: AtomicBool,
    /// What ends that read without shutting the connection down.
    wake: crate::runtime::handle::ReadWake,
}

#[cfg(feature = "tcp")]
impl Held {
    /// The hold of `conn`; an error when its wake cannot be made (the
    /// process has no descriptor left), which the stream gives.
    fn new(conn: &Arc<crate::runtime::handle::TcpStreamHandle>) -> std::io::Result<Arc<Held>> {
        Ok(Arc::new(Held {
            slot: Mutex::new(Some(conn.clone())),
            conn: Arc::downgrade(conn),
            reading: AtomicBool::new(false),
            wake: crate::runtime::handle::ReadWake::new()?,
        }))
    }

    /// The connection, for a read, which is in flight until the guard
    /// is dropped; `None` once the source has let go of it.
    fn for_read(&self) -> Option<(Arc<crate::runtime::handle::TcpStreamHandle>, Reading<'_>)> {
        let slot = self.slot.lock();
        let conn = slot.clone()?;
        self.reading.store(true, Ordering::SeqCst);
        Some((conn, Reading(self)))
    }

    /// The source has ended by itself: it holds the connection no more.
    fn release(&self) {
        let taken = self.slot.lock().take();
        std::mem::drop(taken);
    }

    /// The stream is let go of: the source lets go of the connection,
    /// and a read in flight ends without taking anything. If that read
    /// is the last holder, nobody else can use the connection: it is
    /// shut down, which ends the read, and closed with it. Else the read
    /// is woken ([`ReadWake`]): it ends without taking anything, and the
    /// connection stays its other holders'.
    ///
    /// [`ReadWake`]: crate::runtime::handle::ReadWake
    fn let_go(&self) -> bool {
        self.release();
        self.wake.wake();
        if !self.reading.load(Ordering::SeqCst) {
            return true;
        }
        match self.conn.strong_count() {
            0 => true,
            1 => {
                if let Some(conn) = self.conn.upgrade() {
                    conn.shut_down();
                }
                true
            }
            _ => true,
        }
    }
}

/// A read of a source on a connection is in flight ([`Held::for_read`]).
#[cfg(feature = "tcp")]
struct Reading<'a>(&'a Held);

#[cfg(feature = "tcp")]
impl Drop for Reading<'_> {
    fn drop(&mut self) {
        self.0.reading.store(false, Ordering::SeqCst);
    }
}

/// What a source on a connection works on: its errors are `TcpError`s,
/// and when nobody reads it any more it lets go of the connection
/// ([`Held::let_go`]).
#[cfg(feature = "tcp")]
fn on_held(held: &Arc<Held>) -> On {
    let (stops, times_out) = (held.clone(), held.clone());
    On {
        failure: super::tcp::tcp_timeout_err,
        stop: Some(Arc::new(move || stops.let_go())),
        timed_out: Some(Arc::new(move || {
            if let Some(conn) = times_out.conn.upgrade() {
                conn.shut_down();
            }
            true
        })),
    }
}

/// A source that reads from something that blocks: `read` gives the
/// next items, those one read brings (a source of lines reads up to
/// [`READ_AT_ONCE`] bytes of them at a time, so that a line is not an
/// operation of the I/O pool of its own), none at the end. An `Err(_)`
/// item is the last; so is the error of a read that could not run
/// (`on`'s failure). The items of a read are emitted one by one before
/// the next read.
fn reading(
    vm: &mut Vm,
    name: &'static str,
    on: On,
    read: impl FnMut() -> Vec<Value> + Send + 'static,
) -> Result<Step, VmError> {
    let read = Arc::new(Mutex::new(read));
    let mut failed = false;
    let mut items = std::collections::VecDeque::new();
    let stop = on.stop.clone();
    stage_on(vm, name, vec![], SOURCE, stop, move |got| {
        // The next item read, which ends the source if it is an error.
        fn emit(item: Value, failed: &mut bool) -> Next {
            *failed = matches!(&item, Value::Variant(item) if item.is(bv::ERR));
            Next::Emit(item)
        }
        Ok(match got {
            Got::Start | Got::Emitted if !failed => match items.pop_front() {
                Some(item) => emit(item, &mut failed),
                None => {
                    let read = read.clone();
                    Next::Io(Box::new(move || Value::list((read.lock())())), on.clone())
                }
            },
            Got::Io(Value::List(read)) => {
                items.extend(read.iter());
                match items.pop_front() {
                    Some(item) => emit(item, &mut failed),
                    None => Next::Done(Value::Unit),
                }
            }
            // The read could not run.
            Got::Io(Value::Variant(error)) if error.is(bv::ERR) => {
                emit(Value::Variant(error), &mut failed)
            }
            _ => Next::Done(Value::Unit),
        })
    })
}

/// A `read` of [`reading`] that gives one item a read, or none at the
/// end.
fn one_a_read(
    mut read: impl FnMut() -> Option<Value> + Send + 'static,
) -> impl FnMut() -> Vec<Value> + Send + 'static {
    move || read().into_iter().collect()
}

/// The reads of `stream.stdin_lines`: each gives the lines, without
/// their `\n` or `\r\n`, that are complete in what has been read,
/// reading until one is or stdin ends; at the end, the bytes after the
/// last `\n` are the last line, if there are any. A read takes what
/// stdin has, up to [`READ_AT_ONCE`] bytes, and does not wait for more:
/// a line typed at a terminal comes as it is typed, and a pipe gives
/// many lines a read. A line that is no UTF-8 ends the source with an
/// error, after the lines before it, as `file_lines` ends.
fn stdin_line_reads() -> impl FnMut() -> Vec<Value> + Send + 'static {
    use std::io::Read;
    let mut lines = LineReads::new(|buf: &mut [u8]| std::io::stdin().lock().read(buf));
    move || lines.next_lines()
}

/// The lines of a source read a piece at a time by `read` (see
/// [`stdin_line_reads`]). Each byte is looked at for a `\n` once: a line
/// that many reads bring is not searched again from its start at each
/// one.
struct LineReads<R> {
    read: R,
    /// What was read and is no line yet.
    pending: Vec<u8>,
    /// How much of `pending` is known to have no `\n`.
    scanned: usize,
    /// The source has ended; the lines have ended.
    at_end: bool,
    ended: bool,
    /// How many lines were given.
    given: u64,
    /// How many bytes were looked at for a `\n`, in all (what a test
    /// asks: each byte once).
    #[cfg_attr(not(test), allow(dead_code))]
    looked: u64,
}

impl<R: FnMut(&mut [u8]) -> std::io::Result<usize>> LineReads<R> {
    fn new(read: R) -> LineReads<R> {
        LineReads {
            read,
            pending: Vec::new(),
            scanned: 0,
            at_end: false,
            ended: false,
            given: 0,
            looked: 0,
        }
    }

    fn not_utf8(number: u64) -> Value {
        let why = format!("line {number} of stdin is not UTF-8");
        Value::variant(
            bv::ERR,
            [Value::variant(bv::IO_INVALID_INPUT, [Value::string(why)])],
        )
    }

    /// The lines of the next read (see [`stdin_line_reads`]).
    fn next_lines(&mut self) -> Vec<Value> {
        let mut lines = Vec::new();
        while !self.ended {
            let mut taken = 0;
            let mut held_back = false;
            loop {
                let from = taken.max(self.scanned);
                let Some(at) = self.pending[from..].iter().position(|&b| b == b'\n') else {
                    self.looked += (self.pending.len() - from) as u64;
                    break;
                };
                self.looked += (at + 1) as u64;
                let line = &self.pending[taken..from + at];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                match std::str::from_utf8(line) {
                    Ok(line) => {
                        self.given += 1;
                        lines.push(ok(Value::string(line)));
                    }
                    Err(_) if lines.is_empty() => {
                        self.ended = true;
                        lines.push(Self::not_utf8(self.given + 1));
                        return lines;
                    }
                    // The lines before it are given first: the next read
                    // ends the source with it.
                    Err(_) => {
                        held_back = true;
                        break;
                    }
                }
                taken = from + at + 1;
            }
            self.pending.drain(..taken);
            self.scanned = if held_back { 0 } else { self.pending.len() };
            if held_back || !lines.is_empty() {
                break;
            }
            if self.at_end {
                self.ended = true;
                if !self.pending.is_empty() {
                    let line = self.pending.strip_suffix(b"\r").unwrap_or(&self.pending);
                    lines.push(match std::str::from_utf8(line) {
                        Ok(line) => ok(Value::string(line)),
                        Err(_) => Self::not_utf8(self.given + 1),
                    });
                }
                break;
            }
            let at = self.pending.len();
            self.pending.resize(at + READ_AT_ONCE, 0);
            match (self.read)(&mut self.pending[at..]) {
                Ok(got) => {
                    self.pending.truncate(at + got);
                    self.at_end = got == 0;
                }
                Err(e) => {
                    self.pending.truncate(at);
                    if e.kind() != std::io::ErrorKind::Interrupted {
                        self.ended = true;
                        lines.push(io_result_err(&e, ""));
                    }
                }
            }
        }
        lines
    }
}

/// What one read of `stream.tcp_lines` asks for, the one that may wait
/// for the peer and each that does not.
#[cfg(feature = "tcp")]
const LINES_READ: usize = 4 * 1024;

/// The reads of `stream.tcp_lines` on `conn`: each gives the lines,
/// without their `\n` or `\r\n`, that are complete in what has been
/// read, reading until one is, or the peer has closed; then the bytes
/// after the last `\n` are the last line, if there are any. What one
/// read brings past its last line waits for the next. A line that is no
/// UTF-8 ends the source with an error, as `file_lines` ends, after the
/// lines before it; the connection is shut down then, as for any source
/// on it that stops.
///
/// Every read asks for [`LINES_READ`] bytes. With a line in hand, the
/// source reads on only what is there without waiting, where the
/// connection can do that (a plain one; a TLS one gives the lines of
/// what it has), while each read fills what it asked for, up to
/// [`READ_AT_ONCE`] bytes. The lines are taken out after each read, so
/// that what is kept is the line not yet complete and room for one
/// read: a source that waits holds that, never more.
#[cfg(feature = "tcp")]
fn tcp_line_reads(held: Arc<Held>) -> impl FnMut() -> Vec<Value> + Send + 'static {
    // What was read and is no line yet; how much of it is known to
    // have no `\n`; the peer has closed; the source has ended; the last
    // read filled what it asked for.
    let mut pending: Vec<u8> = Vec::new();
    let mut scanned = 0;
    let mut closed = false;
    let mut ended = false;
    let mut filled = false;
    let not_utf8 = |conn: &crate::runtime::handle::TcpStreamHandle| {
        conn.shut_down();
        let not_utf8 = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid input: stream did not contain valid UTF-8",
        );
        super::tcp::tcp_io_err(super::tcp::Doing::Using, &not_utf8)
    };
    // Room for one read beside the line not yet complete, no more: a
    // long line or a burst leaves no large buffer behind.
    let give_back = |pending: &mut Vec<u8>| {
        let keep = pending.len() + LINES_READ;
        if pending.len() <= LINES_READ && pending.capacity() > keep {
            pending.shrink_to(keep);
        }
    };
    move || {
        let mut lines = Vec::new();
        if ended {
            return lines;
        }
        // (The guard is dropped before the connection: the read is over
        // while it still holds it.)
        let Some((conn, _reading)) = held.for_read() else {
            ended = true;
            return lines;
        };
        // The bytes this read of the source has read.
        let mut read_here = 0;
        loop {
            let mut taken = 0;
            let mut held_back = false;
            // One allocation for the lines of a read, not a growing list
            // (each size it passes through is memory that the I/O pool
            // thread's allocator keeps while the thread waits).
            lines.reserve(pending[scanned..].iter().filter(|&&b| b == b'\n').count());
            loop {
                let from = taken.max(scanned);
                let Some(at) = pending[from..].iter().position(|&b| b == b'\n') else {
                    break;
                };
                let line = &pending[taken..from + at];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                match std::str::from_utf8(line) {
                    Ok(line) => lines.push(ok(Value::string(line))),
                    Err(_) if lines.is_empty() => {
                        ended = true;
                        lines.push(not_utf8(&conn));
                        held.release();
                        return lines;
                    }
                    // The lines before it are emitted first: the next read
                    // ends the source with it.
                    Err(_) => {
                        held_back = true;
                        break;
                    }
                }
                taken = from + at + 1;
            }
            pending.drain(..taken);
            scanned = if held_back { 0 } else { pending.len() };
            if held_back || closed {
                break;
            }
            // Wait only for a first line; with one in hand, read on what
            // is there.
            let wait = lines.is_empty();
            let read_on = filled && conn.reads_now() && read_here < READ_AT_ONCE;
            if !(wait || read_on) {
                break;
            }
            if wait {
                give_back(&mut pending);
            }
            let at = pending.len();
            // Room for this read and no more, unless a long line is
            // growing (then the list grows as lists do).
            if at <= LINES_READ {
                pending.reserve_exact(LINES_READ);
            }
            pending.resize(at + LINES_READ, 0);
            let read = match wait {
                true => match conn.read_unless_woken(&mut pending[at..], &held.wake) {
                    Ok(Some(got)) => Ok(got),
                    // The stream was let go of: nothing was read, and the
                    // source ends.
                    Ok(None) => {
                        pending.truncate(at);
                        ended = true;
                        return lines;
                    }
                    Err(e) => Err(e),
                },
                false => match conn.read_now(&mut pending[at..]) {
                    Some(got) => Ok(got),
                    None => {
                        pending.truncate(at);
                        break;
                    }
                },
            };
            let got = *read.as_ref().unwrap_or(&0);
            pending.truncate(at + got);
            filled = got == LINES_READ;
            read_here += got;
            match read {
                Ok(0) => closed = true,
                Ok(_) => {}
                Err(e) => {
                    ended = true;
                    lines.push(super::tcp::tcp_io_err(super::tcp::Doing::Using, &e));
                    held.release();
                    return lines;
                }
            }
        }
        give_back(&mut pending);
        if lines.is_empty() && closed {
            ended = true;
            held.release();
            if !pending.is_empty() {
                lines.push(match std::str::from_utf8(&pending) {
                    Ok(line) => ok(Value::string(line)),
                    Err(_) => not_utf8(&conn),
                });
            }
        }
        lines
    }
}

/// The value inside `Ok(_)`, if `v` is one.
fn ok_inner(v: &Value) -> Option<Value> {
    match v {
        Value::Variant(variant) if variant.is(bv::OK) && variant.fields().len() == 1 => {
            Some(variant.fields()[0].clone())
        }
        _ => None,
    }
}

/// A stage that passes every value on.
fn forward(got: Got) -> Result<Next, VmError> {
    Ok(match got {
        Got::Start | Got::Emitted => Next::Take(0),
        Got::Value(_, v) => Next::Emit(v),
        Got::End => Next::Done(Value::Unit),
        Got::Returned(_) | Got::Io(_) => return unexpected(),
    })
}

/// The channels of the `List(Channel)` argument `chs` of `name`.
fn channel_list(name: &str, chs: List) -> Result<Vec<Arc<Channel>>, VmError> {
    chs.iter()
        .map(|ch| match Chan::take(&ch) {
            Some(ch) => Ok(ch.clone()),
            None => Err(unsound(name, "chs")),
        })
        .collect()
}

/// The bytes of an item of a stream that `name` writes out: `Bytes`,
/// or `Ok(Bytes)`. An `Err(_)` item ends the sink with itself (the
/// inner `Err`); anything else is no item of the stream.
fn bytes_to_write(v: Value, name: &str) -> Result<Result<Arc<Vec<u8>>, Value>, VmError> {
    match v {
        Value::Bytes(b) => Ok(Ok(b)),
        Value::Variant(err) if err.is(bv::ERR) && err.fields().len() == 1 => {
            Ok(Err(Value::Variant(err)))
        }
        other => match ok_inner(&other) {
            Some(Value::Bytes(b)) => Ok(Ok(b)),
            _ => Err(unsound(name, "ch")),
        },
    }
}

/// A sink that writes every item of `ch` with `write` (which blocks)
/// and ends with `finish`. Its value is `Ok(())`, or the first
/// `Err(_)`: of a write, or an item that is one.
fn writing(
    name: &'static str,
    ch: Arc<Channel>,
    on: On,
    write: impl FnMut(Option<&[u8]>) -> Result<(), Value> + Send + 'static,
) -> Result<Step, VmError> {
    let write = Arc::new(Mutex::new(write));
    let mut ended = false;
    sink(name, ch, true, move |got| {
        let run = |bytes: Option<Arc<Vec<u8>>>| {
            let write = write.clone();
            Next::Io(
                Box::new(
                    move || match (write.lock())(bytes.as_deref().map(Vec::as_slice)) {
                        Ok(()) => ok(Value::Unit),
                        Err(e) => e,
                    },
                ),
                on.clone(),
            )
        };
        Ok(match got {
            // The first operation opens what is written to.
            Got::Start => run(Some(Arc::new(Vec::new()))),
            Got::Value(_, v) => match bytes_to_write(v, name)? {
                Ok(bytes) => run(Some(bytes)),
                Err(e) => Next::Done(e),
            },
            Got::End => {
                ended = true;
                run(None)
            }
            Got::Io(result) if ended || ok_inner(&result).is_none() => Next::Done(result),
            Got::Io(_) => Next::Take(0),
            _ => return unexpected(),
        })
    })
}

// ── The functions ──────────────────────────────────────────────────────

builtins! {
    // ── Sources ───────────────────────────────────────────────────────────

    fn from_list(vm, xs: List) -> Result<Step, VmError> {
        let mut items = xs.clone().into_iter();
        stage(vm, "stream.from_list", vec![], SOURCE, move |_| {
            Ok(match items.next() {
                Some(value) => Next::Emit(value),
                None => Next::Done(Value::Unit),
            })
        })
    }

    fn from_range(vm, lo: i64, hi: i64) -> Result<Step, VmError> {
        let mut next = Some(lo);
        stage(
            vm,
            "stream.from_range",
            vec![],
            SOURCE,
            move |_| {
                Ok(match next.filter(|i| *i <= hi) {
                    Some(i) => {
                        next = i.checked_add(1);
                        Next::Emit(Value::Int(i))
                    }
                    None => Next::Done(Value::Unit),
                })
            },
        )
    }

    fn repeat(vm, x: &Value) -> Result<Step, VmError> {
        let v = x.clone();
        stage(vm, "stream.repeat", vec![], SOURCE, move |_| {
            Ok(Next::Emit(v.clone()))
        })
    }

    fn unfold(vm, seed: &Value, f: &Value) -> Result<Step, VmError> {
        let mut state = seed.clone();
        let fn_val = f.clone();
        stage(vm, "stream.unfold", vec![], SOURCE, move |got| {
            Ok(match got {
                Got::Start | Got::Emitted => Next::Call(fn_val.clone(), vec![state.clone()]),
                // Fn(state) -> Option((value, next_state))
                Got::Returned(Value::Variant(some))
                    if some.is(bv::SOME) && some.fields().len() == 1 =>
                {
                    match &some.fields()[0] {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            state = pair[1].clone();
                            Next::Emit(pair[0].clone())
                        }
                        _ => Next::Done(Value::Unit),
                    }
                }
                _ => Next::Done(Value::Unit),
            })
        })
    }

    fn file_chunks(vm, path: &str, size: i64) -> Result<Step, VmError> {
        let Ok(n @ 1..) = usize::try_from(size) else {
            return Err(VmError::new("stream.file_chunks: size must be positive".into()));
        };
        let path = path.to_string();
        let mut file: Option<std::fs::File> = None;
        reading(vm, "stream.file_chunks", ON_A_FILE, one_a_read(move || {
            use std::io::{Read, Seek};
            if file.is_none() {
                match std::fs::File::open(&path) {
                    Ok(opened) => file = Some(opened),
                    Err(e) => return Some(path_err(&e, &path)),
                }
            }
            let file = file.as_mut()?;
            // A chunk larger than one read takes is as large as what
            // the file still has, if that is known.
            let size = match n <= READ_AT_ONCE {
                true => n,
                false => {
                    let left = file.metadata().ok().and_then(|meta| {
                        let at = file.stream_position().ok()?;
                        usize::try_from(meta.len().saturating_sub(at)).ok()
                    });
                    n.min(left.unwrap_or(0).max(READ_AT_ONCE))
                }
            };
            let mut buf = vec![0u8; size];
            match file.read(&mut buf) {
                Ok(0) => None,
                Ok(read) => {
                    buf.truncate(read);
                    Some(ok(Value::Bytes(Arc::new(buf))))
                }
                Err(e) => Some(path_err(&e, &path)),
            }
        }))
    }

    fn file_lines(vm, path: &str) -> Result<Step, VmError> {
        let path = path.to_string();
        let mut file: Option<std::io::BufReader<std::fs::File>> = None;
        let mut ended = false;
        // How many lines were read.
        let mut read_lines: u64 = 0;
        // The lines of up to READ_AT_ONCE bytes a read, each without its
        // `\n` or `\r\n` (as `BufRead::lines` gives them).
        reading(vm, "stream.file_lines", ON_A_FILE, move || {
            use std::io::BufRead;
            let mut lines = Vec::new();
            if ended {
                return lines;
            }
            if file.is_none() {
                match std::fs::File::open(&path) {
                    Ok(opened) => file = Some(std::io::BufReader::new(opened)),
                    Err(e) => {
                        ended = true;
                        lines.push(path_err(&e, &path));
                        return lines;
                    }
                }
            }
            let Some(reader) = file.as_mut() else {
                return lines;
            };
            let mut bytes = 0;
            while bytes < READ_AT_ONCE {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        ended = true;
                        break;
                    }
                    Ok(read) => {
                        bytes += read;
                        read_lines += 1;
                        if line.ends_with('\n') {
                            line.pop();
                            if line.ends_with('\r') {
                                line.pop();
                            }
                        }
                        lines.push(ok(Value::string(line)));
                    }
                    // A line that is no UTF-8 is `InvalidInput`, named by
                    // its number and the path.
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        ended = true;
                        let number = read_lines + 1;
                        let why = format!("line {number} of {path} is not UTF-8");
                        lines.push(Value::variant(
                            bv::ERR,
                            [Value::variant(bv::IO_INVALID_INPUT, [Value::string(why)])],
                        ));
                        break;
                    }
                    Err(e) => {
                        ended = true;
                        lines.push(path_err(&e, &path));
                        break;
                    }
                }
            }
            lines
        })
    }

    // (Stdin is read as a file is: a read under way is not stopped.)
    fn stdin_lines(vm) -> Result<Step, VmError> {
        reading(vm, "stream.stdin_lines", ON_A_FILE, stdin_line_reads())
    }

    #[cfg(feature = "tcp")]
    fn tcp_chunks(vm, stream: TcpStream, size: i64) -> Result<Step, VmError> {
        let Ok(n @ 1..) = usize::try_from(size) else {
            return Err(VmError::new("stream.tcp_chunks: size must be positive".into()));
        };
        let held = match Held::new(stream) {
            Ok(held) => held,
            Err(e) => return Ok(failed_stream(vm, super::tcp::tcp_io_err(super::tcp::Doing::Using, &e))),
        };
        let on = on_held(&held);
        reading(
            vm,
            "stream.tcp_chunks",
            on,
            one_a_read(move || {
                let (conn, _reading) = held.for_read()?;
                let mut buf = vec![0u8; n.min(READ_AT_ONCE)];
                let read = conn.read_unless_woken(&mut buf, &held.wake);
                match read {
                    // The stream was let go of: nothing was read.
                    Ok(None) => None,
                    Ok(Some(0)) => {
                        held.release();
                        None
                    }
                    Ok(Some(read)) => {
                        buf.truncate(read);
                        Some(ok(Value::Bytes(Arc::new(buf))))
                    }
                    Err(e) => {
                        held.release();
                        Some(super::tcp::tcp_io_err(super::tcp::Doing::Using, &e))
                    }
                }
            }),
        )
    }

    #[cfg(feature = "tcp")]
    fn tcp_lines(vm, stream: TcpStream) -> Result<Step, VmError> {
        let held = match Held::new(stream) {
            Ok(held) => held,
            Err(e) => return Ok(failed_stream(vm, super::tcp::tcp_io_err(super::tcp::Doing::Using, &e))),
        };
        let on = on_held(&held);
        reading(vm, "stream.tcp_lines", on, tcp_line_reads(held))
    }

    // ── Transforms ────────────────────────────────────────────────────────

    fn map(vm, ch: Chan, f: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), f.clone());
        stage(
            vm,
            "stream.map",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => Next::Call(fn_val.clone(), vec![v]),
                    Got::Returned(result) => Next::Emit(result),
                    Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    // Each element with its index, counting from 0 as
    // `list.enumerate` does.
    fn enumerate(vm, ch: Chan) -> Result<Step, VmError> {
        let in_ch = ch.clone();
        let mut index: i64 = 0;
        stage(
            vm,
            "stream.enumerate",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => {
                        let pair = Value::tuple(vec![Value::Int(index), v]);
                        index = index.saturating_add(1);
                        Next::Emit(pair)
                    }
                    Got::End => Next::Done(Value::Unit),
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn filter(vm, ch: Chan, pred: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), pred.clone());
        let mut held = None;
        stage(
            vm,
            "stream.filter",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => {
                        held = Some(v.clone());
                        Next::Call(fn_val.clone(), vec![v])
                    }
                    Got::Returned(Value::Bool(true)) => match held.take() {
                        Some(v) => Next::Emit(v),
                        None => return unexpected(),
                    },
                    Got::Returned(_) => Next::Take(0),
                    Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn flat_map(vm, ch: Chan, f: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), f.clone());
        // What is left of the list the function returned last.
        let mut items: Option<crate::value::IntoIter> = None;
        stage(
            vm,
            "stream.flat_map",
            vec![in_ch],
            DRAINS,
            move |got| {
                match got {
                    Got::Value(_, v) => return Ok(Next::Call(fn_val.clone(), vec![v])),
                    Got::Returned(returned) => match List::take(&returned) {
                        Some(xs) => items = Some(xs.clone().into_iter()),
                        None => return Err(unsound("stream.flat_map", "f")),
                    },
                    Got::End => return Ok(Next::Done(Value::Unit)),
                    Got::Start | Got::Emitted => {}
                    Got::Io(_) => return unexpected(),
                }
                Ok(match items.as_mut().and_then(Iterator::next) {
                    Some(item) => Next::Emit(item),
                    None => Next::Take(0),
                })
            },
        )
    }

    fn take(vm, ch: Chan, n: i64) -> Result<Step, VmError> {
        // It reads nothing: its input is cut as a `take` that has all
        // it reads cuts it.
        if n <= 0 {
            ch.abandon(vm.scheduler().wake());
            return Ok(closed_channel(vm));
        }
        let in_ch = ch.clone();
        let mut left = n;
        stage(
            vm,
            "stream.take",
            vec![in_ch],
            STOPS,
            move |got| {
                Ok(match got {
                    Got::Start => Next::Take(0),
                    Got::Value(_, v) => {
                        left -= 1;
                        Next::Emit(v)
                    }
                    Got::Emitted if left > 0 => Next::Take(0),
                    Got::Emitted | Got::End => Next::Done(Value::Unit),
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn drop(vm, ch: Chan, n: i64) -> Result<Step, VmError> {
        let in_ch = ch.clone();
        let mut left = n.max(0);
        stage(
            vm,
            "stream.drop",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(..) if left > 0 => {
                        left -= 1;
                        Next::Take(0)
                    }
                    Got::Value(_, v) => Next::Emit(v),
                    Got::End => Next::Done(Value::Unit),
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn take_while(vm, ch: Chan, pred: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), pred.clone());
        let mut held = None;
        stage(
            vm,
            "stream.take_while",
            vec![in_ch],
            STOPS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => {
                        held = Some(v.clone());
                        Next::Call(fn_val.clone(), vec![v])
                    }
                    Got::Returned(Value::Bool(true)) => match held.take() {
                        Some(v) => Next::Emit(v),
                        None => return unexpected(),
                    },
                    Got::Returned(_) | Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn drop_while(vm, ch: Chan, pred: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), pred.clone());
        let mut dropping = true;
        let mut held = None;
        stage(
            vm,
            "stream.drop_while",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) if dropping => {
                        held = Some(v.clone());
                        Next::Call(fn_val.clone(), vec![v])
                    }
                    Got::Value(_, v) => Next::Emit(v),
                    Got::Returned(Value::Bool(true)) => Next::Take(0),
                    Got::Returned(_) => {
                        dropping = false;
                        match held.take() {
                            Some(v) => Next::Emit(v),
                            None => return unexpected(),
                        }
                    }
                    Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn chunks(vm, ch: Chan, n: i64) -> Result<Step, VmError> {
        let Ok(n @ 1..) = usize::try_from(n) else {
            return Err(VmError::new("stream.chunks: n must be positive".into()));
        };
        let in_ch = ch.clone();
        // (It grows as values arrive: `n` may be any number. A chunk is
        // a list, so it ends the stage where it would grow past the
        // cap of one.)
        let mut buffer: Vec<Value> = Vec::new();
        let mut ended = false;
        stage(
            vm,
            "stream.chunks",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start => Next::Take(0),
                    Got::Emitted if ended => Next::Done(Value::Unit),
                    Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => {
                        fits("stream.chunks", Kind::Chunk, buffer.len() + 1)?;
                        buffer.push(v);
                        if buffer.len() < n {
                            Next::Take(0)
                        } else {
                            let chunk = std::mem::take(&mut buffer);
                            Next::Emit(Value::list(chunk))
                        }
                    }
                    Got::End if buffer.is_empty() => Next::Done(Value::Unit),
                    Got::End => {
                        ended = true;
                        Next::Emit(Value::list(std::mem::take(&mut buffer)))
                    }
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn scan(vm, ch: Chan, init: &Value, f: &Value) -> Result<Step, VmError> {
        let in_ch = ch.clone();
        let mut acc = init.clone();
        let fn_val = f.clone();
        stage(
            vm,
            "stream.scan",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => Next::Call(fn_val.clone(), vec![acc.clone(), v]),
                    Got::Returned(new_acc) => {
                        acc = new_acc;
                        Next::Emit(acc.clone())
                    }
                    Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn dedup(vm, ch: Chan) -> Result<Step, VmError> {
        let in_ch = ch.clone();
        let mut prev: Option<Value> = None;
        stage(
            vm,
            "stream.dedup",
            vec![in_ch],
            DRAINS,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    // Functions are not comparable: the stage fails, and the
                    // sink at the end of the pipeline raises the error.
                    Got::Value(_, v) if v.contains_fn() => {
                        return Err(VmError::new(
                            "stream.dedup: type 'Fn' does not implement Equal".to_string(),
                        ));
                    }
                    Got::Value(_, v) if prev.as_ref() == Some(&v) => Next::Take(0),
                    Got::Value(_, v) => {
                        prev = Some(v.clone());
                        Next::Emit(v)
                    }
                    Got::End => Next::Done(Value::Unit),
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                })
            },
        )
    }

    fn buffered(vm, ch: Chan, n: i64) -> Result<Step, VmError> {
        let in_ch = ch.clone();
        let n = usize::try_from(n).unwrap_or(0);
        // It holds `n` values that nobody asked for; for a reader that
        // drains it, as many as any stage, or `n` if that is more.
        let output = Output {
            capacity: n.max(AHEAD),
            ahead: n,
            drains: true,
        };
        stage(vm, "stream.buffered", vec![in_ch], output, forward)
    }

    // ── Combinators ───────────────────────────────────────────────────────

    fn merge(vm, chs: List) -> Result<Step, VmError> {
        let channels = channel_list("stream.merge", chs)?;
        let mut open = channels.len();
        stage(vm, "stream.merge", channels, DRAINS, move |got| {
            match got {
                Got::Value(_, v) => return Ok(Next::Emit(v)),
                Got::End => open -= 1,
                Got::Start | Got::Emitted => {}
                Got::Returned(_) | Got::Io(_) => return unexpected(),
            }
            Ok(match open {
                0 => Next::Done(Value::Unit),
                _ => Next::TakeAny,
            })
        })
    }

    fn zip(vm, a: Chan, b: Chan) -> Result<Step, VmError> {
        let (a, b) = (a.clone(), b.clone());
        let mut left = None;
        stage(vm, "stream.zip", vec![a, b], STOPS, move |got| {
            Ok(match got {
                Got::Start | Got::Emitted => Next::Take(0),
                Got::Value(0, va) => {
                    left = Some(va);
                    Next::Take(1)
                }
                Got::Value(_, vb) => match left.take() {
                    Some(va) => Next::Emit(Value::tuple(vec![va, vb])),
                    None => return unexpected(),
                },
                Got::End => Next::Done(Value::Unit),
                Got::Returned(_) | Got::Io(_) => return unexpected(),
            })
        })
    }

    fn concat(vm, chs: List) -> Result<Step, VmError> {
        let channels = channel_list("stream.concat", chs)?;
        let count = channels.len();
        let mut current = 0;
        stage(
            vm,
            "stream.concat",
            channels,
            DRAINS,
            move |got| {
                match got {
                    Got::Value(_, v) => return Ok(Next::Emit(v)),
                    Got::End => current += 1,
                    Got::Start | Got::Emitted => {}
                    Got::Returned(_) | Got::Io(_) => return unexpected(),
                }
                Ok(if current < count {
                    Next::Take(current)
                } else {
                    Next::Done(Value::Unit)
                })
            },
        )
    }

    // ── Sinks ─────────────────────────────────────────────────────────────

    fn collect(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut out = Vec::new();
        sink("stream.collect", ch, true, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => {
                    fits("stream.collect", Kind::List, out.len() + 1)?;
                    out.push(v);
                    Next::Take(0)
                }
                Got::End => Next::Done(Value::list(std::mem::take(&mut out))),
                _ => return unexpected(),
            })
        })
    }

    fn fold(ch: Chan, init: &Value, f: &Value) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut acc = init.clone();
        let callback = f.clone();
        sink("stream.fold", ch, true, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => Next::Call(callback.clone(), vec![acc.clone(), v]),
                Got::Returned(next) => {
                    acc = next;
                    Next::Take(0)
                }
                Got::End => Next::Done(acc.clone()),
                _ => return unexpected(),
            })
        })
    }

    fn length(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut n: i64 = 0;
        sink("stream.length", ch, true, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(..) => {
                    n += 1;
                    Next::Take(0)
                }
                Got::End => Next::Done(Value::Int(n)),
                _ => return unexpected(),
            })
        })
    }

    fn first(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        sink("stream.first", ch, false, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => Next::Done(Value::variant(bv::SOME, [v])),
                Got::End => Next::Done(Value::variant(bv::NONE, [])),
                _ => return unexpected(),
            })
        })
    }

    fn last(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut last: Option<Value> = None;
        sink("stream.last", ch, true, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => {
                    last = Some(v);
                    Next::Take(0)
                }
                Got::End => Next::Done(match last.take() {
                    Some(v) => Value::variant(bv::SOME, [v]),
                    None => Value::variant(bv::NONE, []),
                }),
                _ => return unexpected(),
            })
        })
    }

    #[cfg(feature = "tcp")]
    fn write_to_tcp(ch: Chan, stream: TcpStream) -> Result<Step, VmError> {
        let (ch, stream_handle) = (ch.clone(), stream.clone());
        let on = on_tcp(&stream_handle);
        writing("stream.write_to_tcp", ch, on, move |bytes| {
            let Some(bytes) = bytes else {
                return Ok(());
            };
            stream_handle.write_all(bytes).map_err(|e| super::tcp::tcp_io_err(super::tcp::Doing::Using, &e))
        })
    }

    fn write_to_file(ch: Chan, path: &str) -> Result<Step, VmError> {
        let (ch, path) = (ch.clone(), path.to_string());
        let mut file: Option<std::fs::File> = None;
        writing("stream.write_to_file", ch, ON_A_FILE, move |bytes| {
            use std::io::Write;
            if file.is_none() {
                file = Some(std::fs::File::create(&path).map_err(|e| path_err(&e, &path))?);
            }
            let Some(file) = file.as_mut() else {
                return Ok(());
            };
            match bytes {
                Some(bytes) => file.write_all(bytes).map_err(|e| path_err(&e, &path)),
                None => file.flush().map_err(|e| path_err(&e, &path)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of `input`, given `piece` bytes a read, and how many
    /// bytes were looked at for a `\n`.
    fn piece_lines_of(input: &[u8], piece: usize) -> (Vec<String>, u64) {
        let mut at = 0;
        let input = input.to_vec();
        let mut lines = LineReads::new(move |buf: &mut [u8]| {
            let n = piece.min(buf.len()).min(input.len() - at);
            buf[..n].copy_from_slice(&input[at..at + n]);
            at += n;
            Ok(n)
        });
        let mut got = Vec::new();
        loop {
            let read = lines.next_lines();
            if read.is_empty() {
                break;
            }
            // A line's text, or "ERR" for an error.
            got.extend(read.iter().map(|line| match line {
                Value::Variant(v) if v.is(bv::OK) => match v.fields() {
                    [Value::String(text, _)] => text.to_string(),
                    _ => "?".to_string(),
                },
                _ => "ERR".to_string(),
            }));
        }
        (got, lines.looked)
    }

    /// A line that many reads bring is looked at once, not again from
    /// its start at each read: one line of 2,000,000 bytes in reads of
    /// 100 bytes looks at 2,000,000 bytes and a few, not 2 * 10^10.
    #[test]
    fn a_long_line_over_many_reads_is_looked_at_once() {
        let mut input = vec![b'a'; 2_000_000];
        input.extend_from_slice(b"\nb\r\nlast");
        let (lines, looked) = piece_lines_of(&input, 100);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), 2_000_000);
        assert_eq!(lines[1..], ["b".to_string(), "last".to_string()]);
        assert!(
            looked <= input.len() as u64,
            "looked at {looked} bytes of {}",
            input.len()
        );
    }

    #[test]
    fn line_reads_give_the_lines_before_one_that_is_no_utf8() {
        let (lines, _) = piece_lines_of(b"one\ntwo\n\xff\nfour\n", 3);
        assert_eq!(lines, ["one", "two", "ERR"]);
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A source like one on a connection: its items are what the test
    /// sends, and `stop` (the shutdown of the connection) is a flag.
    /// The VM, the source's output, the sender of the items, and the
    /// flag.
    fn source() -> (Vm, Arc<Channel>, mpsc::Sender<Value>, Arc<AtomicBool>) {
        let mut vm = Vm::new(crate::HostIo::process());
        let (items, next) = mpsc::channel::<Value>();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let stop: Stop = Arc::new(move || {
            flag.store(true, Ordering::SeqCst);
            true
        });
        let on = On {
            failure: io_failure,
            stop: Some(stop),
            timed_out: None,
        };
        let started = reading(
            &mut vm,
            "test.source",
            on,
            one_a_read(move || next.recv().ok()),
        );
        let Ok(Step::Done(Value::Channel(out))) = started else {
            panic!("the source did not start");
        };
        (vm, out, items, stopped)
    }

    /// Wait, for at most ten seconds, until `done` holds.
    fn until(what: &str, done: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < limit, "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A source whose read could not run (here: it panicked) ends with
    /// the error of its module as its last item, not as if its input
    /// had ended: a source on a connection with a `TcpError`.
    #[test]
    #[cfg(feature = "tcp")]
    fn a_source_whose_read_could_not_run_ends_with_its_modules_error() {
        let mut vm = Vm::new(crate::HostIo::process());
        let stream = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| std::net::TcpStream::connect(l.local_addr()?))
            .expect("a connection");
        let conn = crate::runtime::handle::TcpStreamHandle::plain(0, stream);
        let started = reading(&mut vm, "test.source", on_tcp(&conn), || panic!("no read"));
        let Ok(Step::Done(Value::Channel(out))) = started else {
            panic!("the source did not start");
        };
        until("no item came", || !out.is_empty() || out.is_closed());
        let TryReceive::Value(Value::Variant(item)) = out.try_receive(vm.scheduler().wake()) else {
            panic!("the source ended without its error");
        };
        assert!(item.is(bv::ERR));
        let [Value::Variant(error)] = item.fields() else {
            panic!("an Err with one field");
        };
        assert!(error.is(bv::TCP_OTHER), "{}", error.name());
        until("the source did not end", || out.is_closed());
    }

    /// A connection whose bytes are `data`, given at most `most` a
    /// read, then the end or (with `fail`) that error; and how many
    /// reads it was asked for, and the most bytes one read asked for.
    #[cfg(feature = "tcp")]
    fn counted(
        data: &[u8],
        most: usize,
        fail: Option<std::io::ErrorKind>,
    ) -> (
        Arc<crate::runtime::handle::TcpStreamHandle>,
        Arc<[std::sync::atomic::AtomicUsize; 2]>,
    ) {
        use crate::runtime::handle::{ReadWrite, Shutter, TcpStreamHandle};
        use std::sync::atomic::AtomicUsize;
        struct Counted {
            data: std::io::Cursor<Vec<u8>>,
            most: usize,
            fail: Option<std::io::ErrorKind>,
            reads: Arc<[AtomicUsize; 2]>,
        }
        impl std::io::Read for Counted {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads[0].fetch_add(1, Ordering::SeqCst);
                self.reads[1].fetch_max(buf.len(), Ordering::SeqCst);
                let most = buf.len().min(self.most);
                match (self.data.read(&mut buf[..most])?, self.fail) {
                    (0, Some(fail)) => Err(fail.into()),
                    (read, _) => Ok(read),
                }
            }
        }
        impl std::io::Write for Counted {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl ReadWrite for Counted {}
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let _peer =
            std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (sock, _) = listener.accept().expect("accept");
        let shutter = Shutter::default();
        assert!(shutter.set(&sock));
        let reads = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let fake = Counted {
            data: std::io::Cursor::new(data.to_vec()),
            most,
            fail,
            reads: reads.clone(),
        };
        let conn = TcpStreamHandle::whole(0, &sock, &shutter, move || {
            Ok(Box::new(fake) as Box<dyn ReadWrite>)
        })
        .expect("a connection");
        (conn, reads)
    }

    /// Every item of the reads of `stream.tcp_lines` until they end, as
    /// text: a line as itself, an error as `Err:` and its variant's
    /// name; and how many reads of the source there were.
    #[cfg(feature = "tcp")]
    fn lines_of(conn: &Arc<crate::runtime::handle::TcpStreamHandle>) -> (Vec<String>, usize) {
        let mut read = tcp_line_reads(Held::new(conn).expect("a wake"));
        let (mut items, mut calls) = (Vec::new(), 0);
        loop {
            calls += 1;
            let got = read();
            if got.is_empty() {
                return (items, calls);
            }
            for item in got {
                let Value::Variant(item) = item else {
                    panic!("an item that is no Result");
                };
                let [field] = item.fields() else {
                    panic!("a Result with one field");
                };
                items.push(match (item.is(bv::OK), field) {
                    (true, Value::String(line, _)) => line.to_string(),
                    (false, Value::Variant(error)) => format!("Err:{}", error.name()),
                    _ => panic!("an item that is no Result of a line"),
                });
            }
        }
    }

    /// `stream.tcp_lines` reads a buffer at a time, not a byte: 10,000
    /// lines of 40 bytes take about a hundred reads of 4 KiB on a
    /// connection that cannot read without waiting (TLS; they took one
    /// read a byte, 400,001, and 430 times as long as `tcp_chunks` over
    /// loopback). No read asks for more than 4 KiB. The lines of one
    /// read come in one read of the source.
    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_lines_reads_a_buffer_at_a_time() {
        let line = format!("{}\n", "k".repeat(39));
        let (conn, reads) = counted(line.repeat(10_000).as_bytes(), usize::MAX, None);
        let (lines, calls) = lines_of(&conn);
        assert_eq!(lines.len(), 10_000);
        assert!(lines.iter().all(|l| *l == line[..39]));
        let most = reads[1].load(Ordering::SeqCst);
        let reads = reads[0].load(Ordering::SeqCst);
        assert!(
            reads <= 400_000 / LINES_READ + 3,
            "{reads} reads of the connection"
        );
        assert!(most <= LINES_READ, "a read asked for {most} bytes");
        assert!(calls <= reads + 1, "{calls} reads of the source");
    }

    /// On a plain connection, with a line in hand, what is there is read
    /// on without waiting, 4 KiB at a time up to 64 KiB: 32,000 bytes of
    /// lines that the peer sent before the source began come in one read
    /// of the source (one a line, 801, a byte at a time).
    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_lines_reads_what_is_there_in_one_read_on_a_plain_connection() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer =
            std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (sock, _) = listener.accept().expect("accept");
        let line = format!("{}\n", "k".repeat(39));
        peer.write_all(line.repeat(800).as_bytes())
            .expect("the lines are sent");
        peer.shutdown(std::net::Shutdown::Write)
            .expect("the peer is done");
        let conn = crate::runtime::handle::TcpStreamHandle::plain(0, sock);
        let (lines, calls) = lines_of(&conn);
        assert_eq!(lines.len(), 800);
        // The lines in one read of the source, then the end: 2. Every
        // read one that waits, it would be 9.
        assert!(calls <= 4, "{calls} reads of the source");
    }

    /// The lines are what they were when they were read a byte at a
    /// time, however the bytes come: `\n` and `\r\n` end a line, a lone
    /// `\r` is kept, an empty line is one, a final `\n` makes no empty
    /// last line, and what follows the last `\n` is the last line (a
    /// final `\r` with it).
    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_lines_gives_the_same_lines_however_the_bytes_come() {
        let cases: [(&[u8], &[&str]); 5] = [
            (b"a\r\nb\n\nc\rd\ne\r", &["a", "b", "", "c\rd", "e\r"]),
            (b"x\n", &["x"]),
            (b"", &[]),
            (b"\n\r\n", &["", ""]),
            ("é\n日本\r\nz".as_bytes(), &["é", "日本", "z"]),
        ];
        for (data, want) in cases {
            for most in [1, 2, 3, 7, usize::MAX] {
                let (conn, _) = counted(data, most, None);
                assert_eq!(lines_of(&conn).0, want, "{data:?} read {most} at a time");
                assert!(
                    !conn.is_closed(),
                    "a source that ends with the peer shuts nothing down"
                );
            }
        }
    }

    /// A line that is no UTF-8 ends the source after the lines before
    /// it, in a read of its own: the connection is shut down there, not
    /// when the bytes came in. A read that fails ends the source with
    /// its error, after the complete lines.
    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_lines_ends_at_a_line_that_is_no_utf8_or_a_read_that_fails() {
        let (conn, _) = counted(b"a\nb\n\xff\nc\n", usize::MAX, None);
        let mut read = tcp_line_reads(Held::new(&conn).expect("a wake"));
        let mut lines = 0;
        let ended = loop {
            let got = read();
            if got.iter().any(|item| ok_inner(item).is_none()) {
                break got;
            }
            lines += got.len();
            assert!(!got.is_empty(), "the source ended without its error");
            assert!(
                !conn.is_closed(),
                "shut down before the lines before it were emitted"
            );
        };
        assert_eq!(lines, 2);
        assert!(matches!(&ended[..], [Value::Variant(e)] if e.is(bv::ERR)));
        assert!(conn.is_closed());
        assert!(read().is_empty());
        for (data, most) in [
            (&b"a\n\xff"[..], usize::MAX),
            (b"a\n\xff", 1),
            (b"\xff\na\n", 2),
        ] {
            let (conn, _) = counted(data, most, None);
            let (lines, _) = lines_of(&conn);
            assert_eq!(
                lines.last().map(|l| l.starts_with("Err:")),
                Some(true),
                "{data:?}: {lines:?}"
            );
            assert!(conn.is_closed());
        }
        let (conn, _) = counted(b"a\nb\nhalf", 3, Some(std::io::ErrorKind::ConnectionReset));
        let (lines, _) = lines_of(&conn);
        assert_eq!(lines[..2], ["a", "b"]);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[2].starts_with("Err:"), "{lines:?}");
    }

    /// The reader of a source on a connection is gone when the source
    /// comes with an item for it: the source ends, and runs its stop on
    /// the connection (here a flag; `stream.tcp_*`: it lets go of it),
    /// as it does when it is stopped in a read.
    #[test]
    fn a_source_that_finds_its_output_closed_stops_its_connection() {
        let (vm, out, items, stopped) = source();
        out.close(Close::default(), vm.scheduler().wake());
        items.send(Value::Int(1)).expect("the source reads");
        until("the connection was left open", || {
            stopped.load(Ordering::SeqCst)
        });
    }

    /// A pipe on a connection whose shut-down is armed, and the flag
    /// that the shut-down sets.
    fn armed_pipe(
        logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
    ) -> (Pipe, Arc<AtomicBool>) {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let mut pipe = Pipe::new("test.source", vec![], logic);
        pipe.armed = Some(Arc::new(Armed::new(Arc::new(move || {
            flag.store(true, Ordering::SeqCst);
            true
        }))));
        (pipe, stopped)
    }

    /// A source on a connection that is stopped before it has run (a
    /// worker ends a task that was cancelled before its first slice:
    /// its frames are abandoned) runs its stop on its connection; it does not
    /// take the stop for an end of its own. (Before, its abandon
    /// disarmed the shut-down, and a reader that stopped it at that
    /// moment left the connection open.)
    #[test]
    fn a_source_abandoned_before_it_has_run_shuts_its_connection() {
        let mut vm = Vm::new(crate::HostIo::process());
        let (mut pipe, stopped) = armed_pipe(|_| unexpected());
        let armed = pipe.armed.clone().expect("armed");
        pipe.abandon(&mut vm);
        assert!(
            stopped.load(Ordering::SeqCst),
            "the connection was left open"
        );
        // Once: whoever stops it later shuts nothing again.
        stopped.store(false, Ordering::SeqCst);
        armed.fire();
        assert!(!stopped.load(Ordering::SeqCst));
    }

    /// A source on a connection that ends by itself leaves the
    /// connection to the program: a reader that goes afterwards shuts
    /// nothing down.
    #[test]
    fn a_source_that_ended_by_itself_leaves_its_connection() {
        let mut vm = Vm::new(crate::HostIo::process());
        let (mut pipe, stopped) = armed_pipe(|_| Ok(Next::Done(Value::Unit)));
        let armed = pipe.armed.clone().expect("armed");
        assert!(matches!(
            pipe.run(&mut vm, Next::Done(Value::Unit)),
            Ok(Step::Done(_))
        ));
        armed.fire();
        pipe.abandon(&mut vm);
        assert!(
            !stopped.load(Ordering::SeqCst),
            "the connection was shut down"
        );
    }

    /// The same when the source waits with an item for room in its
    /// output, which is full, and the reader goes then.
    #[test]
    fn a_source_whose_output_closes_while_it_waits_for_room_stops_its_connection() {
        let (vm, out, items, stopped) = source();
        for item in 0..=AHEAD {
            items
                .send(Value::Int(item as i64))
                .expect("the source reads");
        }
        // (It waits to be asked for room, with its item.)
        until("the source did not wait for room", || {
            out.len() == AHEAD && out.feeders() > 0
        });
        assert!(!stopped.load(Ordering::SeqCst));
        out.close(Close::default(), vm.scheduler().wake());
        until("the connection was left open", || {
            stopped.load(Ordering::SeqCst)
        });
    }

    /// A stage that was stopped takes nothing more from its input: a
    /// cancelled task still runs for some steps, and the stage of one
    /// that came to take its next element there, which lies ready,
    /// leaves it where it is and ends, with its output closed. A sink
    /// in a task that was cancelled takes nothing either, and ends the
    /// task: it begins the wait that such a task is refused.
    #[test]
    fn a_stage_that_was_stopped_takes_nothing_more() {
        let mut vm = Vm::new(crate::HostIo::process());
        let wake = vm.scheduler().clone();
        let input = Channel::new(vm.next_channel_id(), 4);
        for item in 0..3 {
            let sent = input.try_send(Value::Int(item), wake.wake());
            assert!(matches!(sent, TrySend::Sent));
        }
        let stage = |vm: &mut Vm, stopped: bool| {
            // (Room in its output: it may take without being asked.)
            let out = Channel::new(vm.next_channel_id(), 1);
            let handle = Arc::new(TaskHandle::new(vm.next_task_id()));
            if stopped {
                handle.cancel(wake.wake());
            }
            // What `stream.filter` with a predicate that is never true
            // does: take, call, take.
            let mut pipe = Pipe::new("test.stage", vec![input.clone()], |got| {
                Ok(match got {
                    Got::Start | Got::Returned(_) => Next::Take(0),
                    Got::Value(_, v) => Next::Call(Value::Unit, vec![v]),
                    _ => Next::Done(Value::Unit),
                })
            });
            pipe.out = Some(Arc::downgrade(&out));
            // The VM of a stage's task has the task's flag.
            vm.cancelled = Some(handle.cancel_flag());
            pipe.handle = Some(handle);
            (pipe.resume(vm, Value::Unit), out)
        };
        let (step, out) = stage(&mut vm, true);
        assert!(matches!(step, Ok(Step::Done(Value::Unit))));
        assert_eq!(input.len(), 3, "a stopped stage took an element");
        assert!(out.is_closed());
        // A sink has no output and no task of its own.
        let mut sink = Pipe::new("test.sink", vec![input.clone()], |got| {
            Ok(match got {
                Got::Start | Got::Value(..) => Next::Take(0),
                _ => Next::Done(Value::Unit),
            })
        });
        let step = sink.resume(&mut vm, Value::Unit);
        assert!(matches!(step, Ok(Step::Park(wait)) if wait.arms.is_empty()));
        assert_eq!(input.len(), 3, "a sink in a cancelled task took an element");
        // (A stage that was not stopped takes the first and calls with
        // it.)
        let (step, out) = stage(&mut vm, false);
        assert!(matches!(step, Ok(Step::Call { .. })));
        assert_eq!(input.len(), 2);
        assert!(!out.is_closed());
    }
}
