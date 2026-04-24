//! Builtin registration and dispatch.

use std::panic::AssertUnwindSafe;

use super::{Vm, VmError};
use crate::builtins;
use crate::module;
use crate::value::Value;

/// Invoke a registered foreign function while catching panics that escape it.
///
/// A panicking foreign function would otherwise tear down the scheduler worker
/// thread (or the main thread), leaving other tasks unable to progress. We
/// instead convert a caught panic into a [`VmError`] whose message preserves
/// the panic payload when it is a `&str` or `String`.
fn invoke_foreign_fn(
    name: &str,
    f: &super::runtime::ForeignFn,
    args: &[Value],
) -> Result<Value, VmError> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| f(args))) {
        Ok(result) => result,
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            Err(VmError::new(format!(
                "foreign function '{name}' panicked: {msg}"
            )))
        }
    }
}

/// Decode a panic payload into a human-readable string, preserving the
/// common `&'static str` and `String` cases and falling back to a
/// placeholder for other payload types.
fn decode_panic_payload(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Run a builtin module dispatch arm under `catch_unwind`, converting any
/// panic that escapes the builtin into a clean `VmError`. This mirrors
/// [`invoke_foreign_fn`] for user-registered FFI — a panic in a builtin
/// would otherwise tear down the current scheduler worker thread.
///
/// Intended to wrap each arm of the module-name match in `dispatch_builtin`.
/// Callers that capture `&mut Vm` (or other non-`UnwindSafe` state) should
/// wrap the closure in [`AssertUnwindSafe`] before passing it here.
fn catch_builtin_panic<F>(module: &str, f: F) -> Result<Value, VmError>
where
    F: FnOnce() -> Result<Value, VmError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(result) => result,
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            Err(VmError::new(format!(
                "builtin module '{module}' panicked: {msg}"
            )))
        }
    }
}

