//! Builtin registration and dispatch.

use std::panic::AssertUnwindSafe;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins;
use crate::typeinfo::Tag;
use crate::value::{HostFn, Value};

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

/// Run a builtin of `module` under `catch_unwind`, converting any
/// panic that escapes the builtin into a clean `VmError`. This mirrors
/// [`invoke_host_fn`] for host functions — a panic in a builtin
/// would otherwise tear down the current scheduler worker thread.
///
/// Callers that capture `&mut Vm` (or other non-`UnwindSafe` state) should
/// wrap the closure in [`AssertUnwindSafe`] before passing it here.
pub(super) fn catch_builtin_panic<F, T>(module: &str, f: F) -> Result<T, VmError>
where
    F: FnOnce() -> Result<T, VmError> + std::panic::UnwindSafe,
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

/// Resume the frame of a builtin, as a call of the builtin is made:
/// a panic that escapes it is an error of the program.
pub(super) fn resume_native(
    vm: &mut Vm,
    native: &mut dyn Native,
    input: Value,
) -> Result<Step, VmError> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| native.resume(vm, input))) {
        Ok(step) => step,
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            let module = native.name().split('.').next().unwrap_or_default();
            Err(VmError::new(format!(
                "builtin module '{module}' panicked: {msg}"
            )))
        }
    }
}

/// Uniform signature shared by every `call_<x>_error_trait` helper.
type ErrorTraitFn = fn(&str, &[Value]) -> Result<Value, VmError>;

/// The dispatch table for built-in `trait Error` impls: for each
/// error enum, by its name, the helper that gives its `message`.
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
                // `Equal` is structural: `PartialEq for Value`
                // (src/value/key.rs) is its one implementation, for `==`
                // and for `.equal()`.
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
                    // List vs List: a list has Compare when its
                    // elements do. Defer to the existing element-wise
                    // ordering on `Value::cmp`, which already handles
                    // List/Range pairings (see src/vm/arithmetic.rs:152).
                    (Value::List(_), Value::List(_))
                    | (Value::List(_), Value::Range(..))
                    | (Value::Range(..), Value::List(_))
                    | (Value::Range(..), Value::Range(..)) => receiver.cmp(other),
                    // `Compare` is structural: `Ord for Value`
                    // (src/value/key.rs) orders records by their
                    // declared fields and variants by declaration, for
                    // `<` and for `.compare()`.
                    (Value::Variant(..), Value::Variant(..))
                    | (Value::Record(..), Value::Record(..)) => receiver.cmp(other),
                    //
                    // Unit vs Unit: all units are equal.
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
                // `Hash` is structural.
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
                // The types that have `Hash`.
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
}
