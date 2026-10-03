//! `stream.*` builtin functions: a library of channel-backed sources,
//! transforms, and sinks. The underlying primitive is `Value::Channel(_)`
//! — there is no separate stream value type. Each transform spawns an OS
//! thread (with its own child VM) that reads its input channel, calls
//! the user closure via `vm.invoke_callable`, and writes results to the
//! output channel. Backpressure is provided by channel capacity: when the
//! output is full the pump thread sleeps briefly and retries.
//!
//! Sinks (collect, fold, count, etc.) run synchronously in the caller's
//! task. Because every source/transform pump is on an OS thread (not a
//! scheduler worker), sinks can safely block on `receive_blocking` even
//! when called from inside `task.spawn` — the producer side keeps making
//! progress regardless of scheduler state.
//!
//! Forward-compat: the function names mirror what method-form dispatch
//! (`s.map(f)`) would look like once silt grows a `Stream` trait. Existing
//! silt programs will continue to compile and behave identically when that
//! trait lands.

use std::sync::Arc;
use std::time::Duration;

use super::common::ok;
use crate::typeinfo::bv;
use crate::value::{Channel, TryReceiveResult, TrySendResult, Value};
use crate::vm::{BuiltinAcc, SuspendedBuiltin, Vm, VmError};

const DEFAULT_CAPACITY: usize = 16;
const SEND_BACKOFF: Duration = Duration::from_micros(100);

/// Dispatch `stream.<name>(args)`.
pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
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
        "fold" => fold(vm, args),
        "each" => each(vm, args),
        "count" => count(args),
        "first" => first(args),
        "last" => last(args),
        "write_to_tcp" => write_to_tcp(args),
        "write_to_file" => write_to_file(args),

        _ => Err(VmError::new(format!("unknown stream function: {name}"))),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Create a channel that no stage feeds: the result of a call that
/// starts no thread (`stream.take(s, 0)`, a chunk size of 0). For the
/// output of a stage use `stage_output`.
fn make_channel(vm: &mut Vm, capacity: usize) -> Arc<Channel> {
    let id = vm.next_channel_id();
    Arc::new(Channel::new(id, capacity))
}

/// Create the output channel of a stream stage and record it as
/// stream-fed.
///
/// EVERY function that starts a stage thread must get the channel that
/// thread writes to from this helper, not from `make_channel`. A stage
/// thread is not a scheduler task, so the main-thread deadlock
/// detection cannot see it; the record (`mark_stream_fed` in
/// `concurrency.rs`) is what stops a main-thread `channel.receive`,
/// `channel.each` or `channel.select` on the stage's output from being
/// reported as a deadlock while the stage is still about to deliver.
///
/// In return the stage must close this channel when it ends: a receive
/// on it waits for a value or for `Closed`, and gets no verdict.
fn stage_output(vm: &mut Vm, capacity: usize) -> Arc<Channel> {
    let out = make_channel(vm, capacity);
    super::concurrency::mark_stream_fed(&out);
    out
}

/// Push a value onto an output channel with backpressure. The Channel
/// `try_send` consumes its argument, so we clone once per attempt.
fn push(out: &Channel, val: &Value) -> bool {
    loop {
        match out.try_send(val.clone()) {
            TrySendResult::Sent => return true,
            TrySendResult::Closed => return false,
            TrySendResult::Full => std::thread::sleep(SEND_BACKOFF),
        }
    }
}

// The marker used to carry a pump-thread type error in-band. Stream
// transforms run on detached OS threads with no `VmError` path back to
// the caller, so a runtime gate that fires there (currently only the
// `stream.dedup` Fn gate) pushes this marker onto its output channel
// and closes it. `spawn_pump`-based transforms forward the marker
// unchanged, and the synchronous sinks (`collect` / `fold` / `each` /
// `count` / `first` / `last`) translate it into the canonical `VmError`.
// Same in-band-marker pattern as `bv::MAP_ERROR` in
// src/builtins/collections.rs. Its type is no type a program can name.
// Locked by tests/lang/collection_fn_gate_sibling_surfaces_tests.rs.

/// Build the in-band error marker for a pump-thread type error.
fn stream_type_error(msg: String) -> Value {
    Value::variant(bv::STREAM_ERROR, vec![Value::String(msg)])
}