impl Vm {
    /// Register all builtin functions and variant constructors in globals.
    pub(super) fn register_builtins(&mut self) {
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
        self.globals
            .insert("Sent".into(), Value::Variant("Sent".into(), Vec::new()));
        // ChannelOp constructors for `channel.select`. `Recv(ch)` and
        // `Send(ch, value)` are the one-and-only shapes accepted by the
        // select op list.
        self.globals
            .insert("Recv".into(), Value::VariantConstructor("Recv".into(), 1));
        self.globals
            .insert("Send".into(), Value::VariantConstructor("Send".into(), 2));
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

        // ── Stdlib error variants ──
        // Phase 0 of the stdlib error redesign (see
        // `docs/proposals/stdlib-errors.md`). Each variant is globally
        // unique (module-prefixed) so we can register it as a bare
        // global the same way other builtin variants are. N-ary variants
        // land as `VariantConstructor`; nullary variants land as
        // `Variant` values.

        // IoError
        for (name, arity) in [
            ("IoNotFound", 1usize),
            ("IoPermissionDenied", 1),
            ("IoAlreadyExists", 1),
            ("IoInvalidInput", 1),
            ("IoUnknown", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }
        for name in ["IoInterrupted", "IoUnexpectedEof", "IoWriteZero"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // JsonError
        for (name, arity) in [
            ("JsonSyntax", 2usize),
            ("JsonTypeMismatch", 2),
            ("JsonMissingField", 1),
            ("JsonUnknown", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }

        // TomlError
        for (name, arity) in [
            ("TomlSyntax", 2usize),
            ("TomlTypeMismatch", 2),
            ("TomlMissingField", 1),
            ("TomlUnknown", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }

        // ParseError
        self.globals.insert(
            "ParseInvalidDigit".into(),
            Value::VariantConstructor("ParseInvalidDigit".into(), 1),
        );
        for name in ["ParseEmpty", "ParseOverflow", "ParseUnderflow"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // HttpError
        for (name, arity) in [
            ("HttpConnect", 1usize),
            ("HttpTls", 1),
            ("HttpInvalidUrl", 1),
            ("HttpInvalidResponse", 1),
            ("HttpStatusCode", 2),
            ("HttpUnknown", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }
        for name in ["HttpTimeout", "HttpClosedEarly"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // RegexError
        self.globals.insert(
            "RegexInvalidPattern".into(),
            Value::VariantConstructor("RegexInvalidPattern".into(), 2),
        );
        self.globals.insert(
            "RegexTooBig".into(),
            Value::Variant("RegexTooBig".into(), Vec::new()),
        );

        // PgError
        for (name, arity) in [
            ("PgConnect", 1usize),
            ("PgTls", 1),
            ("PgAuthFailed", 1),
            ("PgQuery", 2),
            ("PgTypeMismatch", 3),
            ("PgNoSuchColumn", 1),
            ("PgUnknown", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }
        for name in ["PgClosed", "PgTimeout", "PgTxnAborted"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // TcpError
        for (name, arity) in [("TcpConnect", 1usize), ("TcpTls", 1), ("TcpUnknown", 1)] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }
        for name in ["TcpClosed", "TcpTimeout"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // TimeError
        for (name, arity) in [("TimeParseFormat", 1usize), ("TimeOutOfRange", 1)] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }

        // BytesError
        for (name, arity) in [
            ("BytesInvalidUtf8", 1usize),
            ("BytesInvalidHex", 1),
            ("BytesInvalidBase64", 1),
            ("BytesByteOutOfRange", 1),
            ("BytesOutOfBounds", 1),
        ] {
            self.globals
                .insert(name.into(), Value::VariantConstructor(name.into(), arity));
        }

        // ChannelError
        for name in ["ChannelTimeout", "ChannelClosed"] {
            self.globals
                .insert(name.into(), Value::Variant(name.into(), Vec::new()));
        }

        // ── __type_of__<variant> mappings for builtin error enums ──
        // Phase 1 of the stdlib error redesign: `CallMethod` dispatch
        // looks up the parent type name via `__type_of__<tag>` to route
        // `err.message()` to `IoError.message`, etc. User-declared enums
        // register these globals at codegen time; builtin enums have to
        // be seeded here so the same dispatch works.
        for (enum_name, variants) in module::builtin_enum_variants() {
            for variant in *variants {
                let key = format!("__type_of__{variant}");
                self.globals.insert(key, Value::String((*enum_name).into()));
            }
        }

        // ── trait Error for builtin error enums — compiled message() ──
        // Registered as built-in functions so `err.message()` dispatches
        // via `CallMethod` → `<EnumName>.message` global. The function
        // body lives in the corresponding `call_*_error_trait` helper
        // and is routed through the matching `dispatch_builtin` arm.
        // `.display()` is handled generically by `dispatch_trait_method`
        // via `display_value`, so it does not need a per-enum BuiltinFn.
        for enum_name in &[
            "IoError",
            "JsonError",
            "TomlError",
            "ParseError",
            "HttpError",
            "RegexError",
            "PgError",
            "TcpError",
            "TimeError",
            "BytesError",
            "ChannelError",
        ] {
            let key = format!("{enum_name}.message");
            self.globals
                .insert(key.clone(), Value::BuiltinFn(key.clone()));
        }

        // Primitive type descriptors
        self.globals
            .insert("Int".into(), Value::PrimitiveDescriptor("Int".into()));
        self.globals
            .insert("Float".into(), Value::PrimitiveDescriptor("Float".into()));
        self.globals.insert(
            "ExtFloat".into(),
            Value::PrimitiveDescriptor("ExtFloat".into()),
        );
        self.globals
            .insert("String".into(), Value::PrimitiveDescriptor("String".into()));
        self.globals
            .insert("Bool".into(), Value::PrimitiveDescriptor("Bool".into()));

        // Builtin container type descriptors — uppercase names so users
        // can pass `List`, `Map`, etc. as `type a` arguments or invoke
        // static-style trait methods (`List.empty()`). These don't
        // collide with the lowercase module names (`list`, `map`) used
        // for module calls.
        for name in &["List", "Map", "Set", "Channel", "Tuple"] {
            self.globals
                .insert((*name).into(), Value::TypeDescriptor((*name).into()));
        }

        // Math constants
        self.globals
            .insert("math.pi".into(), Value::Float(std::f64::consts::PI));
        self.globals
            .insert("math.e".into(), Value::Float(std::f64::consts::E));

        // Non-module builtin functions (not scoped to a module)
        for name in ["print", "println", "panic"] {
            self.globals
                .insert(name.into(), Value::BuiltinFn(name.into()));
        }

        // Module-scoped builtin functions — derived from the module registry
        // so that module.rs is the single source of truth.
        for &module_name in module::BUILTIN_MODULES {
            for func in module::builtin_module_functions(module_name) {
                let qualified = format!("{module_name}.{func}");
                self.globals
                    .insert(qualified.clone(), Value::BuiltinFn(qualified));
            }
        }

        // Float constants — registered after builtin function names so that
        // `float.max` and `float.min` resolve to the constant values (f64::MAX,
        // f64::MIN) when used as bare expressions, overriding the BuiltinFn
        // entries.  Function calls like `float.max(a, b)` still work because
        // they compile to Op::CallBuiltin, which bypasses the global lookup.
        self.globals
            .insert("float.max_value".into(), Value::Float(f64::MAX));
        self.globals
            .insert("float.min_value".into(), Value::Float(f64::MIN));
        self.globals
            .insert("float.epsilon".into(), Value::Float(f64::EPSILON));
        self.globals
            .insert("float.min_positive".into(), Value::Float(f64::MIN_POSITIVE));
        self.globals
            .insert("float.infinity".into(), Value::ExtFloat(f64::INFINITY));
        self.globals.insert(
            "float.neg_infinity".into(),
            Value::ExtFloat(f64::NEG_INFINITY),
        );
        self.globals
            .insert("float.nan".into(), Value::ExtFloat(f64::NAN));
    }

    // ── Built-in trait methods on primitive types ──────────────────

    /// Handle built-in trait methods like .display(), .equal(), .compare()
    /// on primitive types. Returns Some(result) if handled, None otherwise.
    pub(super) fn dispatch_trait_method(
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
                    (Value::Float(a), Value::Float(b)) => match a.partial_cmp(b) {
                        Some(ord) => ord,
                        None => {
                            return Some(Err(VmError::new(
                                "compare() cannot compare non-finite float values".into(),
                            )));
                        }
                    },
                    // ExtFloat (produced by `Float / Float`) and mixed
                    // Float/ExtFloat: mirror the arithmetic.rs compare
                    // path — widen to f64 and error on NaN.
                    (Value::ExtFloat(a), Value::ExtFloat(b)) => match a.partial_cmp(b) {
                        Some(ord) => ord,
                        None => {
                            return Some(Err(VmError::new(
                                "compare() cannot compare NaN values".into(),
                            )));
                        }
                    },
                    (Value::Float(a), Value::ExtFloat(b)) => match a.partial_cmp(b) {
                        Some(ord) => ord,
                        None => {
                            return Some(Err(VmError::new(
                                "compare() cannot compare NaN values".into(),
                            )));
                        }
                    },
                    (Value::ExtFloat(a), Value::Float(b)) => match a.partial_cmp(b) {
                        Some(ord) => ord,
                        None => {
                            return Some(Err(VmError::new(
                                "compare() cannot compare NaN values".into(),
                            )));
                        }
                    },
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    // List vs List: the typechecker auto-derives Compare for
                    // List (see src/typechecker/mod.rs:3386), so a value of
                    // `List(T)` flowing through a `Compare` bound must
                    // resolve here. Defer to the existing element-wise
                    // ordering on `Value::cmp`, which already handles
                    // List/Range pairings (see src/vm/arithmetic.rs:138).
                    (Value::List(_), Value::List(_))
                    | (Value::List(_), Value::Range(..))
                    | (Value::Range(..), Value::List(_))
                    | (Value::Range(..), Value::Range(..)) => receiver.cmp(other),
                    // Variant vs Variant: typechecker auto-derives Compare
                    // for enum variants (e.g. user `type Color { Red, Green }`
                    // and built-in Weekday from the time module).
                    // `Value::cmp` handles Variant content-wise (see
                    // src/value.rs:1551) and uses weekday ordinals when
                    // applicable, so defer to it.
                    (Value::Variant(..), Value::Variant(..)) => receiver.cmp(other),
                    // Record vs Record: typechecker auto-derives Compare for
                    // user-declared records. `Value::cmp` orders records by
                    // name then lexicographically by fields (src/value.rs:1537)
                    // with canonical Date/Time/DateTime field ordering.
                    (Value::Record(..), Value::Record(..)) => receiver.cmp(other),
                    // Unit vs Unit: typechecker auto-derives Compare for `()`
                    // (src/typechecker/mod.rs:3383). All units are equal.
                    (Value::Unit, Value::Unit) => std::cmp::Ordering::Equal,
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
            "hash" => {
                // The typechecker auto-derives `Hash` for Int / Float /
                // ExtFloat / Bool / String / List (and more). At runtime,
                // user-defined `trait Hash for T` impls are resolved via
                // the qualified-global path in `Op::CallMethod`; only
                // auto-derived primitives fall through to here.
                //
                // `Value` already implements `std::hash::Hash` with a
                // canonical bit-hash for floats (see src/value.rs:1759).
                // We reuse that impl via `DefaultHasher` so the result
                // matches `HashMap<Value, Value>` keying.
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("hash() takes no arguments".into())));
                }
                // Only honour hash() for types the typechecker actually
                // auto-derives Hash for — emitting a dispatch error for
                // anything else keeps the user-impl path authoritative.
                match receiver {
                    Value::Int(_)
                    | Value::Float(_)
                    | Value::ExtFloat(_)
                    | Value::Bool(_)
                    | Value::String(_)
                    | Value::List(_)
                    // Range hashes via the same `impl Hash for Value`
                    // (src/value.rs:1791); typechecker registers Hash for
                    // every `List(T)` that flows through a `Hash` bound,
                    // and `1..5` reaches dispatch as `Value::Range`.
                    | Value::Range(..)
                    | Value::Tuple(_)
                    | Value::Map(_)
                    | Value::Set(_)
                    // Variant covers the auto-derived Hash for Option/Result
                    // (src/typechecker/mod.rs:3391). The existing
                    // `impl Hash for Value` (src/value.rs:1821) already
                    // hashes Variant by name + payload.
                    | Value::Variant(..)
                    // Record covers user-declared records the typechecker
                    // auto-derives Hash for. `impl Hash for Value`
                    // (src/value.rs:1814) hashes records structurally by
                    // name + fields, matching the HashMap<Value, Value>
                    // keying contract.
                    | Value::Record(..)
                    | Value::Unit => {
                        use std::collections::hash_map::DefaultHasher;
                        use std::hash::{Hash, Hasher};
                        let mut hasher = DefaultHasher::new();
                        receiver.hash(&mut hasher);
                        // Preserve the full hash width via bit-cast — the
                        // typechecker declares the return type as `Int`
                        // (i64), and a wrapping reinterpretation is
                        // cheaper and more collision-resistant than
                        // truncation.
                        Some(Ok(Value::Int(hasher.finish() as i64)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    pub(super) fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        // Foreign functions take priority -- lets embedders override builtins.
        if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
            return invoke_foreign_fn(name, &f, args);
        }
        if let Some((module, func)) = name.split_once('.') {
            // Each arm is wrapped in `catch_builtin_panic` so that a panic
            // inside a builtin module becomes a clean `VmError` instead of
            // tearing down the current scheduler worker thread. Mirrors
            // `invoke_foreign_fn` for user-registered FFI.
            match module {
                "list" => catch_builtin_panic(
                    "list",
                    AssertUnwindSafe(|| builtins::collections::call_list(self, func, args)),
                ),
                "string" => catch_builtin_panic(
                    "string",
                    AssertUnwindSafe(|| builtins::string::call(self, func, args)),
                ),
                "int" => catch_builtin_panic(
                    "int",
                    AssertUnwindSafe(|| builtins::numeric::call_int(func, args)),
                ),
                "float" => catch_builtin_panic(
                    "float",
                    AssertUnwindSafe(|| builtins::numeric::call_float(func, args)),
                ),
                "map" => catch_builtin_panic(
                    "map",
                    AssertUnwindSafe(|| builtins::collections::call_map(self, func, args)),
                ),
                "set" => catch_builtin_panic(
                    "set",
                    AssertUnwindSafe(|| builtins::collections::call_set(self, func, args)),
                ),
                "result" => catch_builtin_panic(
                    "result",
                    AssertUnwindSafe(|| builtins::core::call_result(self, func, args)),
                ),
                "option" => catch_builtin_panic(
                    "option",
                    AssertUnwindSafe(|| builtins::core::call_option(self, func, args)),
                ),
                "io" => catch_builtin_panic(
                    "io",
                    AssertUnwindSafe(|| builtins::io::call(self, func, args)),
                ),
                "bytes" => catch_builtin_panic(
                    "bytes",
                    AssertUnwindSafe(|| builtins::bytes::call(self, func, args)),
                ),
                "crypto" => catch_builtin_panic(
                    "crypto",
                    AssertUnwindSafe(|| builtins::crypto::call(self, func, args)),
                ),
                "encoding" => catch_builtin_panic(
                    "encoding",
                    AssertUnwindSafe(|| builtins::encoding::call(self, func, args)),
                ),
                "uuid" => catch_builtin_panic(
                    "uuid",
                    AssertUnwindSafe(|| builtins::uuid::call(self, func, args)),
                ),
                "stream" => catch_builtin_panic(
                    "stream",
                    AssertUnwindSafe(|| builtins::stream::call(self, func, args)),
                ),
                #[cfg(feature = "tcp")]
                "tcp" => catch_builtin_panic(
                    "tcp",
                    AssertUnwindSafe(|| builtins::tcp::call(self, func, args)),
                ),
                "fs" => catch_builtin_panic(
                    "fs",
                    AssertUnwindSafe(|| builtins::io::call_fs(self, func, args)),
                ),
                "env" => catch_builtin_panic(
                    "env",
                    AssertUnwindSafe(|| builtins::io::call_env(self, func, args)),
                ),
                "test" => catch_builtin_panic(
                    "test",
                    AssertUnwindSafe(|| builtins::core::call_test(self, func, args)),
                ),
                "math" => catch_builtin_panic(
                    "math",
                    AssertUnwindSafe(|| builtins::numeric::call_math(func, args)),
                ),
                "regex" => catch_builtin_panic(
                    "regex",
                    AssertUnwindSafe(|| builtins::data::call_regex(self, func, args)),
                ),
                "json" => catch_builtin_panic(
                    "json",
                    AssertUnwindSafe(|| builtins::data::call_json(self, func, args)),
                ),
                "toml" => catch_builtin_panic(
                    "toml",
                    AssertUnwindSafe(|| builtins::toml::call(self, func, args)),
                ),
                "channel" => catch_builtin_panic(
                    "channel",
                    AssertUnwindSafe(|| builtins::concurrency::call_channel(self, func, args)),
                ),
                "task" => catch_builtin_panic(
                    "task",
                    AssertUnwindSafe(|| builtins::concurrency::call_task(self, func, args)),
                ),
                "time" => catch_builtin_panic(
                    "time",
                    AssertUnwindSafe(|| builtins::data::call_time(self, func, args)),
                ),
                "http" => catch_builtin_panic(
                    "http",
                    AssertUnwindSafe(|| builtins::data::call_http(self, func, args)),
                ),
                #[cfg(feature = "postgres")]
                "postgres" => catch_builtin_panic(
                    "postgres",
                    AssertUnwindSafe(|| builtins::postgres::call(self, func, args)),
                ),
                // ── Built-in trait Error impls ──
                // Phase 1 of the stdlib error redesign: `trait Error`
                // impls for builtin error enums are routed through
                // dedicated dispatch helpers rather than compiled from
                // silt source. IoError, JsonError, TomlError, and
                // ParseError are wired up; HttpError/RegexError trait
                // impls still return `Result(_, String)` today and
                // will migrate in later phases.
                "IoError" => catch_builtin_panic(
                    "IoError",
                    AssertUnwindSafe(|| builtins::io::call_io_error_trait(func, args)),
                ),
                "JsonError" => catch_builtin_panic(
                    "JsonError",
                    AssertUnwindSafe(|| builtins::data::call_json_error_trait(func, args)),
                ),
                "TomlError" => catch_builtin_panic(
                    "TomlError",
                    AssertUnwindSafe(|| builtins::toml::call_toml_error_trait(func, args)),
                ),
                "ParseError" => catch_builtin_panic(
                    "ParseError",
                    AssertUnwindSafe(|| builtins::numeric::call_parse_error_trait(func, args)),
                ),
                "HttpError" => catch_builtin_panic(
                    "HttpError",
                    AssertUnwindSafe(|| builtins::data::call_http_error_trait(func, args)),
                ),
                "RegexError" => catch_builtin_panic(
                    "RegexError",
                    AssertUnwindSafe(|| builtins::data::call_regex_error_trait(func, args)),
                ),
                #[cfg(feature = "postgres")]
                "PgError" => catch_builtin_panic(
                    "PgError",
                    AssertUnwindSafe(|| builtins::postgres::call_pg_error_trait(func, args)),
                ),
                #[cfg(feature = "tcp")]
                "TcpError" => catch_builtin_panic(
                    "TcpError",
                    AssertUnwindSafe(|| builtins::tcp::call_tcp_error_trait(func, args)),
                ),
                "TimeError" => catch_builtin_panic(
                    "TimeError",
                    AssertUnwindSafe(|| builtins::data::call_time_error_trait(func, args)),
                ),
                "BytesError" => catch_builtin_panic(
                    "BytesError",
                    AssertUnwindSafe(|| builtins::bytes::call_bytes_error_trait(func, args)),
                ),
                "ChannelError" => catch_builtin_panic(
                    "ChannelError",
                    AssertUnwindSafe(|| {
                        builtins::concurrency::call_channel_error_trait(func, args)
                    }),
                ),
                #[cfg(test)]
                "__test_panic_builtin" => {
                    catch_builtin_panic(
                        "__test_panic_builtin",
                        AssertUnwindSafe(|| {
                            // Force a panic regardless of the function name; the
                            // test harness uses this arm to verify that
                            // `catch_builtin_panic` converts the panic into a
                            // clean `VmError`.
                            let _ = (&*self, func, args);
                            panic!("synthetic builtin panic for test")
                        }),
                    )
                }
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        invoke_foreign_fn(name, &f, args)
                    } else {
                        Err(VmError::new(format!("unknown module: {module}")))
                    }
                }
            }
        } else {
            match name {
                "println" => {
                    if args.len() != 1 {
                        return Err(VmError::new(format!(
                            "println takes 1 argument, got {}",
                            args.len()
                        )));
                    }
                    println!("{}", self.display_value(&args[0]));
                    Ok(Value::Unit)
                }
                "print" => {
                    if args.len() != 1 {
                        return Err(VmError::new(format!(
                            "print takes 1 argument, got {}",
                            args.len()
                        )));
                    }
                    print!("{}", self.display_value(&args[0]));
                    Ok(Value::Unit)
                }
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: {msg}")))
                }
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        invoke_foreign_fn(name, &f, args)
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
            match invoke_foreign_fn("__wasm_epoch_ms", f, &[])? {
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
