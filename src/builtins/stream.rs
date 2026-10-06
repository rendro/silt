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

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

use super::common::ok;
use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Channel, Close, Fired, Outcome, TryReceive, TrySend, Wait};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{IoOp, Native, Step, Vm, VmError};

const DEFAULT_CAPACITY: usize = 16;

/// Dispatch `stream.<name>(args)`.
pub(crate) fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        // Sources
        "from_list" => from_list(vm, args),
        "from_range" => from_range(vm, args),
        "repeat" => repeat(vm, args),
        "unfold" => unfold(vm, args),
        "file_chunks" => file_chunks(vm, args),
        "file_lines" => file_lines(vm, args),
        "tcp_chunks" => tcp_chunks(vm, args),
        "tcp_lines" => tcp_lines(vm, args),

        // Transforms
        "map" => map(vm, args),
        "map_ok" => map_ok(vm, args),
        "filter" => filter(vm, args),
        "filter_ok" => filter_ok(vm, args),
        "flat_map" => flat_map(vm, args),
        "take" => take(vm, args),
        "drop" => drop_n(vm, args),
        "take_while" => take_while(vm, args),
        "drop_while" => drop_while(vm, args),
        "chunks" => chunks(vm, args),
        "scan" => scan(vm, args),
        "dedup" => dedup(vm, args),
        "buffered" => buffered(vm, args),

        // Combinators
        "merge" => merge(vm, args),
        "zip" => zip(vm, args),
        "concat" => concat(vm, args),

        // Sinks
        "collect" => collect(args),
        "fold" => fold(args),
        "each" => each(args),
        "count" => count(args),
        "first" => first(args),
        "last" => last(args),
        "write_to_tcp" => write_to_tcp(args),
        "write_to_file" => write_to_file(args),

        _ => Err(VmError::new(format!("unknown stream function: {name}"))),
    }
}

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
    /// Run a blocking operation on the I/O pool.
    Io(Box<dyn FnOnce() -> Value + Send>),
    /// It is finished. A sink's value; a stage's is ignored.
    Done(Value),
}

/// The part of a stage or sink that is its own: given what happened,
/// what comes next.
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
}

/// How many steps a pipe takes without waiting before it gives way to
/// the other tasks.
const BURST: usize = 256;

/// The error of an I/O operation of a pipe that could not run.
fn io_failure(msg: &str) -> Value {
    err_io_unknown(msg)
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
                Next::Io(operation) => {
                    let op = vm.runtime.io_pool.submit(io_failure, operation);
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
    let handle = Arc::new(TaskHandle::with_owner(
        id,
        crate::scheduler::current_task_owner(),
    ));
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

fn require_channel<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Arc<Channel>, VmError> {
    match arg {
        Value::Channel(c) => Ok(c),
        other => Err(VmError::new(format!(
            "{fn_label} requires Channel, got {}",
            super::common::value_kind(other)
        ))),
    }
}

// Round 65 dedup (DC2): see comment in `tcp.rs`. These thin wrappers
// preserve the local function names so existing call sites stay
// unchanged; the bodies live in `super::common`.
fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    super::common::require_int(arg, fn_label)
}

fn require_string<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    super::common::require_str_borrow(arg, fn_label)
}

fn require_callable<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Value, VmError> {
    match arg {
        Value::VmClosure(_)
        | Value::BuiltinFn(_)
        | Value::HostFn(_)
        | Value::VariantConstructor(..) => Ok(arg),
        other => Err(VmError::new(format!(
            "{fn_label} requires Fn, got {}",
            super::common::value_kind(other)
        ))),
    }
}

