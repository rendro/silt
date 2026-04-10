//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.

mod arithmetic;
mod dispatch;
mod error;
mod execute;
mod runtime;

pub use error::VmError;
pub use runtime::Runtime;
pub(crate) use execute::BuiltinIterKind;
pub(crate) use runtime::{BlockReason, BuiltinAcc, CallFrame, SelectOpKind};

use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::builtins::data::FieldType;
use crate::bytecode::{Chunk, Function, VmClosure};
use crate::scheduler::Scheduler;
use crate::value::{FromValue, IntoValue, IoCompletion, Value};
use runtime::{IoPool, RegexCache, TimerManager};

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) frames: Vec<CallFrame>,
    pub(crate) stack: Vec<Value>,
    pub(crate) globals: HashMap<String, Value>,
    /// Maps record type names to their field definitions (name, type) for json.parse.
    pub(crate) record_types: HashMap<String, Vec<(String, FieldType)>>,

    // ── Concurrency state ────────────────────────────────────────
    next_channel_id: Arc<AtomicU64>,
    next_task_id: Arc<AtomicU64>,

    // ── M:N scheduler state ─────────────────────────────────────
    /// Set by channel/task ops when they need to park this task.
    /// Consumed by execute_slice to return SliceResult::Blocked.
    pub(crate) block_reason: Option<BlockReason>,
    /// True when this VM is running as a scheduled task (not on the main thread).
    pub(crate) is_scheduled_task: bool,
    /// Pending I/O completion handle (persists across yield/re-execute).
    pub(crate) pending_io: Option<Arc<IoCompletion>>,
    /// Saved state from an `invoke_callable` that was interrupted by a yield.
    pub(crate) suspended_invoke: Option<runtime::SuspendedInvoke>,
    /// Saved iteration state for a higher-order builtin (e.g. `list.map`)
    /// whose callback yielded (e.g. via I/O).  On resume, the outer
    /// `CallBuiltin` re-dispatches the same builtin, which picks up its
    /// iteration state from this slot instead of restarting from index 0.
    pub(crate) suspended_builtin: Option<runtime::SuspendedBuiltin>,

    // ── Caches ──────────────────────────────────────────────────
    /// Cache for compiled regex patterns (bounded, LRU-like eviction).
    pub(crate) regex_cache: RegexCache,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a finite float Value, returning an error if the result is NaN or Infinity.
/// Also canonicalizes -0.0 to 0.0.
fn finite_float(f: f64, op_desc: &str) -> Result<Value, VmError> {
    if !f.is_finite() {
        return Err(VmError::new(format!("float overflow: {op_desc}")));
    }
    Ok(Value::Float(if f == 0.0 { 0.0 } else { f }))
}

impl Vm {
    pub fn new() -> Self {
        let mut vm = Vm {
            runtime: Arc::new(Runtime {
                variant_types: HashMap::new(),
                foreign_fns: HashMap::new(),
                scheduler: parking_lot::Mutex::new(None),
                timer: TimerManager::new(),
                io_pool: IoPool::new(
                    std::thread::available_parallelism()
                        .map(|n| n.get().min(4))
                        .unwrap_or(2),
                ),
            }),
            frames: Vec::new(),
            stack: Vec::new(),
            globals: HashMap::new(),
            record_types: HashMap::new(),
            next_channel_id: Arc::new(AtomicU64::new(0)),
            next_task_id: Arc::new(AtomicU64::new(0)),
            block_reason: None,
            is_scheduled_task: false,
            pending_io: None,
            suspended_invoke: None,
            suspended_builtin: None,
            regex_cache: RegexCache::new(),
        };
        vm.register_builtins();
        vm
    }

    // ── Foreign function registration ───────────────────────────

    /// Register a foreign function callable from Silt.
    ///
    /// The function receives `&[Value]` and returns `Result<Value, VmError>`.
    /// Use `FromValue` / `IntoValue` traits for type-safe marshalling.
    ///
    /// # Panics
    /// The closure must not panic. A panic inside `func` will abort the
    /// scheduler worker thread that is executing the call, which may leave
    /// other tasks unable to make progress. Always return `Err(VmError)`
    /// for error conditions instead of panicking.
    ///
    /// # Errors
    /// Returns an error if the VM's runtime has already been shared (e.g. via
    /// task spawning). All foreign functions must be registered before running
    /// any Silt code that spawns tasks.
    pub fn register_fn(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(&[Value]) -> Result<Value, VmError> + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let name = name.into();
        let runtime = Arc::get_mut(&mut self.runtime).ok_or_else(|| {
            VmError::new(format!(
                "cannot register function '{}': VM runtime has already been shared \
                 (register all foreign functions before spawning tasks)",
                name
            ))
        })?;
        runtime.foreign_fns.insert(name.clone(), Arc::new(func));
        self.globals.insert(name.clone(), Value::BuiltinFn(name));
        Ok(())
    }

