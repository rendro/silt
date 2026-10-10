//! `stream.*` builtin functions: a library of channel-backed sources,
//! transforms, and sinks. The underlying primitive is `Value::Channel(_)`
//! — there is no separate stream value type.
//!
//! A source or a transform is a stage: a task of the scheduler that
//! reads its input channels, calls the user's function, and writes to
//! its output channel. It waits where any task waits (for a value, for
//! room in its output, for an I/O operation), so a thousand stages
//! cost no thread. Backpressure is the capacity of the output.
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
//!
//! Every stage and every sink is one `Pipe`: a frame that does the
//! waiting, around a function (`Logic`) that says what comes next.

use std::sync::Arc;

use parking_lot::Mutex;

use super::common::{READ_AT_ONCE, ok};
#[cfg(feature = "tcp")]
use super::typed::TcpStream;
use super::typed::{Arg, Chan, List, builtins, unsound};
use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{IoOp, Native, Step, Vm, VmError};

const DEFAULT_CAPACITY: usize = 16;

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
    /// Run a blocking operation on the I/O pool. If the pipe is
    /// dropped while it runs, the second is called to make it return.
    Io(Box<dyn FnOnce() -> Value + Send>, Option<Stop>),
    /// It is finished. A sink's value; a stage's is ignored.
    Done(Value),
}

/// The part of a stage or sink that is its own: given what happened,
/// what comes next.
/// What makes the blocking operations of a source or a sink return
/// when nobody waits for them any more (a connection is shut down).
type Stop = Arc<dyn Fn() + Send + Sync>;

type Logic = Box<dyn FnMut(Got) -> Result<Next, VmError> + Send>;

/// Where a pipe stands between two resumptions.
enum State {
    /// It goes on with this.
    Go(Got),
    /// It waits for a value; the wait's arms are these inputs.
    Taking(Vec<usize>),
    Emitting,
    Calling,
    Io(IoOp),
}

/// A stage or a sink: the frame that waits for what its [`Logic`]
/// asks for.
struct Pipe {
    name: &'static str,
    inputs: Vec<Arc<Channel>>,
    /// Which inputs have not ended.
    open: Vec<bool>,
    /// The output of a stage; a sink has none.
    out: Option<Arc<Channel>>,
    /// The handle of a stage's own task: where its failure is read
    /// when the task ends by one.
    handle: Option<Arc<TaskHandle>>,
    logic: Logic,
    state: State,
    /// Which input a wait for any of them tries first: each in turn.
    turn: usize,
    finished: bool,
    /// What ends its blocking operations, for a pipe on a connection:
    /// called when the pipe is stopped before its end, whether an
    /// operation is in flight then or not.
    stop: Option<Stop>,
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
            finished: false,
            stop: None,
        }
    }

    /// The input `index` is closed and empty. If the stage that fed it
    /// failed, this one fails with the same error.
    fn ended(&mut self, index: usize, close: Close) -> Result<Got, VmError> {
        self.open[index] = false;
        match close.failure {
            Some(failure) => Err((*failure).clone()),
            None => Ok(Got::End),
        }
    }

    /// The pipe is over: its output closes, with the failure if it
    /// ends by one, and the stages that feed its inputs are told that
    /// nobody reads them any more.
    fn finish(&mut self, vm: &Vm, failure: Option<Arc<VmError>>) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let wake = vm.scheduler().wake();
        if let Some(out) = &self.out {
            out.close(Close { failure }, wake);
        }
        for (input, open) in self.inputs.iter().zip(&self.open) {
            if *open {
                input.abandon(wake);
            }
        }
    }
}