/// Wrap an `io::Error` into `Err(IoError)` — used by file-backed stream
/// sources (`file_lines`, `file_chunks`, `write_to_file`).
fn err_io(e: &std::io::Error) -> Value {
    use std::io::ErrorKind;
    let msg = e.to_string();
    let inner = match e.kind() {
        ErrorKind::NotFound => Value::variant(bv::IO_NOT_FOUND, vec![Value::String(msg)]),
        ErrorKind::PermissionDenied => {
            Value::variant(bv::IO_PERMISSION_DENIED, vec![Value::String(msg)])
        }
        ErrorKind::AlreadyExists => Value::variant(bv::IO_ALREADY_EXISTS, vec![Value::String(msg)]),
        ErrorKind::InvalidInput | ErrorKind::InvalidData => {
            Value::variant(bv::IO_INVALID_INPUT, vec![Value::String(msg)])
        }
        ErrorKind::Interrupted => Value::variant(bv::IO_INTERRUPTED, vec![]),
        ErrorKind::UnexpectedEof => Value::variant(bv::IO_UNEXPECTED_EOF, vec![]),
        ErrorKind::WriteZero => Value::variant(bv::IO_WRITE_ZERO, vec![]),
        _ => Value::variant(bv::IO_UNKNOWN, vec![Value::String(msg)]),
    };
    Value::variant(bv::ERR, vec![inner])
}

/// Build an `Err(IoUnknown(msg))` for string-only failures.
fn err_io_unknown(s: impl Into<String>) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::IO_UNKNOWN,
            vec![Value::String(s.into())],
        )],
    )
}

/// Build an `Err(TcpUnknown(msg))` for string-only failures in tcp
/// write paths.
#[cfg(feature = "tcp")]
fn err_tcp_unknown(s: impl Into<String>) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::TCP_UNKNOWN,
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
            Value::variant(bv::TCP_CONNECT, vec![Value::String(msg)])
        }
        ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => {
            Value::variant(bv::TCP_CLOSED, vec![])
        }
        ErrorKind::TimedOut | ErrorKind::WouldBlock => Value::variant(bv::TCP_TIMEOUT, vec![]),
        _ => Value::variant(bv::TCP_UNKNOWN, vec![Value::String(msg)]),
    };
    Value::variant(bv::ERR, vec![inner])
}

// ── Sources ────────────────────────────────────────────────────────────

fn from_list(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.from_list takes 1 argument".into()));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new("stream.from_list requires a List".into()));
    };
    let xs = xs.clone();
    let mut next = 0;
    stage(
        vm,
        "stream.from_list",
        vec![],
        DEFAULT_CAPACITY,
        move |_| {
            Ok(match xs.get(next) {
                Some(value) => {
                    next += 1;
                    Next::Emit(value.clone())
                }
                None => Next::Done(Value::Unit),
            })
        },
    )
}

fn from_range(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.from_range takes 2 arguments".into()));
    }
    let lo = require_int(&args[0], "stream.from_range")?;
    let hi = require_int(&args[1], "stream.from_range")?;
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

fn repeat(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.repeat takes 1 argument".into()));
    }
    let v = args[0].clone();
    stage(vm, "stream.repeat", vec![], DEFAULT_CAPACITY, move |_| {
        Ok(Next::Emit(v.clone()))
    })
}

fn unfold(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.unfold takes 2 arguments (init, fn)".into(),
        ));
    }
    let mut state = args[0].clone();
    let fn_val = require_callable(&args[1], "stream.unfold")?.clone();
    stage(vm, "stream.unfold", vec![], DEFAULT_CAPACITY, move |got| {
        Ok(match got {
            Got::Start | Got::Emitted => Next::Call(fn_val.clone(), vec![state.clone()]),
            // Fn(state) -> Option((value, next_state))
            Got::Returned(Value::Variant(name, mut fields))
                if name.is(bv::SOME) && fields.len() == 1 =>
            {
                match fields.pop() {
                    Some(Value::Tuple(mut pair)) if pair.len() == 2 => {
                        state = pair.pop().unwrap_or(Value::Unit);
                        Next::Emit(pair.pop().unwrap_or(Value::Unit))
                    }
                    _ => Next::Done(Value::Unit),
                }
            }
            _ => Next::Done(Value::Unit),
        })
    })
}

