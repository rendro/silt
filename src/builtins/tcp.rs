//! `tcp.*` builtin functions: TCP listeners and streams with cooperative
//! I/O integration.
//!
//! Blocking ops (`accept`, `connect`, `read`, `read_exact`, `write`)
//! follow the same pattern as `io.read_file`: check `vm.io_entry_guard`,
//! submit to `vm.runtime.io_pool`, return a yield signal so the scheduler
//! can run other tasks while this one waits. On wake, the entry guard
//! polls completion and resumes.
//!
//! Stream payload type is `Value::Bytes` from PR 1 — read returns Bytes,
//! write accepts Bytes. The `TcpStreamHandle` wraps `Box<dyn ReadWrite>`
//! so v0.9 PR 3 can transparently substitute a TLS-wrapped stream behind
//! the same handle type.
//!
//! Listeners are deliberately bare `std::net::TcpListener` — `accept` is
//! the only blocking op and it locks the listener via the io_pool thread
//! so concurrent silt tasks do not deadlock.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::value::{ReadWrite, TcpListenerHandle, TcpStreamHandle, Value};
use crate::vm::{BlockReason, Vm, VmError};

pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "listen" => listen(vm, args),
        "accept" => accept(vm, args),
        "connect" => connect(vm, args),
        "read" => read(vm, args),
        "read_exact" => read_exact(vm, args),
        "write" => write(vm, args),
        "close" => close(args),
        "peer_addr" => peer_addr(args),
        "set_nodelay" => set_nodelay(args),
        _ => Err(VmError::new(format!("unknown tcp function: {name}"))),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

fn err(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
}

fn require_string<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    match arg {
        Value::String(s) => Ok(s.as_str()),
        _ => Err(VmError::new(format!("{fn_label} requires String"))),
    }
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        _ => Err(VmError::new(format!("{fn_label} requires Int"))),
    }
}

fn require_bool(arg: &Value, fn_label: &str) -> Result<bool, VmError> {
    match arg {
        Value::Bool(b) => Ok(*b),
        _ => Err(VmError::new(format!("{fn_label} requires Bool"))),
    }
}

fn require_listener<'a>(
    arg: &'a Value,
    fn_label: &str,
) -> Result<&'a Arc<TcpListenerHandle>, VmError> {
    match arg {
        Value::TcpListener(l) => Ok(l),
        _ => Err(VmError::new(format!("{fn_label} requires TcpListener"))),
    }
}

fn require_stream<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Arc<TcpStreamHandle>, VmError> {
    match arg {
        Value::TcpStream(s) => Ok(s),
        _ => Err(VmError::new(format!("{fn_label} requires TcpStream"))),
    }
}

fn make_stream(stream: TcpStream, vm: &mut Vm) -> Value {
    let id = vm.next_tcp_id();
    Value::TcpStream(Arc::new(TcpStreamHandle {
        id,
        inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
        closed: AtomicBool::new(false),
    }))
}

// ── Non-blocking ops ───────────────────────────────────────────────────

fn listen(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.listen takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.listen")?;
    match TcpListener::bind(addr) {
        Ok(listener) => {
            let id = vm.next_tcp_id();
            Ok(ok(Value::TcpListener(Arc::new(TcpListenerHandle {
                id,
                listener,
            }))))
        }
        Err(e) => Ok(err(format!("tcp.listen({addr}): {e}"))),
    }
}

fn close(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.close takes 1 argument".into()));
    }
    let s = require_stream(&args[0], "tcp.close")?;
    if !s.closed.swap(true, Ordering::SeqCst) {
        // Best-effort shutdown. We don't surface errors — a closed-twice
        // stream returning Ok matches typical close() ergonomics elsewhere
        // in the stdlib.
        let mut guard = s.inner.lock();
        let _ = guard.flush();
        // Safe-downcast for the shutdown call: only plain TcpStream has
        // shutdown(). Trait-object form means we can't call it directly
        // without unsafe-ish downcast machinery, so we rely on Drop to
        // close the underlying fd when the Arc count hits zero. Mark as
        // closed to make future operations error.
    }
    Ok(Value::Unit)
}

