//! Builtin registration and dispatch.

use std::panic::AssertUnwindSafe;

use super::{Vm, VmError};
use crate::builtins;
use crate::typeinfo::Tag;
use crate::value::{HostFn, Value};

// ── Round-62 follow-up: dispatch arms for (Variant, Variant) /
// (Record, Record) — and the corresponding entries in the hash
// allowlist — were deleted in this round.
//
// The auto-derive synthesis pass in
// `src/typechecker/mod.rs::synthesize_auto_derive_impls` now
// processes BOTH user-declared types AND built-in enums / records
// (Option, Result, Weekday, Method, ChannelResult, Step, IoError /
// JsonError / ..., Date, Time, DateTime, Duration, Instant,
// FileStat, Response, Request). Every (trait, type) pair stamped
// in `trait_impl_set` receives a synthesized impl, whose methods get
// global slots at compile time, and `Op::CallMethod`'s method lookup
// (`Globals::method`) resolves the call before it ever reaches
// `dispatch_trait_method`.
//
// The deadness instrumentation (six atomic counters + their
// reset/snapshot helpers) was removed alongside the arms. The
// barrage in `tests/meta/auto_derive_dead_arm_proof_tests.rs` now
// stands as a behavioural lock — every shape (user and built-in)
// must produce the expected output, which it cannot do via the
// catch-all error arm that remains in `dispatch_trait_method`.

/// Write `text` to the host's stdout for `print` / `println`. A failure
/// is a runtime error.
fn write_stdout(vm: &Vm, text: &str) -> Result<(), VmError> {
    vm.runtime.io.out(text).map_err(|e| {
        VmError::new(format!(
            "cannot write to stdout: {}",
            crate::diagnostic::io_error_text(&e)
        ))
    })
}