/// A source that reads from something that blocks: `read` gives the
/// next item, `None` at the end. An `Err(_)` item is the last.
fn reading(
    vm: &mut Vm,
    name: &'static str,
    read: impl FnMut() -> Option<Value> + Send + 'static,
) -> Result<Step, VmError> {
    let read = Arc::new(Mutex::new(read));
    let mut failed = false;
    stage(vm, name, vec![], DEFAULT_CAPACITY, move |got| {
        Ok(match got {
            Got::Start | Got::Emitted if !failed => {
                let read = read.clone();
                Next::Io(Box::new(move || match (read.lock())() {
                    Some(item) => Value::variant(bv::SOME, vec![item]),
                    None => Value::variant(bv::NONE, vec![]),
                }))
            }
            Got::Io(Value::Variant(name, mut fields)) if name.is(bv::SOME) => {
                let item = fields.pop().unwrap_or(Value::Unit);
                failed = matches!(&item, Value::Variant(name, _) if name.is(bv::ERR));
                Next::Emit(item)
            }
            _ => Next::Done(Value::Unit),
        })
    })
}

fn file_chunks(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.file_chunks takes 2 arguments (path, chunk_size)".into(),
        ));
    }
    let path = require_string(&args[0], "stream.file_chunks")?.to_string();
    let n = require_int(&args[1], "stream.file_chunks")?;
    if n <= 0 {
        return Ok(closed_channel(vm));
    }
    let n = n as usize;
    let mut file: Option<std::fs::File> = None;
    reading(vm, "stream.file_chunks", move || {
        use std::io::Read;
        if file.is_none() {
            match std::fs::File::open(&path) {
                Ok(opened) => file = Some(opened),
                Err(e) => return Some(err_io(&e)),
            }
        }
        let mut buf = vec![0u8; n];
        match file.as_mut()?.read(&mut buf) {
            Ok(0) => None,
            Ok(read) => {
                buf.truncate(read);
                Some(ok(Value::Bytes(Arc::new(buf))))
            }
            Err(e) => Some(err_io(&e)),
        }
    })
}

fn file_lines(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.file_lines takes 1 argument".into()));
    }
    let path = require_string(&args[0], "stream.file_lines")?.to_string();
    let mut lines: Option<std::io::Lines<std::io::BufReader<std::fs::File>>> = None;
    reading(vm, "stream.file_lines", move || {
        use std::io::BufRead;
        if lines.is_none() {
            match std::fs::File::open(&path) {
                Ok(file) => lines = Some(std::io::BufReader::new(file).lines()),
                Err(e) => return Some(err_io(&e)),
            }
        }
        match lines.as_mut()?.next()? {
            Ok(line) => Some(ok(Value::String(line))),
            Err(e) => Some(err_io(&e)),
        }
    })
}

#[cfg(feature = "tcp")]
fn tcp_chunks(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.tcp_chunks takes 2 arguments (conn, chunk_size)".into(),
        ));
    }
    let stream_handle = match &args[0] {
        Value::TcpStream(s) => s.clone(),
        _ => {
            return Err(VmError::new(
                "stream.tcp_chunks requires a TcpStream".into(),
            ));
        }
    };
    let n = require_int(&args[1], "stream.tcp_chunks")?;
    if n <= 0 {
        return Ok(closed_channel(vm));
    }
    let n = n as usize;
    reading(vm, "stream.tcp_chunks", move || {
        use std::io::Read;
        let mut buf = vec![0u8; n];
        let read = stream_handle.inner.lock().read(&mut buf);
        match read {
            Ok(0) => None,
            Ok(read) => {
                buf.truncate(read);
                Some(ok(Value::Bytes(Arc::new(buf))))
            }
            Err(e) => Some(err_tcp(&e)),
        }
    })
}

#[cfg(not(feature = "tcp"))]
fn tcp_chunks(_vm: &mut Vm, _args: &[Value]) -> Result<Step, VmError> {
    Err(VmError::new(
        "stream.tcp_chunks requires the 'tcp' feature".into(),
    ))
}

#[cfg(feature = "tcp")]
fn tcp_lines(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.tcp_lines takes 1 argument".into()));
    }
    let stream_handle = match &args[0] {
        Value::TcpStream(s) => s.clone(),
        _ => return Err(VmError::new("stream.tcp_lines requires a TcpStream".into())),
    };
    let mut at_end = false;
    reading(vm, "stream.tcp_lines", move || {
        // The stream is behind a lock and cannot be wrapped in a
        // buffered reader that outlives one call, so a line is read
        // byte by byte: the network buffer dominates the cost.
        use std::io::Read;
        if at_end {
            return None;
        }
        let mut current = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            let read = stream_handle.inner.lock().read(&mut byte);
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
        Some(ok(Value::String(line)))
    })
}

