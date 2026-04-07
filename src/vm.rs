//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.
//! Phase 2: full function calls (VmClosure + builtins), many builtin
//! dispatches, variant constructors, and end-to-end program execution.

use regex::Regex;
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::builtins;
use crate::builtins::data::FieldType;
use crate::bytecode::{Chunk, Function, Op, VmClosure};
use crate::lexer::Span;
use crate::scheduler::{Scheduler, SliceResult};
use crate::value::{Channel, FromValue, IntoValue, TaskHandle, Value};

/// Type alias for foreign (Rust-side) functions registered with the VM.
type ForeignFn = Arc<dyn Fn(&[Value]) -> Result<Value, VmError> + Send + Sync>;

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct VmError {
    pub message: String,
    /// If true, this error signals a cooperative yield, not a real error.
    pub is_yield: bool,
    /// Source span where the error occurred (if available).
    pub span: Option<Span>,
    /// Call stack at the time of the error: (function_name, span).
    pub call_stack: Vec<(String, Span)>,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError {
            message,
            is_yield: false,
            span: None,
            call_stack: Vec::new(),
        }
    }

    pub(crate) fn yield_signal() -> Self {
        VmError {
            message: String::new(),
            is_yield: true,
            span: None,
            call_stack: Vec::new(),
        }
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VM error: {}", self.message)
    }
}

impl std::error::Error for VmError {}

// ── Call frame ────────────────────────────────────────────────────

pub(crate) struct CallFrame {
    pub(crate) closure: Arc<VmClosure>,
    pub(crate) ip: usize,
    pub(crate) base_slot: usize,
}

// ── Block reason (for M:N scheduler) ────────────────────────────

/// Describes why a task wants to park (block without holding an OS thread).
/// Describes whether a select operation is a receive or send.
#[derive(Clone)]
pub(crate) enum SelectOpKind {
    Receive,
    Send,
}

pub(crate) enum BlockReason {
    /// Blocked on channel.receive (channel was empty).
    Receive(Arc<Channel>),
    /// Blocked on channel.send (channel buffer was full).
    Send(Arc<Channel>),
    /// Blocked on channel.select — carries channels with their operation kinds.
    Select(Vec<(Arc<Channel>, SelectOpKind)>),
    /// Blocked on task.join (target task not yet complete).
    Join(Arc<TaskHandle>),
}

// ── Timer manager (shared single-thread timer wheel) ────────────

/// Manages all pending channel timeouts on a single background thread.
/// Instead of spawning one OS thread per `channel.timeout`, all deadlines
/// are submitted here and fired from a single long-lived thread.
pub(crate) struct TimerManager {
    sender: std::sync::Mutex<std::sync::mpsc::Sender<(Instant, Arc<Channel>)>>,
}

impl TimerManager {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(Instant, Arc<Channel>)>();
        std::thread::spawn(move || {
            let mut deadlines: BTreeMap<Instant, Vec<Arc<Channel>>> = BTreeMap::new();
            loop {
                // Calculate how long to sleep until the next deadline.
                let timeout = deadlines
                    .first_key_value()
                    .map(|(deadline, _)| deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::from_secs(60));

                // Wait for a new timeout request or until the next deadline fires.
                match rx.recv_timeout(timeout) {
                    Ok((deadline, ch)) => {
                        deadlines.entry(deadline).or_default().push(ch);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }

                // Fire all expired deadlines.
                let now = Instant::now();
                let expired: Vec<Instant> = deadlines.range(..=now).map(|(k, _)| *k).collect();
                for key in expired {
                    if let Some(channels) = deadlines.remove(&key) {
                        for ch in channels {
                            ch.close();
                        }
                    }
                }
            }
        });
        TimerManager {
            sender: std::sync::Mutex::new(tx),
        }
    }

    /// Schedule a channel to be closed after `delay`.
    pub(crate) fn schedule(&self, delay: Duration, ch: Arc<Channel>) {
        let deadline = Instant::now() + delay;
        let _ = self.sender.lock().unwrap().send((deadline, ch));
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// Shared, read-only-after-init state for a Silt program.
/// Created once during initialization, then shared across spawned tasks via `Arc`.
pub struct Runtime {
    /// Maps variant tag names to their parent type name, for method dispatch.
    #[allow(dead_code)]
    variant_types: HashMap<String, String>,

    // ── Foreign function interface ──────────────────────────────
    foreign_fns: HashMap<String, ForeignFn>,

    // ── M:N scheduler ──────────────────────────────────────────
    /// The shared scheduler for spawned tasks (None until first task.spawn).
    scheduler: std::sync::Mutex<Option<Arc<Scheduler>>>,

    // ── Timer manager ──────────────────────────────────────────
    /// Shared timer thread for `channel.timeout`.
    pub(crate) timer: TimerManager,
}

// ── Regex cache ──────────────────────────────────────────────────

/// Bounded cache for compiled regex patterns.
///
/// Tracks insertion order with a `VecDeque`. When the cache exceeds
/// `MAX_ENTRIES`, the oldest 25% of entries are evicted instead of
/// clearing the entire cache.
pub(crate) struct RegexCache {
    map: HashMap<String, Regex>,
    order: VecDeque<String>,
}

impl RegexCache {
    const MAX_ENTRIES: usize = 256;
    const EVICT_COUNT: usize = 64; // 25% of MAX_ENTRIES

    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Return a reference to the cached `Regex` for `pattern`, compiling and
    /// caching it if necessary.
    fn get(&mut self, pattern: &str) -> Result<&Regex, VmError> {
        if !self.map.contains_key(pattern) {
            let re =
                Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
            if self.map.len() >= Self::MAX_ENTRIES {
                // Evict the oldest 25% of entries.
                for _ in 0..Self::EVICT_COUNT {
                    if let Some(old_key) = self.order.pop_front() {
                        self.map.remove(&old_key);
                    }
                }
            }
            self.order.push_back(pattern.to_string());
            self.map.insert(pattern.to_string(), re);
        }
        Ok(self.map.get(pattern).unwrap())
    }
}

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) frames: Vec<CallFrame>,
    pub(crate) stack: Vec<Value>,
    pub(crate) globals: HashMap<String, Value>,
    /// Maps record type names to their field definitions (name, type) for json.parse.
    pub(crate) record_types: HashMap<String, Vec<(String, FieldType)>>,

    // ── Concurrency state ────────────────────────────────────────
    next_channel_id: usize,
    next_task_id: usize,

    // ── M:N scheduler state ─────────────────────────────────────
    /// Set by channel/task ops when they need to park this task.
    /// Consumed by execute_slice to return SliceResult::Blocked.
    pub(crate) block_reason: Option<BlockReason>,
    /// True when this VM is running as a scheduled task (not on the main thread).
    pub(crate) is_scheduled_task: bool,