    /// Register a 0-argument foreign function with automatic marshalling.
    pub fn register_fn0<R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn() -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if !args.is_empty() {
                return Err(VmError::new(format!(
                    "{n2} expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(func().into_value())
        })
    }

    /// Register a 1-argument foreign function with automatic marshalling.
    pub fn register_fn1<A: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A) -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if args.len() != 1 {
                return Err(VmError::new(format!(
                    "{n2} expects 1 argument, got {}",
                    args.len()
                )));
            }
            let a = A::from_value(&args[0]).map_err(|e| VmError::new(format!("{n2}: {e}")))?;
            Ok(func(a).into_value())
        })
    }

    /// Register a 2-argument foreign function with automatic marshalling.
    pub fn register_fn2<A: FromValue, B: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A, B) -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if args.len() != 2 {
                return Err(VmError::new(format!(
                    "{n2} expects 2 arguments, got {}",
                    args.len()
                )));
            }
            let a =
                A::from_value(&args[0]).map_err(|e| VmError::new(format!("{n2}: arg 1: {e}")))?;
            let b =
                B::from_value(&args[1]).map_err(|e| VmError::new(format!("{n2}: arg 2: {e}")))?;
            Ok(func(a, b).into_value())
        })
    }

    /// Register a 3-argument foreign function with automatic marshalling.
    pub fn register_fn3<A: FromValue, B: FromValue, C: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A, B, C) -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if args.len() != 3 {
                return Err(VmError::new(format!(
                    "{n2} expects 3 arguments, got {}",
                    args.len()
                )));
            }
            let a =
                A::from_value(&args[0]).map_err(|e| VmError::new(format!("{n2}: arg 1: {e}")))?;
            let b =
                B::from_value(&args[1]).map_err(|e| VmError::new(format!("{n2}: arg 2: {e}")))?;
            let c =
                C::from_value(&args[2]).map_err(|e| VmError::new(format!("{n2}: arg 3: {e}")))?;
            Ok(func(a, b, c).into_value())
        })
    }

    /// Create a child VM that shares runtime state (variant types, foreign functions)
    /// via Arc and clones per-task state (globals, record types cache).
    /// Used for thread-per-task spawning.
    pub(crate) fn spawn_child(&self) -> Self {
        Vm {
            runtime: self.runtime.clone(), // Arc clone = cheap
            frames: Vec::new(),
            stack: Vec::new(),
            globals: self.globals.clone(),
            record_types: self.record_types.clone(),
            next_channel_id: self.next_channel_id.clone(),
            next_task_id: self.next_task_id.clone(),
            block_reason: None,
            is_scheduled_task: false,
            pending_io: None,
            suspended_invoke: None,
            suspended_builtin: None,
            regex_cache: RegexCache::new(),
        }
    }

    /// Get or create the shared scheduler.
    pub(crate) fn get_or_create_scheduler(&self) -> Arc<Scheduler> {
        let mut guard = self.runtime.scheduler.lock();
        if let Some(ref sched) = *guard {
            sched.clone()
        } else {
            let sched = Arc::new(Scheduler::new());
            *guard = Some(sched.clone());
            sched
        }
    }

    /// Take the block_reason out of this VM (consuming it).
    pub(crate) fn take_block_reason(&mut self) -> Option<BlockReason> {
        self.block_reason.take()
    }

    /// Allocate a new unique channel ID.
    pub(crate) fn next_channel_id(&mut self) -> usize {
        self.next_channel_id.fetch_add(1, Ordering::Relaxed) as usize
    }

    /// Allocate a new unique task ID.
    pub(crate) fn next_task_id(&mut self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed) as usize
    }

    /// Load a compiled top-level function and execute it.
    pub fn run(&mut self, script: Arc<Function>) -> Result<Value, VmError> {
        let closure = Arc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
        self.frames.push(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        });
        self.execute().map_err(|e| self.enrich_error(e))
    }

    // ── Stack operations ──────────────────────────────────────────

    pub(crate) fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or_else(|| {
            let ip = self.frames.last().map(|f| f.ip).unwrap_or(0);
            VmError::new(format!("internal: stack underflow at ip={ip}"))
        })
    }

    fn peek(&self) -> Result<&Value, VmError> {
        self.stack.last().ok_or_else(|| {
            let ip = self.frames.last().map(|f| f.ip).unwrap_or(0);
            VmError::new(format!("internal: stack underflow at ip={ip}"))
        })
    }

    // ── Bytecode reading ──────────────────────────────────────────

    fn read_byte(&mut self) -> Result<u8, VmError> {
        let frame = self
            .frames
            .last()
            .ok_or_else(|| VmError::new("internal: no call frame in read_byte".to_string()))?;
        let ip = frame.ip;
        let byte =
            *frame.closure.function.chunk.code.get(ip).ok_or_else(|| {
                VmError::new(format!("internal: bytecode out of bounds at ip={ip}"))
            })?;
        self.frames
            .last_mut()
            .ok_or_else(|| VmError::new("internal: no call frame in read_byte".to_string()))?
            .ip = ip + 1;
        Ok(byte)
    }

    fn read_u8(&mut self) -> Result<u8, VmError> {
        self.read_byte()
    }

    fn read_u16(&mut self) -> Result<u16, VmError> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_constant(&self, index: usize) -> Result<Value, VmError> {
        let frame = self.current_frame()?;
        frame
            .closure
            .function
            .chunk
            .constants
            .get(index)
            .cloned()
            .ok_or_else(|| VmError::new(format!("internal: constant index {index} out of bounds")))
    }

    fn read_constant_string(&self, index: usize) -> Result<String, VmError> {
        let val = self.read_constant(index)?;
        match val {
            Value::String(s) => Ok(s),
            other => Err(VmError::new(format!(
                "expected string constant at index {index}, got {}",
                self.type_name(&other)
            ))),
        }
    }

    // ── Frame access ──────────────────────────────────────────────

    fn current_frame(&self) -> Result<&CallFrame, VmError> {
        self.frames
            .last()
            .ok_or_else(|| VmError::new("internal: no call frame".to_string()))
    }

    fn current_frame_mut(&mut self) -> Result<&mut CallFrame, VmError> {
        self.frames
            .last_mut()
            .ok_or_else(|| VmError::new("internal: no call frame".to_string()))
    }

    #[allow(dead_code)]
    fn current_chunk(&self) -> Result<&Chunk, VmError> {
        Ok(&self.current_frame()?.closure.function.chunk)
    }

    // ── Error enrichment ─────────────────────────────────────────

    /// Enrich a VmError with the current instruction's source span and the
    /// call stack derived from the VM's frame list.
    pub(crate) fn enrich_error(&self, mut err: VmError) -> VmError {
        if err.is_yield || err.span.is_some() {
            return err;
        }
        // Capture span from current frame's IP position.
        if let Some(frame) = self.frames.last() {
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk.span_at(ip);
            if span.line > 0 {
                err.span = Some(span);
            }
        }
        // Build call stack from all frames (skip the top frame since that's the error site).
        let mut stack = Vec::new();
        for frame in self.frames.iter().rev() {
            let func_name = frame.closure.function.name.clone();
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk.span_at(ip);
            stack.push((func_name, span));
        }
        err.call_stack = stack;
        err
    }

    // ── Truthiness ────────────────────────────────────────────────

    pub(crate) fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Unit => false,
            _ => true,
        }
    }

    fn is_falsy(&self, val: &Value) -> bool {
        !self.is_truthy(val)
    }

    // ── Value display ─────────────────────────────────────────────

    pub(crate) fn display_value(&self, val: &Value) -> String {
        match val {
            Value::String(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            Value::Float(f) => f.to_string(),
            Value::ExtFloat(f) => f.to_string(),
            Value::Range(lo, hi) => format!("{lo}..{hi}"),
            _ => format!("{val}"),
        }
    }

    pub(crate) fn get_regex<'a>(
        cache: &'a mut RegexCache,
        pattern: &str,
    ) -> Result<&'a Regex, VmError> {
        cache.get(pattern)
    }

    fn type_name(&self, val: &Value) -> &'static str {
        match val {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::ExtFloat(_) => "ExtFloat",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::List(_) => "List",
            Value::Range(..) => "Range",
            Value::Map(_) => "Map",
            Value::Set(_) => "Set",
            Value::Tuple(_) => "Tuple",
            Value::Record(..) => "Record",
            Value::Variant(..) => "Variant",
            Value::VmClosure(_) => "Function",
            Value::BuiltinFn(_) => "BuiltinFn",
            Value::VariantConstructor(..) => "VariantConstructor",
            Value::RecordDescriptor(_) => "RecordDescriptor",
            Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
            Value::Channel(_) => "Channel",
            Value::Handle(_) => "Handle",
            Value::Unit => "Unit",
        }
    }

    /// Get the type name for method dispatch. For variants, looks up the parent type.
    fn value_type_name_for_dispatch(&self, val: &Value) -> String {
        match val {
            Value::Variant(tag, _) => {
                // Look up the parent type from the __type_of__ mapping
                let key = format!("__type_of__{tag}");
                if let Some(Value::String(type_name)) = self.globals.get(&key) {
                    type_name.clone()
                } else {
                    tag.clone() // fallback: use the tag itself
                }
            }
            Value::Record(type_name, _) => type_name.clone(),
            Value::Int(_) => "Int".to_string(),
            Value::Float(_) => "Float".to_string(),
            Value::Bool(_) => "Bool".to_string(),
            Value::String(_) => "String".to_string(),
            Value::List(_) => "List".to_string(),
            Value::Map(_) => "Map".to_string(),
            Value::Set(_) => "Set".to_string(),
            Value::Tuple(_) => "Tuple".to_string(),
            _ => "Unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