impl Native for Pipe {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        let mut got = match std::mem::replace(&mut self.state, State::Calling) {
            State::Go(got) => got,
            State::Calling => Got::Returned(input),
            State::Taking(arms) => match vm.woken()? {
                Fired::Arm(arm, Outcome::Received(value)) => Got::Value(arms[arm], value),
                Fired::Arm(arm, Outcome::Closed(close)) => self.ended(arms[arm], close)?,
                _ => return Err(internal("was woken without a value")),
            },
            State::Emitting => match vm.woken()? {
                Fired::Arm(_, Outcome::Sent) => Got::Emitted,
                // Nobody reads the output.
                _ => {
                    self.finish(vm, None);
                    return Ok(Step::Done(Value::Unit));
                }
            },
            State::Io(op) => {
                vm.woken()?;
                Got::Io(op.cell.get().cloned().unwrap_or(Value::Unit))
            }
        };
        for _ in 0..BURST {
            match (self.logic)(got)? {
                Next::Take(index) => {
                    let input = &self.inputs[index];
                    match input.try_receive(vm.scheduler().wake()) {
                        TryReceive::Value(value) => got = Got::Value(index, value),
                        TryReceive::Closed(close) => got = self.ended(index, close)?,
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
                    let Some(out) = &self.out else {
                        return Err(internal("has no output"));
                    };
                    match out.try_send(value, vm.scheduler().wake()) {
                        TrySend::Sent => got = Got::Emitted,
                        TrySend::Closed(_) => {
                            self.finish(vm, None);
                            return Ok(Step::Done(Value::Unit));
                        }
                        TrySend::Full(value) => {
                            let wait = Wait::new(vec![Arm::Send(out.clone(), value)]);
                            self.state = State::Emitting;
                            return Ok(Step::Park(wait));
                        }
                    }
                }
                Next::Call(callee, args) => {
                    self.state = State::Calling;
                    return Ok(vm.call(callee, args));
                }
                Next::Io(operation, stop) => {
                    let mut op = vm.runtime.io_pool.submit(io_failure, operation);
                    if let Some(stop) = stop {
                        self.stop = Some(stop.clone());
                        op = op.stop_with(move || stop());
                    }
                    let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]);
                    self.state = State::Io(op);
                    return Ok(Step::Park(wait));
                }
                Next::Done(value) => {
                    self.finish(vm, None);
                    return Ok(Step::Done(value));
                }
            }
        }
        self.state = State::Go(got);
        Ok(Step::Yield)
    }

    fn abandon(&mut self, vm: &mut Vm) {
        // A pipe on a connection that is stopped before its end shuts
        // the connection down, whatever it was doing: not only when a
        // read happened to be in flight.
        if !self.finished
            && let Some(stop) = self.stop.take()
        {
            stop();
        }
        // A stage that failed hands its failure to whoever reads its
        // output: that reader raises it, so it is not reported as a
        // failure that nobody joined. A stage that was stopped closes
        // its output plainly.
        let failure = self.handle.as_ref().and_then(|handle| {
            if handle.is_cancelled() {
                return None;
            }
            let error = handle.try_get()?.err()?;
            handle.mark_joined();
            Some(Arc::new(error))
        });
        self.finish(vm, failure);
    }
}

/// Start a stage: a task that runs `logic` over `inputs` and feeds the
/// channel that is returned, of capacity `capacity`.
fn stage(
    vm: &mut Vm,
    name: &'static str,
    inputs: Vec<Arc<Channel>>,
    capacity: usize,
    logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    let out = Channel::new(vm.next_channel_id(), capacity);
    let id = vm.next_task_id();
    let handle = Arc::new(TaskHandle::with_owner(id, vm.scheduler().current_owner()));
    // Whoever reads the output and stops reading stops the stage.
    let scheduler = Arc::downgrade(vm.scheduler());
    let stopped = handle.clone();
    out.stop_feeder_with(move || {
        if let Some(scheduler) = scheduler.upgrade() {
            scheduler.cancel(&stopped);
        }
    });
    let mut pipe = Pipe::new(name, inputs, logic);
    pipe.out = Some(out.clone());
    pipe.handle = Some(handle.clone());
    let mut child = vm.spawn_child();
    child.spawned = true;
    child.push_native_frame(Box::new(pipe));
    vm.scheduler()
        .submit(id, child, handle)
        .map_err(VmError::new)?;
    Ok(Step::Done(Value::Channel(out)))
}

/// A channel that is closed and empty: the output of a stage that has
/// nothing to do (`stream.take(s, 0)`).
fn closed_channel(vm: &mut Vm) -> Step {
    let out = Channel::new(vm.next_channel_id(), 1);
    out.close(Close::default(), vm.scheduler().wake());
    Step::Done(Value::Channel(out))
}

/// Run a sink in the caller's task: its value is the call's.
fn sink(
    name: &'static str,
    input: Arc<Channel>,
    logic: impl FnMut(Got) -> Result<Next, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    Ok(Step::Run(Box::new(Pipe::new(name, vec![input], logic))))
}