fn peer_addr(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.peer_addr takes 1 argument".into()));
    }
    let _ = require_stream(&args[0], "tcp.peer_addr")?;
    // peer_addr requires the underlying TcpStream; the trait-object form
    // hides it. Returning a placeholder Err for now — full support comes
    // when TLS streams need a uniform peer_addr interface.
    // For PR 2 alone, this is acceptable since most flows don't need it.
    Ok(err(
        "tcp.peer_addr is not yet implemented for trait-object stream handles",
    ))
}

fn set_nodelay(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.set_nodelay takes 2 arguments".into()));
    }
    let _ = require_stream(&args[0], "tcp.set_nodelay")?;
    let _ = require_bool(&args[1], "tcp.set_nodelay")?;
    // Same trait-object issue as peer_addr.
    Ok(err(
        "tcp.set_nodelay is not yet implemented for trait-object stream handles",
    ))
}

// ── Cooperative I/O ops ────────────────────────────────────────────────

fn accept(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.accept takes 1 argument".into()));
    }
    let listener = require_listener(&args[0], "tcp.accept")?.clone();
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let next_id = vm.next_tcp_id();
        let completion = vm
            .runtime
            .io_pool
            .submit(move || match listener.listener.accept() {
                Ok((stream, _addr)) => {
                    let handle = Arc::new(TcpStreamHandle {
                        id: next_id,
                        inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
                        closed: AtomicBool::new(false),
                    });
                    Value::Variant("Ok".into(), vec![Value::TcpStream(handle)])
                }
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    // Main thread: synchronous fallback.
    match listener.listener.accept() {
        Ok((stream, _)) => Ok(ok(make_stream(stream, vm))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn connect(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.connect takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.connect")?.to_string();
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let next_id = vm.next_tcp_id();
        let addr_for_closure = addr.clone();
        let completion =
            vm.runtime
                .io_pool
                .submit(move || match TcpStream::connect(&addr_for_closure) {
                    Ok(stream) => {
                        let handle = Arc::new(TcpStreamHandle {
                            id: next_id,
                            inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
                            closed: AtomicBool::new(false),
                        });
                        Value::Variant("Ok".into(), vec![Value::TcpStream(handle)])
                    }
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
                });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    match TcpStream::connect(&addr) {
        Ok(stream) => Ok(ok(make_stream(stream, vm))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn read(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read")?.clone();
    let max = require_int(&args[1], "tcp.read")?;
    if max < 0 {
        return Ok(err(format!("max must be non-negative, got {max}")));
    }
    let max = max as usize;
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.read: stream is closed"));
    }
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut buf = vec![0u8; max];
            let mut guard = stream_clone.inner.lock();
            match guard.read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    Value::Variant("Ok".into(), vec![Value::Bytes(Arc::new(buf))])
                }
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut buf = vec![0u8; max];
    let mut guard = stream.inner.lock();
    match guard.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Ok(ok(Value::Bytes(Arc::new(buf))))
        }
        Err(e) => Ok(err(e.to_string())),
    }
}

fn read_exact(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read_exact takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read_exact")?.clone();
    let n = require_int(&args[1], "tcp.read_exact")?;
    if n < 0 {
        return Ok(err(format!("n must be non-negative, got {n}")));
    }
    let n = n as usize;
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.read_exact: stream is closed"));
    }
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut buf = vec![0u8; n];
            let mut guard = stream_clone.inner.lock();
            match guard.read_exact(&mut buf) {
                Ok(()) => Value::Variant("Ok".into(), vec![Value::Bytes(Arc::new(buf))]),
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut buf = vec![0u8; n];
    let mut guard = stream.inner.lock();
    match guard.read_exact(&mut buf) {
        Ok(()) => Ok(ok(Value::Bytes(Arc::new(buf)))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn write(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.write takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.write")?.clone();
    let buf = match &args[1] {
        Value::Bytes(b) => b.clone(),
        _ => return Err(VmError::new("tcp.write requires Bytes".into())),
    };
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.write: stream is closed"));
    }
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut guard = stream_clone.inner.lock();
            match guard.write_all(&buf) {
                Ok(()) => match guard.flush() {
                    Ok(()) => Value::Variant("Ok".into(), vec![Value::Unit]),
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
                },
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut guard = stream.inner.lock();
    match guard.write_all(&buf) {
        Ok(()) => match guard.flush() {
            Ok(()) => Ok(ok(Value::Unit)),
            Err(e) => Ok(err(e.to_string())),
        },
        Err(e) => Ok(err(e.to_string())),
    }
}