#[cfg(not(feature = "tcp"))]
fn tcp_lines(_vm: &mut Vm, _args: &[Value]) -> Result<Step, VmError> {
    Err(VmError::new(
        "stream.tcp_lines requires the 'tcp' feature".into(),
    ))
}

// ── Transforms ─────────────────────────────────────────────────────────

/// The value inside `Ok(_)`, if `v` is one.
fn ok_inner(v: &Value) -> Option<Value> {
    match v {
        Value::Variant(name, fields) if name.is(bv::OK) && fields.len() == 1 => {
            Some(fields[0].clone())
        }
        _ => None,
    }
}

fn map(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.map takes 2 arguments (channel, fn)".into(),
        ));
    }
    let in_ch = require_channel(&args[0], "stream.map")?.clone();
    let fn_val = require_callable(&args[1], "stream.map")?.clone();
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

fn map_ok(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.map_ok takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.map_ok")?.clone();
    let fn_val = require_callable(&args[1], "stream.map_ok")?.clone();
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

fn filter(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.filter takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.filter")?.clone();
    let fn_val = require_callable(&args[1], "stream.filter")?.clone();
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

fn filter_ok(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.filter_ok takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.filter_ok")?.clone();
    let fn_val = require_callable(&args[1], "stream.filter_ok")?.clone();
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

fn flat_map(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.flat_map takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.flat_map")?.clone();
    let fn_val = require_callable(&args[1], "stream.flat_map")?.clone();
    let mut items: VecDeque<Value> = VecDeque::new();
    stage(
        vm,
        "stream.flat_map",
        vec![in_ch],
        DEFAULT_CAPACITY,
        move |got| {
            match got {
                Got::Value(_, v) => return Ok(Next::Call(fn_val.clone(), vec![v])),
                Got::Returned(Value::List(xs)) => items.extend(xs.iter().cloned()),
                Got::Returned(other) => {
                    return Err(VmError::type_confusion(format!(
                        "stream.flat_map: the function must return a List, got {}",
                        super::common::value_kind(&other)
                    )));
                }
                Got::End => return Ok(Next::Done(Value::Unit)),
                Got::Start | Got::Emitted => {}
                Got::Io(_) => return unexpected(),
            }
            Ok(match items.pop_front() {
                Some(item) => Next::Emit(item),
                None => Next::Take(0),
            })
        },
    )
}

fn take(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.take takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.take")?.clone();
    let n = require_int(&args[1], "stream.take")?;
    if n <= 0 {
        return Ok(closed_channel(vm));
    }
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

fn drop_n(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.drop takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.drop")?.clone();
    let mut left = require_int(&args[1], "stream.drop")?.max(0);
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

fn take_while(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.take_while takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.take_while")?.clone();
    let fn_val = require_callable(&args[1], "stream.take_while")?.clone();
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

fn drop_while(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.drop_while takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.drop_while")?.clone();
    let fn_val = require_callable(&args[1], "stream.drop_while")?.clone();
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

fn chunks(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.chunks takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.chunks")?.clone();
    let n = require_int(&args[1], "stream.chunks")?;
    if n <= 0 {
        return Err(VmError::new("stream.chunks: n must be positive".into()));
    }
    let n = n as usize;
    let mut buffer: Vec<Value> = Vec::with_capacity(n);
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
                        let chunk = std::mem::replace(&mut buffer, Vec::with_capacity(n));
                        Next::Emit(Value::List(Arc::new(chunk)))
                    }
                }
                Got::End if buffer.is_empty() => Next::Done(Value::Unit),
                Got::End => {
                    ended = true;
                    Next::Emit(Value::List(Arc::new(std::mem::take(&mut buffer))))
                }
                Got::Returned(_) | Got::Io(_) => return unexpected(),
            })
        },
    )
}

fn scan(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "stream.scan takes 3 arguments (channel, init, fn)".into(),
        ));
    }
    let in_ch = require_channel(&args[0], "stream.scan")?.clone();
    let mut acc = args[1].clone();
    let fn_val = require_callable(&args[2], "stream.scan")?.clone();
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