/// If `v` is the in-band pump-thread error marker, return the `VmError`
/// it carries.
fn take_stream_type_error(v: &Value) -> Option<VmError> {
    if let Value::Variant(tag, fields) = v
        && tag.is(bv::STREAM_ERROR)
        && let Some(Value::String(msg)) = fields.first()
    {
        return Some(VmError::new(msg.clone()));
    }
    None
}

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

fn from_list(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.from_list takes 1 argument".into()));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new("stream.from_list requires a List".into()));
    };
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let xs = xs.clone();
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        for v in xs.iter() {
            if !push(&out_clone, v) {
                break;
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn from_range(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.from_range takes 2 arguments".into()));
    }
    let lo = require_int(&args[0], "stream.from_range")?;
    let hi = require_int(&args[1], "stream.from_range")?;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        for i in lo..=hi {
            if !push(&out_clone, &Value::Int(i)) {
                break;
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn repeat(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.repeat takes 1 argument".into()));
    }
    let v = args[0].clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        loop {
            if !push(&out_clone, &v) {
                break;
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn unfold(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.unfold takes 2 arguments (init, fn)".into(),
        ));
    }
    let init = args[0].clone();
    let fn_val = require_callable(&args[1], "stream.unfold")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    crate::vm::spawn_callback_thread(move || {
        let mut state = init;
        loop {
            // Fn(state) -> Option((value, next_state))
            let res = child_vm.invoke_callable(&fn_val, &[state.clone()]);
            let Ok(opt) = res else { break };
            match opt {
                Value::Variant(name, fields) if name.is(bv::SOME) && fields.len() == 1 => {
                    if let Value::Tuple(pair) = &fields[0]
                        && pair.len() == 2
                    {
                        let (value, next_state) = (pair[0].clone(), pair[1].clone());
                        if !push(&out_clone, &value) {
                            break;
                        }
                        state = next_state;
                    } else {
                        break; // bad shape
                    }
                }
                _ => break, // None or unexpected
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn file_chunks(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.file_chunks takes 2 arguments (path, chunk_size)".into(),
        ));
    }
    let path = require_string(&args[0], "stream.file_chunks")?.to_string();
    let n = require_int(&args[1], "stream.file_chunks")?;
    if n <= 0 {
        return Ok(Value::Channel(make_channel(vm, 1)));
    }
    let n = n as usize;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        use std::io::Read;
        match std::fs::File::open(&path) {
            Ok(mut file) => {
                let mut buf = vec![0u8; n];
                loop {
                    match file.read(&mut buf) {
                        Ok(0) => break,
                        Ok(read) => {
                            let chunk = Value::Bytes(Arc::new(buf[..read].to_vec()));
                            if !push(&out_clone, &ok(chunk)) {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = push(&out_clone, &err_io(&e));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = push(&out_clone, &err_io(&e));
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn file_lines(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.file_lines takes 1 argument".into()));
    }
    let path = require_string(&args[0], "stream.file_lines")?.to_string();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        use std::io::BufRead;
        match std::fs::File::open(&path) {
            Ok(file) => {
                let reader = std::io::BufReader::new(file);
                for line in reader.lines() {
                    match line {
                        Ok(s) => {
                            if !push(&out_clone, &ok(Value::String(s))) {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = push(&out_clone, &err_io(&e));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = push(&out_clone, &err_io(&e));
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

#[cfg(feature = "tcp")]
fn tcp_chunks(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
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
        return Ok(Value::Channel(make_channel(vm, 1)));
    }
    let n = n as usize;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        use std::io::Read;
        loop {
            let mut buf = vec![0u8; n];
            let read = {
                let mut guard = stream_handle.inner.lock();
                guard.read(&mut buf)
            };
            match read {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = Value::Bytes(Arc::new(buf[..read].to_vec()));
                    if !push(&out_clone, &ok(chunk)) {
                        break;
                    }
                }
                Err(e) => {
                    let _ = push(&out_clone, &err_tcp(&e));
                    break;
                }
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

#[cfg(not(feature = "tcp"))]
fn tcp_chunks(_vm: &mut Vm, _args: &[Value]) -> Result<Value, VmError> {
    Err(VmError::new(
        "stream.tcp_chunks requires the 'tcp' feature".into(),
    ))
}

#[cfg(feature = "tcp")]
fn tcp_lines(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.tcp_lines takes 1 argument".into()));
    }
    let stream_handle = match &args[0] {
        Value::TcpStream(s) => s.clone(),
        _ => return Err(VmError::new("stream.tcp_lines requires a TcpStream".into())),
    };
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        // We can't easily wrap the trait-object stream in a BufReader
        // because BufReader requires owning the reader (can't borrow from
        // a Mutex guard across loop iterations). Read byte-by-byte —
        // simple and correct; performance is acceptable for typical
        // line-oriented protocols since the network buffer dominates.
        use std::io::Read;
        let mut current = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            let read = {
                let mut guard = stream_handle.inner.lock();
                guard.read(&mut byte)
            };
            match read {
                Ok(0) => {
                    if !current.is_empty() {
                        let line = String::from_utf8_lossy(&current).to_string();
                        let _ = push(&out_clone, &ok(Value::String(line)));
                    }
                    break;
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        // Strip trailing \r if present.
                        if current.last() == Some(&b'\r') {
                            current.pop();
                        }
                        let line = String::from_utf8_lossy(&current).to_string();
                        if !push(&out_clone, &ok(Value::String(line))) {
                            break;
                        }
                        current.clear();
                    } else {
                        current.push(byte[0]);
                    }
                }
                Err(e) => {
                    let _ = push(&out_clone, &err_tcp(&e));
                    break;
                }
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

#[cfg(not(feature = "tcp"))]
fn tcp_lines(_vm: &mut Vm, _args: &[Value]) -> Result<Value, VmError> {
    Err(VmError::new(
        "stream.tcp_lines requires the 'tcp' feature".into(),
    ))
}

// ── Transforms ─────────────────────────────────────────────────────────

/// Generic pump: spawn a thread that drains `in_ch`, applies `each` to
/// each value, and writes the result(s) to `out_ch`. `each` is a Rust
/// closure that decides what to do per element (transform, filter, etc.).
fn spawn_pump<F>(in_ch: Arc<Channel>, out_ch: Arc<Channel>, mut each: F)
where
    F: FnMut(Value, &Channel) -> bool + Send + 'static,
{
    crate::vm::spawn_callback_thread(move || {
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    // Forward an in-band pump-thread error marker
                    // unchanged (never into the user callback) so it
                    // reaches the sink that translates it to a VmError.
                    if take_stream_type_error(&v).is_some() {
                        let _ = push(&out_ch, &v);
                        break;
                    }
                    if !each(v, &out_ch) {
                        break;
                    }
                }
                TryReceiveResult::Closed => break,
                TryReceiveResult::Empty => {} // unreachable from blocking
            }
        }
        out_ch.close();
    });
}

fn map(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.map takes 2 arguments (channel, fn)".into(),
        ));
    }
    let in_ch = require_channel(&args[0], "stream.map")?.clone();
    let fn_val = require_callable(&args[1], "stream.map")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| {
        match child_vm.invoke_callable(&fn_val, &[v]) {
            Ok(result) => push(out_ch, &result),
            Err(_) => false, // closure errored — close output
        }
    });
    Ok(Value::Channel(out))
}

fn map_ok(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.map_ok takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.map_ok")?.clone();
    let fn_val = require_callable(&args[1], "stream.map_ok")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| match v {
        Value::Variant(ref name, ref fields) if name.is(bv::OK) && fields.len() == 1 => {
            let inner = fields[0].clone();
            match child_vm.invoke_callable(&fn_val, &[inner]) {
                Ok(result) => push(out_ch, &ok(result)),
                Err(_) => false,
            }
        }
        _ => push(out_ch, &v),
    });
    Ok(Value::Channel(out))
}

fn filter(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.filter takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.filter")?.clone();
    let fn_val = require_callable(&args[1], "stream.filter")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| {
        match child_vm.invoke_callable(&fn_val, std::slice::from_ref(&v)) {
            Ok(Value::Bool(true)) => push(out_ch, &v),
            Ok(_) => true,
            Err(_) => false,
        }
    });
    Ok(Value::Channel(out))
}

fn filter_ok(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.filter_ok takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.filter_ok")?.clone();
    let fn_val = require_callable(&args[1], "stream.filter_ok")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| match v {
        Value::Variant(ref name, ref fields) if name.is(bv::OK) && fields.len() == 1 => {
            let inner = fields[0].clone();
            match child_vm.invoke_callable(&fn_val, &[inner]) {
                Ok(Value::Bool(true)) => push(out_ch, &v),
                Ok(_) => true,
                Err(_) => false,
            }
        }
        _ => push(out_ch, &v),
    });
    Ok(Value::Channel(out))
}

fn flat_map(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.flat_map takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.flat_map")?.clone();
    let fn_val = require_callable(&args[1], "stream.flat_map")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| {
        match child_vm.invoke_callable(&fn_val, &[v]) {
            Ok(Value::List(xs)) => {
                for item in xs.iter() {
                    if !push(out_ch, item) {
                        return false;
                    }
                }
                true
            }
            Ok(_) => false, // user fn returned non-List
            Err(_) => false,
        }
    });
    Ok(Value::Channel(out))
}

fn take(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.take takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.take")?.clone();
    let n = require_int(&args[1], "stream.take")?;
    if n <= 0 {
        let out = make_channel(vm, 1);
        out.close();
        return Ok(Value::Channel(out));
    }
    let n = n as usize;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        let mut emitted = 0;
        while emitted < n {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    if !push(&out_clone, &v) {
                        break;
                    }
                    emitted += 1;
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn drop_n(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.drop takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.drop")?.clone();
    let n = require_int(&args[1], "stream.drop")?;
    let n = n.max(0) as usize;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        let mut dropped = 0;
        while dropped < n {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(_) => dropped += 1,
                TryReceiveResult::Closed => {
                    out_clone.close();
                    return;
                }
                _ => {}
            }
        }
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) if !push(&out_clone, &v) => {
                    break;
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn take_while(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.take_while takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.take_while")?.clone();
    let fn_val = require_callable(&args[1], "stream.take_while")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    crate::vm::spawn_callback_thread(move || {
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    match child_vm.invoke_callable(&fn_val, std::slice::from_ref(&v)) {
                        Ok(Value::Bool(true)) => {
                            if !push(&out_clone, &v) {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn drop_while(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.drop_while takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.drop_while")?.clone();
    let fn_val = require_callable(&args[1], "stream.drop_while")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    crate::vm::spawn_callback_thread(move || {
        let mut dropping = true;
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    if dropping {
                        match child_vm.invoke_callable(&fn_val, std::slice::from_ref(&v)) {
                            Ok(Value::Bool(true)) => continue, // drop
                            _ => {
                                dropping = false;
                                if !push(&out_clone, &v) {
                                    break;
                                }
                            }
                        }
                    } else if !push(&out_clone, &v) {
                        break;
                    }
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn chunks(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.chunks takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.chunks")?.clone();
    let n = require_int(&args[1], "stream.chunks")?;
    if n <= 0 {
        return Err(VmError::new("stream.chunks: n must be positive".into()));
    }
    let n = n as usize;
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        let mut buffer: Vec<Value> = Vec::with_capacity(n);
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    buffer.push(v);
                    if buffer.len() == n {
                        let chunk = Value::List(Arc::new(std::mem::replace(
                            &mut buffer,
                            Vec::with_capacity(n),
                        )));
                        if !push(&out_clone, &chunk) {
                            break;
                        }
                    }
                }
                TryReceiveResult::Closed => {
                    if !buffer.is_empty() {
                        let chunk = Value::List(Arc::new(std::mem::take(&mut buffer)));
                        let _ = push(&out_clone, &chunk);
                    }
                    break;
                }
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn scan(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "stream.scan takes 3 arguments (channel, init, fn)".into(),
        ));
    }
    let in_ch = require_channel(&args[0], "stream.scan")?.clone();
    let init = args[1].clone();
    let fn_val = require_callable(&args[2], "stream.scan")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    crate::vm::spawn_callback_thread(move || {
        let mut acc = init;
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    match child_vm.invoke_callable(&fn_val, &[acc.clone(), v]) {
                        Ok(new_acc) => {
                            acc = new_acc;
                            if !push(&out_clone, &acc) {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn dedup(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.dedup takes 1 argument".into()));
    }
    let in_ch = require_channel(&args[0], "stream.dedup")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        let mut prev: Option<Value> = None;
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
                    // Runtime Fn gate, same policy as `ensure_no_fn` in
                    // src/builtins/collections.rs: `stream.dedup` has an
                    // unbounded signature, so Fn values typecheck and the
                    // `p != &v` comparison below would silently dedup
                    // closures by Arc identity — the exact behavior
                    // `list.unique` rejects. This pump thread has no
                    // `VmError` path, so surface the canonical error
                    // in-band (see `bv::STREAM_ERROR`).
                    if Vm::value_contains_fn(&v) {
                        let _ = push(
                            &out_clone,
                            &stream_type_error(
                                "stream.dedup: type 'Fn' does not implement Equal".to_string(),
                            ),
                        );
                        break;
                    }
                    let emit = match &prev {
                        Some(p) => p != &v,
                        None => true,
                    };
                    if emit {
                        if !push(&out_clone, &v) {
                            break;
                        }
                        prev = Some(v);
                    }
                }
                TryReceiveResult::Closed => break,
                _ => {}
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn buffered(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.buffered takes 2 arguments".into()));
    }
    let in_ch = require_channel(&args[0], "stream.buffered")?.clone();
    let cap = require_int(&args[1], "stream.buffered")?;
    let cap = cap.max(0) as usize;
    let out = stage_output(vm, cap);
    spawn_pump(in_ch, out.clone(), |v, out_ch| push(out_ch, &v));
    Ok(Value::Channel(out))
}

// ── Combinators ───────────────────────────────────────────────────────

fn merge(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new(
            "stream.merge takes 1 argument (List(Channel))".into(),
        ));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new(
            "stream.merge requires a List of Channels".into(),
        ));
    };
    let mut channels = Vec::with_capacity(xs.len());
    for v in xs.iter() {
        match v {
            Value::Channel(c) => channels.push(c.clone()),
            _ => {
                return Err(VmError::new(
                    "stream.merge: list elements must be Channels".into(),
                ));
            }
        }
    }
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let count = channels.len();
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(count));
    for in_ch in channels {
        let out_clone = out.clone();
        let remaining = remaining.clone();
        crate::vm::spawn_callback_thread(move || {
            loop {
                match in_ch.receive_blocking() {
                    TryReceiveResult::Value(v) if !push(&out_clone, &v) => {
                        break;
                    }
                    TryReceiveResult::Closed => break,
                    _ => {}
                }
            }
            if remaining.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                out_clone.close();
            }
        });
    }
    Ok(Value::Channel(out))
}

fn zip(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("stream.zip takes 2 arguments".into()));
    }
    let a = require_channel(&args[0], "stream.zip")?.clone();
    let b = require_channel(&args[1], "stream.zip")?.clone();
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        loop {
            let ra = a.receive_blocking();
            let rb = b.receive_blocking();
            match (ra, rb) {
                (TryReceiveResult::Value(va), TryReceiveResult::Value(vb)) => {
                    let pair = Value::Tuple(vec![va, vb]);
                    if !push(&out_clone, &pair) {
                        break;
                    }
                }
                _ => break,
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

fn concat(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new(
            "stream.concat takes 1 argument (List(Channel))".into(),
        ));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new(
            "stream.concat requires a List of Channels".into(),
        ));
    };
    let mut channels = Vec::with_capacity(xs.len());
    for v in xs.iter() {
        match v {
            Value::Channel(c) => channels.push(c.clone()),
            _ => {
                return Err(VmError::new(
                    "stream.concat: list elements must be Channels".into(),
                ));
            }
        }
    }
    let out = stage_output(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    crate::vm::spawn_callback_thread(move || {
        for ch in channels {
            loop {
                match ch.receive_blocking() {
                    TryReceiveResult::Value(v) if !push(&out_clone, &v) => {
                        out_clone.close();
                        return;
                    }
                    TryReceiveResult::Closed => break,
                    _ => {}
                }
            }
        }
        out_clone.close();
    });
    Ok(Value::Channel(out))
}

// ── Sinks ──────────────────────────────────────────────────────────────

fn collect(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.collect takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.collect")?.clone();
    let mut out = Vec::new();
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                // Translate an in-band pump-thread error marker (see
                // `bv::STREAM_ERROR`) into the canonical VmError.
                if let Some(e) = take_stream_type_error(&v) {
                    return Err(e);
                }
                out.push(v)
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(Value::List(Arc::new(out)))
}

fn fold(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "stream.fold takes 3 arguments (channel, init, fn)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.fold")?.clone();
    let fn_val = require_callable(&args[2], "stream.fold")?.clone();

    // ── Restore state from a prior yield, if any ──────────────────
    // Channels can't be materialized up front (they're potentially
    // unbounded), so this can't ride on `iterate_builtin`. Instead we stash
    // the running accumulator in `BuiltinAcc::Fold` and the channel value
    // in `items` / callback in `callback`. We also pick up a half-completed
    // callback via `suspended_invoke`.
    let mut acc = if let Some(susp) = vm.take_suspended_builtin() {
        if susp.name == "stream.fold" {
            if let BuiltinAcc::Fold(v) = susp.acc {
                v
            } else {
                args[1].clone()
            }
        } else {
            vm.push_suspended_builtin(susp);
            args[1].clone()
        }
    } else {
        args[1].clone()
    };

    // ── Resume a mid-execution callback if needed ────────────────
    if vm.suspended_invoke.is_some() {
        let cb_result = match vm.resume_suspended_invoke() {
            Ok(v) => v,
            Err(e) if e.is_yield => {
                vm.push_suspended_builtin(SuspendedBuiltin {
                    name: "stream.fold".into(),
                    items: Vec::new(),
                    next_index: 0,
                    callback: fn_val.clone(),
                    acc: BuiltinAcc::Fold(acc),
                });
                for a in args {
                    vm.push(a.clone());
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        acc = cb_result;
    }

    // ── Main loop ────────────────────────────────────────────────
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                // In-band pump-thread error marker — see `bv::STREAM_ERROR`.
                if let Some(e) = take_stream_type_error(&v) {
                    return Err(e);
                }
                let invoke_result = vm.invoke_callable(&fn_val, &[acc.clone(), v]);
                match invoke_result {
                    Ok(r) => acc = r,
                    Err(e) if e.is_yield => {
                        vm.push_suspended_builtin(SuspendedBuiltin {
                            name: "stream.fold".into(),
                            items: Vec::new(),
                            next_index: 0,
                            callback: fn_val.clone(),
                            acc: BuiltinAcc::Fold(acc),
                        });
                        for a in args {
                            vm.push(a.clone());
                        }
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(acc)
}

fn each(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.each takes 2 arguments (channel, fn)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.each")?.clone();
    let fn_val = require_callable(&args[1], "stream.each")?.clone();

    // ── Resume a mid-execution callback if needed ────────────────
    // `stream.each` carries no accumulator across yields, so a stale
    // `suspended_invoke` is the only state to restore.  Still: drop any
    // matching `suspended_builtin` we may have left on prior yield so
    // unrelated builtins don't pick it up.
    if let Some(susp) = vm.take_suspended_builtin()
        && susp.name != "stream.each"
    {
        vm.push_suspended_builtin(susp);
    }
    if vm.suspended_invoke.is_some() {
        match vm.resume_suspended_invoke() {
            Ok(_) => {}
            Err(e) if e.is_yield => {
                vm.push_suspended_builtin(SuspendedBuiltin {
                    name: "stream.each".into(),
                    items: Vec::new(),
                    next_index: 0,
                    callback: fn_val.clone(),
                    acc: BuiltinAcc::Unit,
                });
                for a in args {
                    vm.push(a.clone());
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }

    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                // In-band pump-thread error marker — see `bv::STREAM_ERROR`.
                if let Some(e) = take_stream_type_error(&v) {
                    return Err(e);
                }
                let invoke_result = vm.invoke_callable(&fn_val, &[v]);
                match invoke_result {
                    Ok(_) => {}
                    Err(e) if e.is_yield => {
                        vm.push_suspended_builtin(SuspendedBuiltin {
                            name: "stream.each".into(),
                            items: Vec::new(),
                            next_index: 0,
                            callback: fn_val.clone(),
                            acc: BuiltinAcc::Unit,
                        });
                        for a in args {
                            vm.push(a.clone());
                        }
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(Value::Unit)
}

fn count(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.count takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.count")?.clone();
    let mut n: i64 = 0;
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                // In-band pump-thread error marker — see `bv::STREAM_ERROR`.
                if let Some(e) = take_stream_type_error(&v) {
                    return Err(e);
                }
                n += 1
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(Value::Int(n))
}

fn first(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.first takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.first")?.clone();
    match ch.receive_blocking() {
        TryReceiveResult::Value(v) => {
            // In-band pump-thread error marker — see `bv::STREAM_ERROR`.
            if let Some(e) = take_stream_type_error(&v) {
                return Err(e);
            }
            Ok(Value::variant(bv::SOME, vec![v]))
        }
        TryReceiveResult::Closed => Ok(Value::variant(bv::NONE, vec![])),
        _ => Ok(Value::variant(bv::NONE, vec![])),
    }
}

fn last(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.last takes 1 argument".into()));
    }
    let ch = require_channel(&args[0], "stream.last")?.clone();
    let mut last: Option<Value> = None;
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                // In-band pump-thread error marker — see `bv::STREAM_ERROR`.
                if let Some(e) = take_stream_type_error(&v) {
                    return Err(e);
                }
                last = Some(v)
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(match last {
        Some(v) => Value::variant(bv::SOME, vec![v]),
        None => Value::variant(bv::NONE, vec![]),
    })
}

#[cfg(feature = "tcp")]
fn write_to_tcp(args: &[Value]) -> Result<Value, VmError> {
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
    use std::io::Write;
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                let bytes = match v {
                    Value::Bytes(b) => b,
                    Value::Variant(name, fields) if name.is(bv::OK) && fields.len() == 1 => {
                        match fields.into_iter().next().unwrap() {
                            Value::Bytes(b) => b,
                            other => {
                                return Ok(err_tcp_unknown(format!(
                                    "stream.write_to_tcp: Ok(_) wrapper expected Bytes, got {}",
                                    super::common::value_kind(&other)
                                )));
                            }
                        }
                    }
                    Value::Variant(name, mut fields) if name.is(bv::ERR) && fields.len() == 1 => {
                        // Upstream typed error — forward the variant as-is.
                        return Ok(Value::variant(bv::ERR, vec![fields.pop().unwrap()]));
                    }
                    other => {
                        return Ok(err_tcp_unknown(format!(
                            "stream.write_to_tcp expected Bytes, got {}",
                            super::common::value_kind(&other)
                        )));
                    }
                };
                let mut guard = stream_handle.inner.lock();
                if let Err(e) = guard.write_all(&bytes) {
                    return Ok(err_tcp(&e));
                }
                if let Err(e) = guard.flush() {
                    return Ok(err_tcp(&e));
                }
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(ok(Value::Unit))
}

#[cfg(not(feature = "tcp"))]
fn write_to_tcp(_args: &[Value]) -> Result<Value, VmError> {
    Err(VmError::new(
        "stream.write_to_tcp requires the 'tcp' feature".into(),
    ))
}

fn write_to_file(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "stream.write_to_file takes 2 arguments (channel, path)".into(),
        ));
    }
    let ch = require_channel(&args[0], "stream.write_to_file")?.clone();
    let path = require_string(&args[1], "stream.write_to_file")?;
    use std::io::Write;
    let file_result = std::fs::File::create(path);
    let mut file = match file_result {
        Ok(f) => f,
        Err(e) => return Ok(err_io(&e)),
    };
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                let bytes = match v {
                    Value::Bytes(b) => b,
                    Value::Variant(name, fields) if name.is(bv::OK) && fields.len() == 1 => {
                        match fields.into_iter().next().unwrap() {
                            Value::Bytes(b) => b,
                            other => {
                                return Ok(err_io_unknown(format!(
                                    "stream.write_to_file: Ok(_) wrapper expected Bytes, got {}",
                                    super::common::value_kind(&other)
                                )));
                            }
                        }
                    }
                    Value::Variant(name, mut fields) if name.is(bv::ERR) && fields.len() == 1 => {
                        // Upstream typed error — forward the variant as-is.
                        return Ok(Value::variant(bv::ERR, vec![fields.pop().unwrap()]));
                    }
                    other => {
                        return Ok(err_io_unknown(format!(
                            "stream.write_to_file expected Bytes, got {}",
                            super::common::value_kind(&other)
                        )));
                    }
                };
                if let Err(e) = file.write_all(&bytes) {
                    return Ok(err_io(&e));
                }
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    if let Err(e) = file.flush() {
        return Ok(err_io(&e));
    }
    Ok(ok(Value::Unit))
}