    // ── Caches ──────────────────────────────────────────────────
    /// Cache for compiled regex patterns (bounded, LRU-like eviction).
    pub(crate) regex_cache: RegexCache,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Self {
        let mut vm = Vm {
            runtime: Arc::new(Runtime {
                variant_types: HashMap::new(),
                foreign_fns: HashMap::new(),
                scheduler: std::sync::Mutex::new(None),
                timer: TimerManager::new(),
            }),
            frames: Vec::new(),
            stack: Vec::new(),
            globals: HashMap::new(),
            record_types: HashMap::new(),
            next_channel_id: 0,
            next_task_id: 0,
            block_reason: None,
            is_scheduled_task: false,
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
    pub fn register_fn(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(&[Value]) -> Result<Value, VmError> + Send + Sync + 'static,
    ) {
        let name = name.into();
        Arc::get_mut(&mut self.runtime)
            .expect("register_fn called after VM has been shared")
            .foreign_fns
            .insert(name.clone(), Arc::new(func));
        self.globals.insert(name.clone(), Value::BuiltinFn(name));
    }

    /// Register a 0-argument foreign function with automatic marshalling.
    pub fn register_fn0<R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn() -> R + Send + Sync + 'static,
    ) {
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
        });
    }

    /// Register a 1-argument foreign function with automatic marshalling.
    pub fn register_fn1<A: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A) -> R + Send + Sync + 'static,
    ) {
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
        });
    }

    /// Register a 2-argument foreign function with automatic marshalling.
    pub fn register_fn2<A: FromValue, B: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A, B) -> R + Send + Sync + 'static,
    ) {
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
        });
    }

    /// Register a 3-argument foreign function with automatic marshalling.
    pub fn register_fn3<A: FromValue, B: FromValue, C: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A, B, C) -> R + Send + Sync + 'static,
    ) {
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
        });
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
            next_channel_id: self.next_channel_id,
            next_task_id: self.next_task_id,
            block_reason: None,
            is_scheduled_task: false,
            regex_cache: RegexCache::new(),
        }
    }

    /// Get or create the shared scheduler.
    pub(crate) fn get_or_create_scheduler(&self) -> Arc<Scheduler> {
        let mut guard = self.runtime.scheduler.lock().unwrap();
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
        let id = self.next_channel_id;
        self.next_channel_id += 1;
        id
    }

    /// Allocate a new unique task ID.
    pub(crate) fn next_task_id(&mut self) -> usize {
        let id = self.next_task_id;
        self.next_task_id += 1;
        id
    }

    /// Register all builtin functions and variant constructors in globals.
    fn register_builtins(&mut self) {
        // Variant constructors
        self.globals
            .insert("Ok".into(), Value::VariantConstructor("Ok".into(), 1));
        self.globals
            .insert("Err".into(), Value::VariantConstructor("Err".into(), 1));
        self.globals
            .insert("Some".into(), Value::VariantConstructor("Some".into(), 1));
        self.globals
            .insert("None".into(), Value::Variant("None".into(), Vec::new()));
        self.globals
            .insert("Stop".into(), Value::VariantConstructor("Stop".into(), 1));
        self.globals.insert(
            "Continue".into(),
            Value::VariantConstructor("Continue".into(), 1),
        );
        self.globals.insert(
            "Message".into(),
            Value::VariantConstructor("Message".into(), 1),
        );
        self.globals
            .insert("Closed".into(), Value::Variant("Closed".into(), Vec::new()));
        self.globals
            .insert("Empty".into(), Value::Variant("Empty".into(), Vec::new()));
        for day in [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ] {
            self.globals
                .insert(day.into(), Value::Variant(day.into(), Vec::new()));
        }
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            self.globals
                .insert(method.into(), Value::Variant(method.into(), Vec::new()));
        }

        // Primitive type descriptors
        self.globals
            .insert("Int".into(), Value::PrimitiveDescriptor("Int".into()));
        self.globals
            .insert("Float".into(), Value::PrimitiveDescriptor("Float".into()));
        self.globals
            .insert("String".into(), Value::PrimitiveDescriptor("String".into()));
        self.globals
            .insert("Bool".into(), Value::PrimitiveDescriptor("Bool".into()));

        // Math constants
        self.globals
            .insert("math.pi".into(), Value::Float(std::f64::consts::PI));
        self.globals
            .insert("math.e".into(), Value::Float(std::f64::consts::E));

        // All builtin function names
        let builtin_names = [
            "print",
            "println",
            "io.inspect",
            "panic",
            "list.map",
            "list.filter",
            "list.each",
            "list.fold",
            "list.find",
            "list.zip",
            "list.flatten",
            "list.sort_by",
            "list.flat_map",
            "list.filter_map",
            "list.any",
            "list.all",
            "list.fold_until",
            "list.unfold",
            "list.head",
            "list.tail",
            "list.last",
            "list.reverse",
            "list.sort",
            "list.unique",
            "list.contains",
            "list.length",
            "list.append",
            "list.prepend",
            "list.concat",
            "list.get",
            "list.set",
            "list.take",
            "list.drop",
            "list.enumerate",
            "list.group_by",
            "result.unwrap_or",
            "result.map_ok",
            "result.map_err",
            "result.flatten",
            "result.flat_map",
            "result.is_ok",
            "result.is_err",
            "option.map",
            "option.unwrap_or",
            "option.to_result",
            "option.is_some",
            "option.is_none",
            "option.flat_map",
            "string.from",
            "string.split",
            "string.trim",
            "string.trim_start",
            "string.trim_end",
            "string.char_code",
            "string.from_char_code",
            "string.contains",
            "string.replace",
            "string.join",
            "string.length",
            "string.to_upper",
            "string.to_lower",
            "string.starts_with",
            "string.ends_with",
            "string.chars",
            "string.repeat",
            "string.index_of",
            "string.slice",
            "string.pad_left",
            "string.pad_right",
            "string.is_empty",
            "string.is_alpha",
            "string.is_digit",
            "string.is_upper",
            "string.is_lower",
            "string.is_alnum",
            "string.is_whitespace",
            "int.parse",
            "int.abs",
            "int.min",
            "int.max",
            "int.to_float",
            "int.to_string",
            "float.parse",
            "float.round",
            "float.ceil",
            "float.floor",
            "float.abs",
            "float.to_string",
            "float.to_int",
            "float.min",
            "float.max",
            "map.get",
            "map.set",
            "map.delete",
            "map.contains",
            "map.keys",
            "map.values",
            "map.length",
            "map.merge",
            "map.filter",
            "map.map",
            "map.entries",
            "map.from_entries",
            "map.each",
            "map.update",
            "set.new",
            "set.from_list",
            "set.to_list",
            "set.contains",
            "set.insert",
            "set.remove",
            "set.length",
            "set.union",
            "set.intersection",
            "set.difference",
            "set.is_subset",
            "set.map",
            "set.filter",
            "set.each",
            "set.fold",
            "io.read_file",
            "io.write_file",
            "io.read_line",
            "io.args",
            "fs.exists",
            "test.assert",
            "test.assert_eq",
            "test.assert_ne",
            "math.sqrt",
            "math.pow",
            "math.log",
            "math.log10",
            "math.sin",
            "math.cos",
            "math.tan",
            "math.asin",
            "math.acos",
            "math.atan",
            "math.atan2",
            "regex.is_match",
            "regex.find",
            "regex.find_all",
            "regex.split",
            "regex.replace",
            "regex.replace_all",
            "regex.replace_all_with",
            "regex.captures",
            "regex.captures_all",
            "json.parse",
            "json.parse_list",
            "json.parse_map",
            "json.stringify",
            "json.pretty",
            "channel.new",
            "channel.send",
            "channel.receive",
            "channel.close",
            "channel.try_send",
            "channel.try_receive",
            "channel.select",
            "channel.each",
            "task.spawn",
            "task.join",
            "task.cancel",
            "time.now",
            "time.today",
            "time.date",
            "time.time",
            "time.datetime",
            "time.to_datetime",
            "time.to_instant",
            "time.to_utc",
            "time.from_utc",
            "time.format",
            "time.format_date",
            "time.parse",
            "time.parse_date",
            "time.add_days",
            "time.add_months",
            "time.add",
            "time.since",
            "time.hours",
            "time.minutes",
            "time.seconds",
            "time.ms",
            "time.weekday",
            "time.days_between",
            "time.days_in_month",
            "time.is_leap_year",
            "time.sleep",
            "http.get",
            "http.request",
            "http.serve",
            "http.segments",
        ];

        for name in builtin_names {
            self.globals
                .insert(name.into(), Value::BuiltinFn(name.into()));
        }
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

    // ── Main execution loop ───────────────────────────────────────

    pub(crate) fn execute(&mut self) -> Result<Value, VmError> {
        loop {
            let op_byte = self.read_byte()?;
            match Op::from_byte(op_byte) {
                // ── Constants & literals ───────────────────────
                Some(Op::Constant) => {
                    let index = self.read_u16()? as usize;
                    let value = self.read_constant(index)?;
                    self.push(value);
                }
                Some(Op::Unit) => self.push(Value::Unit),
                Some(Op::True) => self.push(Value::Bool(true)),
                Some(Op::False) => self.push(Value::Bool(false)),

                // ── Arithmetic ────────────────────────────────
                Some(Op::Add) => self.binary_arithmetic(Op::Add)?,
                Some(Op::Sub) => self.binary_arithmetic(Op::Sub)?,
                Some(Op::Mul) => self.binary_arithmetic(Op::Mul)?,
                Some(Op::Div) => self.binary_arithmetic(Op::Div)?,
                Some(Op::Mod) => self.binary_arithmetic(Op::Mod)?,

                // ── Comparison ────────────────────────────────
                Some(Op::Eq) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.check_same_type(&a, &b)?;
                    self.push(Value::Bool(a == b));
                }
                Some(Op::Neq) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.check_same_type(&a, &b)?;
                    self.push(Value::Bool(a != b));
                }
                Some(Op::Lt) => self.compare(|ord| ord.is_lt())?,
                Some(Op::Gt) => self.compare(|ord| ord.is_gt())?,
                Some(Op::Leq) => self.compare(|ord| ord.is_le())?,
                Some(Op::Geq) => self.compare(|ord| ord.is_ge())?,

                // ── Unary ─────────────────────────────────────
                Some(Op::Negate) => {
                    let val = self.pop()?;
                    match val {
                        Value::Int(n) => self.push(Value::Int(-n)),
                        Value::Float(n) => self.push(Value::Float(-n)),
                        other => {
                            return Err(VmError::new(format!(
                                "cannot negate {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }
                Some(Op::Not) => {
                    let val = self.pop()?;
                    match val {
                        Value::Bool(b) => self.push(Value::Bool(!b)),
                        other => {
                            return Err(VmError::new(format!(
                                "cannot apply 'not' to {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }

                // ── Logical ───────────────────────────────────
                Some(Op::And) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    match (&a, &b) {
                        (Value::Bool(a_val), Value::Bool(b_val)) => {
                            self.push(Value::Bool(*a_val && *b_val));
                        }
                        _ => {
                            return Err(VmError::new(
                                "logical 'and' requires two booleans".to_string(),
                            ));
                        }
                    }
                }
                Some(Op::Or) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    match (&a, &b) {
                        (Value::Bool(a_val), Value::Bool(b_val)) => {
                            self.push(Value::Bool(*a_val || *b_val));
                        }
                        _ => {
                            return Err(VmError::new(
                                "logical 'or' requires two booleans".to_string(),
                            ));
                        }
                    }
                }

                // ── String interpolation ──────────────────────
                Some(Op::DisplayValue) => {
                    let val = self.pop()?;
                    if matches!(&val, Value::String(_)) {
                        self.push(val);
                    } else {
                        let s = self.display_value(&val);
                        self.push(Value::String(s));
                    }
                }
                Some(Op::StringConcat) => {
                    let count = self.read_u8()? as usize;
                    let start = self.stack.len() - count;
                    // Pre-calculate total capacity to avoid reallocations
                    let mut total_len = 0;
                    for i in start..self.stack.len() {
                        if let Value::String(ref s) = self.stack[i] {
                            total_len += s.len();
                        } else {
                            return Err(VmError::new(
                                "StringConcat: non-string value on stack".to_string(),
                            ));
                        }
                    }
                    let mut result = String::with_capacity(total_len);
                    for i in start..self.stack.len() {
                        if let Value::String(ref s) = self.stack[i] {
                            result.push_str(s);
                        }
                    }
                    self.stack.truncate(start);
                    self.push(Value::String(result));
                }

                // ── Variables ─────────────────────────────────
                Some(Op::GetLocal) => {
                    let slot = self.read_u16()? as usize;
                    let base = self.current_frame()?.base_slot;
                    let value = self.stack[base + slot].clone();
                    self.push(value);
                }
                Some(Op::SetLocal) => {
                    let slot = self.read_u16()? as usize;
                    let base = self.current_frame()?.base_slot;
                    let value = self.peek()?.clone();
                    // Extend stack if needed (for first-time local assignment)
                    let target = base + slot;
                    while self.stack.len() <= target {
                        self.stack.push(Value::Unit);
                    }
                    self.stack[target] = value;
                }
                Some(Op::GetGlobal) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let value = self
                        .globals
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("undefined global: {name}")))?;
                    self.push(value);
                }
                Some(Op::SetGlobal) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let value = self.peek()?.clone();
                    self.globals.insert(name, value);
                }

                // ── Upvalues ──────────────────────────────────
                Some(Op::GetUpvalue) => {
                    let index = self.read_u8()? as usize;
                    let value = self.current_frame()?.closure.upvalues[index].clone();
                    self.push(value);
                }

                // ── Function calls ────────────────────────────
                Some(Op::Call) => {
                    let argc = self.read_u8()? as usize;
                    // The function value sits below the arguments on the stack.
                    let func_slot = self.stack.len() - 1 - argc;
                    let func_val = self.stack[func_slot].clone();
                    self.call_value(func_val, argc, func_slot)?;
                }
                Some(Op::TailCall) => {
                    let argc = self.read_u8()? as usize;
                    let func_slot = self.stack.len() - 1 - argc;
                    let func_val = self.stack[func_slot].clone();
                    match func_val {
                        Value::VmClosure(closure) => {
                            if argc != closure.function.arity as usize {
                                return Err(VmError::new(format!(
                                    "function '{}' expects {} arguments, got {}",
                                    closure.function.name, closure.function.arity, argc
                                )));
                            }
                            // Reuse current frame: move args to base_slot
                            let base = self.current_frame()?.base_slot;
                            for i in 0..argc {
                                self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                            }
                            self.stack.truncate(base + argc);
                            let frame = self.current_frame_mut()?;
                            frame.closure = closure;
                            frame.ip = 0;
                        }
                        _ => {
                            // Fall back to normal call
                            self.call_value(func_val, argc, func_slot)?;
                        }
                    }
                }
                Some(Op::Return) => {
                    let result = self.pop()?;
                    let finished_base = self.current_frame()?.base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return Ok(result);
                    }
                    // Discard the callee's stack window including the function slot.
                    // base_slot = func_slot + 1, so func_slot = base_slot - 1.
                    let func_slot = if finished_base > 0 {
                        finished_base - 1
                    } else {
                        0
                    };
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                Some(Op::CallBuiltin) => {
                    let name_index = self.read_u16()? as usize;
                    let argc = self.read_u8()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let start = self.stack.len() - argc;
                    let args: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    let result = self.dispatch_builtin(&name, &args)?;
                    self.push(result);
                }

                // ── Closures ──────────────────────────────────
                Some(Op::MakeClosure) => {
                    let func_index = self.read_u16()? as usize;
                    let upvalue_count = self.read_u8()? as usize;
                    let constant = self.read_constant(func_index)?;

                    // Collect upvalue values from the descriptors
                    let mut upvalues = Vec::with_capacity(upvalue_count);
                    for _ in 0..upvalue_count {
                        let is_local = self.read_u8()? != 0;
                        let index = self.read_u8()? as usize;
                        let val = if is_local {
                            let base = self.current_frame()?.base_slot;
                            self.stack[base + index].clone()
                        } else {
                            self.current_frame()?.closure.upvalues[index].clone()
                        };
                        upvalues.push(val);
                    }

                    // The constant should be a VmClosure wrapping the function
                    match constant {
                        Value::VmClosure(existing) => {
                            let closure = Arc::new(VmClosure {
                                function: existing.function.clone(),
                                upvalues,
                            });
                            self.push(Value::VmClosure(closure));
                        }
                        _ => {
                            // Fallback: push Unit (shouldn't happen in Phase 2)
                            self.push(Value::Unit);
                        }
                    }
                }

                // ── Data constructors ─────────────────────────
                Some(Op::MakeTuple) => {
                    let count = self.read_u8()? as usize;
                    let start = self.stack.len() - count;
                    let elements: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::Tuple(elements));
                }
                Some(Op::MakeList) => {
                    let count = self.read_u16()? as usize;
                    let start = self.stack.len() - count;
                    let elements: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::List(Arc::new(elements)));
                }
                Some(Op::MakeMap) => {
                    let pair_count = self.read_u16()? as usize;
                    let total = pair_count * 2;
                    let start = self.stack.len() - total;
                    let mut map = BTreeMap::new();
                    for i in (start..self.stack.len()).step_by(2) {
                        let key = self.stack[i].clone();
                        let val = self.stack[i + 1].clone();
                        map.insert(key, val);
                    }
                    self.stack.truncate(start);
                    self.push(Value::Map(Arc::new(map)));
                }
                Some(Op::MakeSet) => {
                    let count = self.read_u16()? as usize;
                    let start = self.stack.len() - count;
                    let mut set = BTreeSet::new();
                    for i in start..self.stack.len() {
                        set.insert(self.stack[i].clone());
                    }
                    self.stack.truncate(start);
                    self.push(Value::Set(Arc::new(set)));
                }
                Some(Op::MakeRecord) => {
                    let type_name_index = self.read_u16()? as usize;
                    let field_count = self.read_u8()? as usize;
                    let mut field_names = Vec::with_capacity(field_count);
                    for _ in 0..field_count {
                        let name_index = self.read_u16()? as usize;
                        field_names.push(self.read_constant_string(name_index)?);
                    }
                    let type_name = self.read_constant_string(type_name_index)?;
                    let start = self.stack.len() - field_count;
                    let mut fields = BTreeMap::new();
                    for (i, name) in field_names.into_iter().enumerate() {
                        fields.insert(name, self.stack[start + i].clone());
                    }
                    self.stack.truncate(start);
                    self.push(Value::Record(type_name, Arc::new(fields)));
                }
                Some(Op::MakeVariant) => {
                    let name_index = self.read_u16()? as usize;
                    let field_count = self.read_u8()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let start = self.stack.len() - field_count;
                    let fields: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::Variant(name, fields));
                }
                Some(Op::RecordUpdate) => {
                    let field_count = self.read_u8()? as usize;
                    let mut field_names = Vec::with_capacity(field_count);
                    for _ in 0..field_count {
                        let name_index = self.read_u16()? as usize;
                        field_names.push(self.read_constant_string(name_index)?);
                    }
                    // Stack: [base_record, new_val_1, ..., new_val_N]
                    let start = self.stack.len() - field_count;
                    let new_values: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    let base = self.pop()?;
                    match base {
                        Value::Record(type_name, existing) => {
                            let mut fields = (*existing).clone();
                            for (name, val) in field_names.into_iter().zip(new_values.into_iter()) {
                                fields.insert(name, val);
                            }
                            self.push(Value::Record(type_name, Arc::new(fields)));
                        }
                        other => {
                            return Err(VmError::new(format!(
                                "record update on non-record: {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }
                Some(Op::MakeRange) => {
                    let end = self.pop()?;
                    let start = self.pop()?;
                    match (&start, &end) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.push(Value::Range(*a, *b));
                        }
                        _ => {
                            return Err(VmError::new("range requires two integers".to_string()));
                        }
                    }
                }
                Some(Op::ListConcat) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let mut result = match a {
                        Value::List(xs) => xs.as_ref().clone(),
                        Value::Range(lo, hi) => (lo..=hi).map(Value::Int).collect(),
                        _ => {
                            return Err(VmError::new(
                                "ListConcat: left operand is not a list or range".into(),
                            ));
                        }
                    };
                    match b {
                        Value::List(xs) => result.extend(xs.iter().cloned()),
                        Value::Range(lo, hi) => result.extend((lo..=hi).map(Value::Int)),
                        _ => {
                            return Err(VmError::new(
                                "ListConcat: right operand is not a list or range".into(),
                            ));
                        }
                    }
                    self.push(Value::List(Arc::new(result)));
                }

                // ── Field access ──────────────────────────────
                Some(Op::GetField) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let target = self.pop()?;
                    match target {
                        Value::Record(_, ref fields) => {
                            let val = fields.get(&name).cloned().ok_or_else(|| {
                                VmError::new(format!("record has no field '{name}'"))
                            })?;
                            self.push(val);
                        }
                        Value::Map(ref map) => {
                            let key = Value::String(name.clone());
                            let val = map
                                .get(&key)
                                .cloned()
                                .ok_or_else(|| VmError::new(format!("map has no key '{name}'")))?;
                            self.push(val);
                        }
                        other => {
                            return Err(VmError::new(format!(
                                "cannot access field '{}' on {}",
                                name,
                                self.type_name(&other)
                            )));
                        }
                    }
                }
                Some(Op::GetIndex) => {
                    let index = self.read_u8()? as usize;
                    let target = self.pop()?;
                    match target {
                        Value::Tuple(ref elems) => {
                            let val = elems.get(index).cloned().ok_or_else(|| {
                                VmError::new(format!(
                                    "tuple index {index} out of bounds (len {})",
                                    elems.len()
                                ))
                            })?;
                            self.push(val);
                        }
                        other => {
                            return Err(VmError::new(format!(
                                "cannot index into {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }

                // ── Control flow ──────────────────────────────
                Some(Op::Jump) => {
                    let offset = self.read_u16()? as usize;
                    let frame = self.current_frame_mut()?;
                    frame.ip += offset;
                }
                Some(Op::JumpBack) => {
                    let offset = self.read_u16()? as usize;
                    let frame = self.current_frame_mut()?;
                    frame.ip -= offset;
                }
                Some(Op::JumpIfFalse) => {
                    let offset = self.read_u16()? as usize;
                    let val = self.pop()?;
                    if self.is_falsy(&val) {
                        let frame = self.current_frame_mut()?;
                        frame.ip += offset;
                    }
                }
                Some(Op::JumpIfTrue) => {
                    let offset = self.read_u16()? as usize;
                    let val = self.pop()?;
                    if self.is_truthy(&val) {
                        let frame = self.current_frame_mut()?;
                        frame.ip += offset;
                    }
                }
                Some(Op::Pop) => {
                    self.pop()?;
                }
                Some(Op::PopN) => {
                    let count = self.read_u8()? as usize;
                    let new_len = self.stack.len().saturating_sub(count);
                    self.stack.truncate(new_len);
                }
                Some(Op::Dup) => {
                    let val = self.peek()?.clone();
                    self.push(val);
                }

                // ── Pattern matching ──────────────────────────
                Some(Op::TestTag) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek()?;
                    let result = matches!(val, Value::Variant(tag, _) if *tag == name);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestEqual) => {
                    let const_index = self.read_u16()? as usize;
                    let constant = self.read_constant(const_index)?;
                    let val = self.peek()?;
                    let result = *val == constant;
                    self.push(Value::Bool(result));
                }
                Some(Op::TestTupleLen) => {
                    let len = self.read_u8()? as usize;
                    let val = self.peek()?;
                    let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestListMin) => {
                    let min_len = self.read_u8()? as usize;
                    let val = self.peek()?;
                    let result = val.collection_len().map_or(false, |len| len >= min_len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestListExact) => {
                    let len = self.read_u8()? as usize;
                    let val = self.peek()?;
                    let result = val.collection_len().map_or(false, |l| l == len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestIntRange) => {
                    let lo_index = self.read_u16()? as usize;
                    let hi_index = self.read_u16()? as usize;
                    let lo = self.read_constant(lo_index)?;
                    let hi = self.read_constant(hi_index)?;
                    let val = self.peek()?;
                    let result = match (val, &lo, &hi) {
                        (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::TestFloatRange) => {
                    let lo_index = self.read_u16()? as usize;
                    let hi_index = self.read_u16()? as usize;
                    let lo = self.read_constant(lo_index)?;
                    let hi = self.read_constant(hi_index)?;
                    let val = self.peek()?;
                    let result = match (val, &lo, &hi) {
                        (Value::Float(n), Value::Float(lo), Value::Float(hi)) => {
                            *n >= *lo && *n <= *hi
                        }
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::TestBool) => {
                    let expected = self.read_u8()? != 0;
                    let val = self.peek()?;
                    let result = matches!(val, Value::Bool(b) if *b == expected);
                    self.push(Value::Bool(result));
                }
                Some(Op::DestructTuple) => {
                    let index = self.read_u8()? as usize;
                    let val = self.peek()?.clone();
                    match val {
                        Value::Tuple(elems) => {
                            self.push(elems[index].clone());
                        }
                        _ => return Err(VmError::new("DestructTuple on non-tuple".to_string())),
                    }
                }
                Some(Op::DestructVariant) => {
                    let index = self.read_u8()? as usize;
                    let val = self.peek()?.clone();
                    match val {
                        Value::Variant(_, fields) => {
                            self.push(fields[index].clone());
                        }
                        _ => {
                            return Err(VmError::new("DestructVariant on non-variant".to_string()));
                        }
                    }
                }
                Some(Op::DestructList) => {
                    let index = self.read_u8()? as usize;
                    let val = self.peek()?.clone();
                    match val {
                        Value::List(xs) => {
                            self.push(xs[index].clone());
                        }
                        Value::Range(lo, hi) => {
                            let i = lo + index as i64;
                            if i > hi {
                                return Err(VmError::new("range index out of bounds".to_string()));
                            }
                            self.push(Value::Int(i));
                        }
                        _ => return Err(VmError::new("DestructList on non-list".to_string())),
                    }
                }
                Some(Op::DestructListRest) => {
                    let start = self.read_u8()? as usize;
                    let val = self.peek()?.clone();
                    match val {
                        Value::List(xs) => {
                            let rest: Vec<Value> = xs[start..].to_vec();
                            self.push(Value::List(Arc::new(rest)));
                        }
                        Value::Range(lo, hi) => {
                            let new_lo = lo + start as i64;
                            if new_lo > hi + 1 {
                                self.push(Value::List(Arc::new(Vec::new())));
                            } else {
                                self.push(Value::Range(new_lo, hi));
                            }
                        }
                        _ => return Err(VmError::new("DestructListRest on non-list".to_string())),
                    }
                }
                Some(Op::DestructRecordField) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek()?.clone();
                    match val {
                        Value::Record(_, fields) => {
                            let field = fields.get(&name).cloned().ok_or_else(|| {
                                VmError::new(format!("record has no field '{name}'"))
                            })?;
                            self.push(field);
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructRecordField on non-record".to_string(),
                            ));
                        }
                    }
                }
                Some(Op::TestRecordTag) => {
                    let name_index = self.read_u16()? as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek()?;
                    let result = matches!(val, Value::Record(tag, _) if *tag == name);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestMapHasKey) => {
                    let const_index = self.read_u16()? as usize;
                    let key_name = self.read_constant_string(const_index)?;
                    let val = self.peek()?;
                    let result = match val {
                        Value::Map(map) => map.contains_key(&Value::String(key_name)),
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::DestructMapValue) => {
                    let const_index = self.read_u16()? as usize;
                    let key_name = self.read_constant_string(const_index)?;
                    let val = self.peek()?.clone();
                    match val {
                        Value::Map(map) => {
                            let value = map
                                .get(&Value::String(key_name.clone()))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::new(format!("map has no key '{key_name}'"))
                                })?;
                            self.push(value);
                        }
                        _ => return Err(VmError::new("DestructMapValue on non-map".to_string())),
                    }
                }

                // ── Loop ──────────────────────────────────────
                Some(Op::LoopSetup) => {
                    let _binding_count = self.read_u8()?;
                    // Loop setup is handled during compilation by placing
                    // bindings in local slots. Nothing to do at runtime.
                }
                Some(Op::Recur) => {
                    let arg_count = self.read_u8()? as usize;
                    let first_slot = self.read_u16()? as usize;
                    // Update loop bindings: the new values are on top of stack.
                    // Copy them back into the binding slots starting at first_slot.
                    let base = self.current_frame()?.base_slot;
                    let start = self.stack.len() - arg_count;
                    for i in 0..arg_count {
                        self.stack[base + first_slot + i] = self.stack[start + i].clone();
                    }
                    // Truncate all the way back to just after loop bindings.
                    // This cleans up any intermediate values from the loop body.
                    self.stack.truncate(base + first_slot + arg_count);
                }

                // ── Error handling ────────────────────────────
                Some(Op::QuestionMark) => {
                    let val = self.peek()?.clone();
                    match val {
                        Value::Variant(ref tag, ref fields) => {
                            match tag.as_str() {
                                "Ok" | "Some" => {
                                    self.pop()?;
                                    if fields.len() == 1 {
                                        self.push(fields[0].clone());
                                    } else {
                                        self.push(Value::Unit);
                                    }
                                }
                                "Err" | "None" => {
                                    // Early return with the error/none value.
                                    let result = self.pop()?;
                                    let finished_base = self.current_frame()?.base_slot;
                                    self.frames.pop();
                                    if self.frames.is_empty() {
                                        return Ok(result);
                                    }
                                    // Truncate to the func_slot (base - 1) to
                                    // clean up the callee's stack window.
                                    let func_slot = if finished_base > 0 {
                                        finished_base - 1
                                    } else {
                                        0
                                    };
                                    self.stack.truncate(func_slot);
                                    self.push(result);
                                }
                                _ => {
                                    return Err(VmError::new(format!(
                                        "? operator on non-Result/Option variant: {tag}"
                                    )));
                                }
                            }
                        }
                        _ => {
                            return Err(VmError::new(format!(
                                "? operator on non-variant: {}",
                                self.type_name(&val)
                            )));
                        }
                    }
                }
                Some(Op::Panic) => {
                    let msg = self.pop()?;
                    return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
                }

                // ── Method dispatch ───────────────────────────
                Some(Op::CallMethod) => {
                    let method_name_index = self.read_u16()? as usize;
                    let argc = self.read_u8()? as usize;
                    let method_name = self.read_constant_string(method_name_index)?;
                    // The receiver is at stack[len - argc] (first arg)
                    let receiver_slot = self.stack.len() - argc;
                    let receiver = self.stack[receiver_slot].clone();
                    let type_name = self.value_type_name_for_dispatch(&receiver);
                    let qualified = format!("{type_name}.{method_name}");
                    if let Some(func) = self.globals.get(&qualified).cloned() {
                        // Replace: treat receiver + args as args to the function
                        let args: Vec<Value> = self.stack[receiver_slot..].to_vec();
                        self.stack.truncate(receiver_slot);
                        let result = self.invoke_callable(&func, &args)?;
                        self.push(result);
                    } else {
                        let extra_args: Vec<Value> = self.stack[receiver_slot + 1..].to_vec();
                        // Try built-in trait methods (display, equal, compare)
                        if let Some(result) =
                            self.dispatch_trait_method(&receiver, &method_name, &extra_args)
                        {
                            self.stack.truncate(receiver_slot);
                            self.push(result?);
                        } else if let Value::Record(_, ref fields) = receiver {
                            if let Some(field_val) = fields.get(&method_name) {
                                let callable = field_val.clone();
                                self.stack.truncate(receiver_slot);
                                let result = self.invoke_callable(&callable, &extra_args)?;
                                self.push(result);
                            } else {
                                return Err(VmError::new(format!(
                                    "no method '{method_name}' for type '{type_name}'"
                                )));
                            }
                        } else {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{type_name}'"
                            )));
                        }
                    }
                }

                // ── Concurrency (stubs for Phase 2) ───────────
                Some(Op::ChanNew)
                | Some(Op::ChanSend)
                | Some(Op::ChanRecv)
                | Some(Op::ChanClose)
                | Some(Op::ChanTrySend)
                | Some(Op::ChanTryRecv)
                | Some(Op::ChanSelect)
                | Some(Op::TaskSpawn)
                | Some(Op::TaskJoin)
                | Some(Op::TaskCancel)
                | Some(Op::Yield) => {
                    return Err(VmError::new(
                        "concurrency opcodes not yet implemented".to_string(),
                    ));
                }

                None => {
                    return Err(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            }
        }
    }

    // ── Sliced execution (for M:N scheduler) ─────────────────────

    /// Run up to `max_steps` instructions and return a `SliceResult`.
    /// Used by the M:N scheduler's worker threads.
    pub fn execute_slice(&mut self, max_steps: usize) -> SliceResult {
        // Helper macro to convert Result to SliceResult::Failed on error.
        macro_rules! try_or_fail {
            ($expr:expr) => {
                match $expr {
                    Ok(v) => v,
                    Err(e) => return SliceResult::Failed(e),
                }
            };
        }
        for _ in 0..max_steps {
            if self.frames.is_empty() {
                let result = if self.stack.is_empty() {
                    Value::Unit
                } else {
                    self.stack.last().cloned().unwrap_or(Value::Unit)
                };
                return SliceResult::Completed(result);
            }
            let saved_ip = try_or_fail!(self.current_frame()).ip;
            let op_byte = try_or_fail!(self.read_byte());
            match Op::from_byte(op_byte) {
                Some(Op::Return) => {
                    let result = try_or_fail!(self.pop());
                    let finished_base = try_or_fail!(self.current_frame()).base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return SliceResult::Completed(result);
                    }
                    let func_slot = if finished_base > 0 {
                        finished_base - 1
                    } else {
                        0
                    };
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                Some(op) => {
                    match self.dispatch_op(op) {
                        Ok(()) => {}
                        Err(e) if e.is_yield => {
                            // Cooperative yield: rewind IP to re-execute.
                            try_or_fail!(self.current_frame_mut()).ip = saved_ip;
                            // Check if this was a block request.
                            if self.block_reason.is_some() {
                                return SliceResult::Blocked;
                            }
                            return SliceResult::Yielded;
                        }
                        Err(e) => return SliceResult::Failed(e),
                    }
                    // Check if a blocking operation set block_reason without yield.
                    if self.block_reason.is_some() {
                        return SliceResult::Blocked;
                    }
                }
                None => {
                    return SliceResult::Failed(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            }
        }
        // Time slice expired.
        SliceResult::Yielded
    }

    // ── Call a value ──────────────────────────────────────────────

    fn call_value(
        &mut self,
        func_val: Value,
        argc: usize,
        func_slot: usize,
    ) -> Result<(), VmError> {
        const MAX_FRAMES: usize = 100_000;
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, argc
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(VmError::new(format!(
                        "stack overflow: recursion depth exceeded {} frames (tip: put the recursive call in tail position to avoid this limit)",
                        MAX_FRAMES
                    )));
                }
                // Push a new call frame. The arguments are already on the stack
                // at positions [func_slot+1 .. func_slot+1+argc].
                // The base_slot for the new frame is func_slot+1 so the args
                // are at locals[0..argc].
                self.frames.push(CallFrame {
                    closure,
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                Ok(())
            }
            Value::BuiltinFn(name) => {
                let start = func_slot + 1;
                let args: Vec<Value> = self.stack[start..start + argc].to_vec();
                // Pop everything including the function slot
                self.stack.truncate(func_slot);
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
                Ok(())
            }
            Value::VariantConstructor(name, arity) => {
                if argc != arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {argc}"
                    )));
                }
                let start = func_slot + 1;
                let fields: Vec<Value> = self.stack[start..start + argc].to_vec();
                self.stack.truncate(func_slot);
                self.push(Value::Variant(name, fields));
                Ok(())
            }
            _ => Err(VmError::new(format!(
                "cannot call value of type {}",
                self.type_name(&func_val)
            ))),
        }
    }

    /// Call a callable Value and return its result. Used for higher-order builtins.
    pub(crate) fn invoke_callable(
        &mut self,
        func: &Value,
        args: &[Value],
    ) -> Result<Value, VmError> {
        match func {
            Value::VmClosure(closure) => {
                if args.len() != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name,
                        closure.function.arity,
                        args.len()
                    )));
                }
                // Save state
                let saved_frame_count = self.frames.len();
                let func_slot = self.stack.len();
                // Push a dummy for the function slot
                self.push(Value::Unit);
                for arg in args {
                    self.push(arg.clone());
                }
                self.frames.push(CallFrame {
                    closure: closure.clone(),
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                // Run the execution loop until we return to the previous frame count
                loop {
                    let op_byte = self.read_byte()?;
                    match Op::from_byte(op_byte) {
                        Some(Op::Return) => {
                            let result = self.pop()?;
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            if self.frames.len() < saved_frame_count {
                                // This shouldn't happen
                                return Err(VmError::new(
                                    "frame underflow in invoke_callable".into(),
                                ));
                            }
                            if self.frames.len() == saved_frame_count {
                                // We've returned from our closure
                                self.stack.truncate(func_slot);
                                return Ok(result);
                            }
                            // Otherwise, it's an inner return (nested calls)
                            let inner_func_slot = if finished_base > 0 {
                                finished_base - 1
                            } else {
                                0
                            };
                            self.stack.truncate(inner_func_slot);
                            self.push(result);
                        }
                        Some(op) => {
                            // Re-run the same dispatch logic. Since we can't easily
                            // factor out the dispatch, let's use a helper.
                            match self.dispatch_op(op) {
                                Ok(()) => {}
                                Err(e) => {
                                    // Clean up stack and frames on error.
                                    self.frames.truncate(saved_frame_count);
                                    self.stack.truncate(func_slot);
                                    return Err(e);
                                }
                            }
                        }
                        None => {
                            self.frames.truncate(saved_frame_count);
                            self.stack.truncate(func_slot);
                            return Err(VmError::new(format!("unknown opcode: {op_byte}")));
                        }
                    }
                }
            }
            Value::BuiltinFn(name) => self.dispatch_builtin(name, args),
            Value::VariantConstructor(name, arity) => {
                if args.len() != *arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Value::Variant(name.clone(), args.to_vec()))
            }
            _ => Err(VmError::new(
                "cannot call value in invoke_callable".to_string(),
            )),
        }
    }

    /// Dispatch a single opcode (factored out so invoke_callable can reuse it).
    fn dispatch_op(&mut self, op: Op) -> Result<(), VmError> {
        match op {
            Op::Constant => {
                let index = self.read_u16()? as usize;
                let value = self.read_constant(index)?;
                self.push(value);
            }
            Op::Unit => self.push(Value::Unit),
            Op::True => self.push(Value::Bool(true)),
            Op::False => self.push(Value::Bool(false)),
            Op::Add => self.binary_arithmetic(Op::Add)?,
            Op::Sub => self.binary_arithmetic(Op::Sub)?,
            Op::Mul => self.binary_arithmetic(Op::Mul)?,
            Op::Div => self.binary_arithmetic(Op::Div)?,
            Op::Mod => self.binary_arithmetic(Op::Mod)?,
            Op::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a == b));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a != b));
            }
            Op::Lt => self.compare(|ord| ord.is_lt())?,
            Op::Gt => self.compare(|ord| ord.is_gt())?,
            Op::Leq => self.compare(|ord| ord.is_le())?,
            Op::Geq => self.compare(|ord| ord.is_ge())?,
            Op::Negate => {
                let val = self.pop()?;
                match val {
                    Value::Int(n) => self.push(Value::Int(-n)),
                    Value::Float(n) => self.push(Value::Float(-n)),
                    other => {
                        return Err(VmError::new(format!(
                            "cannot negate {}",
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::Not => {
                let val = self.pop()?;
                match val {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => {
                        return Err(VmError::new(format!(
                            "cannot apply 'not' to {}",
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val && *b_val))
                    }
                    _ => return Err(VmError::new("logical 'and' requires two booleans".into())),
                }
            }
            Op::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val || *b_val))
                    }
                    _ => return Err(VmError::new("logical 'or' requires two booleans".into())),
                }
            }
            Op::DisplayValue => {
                let val = self.pop()?;
                if matches!(&val, Value::String(_)) {
                    self.push(val);
                } else {
                    let s = self.display_value(&val);
                    self.push(Value::String(s));
                }
            }
            Op::StringConcat => {
                let count = self.read_u8()? as usize;
                let start = self.stack.len() - count;
                // Pre-calculate total capacity to avoid reallocations
                let mut total_len = 0;
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        total_len += s.len();
                    } else {
                        return Err(VmError::new(
                            "StringConcat: non-string value on stack".into(),
                        ));
                    }
                }
                let mut result = String::with_capacity(total_len);
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        result.push_str(s);
                    }
                }
                self.stack.truncate(start);
                self.push(Value::String(result));
            }
            Op::GetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self.stack[base + slot].clone();
                self.push(value);
            }
            Op::SetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self.peek()?.clone();
                let target = base + slot;
                while self.stack.len() <= target {
                    self.stack.push(Value::Unit);
                }
                self.stack[target] = value;
            }
            Op::GetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self
                    .globals
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| VmError::new(format!("undefined global: {name}")))?;
                self.push(value);
            }
            Op::SetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self.peek()?.clone();
                self.globals.insert(name, value);
            }
            Op::GetUpvalue => {
                let index = self.read_u8()? as usize;
                let value = self.current_frame()?.closure.upvalues[index].clone();
                self.push(value);
            }
            Op::Call => {
                let argc = self.read_u8()? as usize;
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                self.call_value(func_val, argc, func_slot)?;
            }
            Op::TailCall => {
                let argc = self.read_u8()? as usize;
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                if let Value::VmClosure(closure) = func_val {
                    let base = self.current_frame()?.base_slot;
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
                    let frame = self.current_frame_mut()?;
                    frame.closure = closure;
                    frame.ip = 0;
                } else {
                    self.call_value(func_val, argc, func_slot)?;
                }
            }
            // Return is handled specially in invoke_callable, not here.
            Op::Return => {
                unreachable!("Return should be handled by caller");
            }
            Op::CallBuiltin => {
                let name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                let start = self.stack.len() - argc;
                let args: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
            }
            Op::MakeClosure => {
                let func_index = self.read_u16()? as usize;
                let upvalue_count = self.read_u8()? as usize;
                let constant = self.read_constant(func_index)?;
                let mut upvalues = Vec::with_capacity(upvalue_count);
                for _ in 0..upvalue_count {
                    let is_local = self.read_u8()? != 0;
                    let index = self.read_u8()? as usize;
                    let val = if is_local {
                        let base = self.current_frame()?.base_slot;
                        self.stack[base + index].clone()
                    } else {
                        self.current_frame()?.closure.upvalues[index].clone()
                    };
                    upvalues.push(val);
                }
                if let Value::VmClosure(existing) = constant {
                    let closure = Arc::new(VmClosure {
                        function: existing.function.clone(),
                        upvalues,
                    });
                    self.push(Value::VmClosure(closure));
                } else {
                    self.push(Value::Unit);
                }
            }
            Op::MakeTuple => {
                let count = self.read_u8()? as usize;
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Tuple(elements));
            }
            Op::MakeList => {
                let count = self.read_u16()? as usize;
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::List(Arc::new(elements)));
            }
            Op::MakeMap => {
                let pair_count = self.read_u16()? as usize;
                let total = pair_count * 2;
                let start = self.stack.len() - total;
                let mut map = BTreeMap::new();
                for i in (start..self.stack.len()).step_by(2) {
                    map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Map(Arc::new(map)));
            }
            Op::MakeSet => {
                let count = self.read_u16()? as usize;
                let start = self.stack.len() - count;
                let mut set = BTreeSet::new();
                for i in start..self.stack.len() {
                    set.insert(self.stack[i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Set(Arc::new(set)));
            }
            Op::MakeRecord => {
                let type_name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let name_index = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(name_index)?);
                }
                let type_name = self.read_constant_string(type_name_index)?;
                let start = self.stack.len() - field_count;
                let mut fields = BTreeMap::new();
                for (i, name) in field_names.into_iter().enumerate() {
                    fields.insert(name, self.stack[start + i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Record(type_name, Arc::new(fields)));
            }
            Op::MakeVariant => {
                let name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                let start = self.stack.len() - field_count;
                let fields: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Variant(name, fields));
            }
            Op::RecordUpdate => {
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let ni = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(ni)?);
                }
                let start = self.stack.len() - field_count;
                let new_values: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let base = self.pop()?;
                if let Value::Record(type_name, existing) = base {
                    let mut fields = (*existing).clone();
                    for (name, val) in field_names.into_iter().zip(new_values) {
                        fields.insert(name, val);
                    }
                    self.push(Value::Record(type_name, Arc::new(fields)));
                } else {
                    return Err(VmError::new("record update on non-record".into()));
                }
            }
            Op::MakeRange => {
                let end = self.pop()?;
                let start = self.pop()?;
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    self.push(Value::Range(*a, *b));
                } else {
                    return Err(VmError::new("range requires two integers".into()));
                }
            }
            Op::ListConcat => {
                let b = self.pop()?;
                let a = self.pop()?;
                let mut result = match a {
                    Value::List(xs) => xs.as_ref().clone(),
                    Value::Range(lo, hi) => (lo..=hi).map(Value::Int).collect(),
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: left operand is not a list or range".into(),
                        ));
                    }
                };
                match b {
                    Value::List(xs) => result.extend(xs.iter().cloned()),
                    Value::Range(lo, hi) => result.extend((lo..=hi).map(Value::Int)),
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: right operand is not a list or range".into(),
                        ));
                    }
                }
                self.push(Value::List(Arc::new(result)));
            }
            Op::GetField => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let target = self.pop()?;
                match target {
                    Value::Record(_, ref fields) => {
                        let val = fields
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                        self.push(val);
                    }
                    Value::Map(ref map) => {
                        let val = map
                            .get(&Value::String(name.clone()))
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("map has no key '{name}'")))?;
                        self.push(val);
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "cannot access field '{}' on {}",
                            name,
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::GetIndex => {
                let index = self.read_u8()? as usize;
                let target = self.pop()?;
                if let Value::Tuple(ref elems) = target {
                    let val = elems
                        .get(index)
                        .cloned()
                        .ok_or_else(|| VmError::new("tuple index out of bounds".to_string()))?;
                    self.push(val);
                } else {
                    return Err(VmError::new(format!(
                        "cannot index into {}",
                        self.type_name(&target)
                    )));
                }
            }
            Op::Jump => {
                let offset = self.read_u16()? as usize;
                self.current_frame_mut()?.ip += offset;
            }
            Op::JumpBack => {
                let offset = self.read_u16()? as usize;
                self.current_frame_mut()?.ip -= offset;
            }
            Op::JumpIfFalse => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_falsy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_truthy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::PopN => {
                let count = self.read_u8()? as usize;
                let new_len = self.stack.len().saturating_sub(count);
                self.stack.truncate(new_len);
            }
            Op::Dup => {
                let val = self.peek()?.clone();
                self.push(val);
            }
            Op::TestTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Variant(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestEqual => {
                let ci = self.read_u16()? as usize;
                let constant = self.read_constant(ci)?;
                let val = self.peek()?;
                let result = *val == constant;
                self.push(Value::Bool(result));
            }
            Op::TestTupleLen => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                self.push(Value::Bool(result));
            }
            Op::TestListMin => {
                let min_len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len().map_or(false, |len| len >= min_len);
                self.push(Value::Bool(result));
            }
            Op::TestListExact => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len().map_or(false, |l| l == len);
                self.push(Value::Bool(result));
            }
            Op::TestIntRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestFloatRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Float(n), Value::Float(lo), Value::Float(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestBool => {
                let expected = self.read_u8()? != 0;
                let val = self.peek()?;
                let result = matches!(val, Value::Bool(b) if *b == expected);
                self.push(Value::Bool(result));
            }
            Op::DestructTuple => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Tuple(elems) = val {
                    self.push(elems[index].clone());
                } else {
                    return Err(VmError::new("DestructTuple on non-tuple".into()));
                }
            }
            Op::DestructVariant => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Variant(_, fields) = val {
                    self.push(fields[index].clone());
                } else {
                    return Err(VmError::new("DestructVariant on non-variant".into()));
                }
            }
            Op::DestructList => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => self.push(xs[index].clone()),
                    Value::Range(lo, hi) => {
                        let i = lo + index as i64;
                        if i > hi {
                            return Err(VmError::new("range index out of bounds".into()));
                        }
                        self.push(Value::Int(i));
                    }
                    _ => return Err(VmError::new("DestructList on non-list".into())),
                }
            }
            Op::DestructListRest => {
                let start = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => self.push(Value::List(Arc::new(xs[start..].to_vec()))),
                    Value::Range(lo, hi) => {
                        let new_lo = lo + start as i64;
                        if new_lo > hi + 1 {
                            self.push(Value::List(Arc::new(Vec::new())));
                        } else {
                            self.push(Value::Range(new_lo, hi));
                        }
                    }
                    _ => return Err(VmError::new("DestructListRest on non-list".into())),
                }
            }
            Op::DestructRecordField => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?.clone();
                if let Value::Record(_, fields) = val {
                    let field = fields
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                    self.push(field);
                } else {
                    return Err(VmError::new("DestructRecordField on non-record".into()));
                }
            }
            Op::TestRecordTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Record(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestMapHasKey => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?;
                let result = match val {
                    Value::Map(map) => map.contains_key(&Value::String(key_name)),
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::DestructMapValue => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?.clone();
                if let Value::Map(map) = val {
                    let value = map
                        .get(&Value::String(key_name.clone()))
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("map has no key '{key_name}'")))?;
                    self.push(value);
                } else {
                    return Err(VmError::new("DestructMapValue on non-map".into()));
                }
            }
            Op::LoopSetup => {
                let _ = self.read_u8()?;
            }
            Op::Recur => {
                let arg_count = self.read_u8()? as usize;
                let first_slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let start = self.stack.len() - arg_count;
                for i in 0..arg_count {
                    self.stack[base + first_slot + i] = self.stack[start + i].clone();
                }
                // Truncate all the way back to just after loop bindings.
                self.stack.truncate(base + first_slot + arg_count);
            }
            Op::QuestionMark => {
                let val = self.peek()?.clone();
                match val {
                    Value::Variant(ref tag, ref fields) => match tag.as_str() {
                        "Ok" | "Some" => {
                            self.pop()?;
                            self.push(if fields.len() == 1 {
                                fields[0].clone()
                            } else {
                                Value::Unit
                            });
                        }
                        "Err" | "None" => {
                            let result = self.pop()?;
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            if self.frames.is_empty() {
                                self.push(result);
                                return Ok(());
                            }
                            self.stack.truncate(finished_base);
                            self.push(result);
                        }
                        _ => return Err(VmError::new(format!("? on non-Result/Option: {tag}"))),
                    },
                    _ => {
                        return Err(VmError::new(format!(
                            "? on non-variant: {}",
                            self.type_name(&val)
                        )));
                    }
                }
            }
            Op::Panic => {
                let msg = self.pop()?;
                return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
            }
            Op::CallMethod => {
                let method_name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let method_name = self.read_constant_string(method_name_index)?;
                let receiver_slot = self.stack.len() - argc;
                let receiver = self.stack[receiver_slot].clone();
                let type_name = self.value_type_name_for_dispatch(&receiver);
                let qualified = format!("{type_name}.{method_name}");
                if let Some(func) = self.globals.get(&qualified).cloned() {
                    let args: Vec<Value> = self.stack[receiver_slot..].to_vec();
                    self.stack.truncate(receiver_slot);
                    let result = self.invoke_callable(&func, &args)?;
                    self.push(result);
                } else {
                    let extra_args: Vec<Value> = self.stack[receiver_slot + 1..].to_vec();
                    // Try built-in trait methods (display, equal, compare)
                    if let Some(result) =
                        self.dispatch_trait_method(&receiver, &method_name, &extra_args)
                    {
                        self.stack.truncate(receiver_slot);
                        self.push(result?);
                    } else if let Value::Record(_, ref fields) = receiver {
                        if let Some(field_val) = fields.get(&method_name) {
                            let callable = field_val.clone();
                            self.stack.truncate(receiver_slot);
                            let result = self.invoke_callable(&callable, &extra_args)?;
                            self.push(result);
                        } else {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{type_name}'"
                            )));
                        }
                    } else {
                        return Err(VmError::new(format!(
                            "no method '{method_name}' for type '{type_name}'"
                        )));
                    }
                }
            }
            Op::ChanNew
            | Op::ChanSend
            | Op::ChanRecv
            | Op::ChanClose
            | Op::ChanTrySend
            | Op::ChanTryRecv
            | Op::ChanSelect
            | Op::TaskSpawn
            | Op::TaskJoin
            | Op::TaskCancel
            | Op::Yield => {
                return Err(VmError::new(
                    "concurrency opcodes not yet implemented".into(),
                ));
            }
        }
        Ok(())
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
    fn enrich_error(&self, mut err: VmError) -> VmError {
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

    // ── Arithmetic helpers ────────────────────────────────────────

    fn binary_arithmetic(&mut self, op: Op) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let result = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => match op {
                Op::Add => Value::Int(a.wrapping_add(*b)),
                Op::Sub => Value::Int(a.wrapping_sub(*b)),
                Op::Mul => Value::Int(a.wrapping_mul(*b)),
                Op::Div => {
                    if *b == 0 {
                        return Err(VmError::new("division by zero".to_string()));
                    }
                    Value::Int(a / b)
                }
                Op::Mod => {
                    if *b == 0 {
                        return Err(VmError::new("modulo by zero".to_string()));
                    }
                    Value::Int(a % b)
                }
                _ => unreachable!(),
            },
            (Value::Float(a), Value::Float(b)) => match op {
                Op::Add => Value::Float(a + b),
                Op::Sub => Value::Float(a - b),
                Op::Mul => Value::Float(a * b),
                Op::Div => Value::Float(a / b),
                Op::Mod => Value::Float(a % b),
                _ => unreachable!(),
            },
            (Value::String(a), Value::String(b)) if op == Op::Add => {
                Value::String(format!("{a}{b}"))
            }
            _ => {
                let op_name = match op {
                    Op::Add => "+",
                    Op::Sub => "-",
                    Op::Mul => "*",
                    Op::Div => "/",
                    Op::Mod => "%",
                    _ => unreachable!(),
                };
                let a_type = self.type_name(&a);
                let b_type = self.type_name(&b);
                // Special error for Int/Float mixing
                if (a_type == "Int" && b_type == "Float") || (a_type == "Float" && b_type == "Int")
                {
                    return Err(VmError::new(
                        "cannot mix Int and Float — use int.to_float or float.to_int for explicit conversion".to_string()
                    ));
                }
                return Err(VmError::new(format!(
                    "cannot apply '{op_name}' to {a_type} and {b_type}",
                )));
            }
        };
        self.push(result);
        Ok(())
    }

    fn compare(&mut self, pred: fn(std::cmp::Ordering) -> bool) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let ordering = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::Record(na, _), Value::Record(nb, _)) if na == nb => a.cmp(&b),
            (Value::Variant(..), Value::Variant(..)) => a.cmp(&b),
            _ => {
                return Err(VmError::new(format!(
                    "unsupported operation: cannot compare {} and {}",
                    self.type_name(&a),
                    self.type_name(&b)
                )));
            }
        };
        self.push(Value::Bool(pred(ordering)));
        Ok(())
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

    /// Returns a discriminant for comparing value types.
    /// Two values are "same type" for equality/comparison if they share a discriminant.
    /// Variants are considered the same type (to allow Ok(1) == Err(2) => false,
    /// and pattern matching across tags). Records are compared by type name.
    fn value_disc(val: &Value) -> u8 {
        match val {
            Value::Int(_) => 0,
            Value::Float(_) => 1,
            Value::Bool(_) => 2,
            Value::String(_) => 3,
            Value::List(_) | Value::Range(..) => 4,
            Value::Map(_) => 5,
            Value::Set(_) => 6,
            Value::Tuple(_) => 7,
            Value::Record(..) => 8,
            Value::Variant(..) => 9,
            Value::Unit => 10,
            Value::Channel(_) => 11,
            Value::Handle(_) => 12,
            Value::VmClosure(_) => 13,
            Value::BuiltinFn(_) => 14,
            Value::VariantConstructor(..) => 15,
            Value::RecordDescriptor(_) => 16,
            Value::PrimitiveDescriptor(_) => 17,
        }
    }

    /// Check that two values have compatible types for equality/comparison.
    fn check_same_type(&self, a: &Value, b: &Value) -> Result<(), VmError> {
        if Self::value_disc(a) != Self::value_disc(b) {
            return Err(VmError::new(format!(
                "unsupported operation: cannot compare {} and {}",
                self.type_name(a),
                self.type_name(b)
            )));
        }
        Ok(())
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

    // ── Built-in trait methods on primitive types ──────────────────

    /// Handle built-in trait methods like .display(), .equal(), .compare()
    /// on primitive types. Returns Some(result) if handled, None otherwise.
    fn dispatch_trait_method(
        &self,
        receiver: &Value,
        method: &str,
        extra_args: &[Value],
    ) -> Option<Result<Value, VmError>> {
        match method {
            "display" => {
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("display() takes no arguments".into())));
                }
                Some(Ok(Value::String(self.display_value(receiver))))
            }
            "equal" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("equal() takes 1 argument".into())));
                }
                Some(Ok(Value::Bool(*receiver == extra_args[0])))
            }
            "compare" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("compare() takes 1 argument".into())));
                }
                let other = &extra_args[0];
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    (Value::Float(a), Value::Float(b)) => {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    _ => {
                        return Some(Err(VmError::new(format!(
                            "compare() not supported between {} and {}",
                            self.type_name(receiver),
                            self.type_name(other)
                        ))));
                    }
                };
                let result = match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Some(Ok(Value::Int(result)))
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    fn dispatch_builtin(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        // Foreign functions take priority -- lets embedders override builtins.
        if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
            return f(args);
        }
        if let Some((module, func)) = name.split_once('.') {
            match module {
                "list" => builtins::collections::call_list(self, func, args),
                "string" => builtins::string::call(self, func, args),
                "int" => builtins::numeric::call_int(func, args),
                "float" => builtins::numeric::call_float(func, args),
                "map" => builtins::collections::call_map(self, func, args),
                "set" => builtins::collections::call_set(self, func, args),
                "result" => builtins::core::call_result(self, func, args),
                "option" => builtins::core::call_option(self, func, args),
                "io" => builtins::io::call(self, func, args),
                "fs" => builtins::io::call_fs(self, func, args),
                "test" => builtins::core::call_test(self, func, args),
                "math" => builtins::numeric::call_math(func, args),
                "regex" => builtins::data::call_regex(self, func, args),
                "json" => builtins::data::call_json(self, func, args),
                "channel" => builtins::concurrency::call_channel(self, func, args),
                "task" => builtins::concurrency::call_task(self, func, args),
                "time" => builtins::data::call_time(self, func, args),
                "http" => builtins::data::call_http(self, func, args),
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        f(args)
                    } else {
                        Err(VmError::new(format!("unknown module: {module}")))
                    }
                }
            }
        } else {
            match name {
                "println" => {
                    match args.len() {
                        0 => println!(),
                        1 => println!("{}", self.display_value(&args[0])),
                        _ => {
                            let parts: Vec<String> =
                                args.iter().map(|v| self.display_value(v)).collect();
                            println!("{}", parts.join(" "));
                        }
                    }
                    Ok(Value::Unit)
                }
                "print" => {
                    match args.len() {
                        0 => {}
                        1 => print!("{}", self.display_value(&args[0])),
                        _ => {
                            let parts: Vec<String> =
                                args.iter().map(|v| self.display_value(v)).collect();
                            print!("{}", parts.join(" "));
                        }
                    }
                    Ok(Value::Unit)
                }
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: {msg}")))
                }
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        f(args)
                    } else {
                        Err(VmError::new(format!("unknown builtin: {name}")))
                    }
                }
            }
        }
    }

    /// Get current epoch milliseconds. Uses `__wasm_epoch_ms` foreign function
    /// if registered (WASM), otherwise falls back to `SystemTime`.
    pub(crate) fn epoch_ms(&self) -> Result<i64, VmError> {
        if let Some(f) = self.runtime.foreign_fns.get("__wasm_epoch_ms") {
            match f(&[])? {
                Value::Int(ms) => Ok(ms),
                _ => Err(VmError::new("__wasm_epoch_ms returned non-Int".into())),
            }
        } else {
            use std::time::{SystemTime, UNIX_EPOCH};
            let dur = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| VmError::new(format!("clock failed: {e}")))?;
            Ok(dur.as_millis() as i64)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Chunk, Function, Op};
    use crate::compiler::Compiler;
    use crate::lexer::{Lexer, Span};
    use crate::parser::Parser;

    /// Helper: build a Function from raw bytecode construction.
    fn make_function(build: impl FnOnce(&mut Chunk)) -> Arc<Function> {
        let mut func = Function::new("<test>".to_string(), 0);
        build(&mut func.chunk);
        Arc::new(func)
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    /// Helper: compile and run a silt program through the VM pipeline.
    fn run_vm(source: &str) -> Value {
        let tokens = Lexer::new(source).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).unwrap();
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        vm.run(script).unwrap()
    }

    // ── Phase 1 bytecode-level tests ──────────────────────────────

    #[test]
    fn test_constant_and_return() {
        let script = make_function(|chunk| {
            let idx = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(idx, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_arithmetic_add_int() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(2));
            let b = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_arithmetic_expression() {
        let script = make_function(|chunk| {
            let two = chunk.add_constant(Value::Int(2));
            let three = chunk.add_constant(Value::Int(3));
            let four = chunk.add_constant(Value::Int(4));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(two, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(three, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(four, span());
            chunk.emit_op(Op::Mul, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_float_arithmetic() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Float(1.5));
            let b = chunk.add_constant(Value::Float(2.5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Float(4.0));
    }

    #[test]
    fn test_negate() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Negate, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(-10));
    }

    #[test]
    fn test_comparison() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(3));
            let b = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Lt, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_boolean_not() {
        let script = make_function(|chunk| {
            chunk.emit_op(Op::True, span());
            chunk.emit_op(Op::Not, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn test_globals() {
        let script = make_function(|chunk| {
            let name = chunk.add_constant(Value::String("x".to_string()));
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::SetGlobal, span());
            chunk.emit_u16(name, span());
            chunk.emit_op(Op::GetGlobal, span());
            chunk.emit_u16(name, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_locals() {
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(10));
            chunk.emit_op(Op::Unit, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::SetLocal, span());
            chunk.emit_u16(0, span());
            chunk.emit_op(Op::Pop, span());
            chunk.emit_op(Op::GetLocal, span());
            chunk.emit_u16(0, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_string_concat() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::String("hello".to_string()));
            let b = chunk.add_constant(Value::String(" ".to_string()));
            let c = chunk.add_constant(Value::String("world".to_string()));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(c, span());
            chunk.emit_op(Op::StringConcat, span());
            chunk.emit_u8(3, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::String("hello world".to_string()));
    }

    #[test]
    fn test_display_value() {
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::DisplayValue, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::String("42".to_string()));
    }

    #[test]
    fn test_jump_if_false() {
        let script = make_function(|chunk| {
            let one = chunk.add_constant(Value::Int(1));
            let two = chunk.add_constant(Value::Int(2));
            chunk.emit_op(Op::False, span());
            let patch = chunk.emit_jump(Op::JumpIfFalse, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(one, span());
            let skip_else = chunk.emit_jump(Op::Jump, span());
            chunk.patch_jump(patch);
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(two, span());
            chunk.patch_jump(skip_else);
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(2));
    }

    #[test]
    fn test_builtin_println() {
        let script = make_function(|chunk| {
            let name = chunk.add_constant(Value::String("println".to_string()));
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::CallBuiltin, span());
            chunk.emit_u16(name, span());
            chunk.emit_u8(1, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Unit);
    }

    #[test]
    fn test_make_tuple() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(1));
            let b = chunk.add_constant(Value::Int(2));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::MakeTuple, span());
            chunk.emit_u8(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Tuple(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn test_make_list() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            let b = chunk.add_constant(Value::Int(20));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::MakeList, span());
            chunk.emit_u16(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(10), Value::Int(20)]))
        );
    }

    #[test]
    fn test_division_by_zero() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            let b = chunk.add_constant(Value::Int(0));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Div, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("division by zero"));
    }

    #[test]
    fn test_unit_and_pop() {
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(99));
            chunk.emit_op(Op::Unit, span());
            chunk.emit_op(Op::Pop, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_dup() {
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Dup, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_eq_neq() {
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Eq, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        assert_eq!(vm.run(script).unwrap(), Value::Bool(true));

        let script2 = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(5));
            let b = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Neq, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm2 = Vm::new();
        assert_eq!(vm2.run(script2).unwrap(), Value::Bool(true));
    }

    #[test]
    fn test_popn() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(1));
            let b = chunk.add_constant(Value::Int(2));
            let c = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(c, span());
            chunk.emit_op(Op::PopN, span());
            chunk.emit_u8(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(1));
    }

    // ── Phase 2 end-to-end tests ──────────────────────────────────

    #[test]
    fn test_e2e_hello_world() {
        run_vm(r#"fn main() { println("hello") }"#);
    }

    #[test]
    fn test_e2e_arithmetic() {
        let result = run_vm(r#"fn main() { 2 + 3 * 4 }"#);
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_e2e_function_call() {
        let result = run_vm(
            r#"
            fn add(a, b) { a + b }
            fn main() { add(10, 20) }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_e2e_let_binding() {
        let result = run_vm(
            r#"
            fn main() {
                let x = 42
                x
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_e2e_let_and_string_interp() {
        run_vm(
            r#"
            fn main() {
                let x = 42
                println("x = {x}")
            }
        "#,
        );
    }

    #[test]
    fn test_e2e_multiple_functions() {
        let result = run_vm(
            r#"
            fn double(n) { n * 2 }
            fn add_one(n) { n + 1 }
            fn main() { add_one(double(5)) }
        "#,
        );
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_e2e_recursion() {
        let result = run_vm(
            r#"
            fn factorial(n) {
                match n {
                    0 -> 1
                    _ -> n * factorial(n - 1)
                }
            }
            fn main() { factorial(5) }
        "#,
        );
        assert_eq!(result, Value::Int(120));
    }

    #[test]
    fn test_e2e_string_operations() {
        let result = run_vm(
            r#"
            import string

            fn main() {
                let s = "hello, world"
                string.length(s)
            }
        "#,
        );
        assert_eq!(result, Value::Int(12));
    }

    #[test]
    fn test_e2e_list_operations() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let xs = [1, 2, 3, 4, 5]
                list.length(xs)
            }
        "#,
        );
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_e2e_test_assert() {
        run_vm(
            r#"
            import test

            fn main() {
                test.assert_eq(2 + 2, 4)
            }
        "#,
        );
    }

    #[test]
    fn test_e2e_nested_calls() {
        let result = run_vm(
            r#"
            fn f(x) { x + 1 }
            fn g(x) { f(x) * 2 }
            fn main() { g(10) }
        "#,
        );
        assert_eq!(result, Value::Int(22));
    }

    #[test]
    fn test_e2e_match_int() {
        let result = run_vm(
            r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    _ -> "other"
                }
            }
            fn main() { classify(1) }
        "#,
        );
        assert_eq!(result, Value::String("one".into()));
    }

    #[test]
    fn test_e2e_boolean_logic() {
        let result = run_vm(
            r#"
            fn main() {
                let a = true
                let b = false
                a && !b
            }
        "#,
        );
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_e2e_builtin_println_call() {
        // Test that println works when called as a regular function via globals
        run_vm(
            r#"
            fn main() {
                println("testing 1 2 3")
            }
        "#,
        );
    }

    #[test]
    fn test_e2e_variant_constructor() {
        let result = run_vm(
            r#"
            fn main() {
                let x = Some(42)
                x
            }
        "#,
        );
        assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(42)]));
    }

    #[test]
    fn test_e2e_int_to_string() {
        let result = run_vm(
            r#"
            import int

            fn main() {
                int.to_string(42)
            }
        "#,
        );
        assert_eq!(result, Value::String("42".into()));
    }

    #[test]
    fn test_e2e_list_append() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let xs = [1, 2, 3]
                list.append(xs, 4)
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4)
            ]))
        );
    }

    // ── Phase 3: Closures and upvalue capture ────────────────────────

    #[test]
    fn test_closure_capture() {
        let result = run_vm(
            r#"
            fn make_adder(n) {
                fn(x) { x + n }
            }
            fn main() {
                let add5 = make_adder(5)
                add5(10)
            }
        "#,
        );
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_closure_in_map() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map(fn(x) { x * factor })
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![
                Value::Int(10),
                Value::Int(20),
                Value::Int(30)
            ]))
        );
    }

    #[test]
    fn test_higher_order() {
        let result = run_vm(
            r#"
            fn apply_twice(f, x) {
                f(f(x))
            }
            fn main() {
                let double = fn(x) { x * 2 }
                apply_twice(double, 3)
            }
        "#,
        );
        assert_eq!(result, Value::Int(12));
    }

    #[test]
    fn test_closure_counter() {
        // Tests that closures capture values, not references
        let result = run_vm(
            r#"
            import list

            fn main() {
                let fns = [1, 2, 3] |> list.map(fn(n) {
                    fn() { n * 10 }
                })
                fns |> list.map(fn(f) { f() })
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![
                Value::Int(10),
                Value::Int(20),
                Value::Int(30)
            ]))
        );
    }

    #[test]
    fn test_closure_multiple_captures() {
        let result = run_vm(
            r#"
            fn make_linear(a, b) {
                fn(x) { a * x + b }
            }
            fn main() {
                let f = make_linear(3, 7)
                f(10)
            }
        "#,
        );
        assert_eq!(result, Value::Int(37));
    }

    #[test]
    fn test_closure_transitive_capture() {
        // outer -> middle -> inner: transitive upvalue chaining
        let result = run_vm(
            r#"
            fn outer(x) {
                let make_inner = fn() {
                    fn() { x }
                }
                make_inner()
            }
            fn main() {
                let f = outer(42)
                f()
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_closure_no_capture() {
        // Lambda that doesn't capture anything (no upvalues needed)
        let result = run_vm(
            r#"
            fn main() {
                let f = fn(x) { x + 1 }
                f(10)
            }
        "#,
        );
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_closure_with_filter() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let threshold = 3
                [1, 2, 3, 4, 5] |> list.filter(fn(x) { x > threshold })
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(4), Value::Int(5)]))
        );
    }

    #[test]
    fn test_closure_with_fold() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let offset = 100
                [1, 2, 3] |> list.fold(offset, fn(acc, x) { acc + x })
            }
        "#,
        );
        assert_eq!(result, Value::Int(106));
    }

    #[test]
    fn test_let_tuple_destructure() {
        let result = run_vm(
            r#"
            fn main() {
                let (a, b) = (10, 20)
                a + b
            }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_let_tuple_destructure_three() {
        let result = run_vm(
            r#"
            fn main() {
                let (a, b, c) = (1, 2, 3)
                a * 100 + b * 10 + c
            }
        "#,
        );
        assert_eq!(result, Value::Int(123));
    }

    #[test]
    fn test_closure_returned_from_fn() {
        // A named function returns a closure that captures a parameter
        let result = run_vm(
            r#"
            fn multiplier(factor) {
                fn(x) { x * factor }
            }
            fn main() {
                let times3 = multiplier(3)
                let times7 = multiplier(7)
                times3(10) + times7(5)
            }
        "#,
        );
        assert_eq!(result, Value::Int(65));
    }

    #[test]
    fn test_closure_with_pipe_and_fn_syntax() {
        // Pipe with explicit fn(x) { ... } closure
        let result = run_vm(
            r#"
            import list

            fn main() {
                let base = 5
                [1, 2, 3] |> list.map(fn(x) { x + base })
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(6), Value::Int(7), Value::Int(8)]))
        );
    }

    #[test]
    fn test_trailing_closure_with_capture() {
        // Pipe with trailing closure syntax { x -> ... }
        let result = run_vm(
            r#"
            import list

            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map { x -> x * factor }
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![
                Value::Int(10),
                Value::Int(20),
                Value::Int(30)
            ]))
        );
    }

    #[test]
    fn test_trailing_closure_filter_with_capture() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let limit = 3
                [1, 2, 3, 4, 5] |> list.filter { x -> x > limit }
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(4), Value::Int(5)]))
        );
    }

    #[test]
    fn test_chained_pipes_with_closures() {
        let result = run_vm(
            r#"
            import list

            fn main() {
                let offset = 10
                let cutoff = 13
                [1, 2, 3, 4, 5]
                    |> list.map(fn(x) { x + offset })
                    |> list.filter(fn(x) { x > cutoff })
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(14), Value::Int(15)]))
        );
    }

    // ── Phase 4: Full pattern matching ──────────────────────────────

    #[test]
    fn test_match_int_literal() {
        let result = run_vm(
            r#"
            fn main() { match 42 { 42 -> "yes" _ -> "no" } }
        "#,
        );
        assert_eq!(result, Value::String("yes".into()));
    }

    #[test]
    fn test_match_int_fallthrough() {
        let result = run_vm(
            r#"
            fn main() { match 99 { 42 -> "yes" _ -> "no" } }
        "#,
        );
        assert_eq!(result, Value::String("no".into()));
    }

    #[test]
    fn test_match_string_literal() {
        let result = run_vm(
            r#"
            fn main() { match "hello" { "hello" -> 1 _ -> 0 } }
        "#,
        );
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_match_bool_literal() {
        let result = run_vm(
            r#"
            fn main() { match true { true -> "yes" false -> "no" } }
        "#,
        );
        assert_eq!(result, Value::String("yes".into()));
    }

    #[test]
    fn test_match_float_literal() {
        let result = run_vm(
            r#"
            fn main() { match 3.14 { 3.14 -> "pi" _ -> "other" } }
        "#,
        );
        assert_eq!(result, Value::String("pi".into()));
    }

    #[test]
    fn test_match_tuple() {
        let result = run_vm(
            r#"
            fn main() {
                match (1, 2) { (1, y) -> y * 10  _ -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(20));
    }

    #[test]
    fn test_match_tuple_wildcard() {
        let result = run_vm(
            r#"
            fn main() {
                match (1, 2) { (_, y) -> y + 100  _ -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(102));
    }

    #[test]
    fn test_match_tuple_len_mismatch() {
        let result = run_vm(
            r#"
            fn main() {
                match (1, 2, 3) { (a, b) -> a + b  _ -> 99 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_match_list_exact() {
        let result = run_vm(
            r#"
            fn main() {
                match [1, 2, 3] { [a, b, c] -> a + b + c  _ -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn test_match_list_exact_mismatch() {
        let result = run_vm(
            r#"
            fn main() {
                match [1, 2] { [a, b, c] -> a + b + c  _ -> 99 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_match_list_head_rest() {
        let result = run_vm(
            r#"
            fn main() {
                match [10, 20, 30] { [h, ..t] -> h  _ -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_list_rest_value() {
        let result = run_vm(
            r#"
            fn main() {
                match [10, 20, 30] { [_, ..t] -> t  _ -> [] }
            }
        "#,
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(20), Value::Int(30)]))
        );
    }

    #[test]
    fn test_match_list_empty_rest() {
        let result = run_vm(
            r#"
            fn main() {
                match [10] { [h, ..t] -> t  _ -> [99] }
            }
        "#,
        );
        assert_eq!(result, Value::List(Arc::new(vec![])));
    }

    #[test]
    fn test_match_constructor_simple() {
        let result = run_vm(
            r#"
            fn main() {
                match Some(42) { Some(n) -> n  None -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_constructor_none() {
        let result = run_vm(
            r#"
            fn main() {
                match None { Some(n) -> n  None -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(0));
    }

    #[test]
    fn test_match_constructor_ok_err() {
        let result = run_vm(
            r#"
            fn main() {
                let v = Ok(42)
                match v { Ok(n) -> n  Err(_) -> -1 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_nested_constructor_tuple() {
        let result = run_vm(
            r#"
            fn main() {
                match Some((1, 2)) { Some((a, b)) -> a + b  None -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_match_nested_constructor_list() {
        let result = run_vm(
            r#"
            fn main() {
                match Some([10, 20]) {
                    Some([h, ..t]) -> h
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_or_pattern() {
        let result = run_vm(
            r#"
            fn main() {
                match 2 { 1 | 2 | 3 -> "small" _ -> "big" }
            }
        "#,
        );
        assert_eq!(result, Value::String("small".into()));
    }

    #[test]
    fn test_match_or_pattern_no_match() {
        let result = run_vm(
            r#"
            fn main() {
                match 5 { 1 | 2 | 3 -> "small" _ -> "big" }
            }
        "#,
        );
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_guard() {
        let result = run_vm(
            r#"
            fn main() {
                match 42 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("positive".into()));
    }

    #[test]
    fn test_match_guard_all_fail() {
        let result = run_vm(
            r#"
            fn main() {
                match -5 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("other".into()));
    }

    #[test]
    fn test_match_range() {
        let result = run_vm(
            r#"
            fn main() {
                match 5 { 1..10 -> "in range" _ -> "out" }
            }
        "#,
        );
        assert_eq!(result, Value::String("in range".into()));
    }

    #[test]
    fn test_match_range_boundary() {
        let result = run_vm(
            r#"
            fn main() {
                match 10 { 1..10 -> "in range" _ -> "out" }
            }
        "#,
        );
        assert_eq!(result, Value::String("in range".into()));
    }

    #[test]
    fn test_match_range_out() {
        let result = run_vm(
            r#"
            fn main() {
                match 11 { 1..10 -> "in range" _ -> "out" }
            }
        "#,
        );
        assert_eq!(result, Value::String("out".into()));
    }

    #[test]
    fn test_guardless_match() {
        let result = run_vm(
            r#"
            fn main() {
                let x = 5
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#,
        );
        assert_eq!(result, Value::String("positive".into()));
    }

    #[test]
    fn test_guardless_match_default() {
        let result = run_vm(
            r#"
            fn main() {
                let x = -5
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#,
        );
        assert_eq!(result, Value::String("other".into()));
    }

    #[test]
    fn test_let_tuple_destructure_nested() {
        let result = run_vm(
            r#"
            fn main() {
                let (a, (b, c)) = (1, (2, 3))
                a + b + c
            }
        "#,
        );
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn test_let_list_destructure() {
        let result = run_vm(
            r#"
            fn main() {
                let [a, b, c] = [10, 20, 30]
                a + b + c
            }
        "#,
        );
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_let_list_head_rest() {
        let result = run_vm(
            r#"
            fn main() {
                let [h, ..t] = [1, 2, 3, 4]
                h
            }
        "#,
        );
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_match_multiple_arms() {
        let result = run_vm(
            r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    2 -> "two"
                    _ -> "many"
                }
            }
            fn main() {
                classify(2)
            }
        "#,
        );
        assert_eq!(result, Value::String("two".into()));
    }

    #[test]
    fn test_match_ident_binding() {
        let result = run_vm(
            r#"
            fn main() {
                match 42 { x -> x + 1 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(43));
    }

    #[test]
    fn test_match_wildcard() {
        let result = run_vm(
            r#"
            fn main() {
                match 42 { _ -> "matched" }
            }
        "#,
        );
        assert_eq!(result, Value::String("matched".into()));
    }

    #[test]
    fn test_match_constructor_with_guard() {
        let result = run_vm(
            r#"
            fn main() {
                match Some(5) {
                    Some(n) when n > 10 -> "big"
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_when_bool_guard() {
        let result = run_vm(
            r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 2) { Ok(n) -> n  Err(_) -> -1 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_when_bool_guard_fails() {
        let result = run_vm(
            r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 0) { Ok(n) -> n  Err(_) -> -1 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_match_list_two_elems_with_rest() {
        let result = run_vm(
            r#"
            fn main() {
                match [1, 2, 3, 4, 5] {
                    [a, b, ..rest] -> a + b
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_match_tuple_three() {
        let result = run_vm(
            r#"
            fn main() {
                match (10, 20, 30) {
                    (a, b, c) -> a + b + c
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_match_nested_tuple_in_list() {
        // Match a list where elements are extracted as simple ints
        let result = run_vm(
            r#"
            fn main() {
                match [1, 2] {
                    [a, b] -> a * 100 + b
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(102));
    }

    #[test]
    fn test_match_constructor_wildcard_field() {
        let result = run_vm(
            r#"
            fn main() {
                match Ok(42) { Ok(_) -> "is ok" Err(_) -> "is err" }
            }
        "#,
        );
        assert_eq!(result, Value::String("is ok".into()));
    }

    #[test]
    fn test_match_or_pattern_constructor() {
        let result = run_vm(
            r#"
            fn main() {
                match None { Some(_) -> "has value"  None -> "empty" }
            }
        "#,
        );
        assert_eq!(result, Value::String("empty".into()));
    }

    #[test]
    fn test_match_deeply_nested() {
        // Some((a, [h, ..t]))
        let result = run_vm(
            r#"
            fn main() {
                match Some((1, [10, 20, 30])) {
                    Some((a, [h, ..t])) -> a + h
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_guardless_match_first_branch() {
        let result = run_vm(
            r#"
            fn main() {
                let x = 50
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#,
        );
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_in_function() {
        let result = run_vm(
            r#"
            fn describe(opt) {
                match opt {
                    Some(n) when n > 0 -> "positive"
                    Some(0) -> "zero"
                    Some(_) -> "negative"
                    None -> "nothing"
                }
            }
            fn main() {
                describe(Some(0))
            }
        "#,
        );
        assert_eq!(result, Value::String("zero".into()));
    }

    #[test]
    fn test_match_float_range() {
        let result = run_vm(
            r#"
            fn main() {
                match 3.14 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("medium".into()));
    }

    #[test]
    fn test_match_float_range_out() {
        let result = run_vm(
            r#"
            fn main() {
                match 10.0 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("large".into()));
    }

    #[test]
    fn test_match_recursive_list_sum() {
        // Use match to destructure a list recursively
        let result = run_vm(
            r#"
            fn sum(xs) {
                match xs {
                    [] -> 0
                    [h, ..t] -> h + sum(t)
                }
            }
            fn main() {
                sum([1, 2, 3, 4, 5])
            }
        "#,
        );
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_match_map_pattern() {
        let result = run_vm(
            r#"
            fn main() {
                let m = #{"name": "Alice", "age": "30"}
                match m {
                    #{"name": n} -> n
                    _ -> "unknown"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("Alice".into()));
    }

    #[test]
    fn test_match_constructor_nested_or() {
        let result = run_vm(
            r#"
            fn main() {
                match 42 {
                    1 | 2 | 3 -> "tiny"
                    n when n > 40 -> "big"
                    _ -> "other"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_tuple_nested_wildcard() {
        let result = run_vm(
            r#"
            fn main() {
                match (1, (2, 3)) {
                    (1, (_, c)) -> c * 10
                    _ -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_match_list_empty() {
        let result = run_vm(
            r#"
            fn main() {
                match [] {
                    [] -> "empty"
                    _ -> "not empty"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("empty".into()));
    }

    #[test]
    fn test_let_constructor_destructure() {
        let result = run_vm(
            r#"
            fn main() {
                let x = Ok(42)
                match x { Ok(n) -> n  Err(_) -> 0 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_multiple_constructors_sequence() {
        let result = run_vm(
            r#"
            fn process(items) {
                match items {
                    [] -> 0
                    [h, ..t] -> h + process(t)
                }
            }
            fn main() {
                process([10, 20, 30])
            }
        "#,
        );
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_match_pin_pattern() {
        let result = run_vm(
            r#"
            fn main() {
                let expected = 42
                match 42 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("matched".into()));
    }

    #[test]
    fn test_match_pin_pattern_no_match() {
        let result = run_vm(
            r#"
            fn main() {
                let expected = 42
                match 99 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#,
        );
        assert_eq!(result, Value::String("nope".into()));
    }

    #[test]
    fn test_when_pattern_match() {
        let result = run_vm(
            r#"
            fn extract(val) {
                when Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(Some(42))
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_when_pattern_match_fails() {
        let result = run_vm(
            r#"
            fn extract(val) {
                when Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(None)
            }
        "#,
        );
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_match_or_pattern_with_binding() {
        // Or-patterns where each alt binds the same variable
        let result = run_vm(
            r#"
            fn main() {
                match Some(5) {
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_guard_with_tuple() {
        let result = run_vm(
            r#"
            fn main() {
                match (3, 4) {
                    (a, b) when a + b > 10 -> "big"
                    (a, b) -> a + b
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(7));
    }

    // ── Phase 5 tests ──────────────────────────────────────────

    #[test]
    fn test_loop_sum() {
        let result = run_vm(
            r#"
            fn main() {
                loop x = 0, sum = 0 {
                    match x >= 10 {
                        true -> sum
                        _ -> loop(x + 1, sum + x)
                    }
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(45));
    }

    #[test]
    fn test_loop_factorial() {
        let result = run_vm(
            r#"
            fn main() {
                loop n = 10, acc = 1 {
                    match n <= 1 {
                        true -> acc
                        _ -> loop(n - 1, acc * n)
                    }
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(3628800));
    }

    #[test]
    fn test_record_create_and_access() {
        let result = run_vm(
            r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                u.age
            }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_record_update() {
        let result = run_vm(
            r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2.age
            }
        "#,
        );
        assert_eq!(result, Value::Int(31));
    }

    #[test]
    fn test_range_expression() {
        // 1..5 inclusive = [1, 2, 3, 4, 5], sum = 15
        let result = run_vm(
            r#"
            import list

            fn main() {
                let nums = 1..5
                nums |> list.fold(0) { acc, n -> acc + n }
            }
        "#,
        );
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_set_literal() {
        let result = run_vm(
            r#"
            import set

            fn main() {
                let s = #[1, 2, 3, 2, 1]
                set.length(s)
            }
        "#,
        );
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_question_mark_ok() {
        let result = run_vm(
            r#"
            import int

            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "20") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_question_mark_err() {
        let result = run_vm(
            r#"
            import int

            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "abc") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#,
        );
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_type_decl_variant_constructors() {
        let result = run_vm(
            r#"
            type Color { Red, Green, Blue }
            fn main() {
                let c = Red
                match c { Red -> 1  Green -> 2  Blue -> 3 }
            }
        "#,
        );
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_type_decl_variant_with_fields() {
        let result = run_vm(
            r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            fn main() {
                let s = Circle(5.0)
                match s {
                    Circle(r) -> r
                    Rect(w, h) -> w + h
                }
            }
        "#,
        );
        assert_eq!(result, Value::Float(5.0));
    }

    #[test]
    fn test_custom_display_trait() {
        let result = run_vm(
            r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            trait Display for Shape {
                fn display(self) -> String {
                    match self {
                        Circle(r) -> "Circle"
                        Rect(w, h) -> "Rect"
                    }
                }
            }
            fn main() {
                let s = Circle(5.0)
                s.display()
            }
        "#,
        );
        assert_eq!(result, Value::String("Circle".to_string()));
    }

    #[test]
    fn test_tuple_index_access() {
        let result = run_vm(
            r#"
            fn main() {
                let pair = (10, 20)
                pair.0 + pair.1
            }
        "#,
        );
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_recursive_variant_eval() {
        let result = run_vm(
            r#"
            type Expr { Num(Int), Add(Expr, Expr) }
            fn eval(expr) {
                match expr {
                    Num(n) -> n
                    Add(l, r) -> eval(l) + eval(r)
                }
            }
            fn main() {
                eval(Add(Num(3), Num(5)))
            }
        "#,
        );
        assert_eq!(result, Value::Int(8));
    }

    #[test]
    fn test_loop_in_function() {
        let result = run_vm(
            r#"
            fn sum_to(n) {
                loop i = 0, acc = 0 {
                    match i > n {
                        true -> acc
                        _ -> loop(i + 1, acc + i)
                    }
                }
            }
            fn main() {
                sum_to(100)
            }
        "#,
        );
        assert_eq!(result, Value::Int(5050));
    }

    // ── Concurrency tests ────────────────────────────────────────────

    #[test]
    fn test_spawn_join() {
        let result = run_vm(
            r#"
            import task

            fn main() {
                let t = task.spawn(fn() { 42 })
                task.join(t)
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_spawn_join_already_completed() {
        // Ensure task.join works when the fiber has already completed
        // before join is called (the original deadlock scenario).
        let result = run_vm(
            r#"
            import channel
            import task

            fn main() {
                let ch = channel.new(1)
                let t = task.spawn(fn() {
                    channel.send(ch, "done")
                    99
                })
                -- Wait for the message, ensuring the fiber runs to completion
                let Message(msg) = channel.receive(ch)
                -- Now the fiber should already be completed
                task.join(t)
            }
        "#,
        );
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_spawn_join_multiple_completed() {
        // Multiple fibers that complete before join is called
        let result = run_vm(
            r#"
            import channel
            import task

            fn main() {
                let ch = channel.new(10)
                let t1 = task.spawn(fn() {
                    channel.send(ch, 1)
                    10
                })
                let t2 = task.spawn(fn() {
                    channel.send(ch, 2)
                    20
                })
                let t3 = task.spawn(fn() {
                    channel.send(ch, 3)
                    30
                })
                -- Drain all messages so fibers complete
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
                -- All fibers should be done; join should not deadlock
                let a = task.join(t1)
                let b = task.join(t2)
                let c = task.join(t3)
                a + b + c
            }
        "#,
        );
        assert_eq!(result, Value::Int(60));
    }

    // ── FFI tests ──────────────────────────────────────────────────

    /// Helper: compile and run silt code on a pre-configured VM (for FFI tests).
    fn run_vm_with(vm: &mut Vm, source: &str) -> Value {
        let tokens = Lexer::new(source).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).unwrap();
        let script = Arc::new(functions.into_iter().next().unwrap());
        vm.run(script).unwrap()
    }

    #[test]
    fn test_foreign_fn_raw() {
        let mut vm = Vm::new();
        vm.register_fn("double", |args: &[Value]| {
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("expected Int".into()));
            };
            Ok(Value::Int(n * 2))
        });
        let result = run_vm_with(&mut vm, "fn main() { double(21) }");
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_foreign_fn1_typed() {
        let mut vm = Vm::new();
        vm.register_fn1("double", |x: i64| -> i64 { x * 2 });
        let result = run_vm_with(&mut vm, "fn main() { double(21) }");
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_foreign_fn2_typed() {
        let mut vm = Vm::new();
        vm.register_fn2("add", |a: i64, b: i64| -> i64 { a + b });
        let result = run_vm_with(&mut vm, "fn main() { add(10, 32) }");
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_foreign_fn0_typed() {
        let mut vm = Vm::new();
        vm.register_fn0("answer", || -> i64 { 42 });
        let result = run_vm_with(&mut vm, "fn main() { answer() }");
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_foreign_fn_string() {
        let mut vm = Vm::new();
        vm.register_fn1("shout", |s: String| -> String { s.to_uppercase() });
        let result = run_vm_with(&mut vm, r#"fn main() { shout("hello") }"#);
        assert_eq!(result, Value::String("HELLO".into()));
    }

    #[test]
    fn test_foreign_fn_returns_option() {
        let mut vm = Vm::new();
        vm.register_fn1("maybe", |x: i64| -> Option<i64> {
            if x > 0 { Some(x) } else { None }
        });
        let result = run_vm_with(&mut vm, "fn main() { maybe(5) }");
        assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(5)]));
        let result = run_vm_with(&mut vm, "fn main() { maybe(-1) }");
        assert_eq!(result, Value::Variant("None".into(), vec![]));
    }

    #[test]
    fn test_foreign_fn_returns_result() {
        let mut vm = Vm::new();
        vm.register_fn1("safe_div", |x: i64| -> Result<i64, String> {
            if x != 0 {
                Ok(100 / x)
            } else {
                Err("division by zero".into())
            }
        });
        let result = run_vm_with(&mut vm, "fn main() { safe_div(5) }");
        assert_eq!(result, Value::Variant("Ok".into(), vec![Value::Int(20)]));
        let result = run_vm_with(&mut vm, "fn main() { safe_div(0) }");
        assert_eq!(
            result,
            Value::Variant("Err".into(), vec![Value::String("division by zero".into())])
        );
    }

    #[test]
    fn test_foreign_fn_higher_order() {
        let mut vm = Vm::new();
        vm.register_fn1("square", |x: i64| -> i64 { x * x });
        let result = run_vm_with(
            &mut vm,
            "import list\nfn main() { [1, 2, 3] |> list.map(square) }",
        );
        assert_eq!(
            result,
            Value::List(Arc::new(vec![Value::Int(1), Value::Int(4), Value::Int(9),]))
        );
    }

    #[test]
    fn test_foreign_fn_module_qualified() {
        let mut vm = Vm::new();
        vm.register_fn1("mylib.double", |x: i64| -> i64 { x * 2 });
        // Module-qualified names go through GetGlobal + Call, not CallBuiltin
        let result = run_vm_with(
            &mut vm,
            r#"
            fn main() {
                let f = mylib.double
                f(21)
            }
        "#,
        );
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_foreign_fn_type_error() {
        let mut vm = Vm::new();
        vm.register_fn1("double", |x: i64| -> i64 { x * 2 });
        let tokens = Lexer::new(r#"fn main() { double("hello") }"#)
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).unwrap();
        let script = Arc::new(functions.into_iter().next().unwrap());
        let err = vm.run(script).unwrap_err();
        assert!(err.message.contains("expected Int"), "got: {}", err.message);
    }
}