fn dedup(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.dedup takes 1 argument".into()));
    }
    let in_ch = require_channel(&args[0], "stream.dedup")?.clone();
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
                Got::Value(_, v) if Vm::value_contains_fn(&v) => {
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

/// A stage that passes every value on.
fn forward(got: Got) -> Result<Next, VmError> {
    Ok(match got {
        Got::Start | Got::Emitted => Next::Take(0),
        Got::Value(_, v) => Next::Emit(v),
        Got::End => Next::Done(Value::Unit),
        Got::Returned(_) | Got::Io(_) => return unexpected(),
    })
}

fn buffered(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.buffered takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.buffered")?.clone();
    let cap = require_int(&args[1], "stream.buffered")?;
    let cap = cap.max(0) as usize;
    stage(vm, "stream.buffered", vec![in_ch], cap, forward)
}

// ── Combinators ────────────────────────────────────────────────────────

/// The channels of a `List(Channel)` argument.
fn channel_list(arg: &Value, name: &str) -> Result<Vec<Arc<Channel>>, VmError> {
    let Value::List(xs) = arg else {
        return Err(VmError::new(format!(
            "stream.{name} requires a List of Channels"
        )));
    };
    xs.iter()
        .map(|v| match v {
            Value::Channel(c) => Ok(c.clone()),
            _ => Err(VmError::new(format!(
                "stream.{name}: list elements must be Channels"
            ))),
        })
        .collect()
}

fn merge(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new(
            "stream.merge takes 1 argument (List(Channel))".into(),
        ));
    }
    let channels = channel_list(&args[0], "merge")?;
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

fn zip(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.zip takes 2 arguments".into()));
    }
    let a = require_channel(&args[0], "stream.zip")?.clone();
    let b = require_channel(&args[1], "stream.zip")?.clone();
    let mut left = None;
    stage(vm, "stream.zip", vec![a, b], DEFAULT_CAPACITY, move |got| {
        Ok(match got {
            Got::Start | Got::Emitted => Next::Take(0),
            Got::Value(0, va) => {
                left = Some(va);
                Next::Take(1)
            }
            Got::Value(_, vb) => match left.take() {
                Some(va) => Next::Emit(Value::Tuple(vec![va, vb])),
                None => return unexpected(),
            },
            Got::End => Next::Done(Value::Unit),
            Got::Returned(_) | Got::Io(_) => return unexpected(),
        })
    })
}

fn concat(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new(
            "stream.concat takes 1 argument (List(Channel))".into(),
        ));
    }
    let channels = channel_list(&args[0], "concat")?;
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

// ── Sinks ──────────────────────────────────────────────────────────────

fn collect(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.collect takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.collect")?.clone();
    let mut out = Vec::new();
    sink("stream.collect", ch, move |got| {
        Ok(match got {
            Got::Start => Next::Take(0),
            Got::Value(_, v) => {
                out.push(v);
                Next::Take(0)
            }
            Got::End => Next::Done(Value::List(Arc::new(std::mem::take(&mut out)))),
            _ => return unexpected(),
        })
    })
}

fn fold(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "stream.fold takes 3 arguments (channel, init, fn)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.fold")?.clone();
    let mut acc = args[1].clone();
    let callback = require_callable(&args[2], "stream.fold")?.clone();
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

fn each(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.each takes 2 arguments (channel, fn)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.each")?.clone();
    let callback = require_callable(&args[1], "stream.each")?.clone();
    sink("stream.each", ch, move |got| {
        Ok(match got {
            Got::Start | Got::Returned(_) => Next::Take(0),
            Got::Value(_, v) => Next::Call(callback.clone(), vec![v]),
            Got::End => Next::Done(Value::Unit),
            _ => return unexpected(),
        })
    })
}