/// What a logic says to an event it never asked for.
fn unexpected() -> Result<Next, VmError> {
    Err(internal("was told of something it did not wait for"))
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Wrap an `io::Error` into `Err(IoError)` — used by file-backed stream
/// sources (`file_lines`, `file_chunks`, `write_to_file`).
fn err_io(e: &std::io::Error) -> Value {
    use std::io::ErrorKind;
    let msg = e.to_string();
    let inner = match e.kind() {
        ErrorKind::NotFound => Value::variant(bv::IO_NOT_FOUND, vec![Value::String(msg.into())]),
        ErrorKind::PermissionDenied => {
            Value::variant(bv::IO_PERMISSION_DENIED, vec![Value::String(msg.into())])
        }
        ErrorKind::AlreadyExists => {
            Value::variant(bv::IO_ALREADY_EXISTS, vec![Value::String(msg.into())])
        }
        ErrorKind::InvalidInput | ErrorKind::InvalidData => {
            Value::variant(bv::IO_INVALID_INPUT, vec![Value::String(msg.into())])
        }
        ErrorKind::Interrupted => Value::variant(bv::IO_INTERRUPTED, vec![]),
        ErrorKind::UnexpectedEof => Value::variant(bv::IO_UNEXPECTED_EOF, vec![]),
        ErrorKind::WriteZero => Value::variant(bv::IO_WRITE_ZERO, vec![]),
        _ => Value::variant(bv::IO_UNKNOWN, vec![Value::String(msg.into())]),
    };
    Value::variant(bv::ERR, vec![inner])
}

/// Build an `Err(IoUnknown(msg))` for string-only failures.
fn err_io_unknown(s: impl Into<Arc<str>>) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::IO_UNKNOWN,
            vec![Value::String(s.into())],
        )],
    )
}

/// Wrap an `io::Error` into `Err(TcpError)` — used by tcp-backed stream
/// sources and `write_to_tcp`.
#[cfg(feature = "tcp")]
fn err_tcp(e: &std::io::Error) -> Value {
    use std::io::ErrorKind;
    let msg = e.to_string();
    let inner = match e.kind() {
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::NotConnected
        | ErrorKind::AddrInUse
        | ErrorKind::AddrNotAvailable
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable => {
            Value::variant(bv::TCP_CONNECT, vec![Value::String(msg.into())])
        }
        ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => {
            Value::variant(bv::TCP_CLOSED, vec![])
        }
        ErrorKind::TimedOut | ErrorKind::WouldBlock => Value::variant(bv::TCP_TIMEOUT, vec![]),
        _ => Value::variant(bv::TCP_UNKNOWN, vec![Value::String(msg.into())]),
    };
    Value::variant(bv::ERR, vec![inner])
}

/// For a source or a sink on `stream`: its operations return when the
/// connection is shut down.
#[cfg(feature = "tcp")]
fn stopper(stream: &Arc<crate::runtime::handle::TcpStreamHandle>) -> Option<Stop> {
    let stream = stream.clone();
    Some(Arc::new(move || stream.shut_down()))
}