/// Call the host function `host` while catching panics that escape it.
///
/// A panicking host function would otherwise tear down the scheduler
/// worker thread (or the main thread), leaving other tasks unable to
/// progress. A caught panic becomes a [`VmError`] whose message preserves
/// the panic payload when it is a `&str` or `String`. A result that is
/// not of the type the signature returns is an error too. Every error
/// names the function.
pub(super) fn invoke_host_fn(host: &HostFn, args: &[Value]) -> Result<Value, VmError> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| (host.call)(args))) {
        Ok(Ok(value)) if host.returns.admits(&value) => Ok(value),
        Ok(Ok(value)) => {
            let mut shown = value.to_string();
            if shown.chars().count() > 80 {
                shown = shown.chars().take(77).collect::<String>() + "...";
            }
            Err(VmError::new(format!(
                "{}: its signature returns {}, but it returned {} {shown}",
                host.name,
                host.returns,
                builtins::value_kind(&value)
            )))
        }
        Ok(Err(e)) => Err(VmError {
            message: format!("{}: {}", host.name, e.message),
            ..e
        }),
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            Err(VmError::new(format!(
                "host function '{}' panicked: {msg}",
                host.name
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
/// [`invoke_host_fn`] for host functions — a panic in a builtin
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

/// Uniform signature shared by every `call_<x>_error_trait` helper.
type ErrorTraitFn = fn(&str, &[Value]) -> Result<Value, VmError>;

/// Round-73 BLOAT-3: dispatch table for built-in `trait Error` impls.
///
/// Each `(enum_name, fn_ptr)` entry corresponds to a previous match
/// arm in `dispatch_builtin` of the form
/// `"<Enum>" => catch_builtin_panic("<Enum>",
///   AssertUnwindSafe(|| <module>::call_<x>_error_trait(func, args)))`.
///
/// PgError / TcpError stay cfg-gated by being conditionally included
/// in the table — the gate must match the gate on the corresponding
/// `call_*_error_trait` symbol.
static ERROR_TRAIT_DISPATCH: &[(&str, ErrorTraitFn)] = &[
    ("IoError", builtins::io::call_io_error_trait),
    ("JsonError", builtins::json::call_json_error_trait),
    ("TomlError", builtins::toml::call_toml_error_trait),
    ("ParseError", builtins::numeric::call_parse_error_trait),
    ("HttpError", builtins::http::call_http_error_trait),
    ("RegexError", builtins::regex::call_regex_error_trait),
    #[cfg(feature = "postgres")]
    ("PgError", builtins::postgres::call_pg_error_trait),
    #[cfg(feature = "tcp")]
    ("TcpError", builtins::tcp::call_tcp_error_trait),
    ("TimeError", builtins::time::call_time_error_trait),
    ("BytesError", builtins::bytes::call_bytes_error_trait),
    (
        "ChannelError",
        builtins::concurrency::call_channel_error_trait,
    ),
];

/// Look up the `trait Error` dispatch helper for a given builtin enum
/// name. Returns `None` for any name not in the table (including
/// cfg-gated names whose feature is disabled).
pub(crate) fn error_trait_dispatch(enum_name: &str) -> Option<ErrorTraitFn> {
    ERROR_TRAIT_DISPATCH
        .iter()
        .find(|(n, _)| *n == enum_name)
        .map(|(_, f)| *f)
}

/// Render a stdlib error variant via its `Error::message()`
/// implementation, returning `None` when the tag isn't a stdlib-error
/// variant (or rendering fails for any reason — caller falls back to
/// the default constructor-form render).
///
/// Used by `Value::Display` to collapse the dual shape between
/// `format!("{e}")` and `e.message()` for stdlib error enums per the
/// silt "explicit over implicit / one way" principle. User-defined
/// enums are not affected: only a variant of a builtin error enum is
/// rendered so, whatever its name.
pub fn render_stdlib_error_message(tag: &Tag, fields: &[Value]) -> Option<String> {
    let ty = tag.ty();
    crate::defs::builtin_types().get(ty.id.0.0 as usize)?;
    let dispatch_fn = error_trait_dispatch(&ty.name)?;
    let variant_value = Value::Variant(tag.clone(), fields.to_vec());
    match dispatch_fn("message", &[variant_value]).ok()? {
        Value::String(s) => Some(s),
        _ => None,
    }
}

impl Vm {
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
                // Runtime Display gate — the .display() twin of the
                // round-95 `Op::DisplayValue` gate (src/vm/run.rs
                // ~:1457). For a *concrete* receiver the typechecker
                // already rejects `.display()` on no-Display types
                // ("unknown method 'display' on type Fn"), but silt
                // enforces inferred trait bounds at the EXECUTION site
                // for polymorphic code, so a Var-typed receiver reaches
                // this arm ungated. Pre-fix, `fn show(x: a) -> String
                // { x.display() }` over a lambda / channel / task handle
                // silently rendered `<fn:..>` / `<channel:0>` /
                // `<handle:0>` — while the equivalent interpolation
                // `"{x}"` errored at runtime and the sibling `.equal()` /
                // `.compare()` arms below carry their own runtime gates.
                // Reject the same set here, sourced from the single
                // oracle `Vm::value_implements_display` so the two
                // execution-site gates cannot drift. Records, variants
                // (incl. stdlib error enums) and every printable
                // built-in pass the oracle and fall through unchanged.
                if !Self::value_implements_display(receiver) {
                    // Same canonical-name reporting as Op::DisplayValue:
                    // function-shaped values collapse to "Fn" via
                    // `dispatch_type_name`; the descriptor values
                    // (whose canonical name is the *carried* type name)
                    // fall back to `type_name` so the diagnostic names
                    // the descriptor kind, not the reflected type.
                    let name = match receiver {
                        Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
                            self.type_name(receiver).to_string()
                        }
                        _ => crate::types::canonical::dispatch_type_name(receiver),
                    };
                    return Some(Err(VmError::new(format!(
                        "type '{name}' does not implement Display"
                    ))));
                }
                Some(Ok(Value::String(self.display_value(receiver))))
            }
            "equal" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("equal() takes 1 argument".into())));
                }
                // Execution-site backstop mirroring the `Op::Eq` gate
                // (`equality_operand_violation`, src/vm/run.rs): an
                // operand that is, or transitively contains, a
                // function-shaped leaf has no Equal impl. A polymorphic
                // wrapper (`fn eq(a: x, b: x) -> Bool { a.equal(b) }`)
                // can launder such values past the typechecker's
                // concrete-operand gate, and `PartialEq for Value` would
                // silently answer with `Arc::ptr_eq` identity.
                if Self::value_contains_fn(receiver) || Self::value_contains_fn(&extra_args[0]) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Equal".into(),
                    )));
                }
                // Defensive fallback. For every valid user/builtin type
                // that passes the round-93 field-aware auto-derive gate
                // (`compute_auto_derive_field_negatives`), a synth-emitted
                // `equal` impl method is produced and `Op::CallMethod`
                // (src/vm/run.rs) resolves it FIRST, so a
                // Variant/Record receiver never reaches this arm. Types
                // with non-supportable fields (e.g. Channel/Map/Tuple/
                // Function/Bytes/Handle) are now statically REJECTED by
                // that gate (`type 'X' does not implement trait`), so the
                // old "such fields are laundered through here" path no
                // longer exists, and no valid program reaches it. `impl PartialEq for Value` (in
                // src/value/key.rs) compares records and variants structurally,
                // so this arm stays sound even on that malformed input.
                Some(Ok(Value::Bool(*receiver == extra_args[0])))
            }
            "compare" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("compare() takes 1 argument".into())));
                }
                let other = &extra_args[0];
                // Execution-site backstop mirroring `ordering_with_fn_gate`
                // (src/vm/arithmetic.rs): reject operands that are, or
                // transitively contain, a function-shaped leaf before any
                // arm can defer to `Value::cmp`, which orders closures by
                // `Arc::as_ptr` — an ASLR-nondeterministic result for a
                // polymorphic `fn cmp(a: x, b: x) -> Int { a.compare(b) }`
                // laundering a container of functions past the typechecker.
                if Self::value_contains_fn(receiver) || Self::value_contains_fn(other) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Compare".into(),
                    )));
                }
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    // A Float is always finite, so `partial_cmp` always
                    // answers; `Equal` is the same safety net `Value::cmp`
                    // uses.
                    (Value::Float(a), Value::Float(b)) => {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    // List vs List: the typechecker auto-derives Compare for
                    // List (see src/typechecker/mod.rs:8176), so a value of
                    // `List(T)` flowing through a `Compare` bound must
                    // resolve here. Defer to the existing element-wise
                    // ordering on `Value::cmp`, which already handles
                    // List/Range pairings (see src/vm/arithmetic.rs:152).
                    (Value::List(_), Value::List(_))
                    | (Value::List(_), Value::Range(..))
                    | (Value::Range(..), Value::List(_))
                    | (Value::Range(..), Value::Range(..)) => receiver.cmp(other),
                    // Defensive fallback. For every valid user/builtin type
                    // that passes the round-93 field-aware auto-derive gate
                    // (`compute_auto_derive_field_negatives`), a synth-emitted
                    // `compare` impl method is produced and `Op::CallMethod`
                    // (src/vm/run.rs) resolves it FIRST, so a
                    // Variant/Record receiver never reaches this arm. Types
                    // with non-supportable fields (e.g. Channel/Map/Tuple/
                    // Function/Bytes/Handle) are now statically REJECTED by
                    // that gate (`type 'X' does not implement trait`), so the
                    // old "such fields are laundered through here" path no
                    // longer exists, and no valid program reaches it. `fn cmp` (in src/value/key.rs)
                    // orders records and variants structurally, so this arm
                    // stays sound even on that malformed input.
                    (Value::Variant(..), Value::Variant(..))
                    | (Value::Record(..), Value::Record(..)) => receiver.cmp(other),
                    //
                    // Unit vs Unit: typechecker auto-derives Compare for `()`
                    // (src/typechecker/mod.rs:8173). All units are equal.
                    (Value::Unit, Value::Unit) => std::cmp::Ordering::Equal,
                    // A tuple is ordered part by part.
                    (Value::Tuple(_), Value::Tuple(_)) => receiver.cmp(other),
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
                // Bool / String / List (and more). At runtime, the
                // synthesized impls of user types are resolved via the
                // method lookup in `Op::CallMethod`; only
                // auto-derived primitives fall through to here.
                //
                // `Value` already implements `std::hash::Hash` with a
                // canonical bit-hash for floats (see `impl Hash for Value` in src/value/key.rs).
                // We reuse that impl via `DefaultHasher` so the result
                // matches `HashMap<Value, Value>` keying.
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("hash() takes no arguments".into())));
                }
                // Execution-site backstop mirroring the `"equal"` /
                // `"compare"` arms above: a receiver that is, or
                // transitively contains, a function-shaped leaf has no
                // Hash (the checker rejects `[{ y -> y }].hash()`; a
                // function has none of the structural traits). The
                // std `Hash` impl on `Value` hashes every closure as a
                // constant discriminant tag ("not meaningfully
                // hashable", src/value/key.rs), so two distinct closures
                // would hash identically and collide silently.
                if Self::value_contains_fn(receiver) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Hash".into(),
                    )));
                }
                // Only honour hash() for types the typechecker actually
                // auto-derives Hash for — emitting a dispatch error for
                // anything else keeps the user-impl path authoritative.
                // The Variant/Record entries below are a defensive
                // fallback. For every valid user/builtin type that passes
                // the round-93 field-aware auto-derive gate
                // (`compute_auto_derive_field_negatives`), a synth-emitted
                // `hash` impl method is produced and `Op::CallMethod`
                // (src/vm/run.rs) resolves it FIRST, so a
                // Variant/Record receiver never reaches this arm. Types
                // with non-supportable fields (e.g. Channel/Map/Tuple/
                // Function/Bytes/Handle) are now statically REJECTED by
                // that gate (`type 'X' does not implement trait`), so the
                // old "such fields are laundered through here" path no
                // longer exists, and no valid program reaches it. `impl Hash for Value` (in
                // src/value/key.rs) hashes records and variants structurally, so
                // this arm stays sound even on that malformed input.
                match receiver {
                    Value::Int(_)
                    | Value::Float(_)
                    | Value::Bool(_)
                    | Value::String(_)
                    | Value::List(_)
                    // Range hashes via the same `impl Hash for Value`
                    // (in src/value/key.rs); typechecker registers Hash for
                    // every `List(T)` that flows through a `Hash` bound,
                    // and `1..5` reaches dispatch as `Value::Range`.
                    | Value::Range(..)
                    | Value::Tuple(_)
                    | Value::Map(_)
                    | Value::Set(_)
                    | Value::Variant(..)
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
            // `trait Error` of the builtin error enums (`IoError`, ...):
            // native, through the enum's dispatch helper.
            "message" => {
                let Value::Variant(tag, _) = receiver else {
                    return None;
                };
                let ty = tag.ty();
                crate::defs::builtin_types().get(ty.id.0.0 as usize)?;
                let dispatch = error_trait_dispatch(&ty.name)?;
                let mut args = Vec::with_capacity(1 + extra_args.len());
                args.push(receiver.clone());
                args.extend(extra_args.iter().cloned());
                Some(catch_builtin_panic(
                    &ty.name,
                    AssertUnwindSafe(|| dispatch("message", &args)),
                ))
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    /// Call the builtin `name`. A host clock that has panicked, in
    /// this call or on one of the runtime's threads, fails the call:
    /// the readings the builtin got since are not real, and the waits
    /// it was woken from have not ended.
    pub(super) fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let value = self.dispatch_builtin_unchecked(name, args)?;
        match self.runtime.io.clock_failure() {
            Some(failure) => Err(VmError::new(failure)),
            None => Ok(value),
        }
    }

    fn dispatch_builtin_unchecked(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        if let Some((module, func)) = name.split_once('.') {
            // A builtin module's function: the body of its row in the
            // builtin registry. A panic inside it becomes a clean
            // `VmError` instead of tearing down the current scheduler
            // worker thread, as for a host function (`invoke_host_fn`).
            #[cfg(test)]
            if module == "__test_panic_builtin" {
                // The test harness uses this name to verify that
                // `catch_builtin_panic` converts a panic into a `VmError`.
                return catch_builtin_panic(
                    "__test_panic_builtin",
                    AssertUnwindSafe(|| {
                        let _ = (&*self, func, args);
                        panic!("synthetic builtin panic for test")
                    }),
                );
            }
            match builtins::registry::registry().module(module) {
                Some(entry) if entry.enabled => match entry.row(func) {
                    Some(row) => {
                        catch_builtin_panic(entry.name, AssertUnwindSafe(|| row.call(self, args)))
                    }
                    None => Err(VmError::new(format!("unknown {module} function: {func}"))),
                },
                _ => Err(VmError::new(format!("unknown builtin namespace: {module}"))),
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
                    let mut text = self.display_value(&args[0]);
                    text.push('\n');
                    write_stdout(self, &text)?;
                    Ok(Value::Unit)
                }
                "print" => {
                    if args.len() != 1 {
                        return Err(VmError::new(format!(
                            "print takes 1 argument, got {}",
                            args.len()
                        )));
                    }
                    write_stdout(self, &self.display_value(&args[0]))?;
                    Ok(Value::Unit)
                }
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: {msg}")))
                }
                _ => Err(VmError::new(format!("unknown builtin: {name}"))),
            }
        }
    }
}
