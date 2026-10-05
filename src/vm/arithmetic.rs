//! Arithmetic, comparison, and type-checking helpers for the VM.

use crate::bytecode::Op;
use crate::value::Value;

use super::{Vm, VmError, finite_float};

impl Vm {
    // ── Arithmetic helpers ────────────────────────────────────────

    pub(super) fn binary_arithmetic(&mut self, op: Op) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let result = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => match op {
                Op::Add => match a.checked_add(*b) {
                    Some(v) => Value::Int(v),
                    None => return Err(VmError::new(format!("integer overflow: {a} + {b}"))),
                },
                Op::Sub => match a.checked_sub(*b) {
                    Some(v) => Value::Int(v),
                    None => return Err(VmError::new(format!("integer overflow: {a} - {b}"))),
                },
                Op::Mul => match a.checked_mul(*b) {
                    Some(v) => Value::Int(v),
                    None => return Err(VmError::new(format!("integer overflow: {a} * {b}"))),
                },
                Op::Div => {
                    if *b == 0 {
                        return Err(VmError::new("division by zero".to_string()));
                    }
                    match a.checked_div(*b) {
                        Some(v) => Value::Int(v),
                        None => return Err(VmError::new(format!("integer overflow: {a} / {b}"))),
                    }
                }
                Op::Mod => {
                    if *b == 0 {
                        return Err(VmError::new("modulo by zero".to_string()));
                    }
                    match a.checked_rem(*b) {
                        Some(v) => Value::Int(v),
                        None => return Err(VmError::new(format!("integer overflow: {a} % {b}"))),
                    }
                }
                _ => unreachable!(),
            },
            (Value::Float(a), Value::Float(b)) => match op {
                Op::Add => finite_float(a + b, &format!("{a} + {b}"))?,
                Op::Sub => finite_float(a - b, &format!("{a} - {b}"))?,
                Op::Mul => finite_float(a * b, &format!("{a} * {b}"))?,
                Op::Div => {
                    if *b == 0.0 {
                        return Err(VmError::new("float division by zero".to_string()));
                    }
                    finite_float(a / b, &format!("{a} / {b}"))?
                }
                Op::Mod => {
                    if *b == 0.0 {
                        return Err(VmError::new("modulo by zero".to_string()));
                    }
                    finite_float(a % b, &format!("{a} % {b}"))?
                }
                _ => unreachable!(),
            },
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
                // Mirror the typechecker's operand-domain message: a
                // String reaching `+` (e.g. through a polymorphic
                // `fn add(a, b) { a + b }`) gets the same pointer.
                let hint = if op == Op::Add && (a_type == "String" || b_type == "String") {
                    "; build strings with interpolation, e.g. \"{a}{b}\""
                } else {
                    ""
                };
                return Err(VmError::new(format!(
                    "cannot apply '{op_name}' to {a_type} and {b_type}{hint}",
                )));
            }
        };
        self.push(result);
        Ok(())
    }

    pub(super) fn compare(&mut self, pred: fn(std::cmp::Ordering) -> bool) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let ordering = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            // A Float is always finite, so `partial_cmp` always answers;
            // `Equal` is the same safety net `Value::cmp` uses.
            (Value::Float(a), Value::Float(b)) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            (Value::String(a), Value::String(b)) => a.cmp(b),
            // List vs List and the mixed List/Range pairings share the same
            // Silt type (`List(T)`), so must be ordered element-wise. The
            // `Value::cmp` impl already handles every pairing, including
            // Range vs List, so defer to it — after the function-leaf gate
            // (see `ordering_with_fn_gate` below).
            (Value::List(_), Value::List(_))
            | (Value::List(_), Value::Range(..))
            | (Value::Range(..), Value::List(_))
            | (Value::Range(..), Value::Range(..)) => Self::ordering_with_fn_gate(&a, &b)?,
            // Round 85: mirror the `<anon>`-wildcard logic from
            // `Value::PartialEq`/`Ord` (src/value/key.rs).
            // The typechecker normally rejects source-level ordering of
            // anon-shaped records, but this is defensive for cases
            // where a nominal flows through `unify_anon_nominal` and
            // ends up compared against an anon-typed value at runtime —
            // the same-type guard alone would skip the dispatch and
            // fall to the catch-all error.
            (Value::Record(ta, _), Value::Record(tb, _))
                if ta.id == tb.id || ta.is_anon() || tb.is_anon() =>
            {
                Self::ordering_with_fn_gate(&a, &b)?
            }
            (Value::Variant(..), Value::Variant(..)) => Self::ordering_with_fn_gate(&a, &b)?,
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

    /// Order two container-shaped operands element-wise via `Value::cmp`,
    /// first rejecting any operand that transitively contains a
    /// function-shaped leaf (`Vm::value_contains_fn`, src/vm/mod.rs).
    ///
    /// This is the execution-site backstop for the round-97 typechecker
    /// gate: the CONCRETE form (`[{ x -> x }] < [{ x -> x }]`) is a
    /// compile error, but a polymorphic wrapper (`fn lt(a: x, b: x) ->
    /// Bool { a < b }`) launders a container of functions past
    /// `pending_numeric_checks` (which skips `Var`-typed operands on the
    /// documented promise that the VM catches the violation at runtime).
    /// Without this gate, `Value::cmp` ordered `VmClosure` leaves by
    /// `Arc::as_ptr` (src/value/key.rs), so the resulting Bool depended on
    /// heap allocation order — nondeterministic across runs. Bare
    /// function-shaped operands never reach this helper: they fall to
    /// `compare()`'s catch-all arm and keep its "cannot compare Fn and
    /// Fn" wording. Locked by
    /// tests/typecheck/container_fn_compare_runtime_gate_tests.rs.
    fn ordering_with_fn_gate(a: &Value, b: &Value) -> Result<std::cmp::Ordering, VmError> {
        if Self::value_contains_fn(a) || Self::value_contains_fn(b) {
            return Err(VmError::new(
                "type 'Fn' does not implement Compare".to_string(),
            ));
        }
        Ok(a.cmp(b))
    }

    // ── Type compatibility ────────────────────────────────────────

    /// Returns a discriminant used by [`check_same_type`] to decide whether
    /// two values may be compared for equality. Silt types that the
    /// typechecker treats interchangeably share a discriminant:
    /// `List`/`Range` (a range has type `List(Int)`).
    pub(super) fn value_disc(val: &Value) -> u8 {
        // These values are compared only for equality in `check_same_type`
        // (never as `Ord`) and are not persisted anywhere — they are a
        // compile-time-agreed label, not a stable serialization tag. So the
        // numbers may be renumbered freely. A historical gap at `2` used to
        // mark a now-removed variant; closed here since closing it is
        // semantically invisible to all current callers.
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
            Value::BuiltinFn(_) | Value::HostFn(_) => 14,
            Value::VariantConstructor(..) => 15,
            Value::TypeDescriptor(_) => 16,
            Value::PrimitiveDescriptor(_) => 17,
            Value::Bytes(_) => 18,
            Value::TcpListener(_) => 19,
            Value::TcpStream(_) => 20,
        }
    }

    /// Check that two values have compatible types for equality/comparison.
    pub(super) fn check_same_type(&self, a: &Value, b: &Value) -> Result<(), VmError> {
        if Self::value_disc(a) != Self::value_disc(b) {
            return Err(VmError::new(format!(
                "unsupported operation: cannot compare {} and {}",
                self.type_name(a),
                self.type_name(b)
            )));
        }
        Ok(())
    }
}
