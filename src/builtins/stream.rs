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
//! v0.10 silt programs will continue to compile and behave identically
//! when that trait lands.

use std::sync::Arc;
use std::time::Duration;

use crate::value::{Channel, TryReceiveResult, TrySendResult, Value};
use crate::vm::{Vm, VmError};

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

fn make_channel(vm: &mut Vm, capacity: usize) -> Arc<Channel> {
    let id = vm.next_channel_id();
    Arc::new(Channel::new(id, capacity))
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

fn require_channel<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Arc<Channel>, VmError> {
    match arg {
        Value::Channel(c) => Ok(c),
        _ => Err(VmError::new(format!("{fn_label} requires Channel"))),
    }
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        _ => Err(VmError::new(format!("{fn_label} requires Int"))),
    }
}

fn require_string<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    match arg {
        Value::String(s) => Ok(s.as_str()),
        _ => Err(VmError::new(format!("{fn_label} requires String"))),
    }
}

fn require_callable<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Value, VmError> {
    match arg {
        Value::VmClosure(_) | Value::BuiltinFn(_) | Value::VariantConstructor(..) => Ok(arg),
        _ => Err(VmError::new(format!("{fn_label} requires a function"))),
    }
}

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}
fn err_v(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
}

// ── Sources ────────────────────────────────────────────────────────────

fn from_list(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("stream.from_list takes 1 argument".into()));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new("stream.from_list requires a List".into()));
    };
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let xs = xs.clone();
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    std::thread::spawn(move || {
        let mut state = init;
        loop {
            // fn(state) -> Option((value, next_state))
            let res = child_vm.invoke_callable(&fn_val, &[state.clone()]);
            let Ok(opt) = res else { break };
            match opt {
                Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
                            let _ = push(&out_clone, &err_v(e.to_string()));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = push(&out_clone, &err_v(format!("open {path}: {e}")));
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
                            let _ = push(&out_clone, &err_v(e.to_string()));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = push(&out_clone, &err_v(format!("open {path}: {e}")));
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
                    let _ = push(&out_clone, &err_v(e.to_string()));
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
                    let _ = push(&out_clone, &err_v(e.to_string()));
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
    std::thread::spawn(move || {
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| match v {
        Value::Variant(ref name, ref fields) if name == "Ok" && fields.len() == 1 => {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let mut child_vm = vm.spawn_child();
    spawn_pump(in_ch, out.clone(), move |v, out_ch| match v {
        Value::Variant(ref name, ref fields) if name == "Ok" && fields.len() == 1 => {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    let mut child_vm = vm.spawn_child();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
        let mut prev: Option<Value> = None;
        loop {
            match in_ch.receive_blocking() {
                TryReceiveResult::Value(v) => {
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
    let out = make_channel(vm, cap);
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let count = channels.len();
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(count));
    for in_ch in channels {
        let out_clone = out.clone();
        let remaining = remaining.clone();
        std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
    let out = make_channel(vm, DEFAULT_CAPACITY);
    let out_clone = out.clone();
    std::thread::spawn(move || {
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
            TryReceiveResult::Value(v) => out.push(v),
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
    let mut acc = args[1].clone();
    let fn_val = require_callable(&args[2], "stream.fold")?.clone();
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                acc = vm.invoke_callable(&fn_val, &[acc, v])?;
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
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                vm.invoke_callable(&fn_val, &[v])?;
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
            TryReceiveResult::Value(_) => n += 1,
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
        TryReceiveResult::Value(v) => Ok(Value::Variant("Some".into(), vec![v])),
        TryReceiveResult::Closed => Ok(Value::Variant("None".into(), vec![])),
        _ => Ok(Value::Variant("None".into(), vec![])),
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
            TryReceiveResult::Value(v) => last = Some(v),
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    Ok(match last {
        Some(v) => Value::Variant("Some".into(), vec![v]),
        None => Value::Variant("None".into(), vec![]),
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
                    Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                        match fields.into_iter().next().unwrap() {
                            Value::Bytes(b) => b,
                            other => {
                                return Ok(err_v(format!(
                                    "stream.write_to_tcp: Ok(_) wrapper expected Bytes, got {}",
                                    type_name(&other)
                                )));
                            }
                        }
                    }
                    Value::Variant(name, fields) if name == "Err" && fields.len() == 1 => {
                        if let Value::String(e) = &fields[0] {
                            return Ok(err_v(e.clone()));
                        }
                        return Ok(err_v("stream error".to_string()));
                    }
                    other => {
                        return Ok(err_v(format!(
                            "stream.write_to_tcp expected Bytes, got {}",
                            type_name(&other)
                        )));
                    }
                };
                let mut guard = stream_handle.inner.lock();
                if let Err(e) = guard.write_all(&bytes) {
                    return Ok(err_v(e.to_string()));
                }
                if let Err(e) = guard.flush() {
                    return Ok(err_v(e.to_string()));
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
        Err(e) => return Ok(err_v(format!("create {path}: {e}"))),
    };
    loop {
        match ch.receive_blocking() {
            TryReceiveResult::Value(v) => {
                let bytes = match v {
                    Value::Bytes(b) => b,
                    Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                        match fields.into_iter().next().unwrap() {
                            Value::Bytes(b) => b,
                            other => {
                                return Ok(err_v(format!(
                                    "stream.write_to_file: Ok(_) wrapper expected Bytes, got {}",
                                    type_name(&other)
                                )));
                            }
                        }
                    }
                    Value::Variant(name, fields) if name == "Err" && fields.len() == 1 => {
                        if let Value::String(e) = &fields[0] {
                            return Ok(err_v(e.clone()));
                        }
                        return Ok(err_v("stream error".to_string()));
                    }
                    other => {
                        return Ok(err_v(format!(
                            "stream.write_to_file expected Bytes, got {}",
                            type_name(&other)
                        )));
                    }
                };
                if let Err(e) = file.write_all(&bytes) {
                    return Ok(err_v(e.to_string()));
                }
            }
            TryReceiveResult::Closed => break,
            _ => {}
        }
    }
    if let Err(e) = file.flush() {
        return Ok(err_v(e.to_string()));
    }
    Ok(ok(Value::Unit))
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Bool(_) => "Bool",
        Value::List(_) => "List",
        Value::Bytes(_) => "Bytes",
        Value::Channel(_) => "Channel",
        Value::Variant(..) => "Variant",
        _ => "value",
    }
}