/// A source that reads from something that blocks: `read` gives the
/// next item, `None` at the end. An `Err(_)` item is the last.
fn reading(
    vm: &mut Vm,
    name: &'static str,
    stop: Option<Stop>,
    read: impl FnMut() -> Option<Value> + Send + 'static,
) -> Result<Step, VmError> {
    let read = Arc::new(Mutex::new(read));
    let mut failed = false;
    stage(vm, name, vec![], DEFAULT_CAPACITY, move |got| {
        Ok(match got {
            Got::Start | Got::Emitted if !failed => {
                let read = read.clone();
                Next::Io(
                    Box::new(move || match (read.lock())() {
                        Some(item) => Value::variant(bv::SOME, vec![item]),
                        None => Value::variant(bv::NONE, vec![]),
                    }),
                    stop.clone(),
                )
            }
            Got::Io(Value::Variant(read)) if read.is(bv::SOME) => {
                let item = read.fields().last().cloned().unwrap_or(Value::Unit);
                failed = matches!(&item, Value::Variant(item) if item.is(bv::ERR));
                Next::Emit(item)
            }
            _ => Next::Done(Value::Unit),
        })
    })
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
    stop: Option<Stop>,
    write: impl FnMut(Option<&[u8]>) -> Result<(), Value> + Send + 'static,
) -> Result<Step, VmError> {
    let write = Arc::new(Mutex::new(write));
    let mut ended = false;
    sink(name, ch, move |got| {
        let run = |bytes: Option<Arc<Vec<u8>>>| {
            let write = write.clone();
            Next::Io(
                Box::new(
                    move || match (write.lock())(bytes.as_deref().map(Vec::as_slice)) {
                        Ok(()) => ok(Value::Unit),
                        Err(e) => e,
                    },
                ),
                stop.clone(),
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
        stage(vm, "stream.from_list", vec![], DEFAULT_CAPACITY, move |_| {
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
            DEFAULT_CAPACITY,
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
        stage(vm, "stream.repeat", vec![], DEFAULT_CAPACITY, move |_| {
            Ok(Next::Emit(v.clone()))
        })
    }

    fn unfold(vm, seed: &Value, f: &Value) -> Result<Step, VmError> {
        let mut state = seed.clone();
        let fn_val = f.clone();
        stage(vm, "stream.unfold", vec![], DEFAULT_CAPACITY, move |got| {
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
            return Ok(closed_channel(vm));
        };
        let path = path.to_string();
        let mut file: Option<std::fs::File> = None;
        reading(vm, "stream.file_chunks", None, move || {
            use std::io::{Read, Seek};
            if file.is_none() {
                match std::fs::File::open(&path) {
                    Ok(opened) => file = Some(opened),
                    Err(e) => return Some(err_io(&e)),
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
                Err(e) => Some(err_io(&e)),
            }
        })
    }

    fn file_lines(vm, path: &str) -> Result<Step, VmError> {
        let path = path.to_string();
        let mut lines: Option<std::io::Lines<std::io::BufReader<std::fs::File>>> = None;
        reading(vm, "stream.file_lines", None, move || {
            use std::io::BufRead;
            if lines.is_none() {
                match std::fs::File::open(&path) {
                    Ok(file) => lines = Some(std::io::BufReader::new(file).lines()),
                    Err(e) => return Some(err_io(&e)),
                }
            }
            match lines.as_mut()?.next()? {
                Ok(line) => Some(ok(Value::String(line.into()))),
                Err(e) => Some(err_io(&e)),
            }
        })
    }

    #[cfg(feature = "tcp")]
    fn tcp_chunks(vm, stream: TcpStream, size: i64) -> Result<Step, VmError> {
        let Ok(n @ 1..) = usize::try_from(size) else {
            return Ok(closed_channel(vm));
        };
        let stream_handle = stream.clone();
        reading(
            vm,
            "stream.tcp_chunks",
            stopper(&stream_handle),
            move || {
                let mut buf = vec![0u8; n.min(READ_AT_ONCE)];
                let read = stream_handle.read(&mut buf);
                match read {
                    Ok(0) => None,
                    Ok(read) => {
                        buf.truncate(read);
                        Some(ok(Value::Bytes(Arc::new(buf))))
                    }
                    Err(e) => Some(err_tcp(&e)),
                }
            },
        )
    }

    #[cfg(feature = "tcp")]
    fn tcp_lines(vm, stream: TcpStream) -> Result<Step, VmError> {
        let stream_handle = stream.clone();
        let mut at_end = false;
        reading(vm, "stream.tcp_lines", stopper(&stream_handle), move || {
            // The stream is behind a lock and cannot be wrapped in a
            // buffered reader that outlives one call, so a line is read
            // byte by byte: the network buffer dominates the cost.
            if at_end {
                return None;
            }
            let mut current = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                let read = stream_handle.read(&mut byte);
                match read {
                    Ok(0) => {
                        at_end = true;
                        if current.is_empty() {
                            return None;
                        }
                        break;
                    }
                    Ok(_) if byte[0] == b'\n' => {
                        // Strip trailing \r if present.
                        if current.last() == Some(&b'\r') {
                            current.pop();
                        }
                        break;
                    }
                    Ok(_) => current.push(byte[0]),
                    Err(e) => return Some(err_tcp(&e)),
                }
            }
            let line = String::from_utf8_lossy(&current).to_string();
            Some(ok(Value::String(line.into())))
        })
    }

    // ── Transforms ────────────────────────────────────────────────────────

    fn map(vm, ch: Chan, f: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), f.clone());
        stage(
            vm,
            "stream.map",
            vec![in_ch],
            DEFAULT_CAPACITY,
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

    fn map_ok(vm, ch: Chan, f: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), f.clone());
        stage(
            vm,
            "stream.map_ok",
            vec![in_ch],
            DEFAULT_CAPACITY,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => match ok_inner(&v) {
                        Some(inner) => Next::Call(fn_val.clone(), vec![inner]),
                        None => Next::Emit(v),
                    },
                    Got::Returned(result) => Next::Emit(ok(result)),
                    Got::End => Next::Done(Value::Unit),
                    Got::Io(_) => return unexpected(),
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
            DEFAULT_CAPACITY,
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

    fn filter_ok(vm, ch: Chan, pred: &Value) -> Result<Step, VmError> {
        let (in_ch, fn_val) = (ch.clone(), pred.clone());
        let mut held = None;
        stage(
            vm,
            "stream.filter_ok",
            vec![in_ch],
            DEFAULT_CAPACITY,
            move |got| {
                Ok(match got {
                    Got::Start | Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => match ok_inner(&v) {
                        Some(inner) => {
                            held = Some(v);
                            Next::Call(fn_val.clone(), vec![inner])
                        }
                        None => Next::Emit(v),
                    },
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
            DEFAULT_CAPACITY,
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
        if n <= 0 {
            return Ok(closed_channel(vm));
        }
        let in_ch = ch.clone();
        let mut left = n;
        stage(
            vm,
            "stream.take",
            vec![in_ch],
            DEFAULT_CAPACITY,
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
            DEFAULT_CAPACITY,
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
            DEFAULT_CAPACITY,
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
            DEFAULT_CAPACITY,
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
        // (It grows as values arrive: `n` may be any number.)
        let mut buffer: Vec<Value> = Vec::new();
        let mut ended = false;
        stage(
            vm,
            "stream.chunks",
            vec![in_ch],
            DEFAULT_CAPACITY,
            move |got| {
                Ok(match got {
                    Got::Start => Next::Take(0),
                    Got::Emitted if ended => Next::Done(Value::Unit),
                    Got::Emitted => Next::Take(0),
                    Got::Value(_, v) => {
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
            DEFAULT_CAPACITY,
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
            DEFAULT_CAPACITY,
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
        let cap = usize::try_from(n).unwrap_or(0);
        stage(vm, "stream.buffered", vec![in_ch], cap, forward)
    }

    // ── Combinators ───────────────────────────────────────────────────────

    fn merge(vm, chs: List) -> Result<Step, VmError> {
        let channels = channel_list("stream.merge", chs)?;
        let mut open = channels.len();
        stage(vm, "stream.merge", channels, DEFAULT_CAPACITY, move |got| {
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
        stage(vm, "stream.zip", vec![a, b], DEFAULT_CAPACITY, move |got| {
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
            DEFAULT_CAPACITY,
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
        sink("stream.collect", ch, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => {
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
        sink("stream.fold", ch, move |got| {
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

    fn each(ch: Chan, f: &Value) -> Result<Step, VmError> {
        let (ch, callback) = (ch.clone(), f.clone());
        sink("stream.each", ch, move |got| {
            Ok(match got {
                Got::Start | Got::Returned(_) => Next::Take(0),
                Got::Value(_, v) => Next::Call(callback.clone(), vec![v]),
                Got::End => Next::Done(Value::Unit),
                _ => return unexpected(),
            })
        })
    }

    fn count(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut n: i64 = 0;
        sink("stream.count", ch, move |got| {
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
        sink("stream.first", ch, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => Next::Done(Value::variant(bv::SOME, vec![v])),
                Got::End => Next::Done(Value::variant(bv::NONE, vec![])),
                _ => return unexpected(),
            })
        })
    }

    fn last(ch: Chan) -> Result<Step, VmError> {
        let ch = ch.clone();
        let mut last: Option<Value> = None;
        sink("stream.last", ch, move |got| {
            Ok(match got {
                Got::Start => Next::Take(0),
                Got::Value(_, v) => {
                    last = Some(v);
                    Next::Take(0)
                }
                Got::End => Next::Done(match last.take() {
                    Some(v) => Value::variant(bv::SOME, vec![v]),
                    None => Value::variant(bv::NONE, vec![]),
                }),
                _ => return unexpected(),
            })
        })
    }

    #[cfg(feature = "tcp")]
    fn write_to_tcp(ch: Chan, stream: TcpStream) -> Result<Step, VmError> {
        let (ch, stream_handle) = (ch.clone(), stream.clone());
        let stop = stopper(&stream_handle);
        writing("stream.write_to_tcp", ch, stop, move |bytes| {
            let Some(bytes) = bytes else {
                return Ok(());
            };
            stream_handle.write_all(bytes).map_err(|e| err_tcp(&e))
        })
    }

    fn write_to_file(ch: Chan, path: &str) -> Result<Step, VmError> {
        let (ch, path) = (ch.clone(), path.to_string());
        let mut file: Option<std::fs::File> = None;
        writing("stream.write_to_file", ch, None, move |bytes| {
            use std::io::Write;
            if file.is_none() {
                file = Some(std::fs::File::create(&path).map_err(|e| err_io(&e))?);
            }
            let Some(file) = file.as_mut() else {
                return Ok(());
            };
            match bytes {
                Some(bytes) => file.write_all(bytes).map_err(|e| err_io(&e)),
                None => file.flush().map_err(|e| err_io(&e)),
            }
        })
    }
}
