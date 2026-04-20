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
