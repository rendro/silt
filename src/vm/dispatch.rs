//! Builtin registration and dispatch.

use std::panic::AssertUnwindSafe;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
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
                value.kind()
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
                    // fall back to their kind so the diagnostic names
                    // the descriptor kind, not the reflected type.
                    let name = match receiver {
                        Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
                            receiver.kind().to_string()
                        }
                        _ => crate::types::canonical::dispatch_type_name(receiver),
                    };
                    return Some(Err(VmError::new(format!(
                        "type '{name}' does not implement Display"
                    ))));
                }
                if let Err(too_long) = receiver.writable() {
                    return Some(Err(too_long.into()));
                }
                Some(Ok(Value::String(self.display_value(receiver).into())))
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
                if receiver.contains_fn() || extra_args[0].contains_fn() {
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
                if receiver.contains_fn() || other.contains_fn() {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Compare".into(),
                    )));
                }
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    (Value::Float(a), Value::Float(b)) => a.cmp(b),
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    // List vs List: a list has Compare when its
                    // elements do, and is ordered element by element
                    // (`Value::cmp`).
                    (Value::List(_), Value::List(_)) => receiver.cmp(other),
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
                            receiver.kind(),
                            other.kind()
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
                if receiver.contains_fn() {
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
            _ => None,
        }
    }
}