fn count(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.count takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.count")?.clone();
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

fn first(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.first takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.first")?.clone();
    sink("stream.first", ch, move |got| {
        Ok(match got {
            Got::Start => Next::Take(0),
            Got::Value(_, v) => Next::Done(Value::variant(bv::SOME, vec![v])),
            Got::End => Next::Done(Value::variant(bv::NONE, vec![])),
            _ => return unexpected(),
        })
    })
}

fn last(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.last takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.last")?.clone();
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

/// The bytes of an item of a stream that is written out: `Bytes`, or
/// `Ok(Bytes)`. Anything else ends the sink with the `Err(_)` given:
/// an `Err(_)` item itself, or `mismatch` of what was expected.
fn bytes_to_write(
    v: Value,
    name: &str,
    mismatch: fn(String) -> Value,
) -> Result<Arc<Vec<u8>>, Value> {
    match v {
        Value::Bytes(b) => Ok(b),
        Value::Variant(tag, mut fields) if tag.is(bv::OK) && fields.len() == 1 => {
            match fields.pop() {
                Some(Value::Bytes(b)) => Ok(b),
                other => Err(mismatch(format!(
                    "{name}: Ok(_) wrapper expected Bytes, got {}",
                    super::common::value_kind(&other.unwrap_or(Value::Unit))
                ))),
            }
        }
        Value::Variant(tag, fields) if tag.is(bv::ERR) && fields.len() == 1 => {
            Err(Value::variant(bv::ERR, fields))
        }
        other => Err(mismatch(format!(
            "{name} expected Bytes, got {}",
            super::common::value_kind(&other)
        ))),
    }
}

/// A sink that writes every item of `ch` with `write` (which blocks)
/// and ends with `finish`. Its value is `Ok(())`, or the first
/// `Err(_)`: of a write, or of an item that is no bytes.
fn writing(
    name: &'static str,
    ch: Arc<Channel>,
    mismatch: fn(String) -> Value,
    write: impl FnMut(Option<&[u8]>) -> Result<(), Value> + Send + 'static,
) -> Result<Step, VmError> {
    let write = Arc::new(Mutex::new(write));
    let mut ended = false;
    sink(name, ch, move |got| {
        let run = |bytes: Option<Arc<Vec<u8>>>| {
            let write = write.clone();
            Next::Io(Box::new(move || {
                match (write.lock())(bytes.as_deref().map(Vec::as_slice)) {
                    Ok(()) => ok(Value::Unit),
                    Err(e) => e,
                }
            }))
        };
        Ok(match got {
            // The first operation opens what is written to.
            Got::Start => run(Some(Arc::new(Vec::new()))),
            Got::Value(_, v) => match bytes_to_write(v, name, mismatch) {
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

#[cfg(feature = "tcp")]
fn write_to_tcp(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.write_to_tcp takes 2 arguments (channel, conn)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.write_to_tcp")?.clone();
    let stream_handle = match &args[1] {
        Value::TcpStream(s) => s.clone(),
        _ => {
            return Err(VmError::new(
                "stream.write_to_tcp requires a TcpStream".into(),
            ));
        }
    };
    fn mismatch(msg: String) -> Value {
        err_tcp_unknown(msg)
    }
    writing("stream.write_to_tcp", ch, mismatch, move |bytes| {
        use std::io::Write;
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let mut guard = stream_handle.inner.lock();
        guard.write_all(bytes).map_err(|e| err_tcp(&e))?;
        guard.flush().map_err(|e| err_tcp(&e))
    })
}

#[cfg(not(feature = "tcp"))]
fn write_to_tcp(_args: &[Value]) -> Result<Step, VmError> {
    Err(VmError::new(
        "stream.write_to_tcp requires the 'tcp' feature".into(),
    ))
}

fn write_to_file(args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.write_to_file takes 2 arguments (channel, path)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.write_to_file")?.clone();
    let path = require_string(&args[1], "stream.write_to_file")?.to_string();
    fn mismatch(msg: String) -> Value {
        err_io_unknown(msg)
    }
    let mut file: Option<std::fs::File> = None;
    writing("stream.write_to_file", ch, mismatch, move |bytes| {
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
