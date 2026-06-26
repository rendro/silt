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
                    let result = a / b;
                    Value::ExtFloat(if result == 0.0 { 0.0 } else { result })
                }
                Op::Mod => {
                    if *b == 0.0 {
                        return Err(VmError::new("modulo by zero".to_string()));
                    }
                    finite_float(a % b, &format!("{a} % {b}"))?
                }
                _ => unreachable!(),
            },
            // Any pair touching `ExtFloat` widens to `ExtFloat` with IEEE-754
            // semantics (no finite-ness check). Collapsed from three arms —
            // `(ExtFloat, ExtFloat)`, `(Float, ExtFloat)`, `(ExtFloat, Float)`
            // — that were byte-identical. The dedicated `(Float, Float)` arm
            // above already handles the finite path, so top-down match order
            // guarantees this or-pattern only runs when at least one operand
            // is `ExtFloat`. Result is always `Value::ExtFloat(_)` regardless
            // of which side was a plain `Float`.
            (Value::Float(a) | Value::ExtFloat(a), Value::Float(b) | Value::ExtFloat(b)) => {
                let (a, b) = (*a, *b);
                let result = match op {
                    Op::Add => a + b,
                    Op::Sub => a - b,
                    Op::Mul => a * b,
                    Op::Div => a / b,
                    Op::Mod => a % b,
                    _ => unreachable!(),
                };
                // Canonicalize -0.0 -> +0.0 to uphold the "an ExtFloat never
                // holds -0.0" invariant, exactly as the `(Float, Float)` Div
                // arm above (and Op::Negate, NarrowFloat, the numeric builtins)
                // already do. ExtFloat Eq/Ord/Hash are *bitwise* (value.rs), so
                // a stray ExtFloat(-0.0) — reachable here via e.g. `(-1.0) *
                // (0.0 / 1.0)` or `(-1.0) / (1.0 / 0.0)` — would be a distinct
                // container key from ExtFloat(+0.0) even though the IEEE `==`
                // operator calls them equal, letting a set/map hold two "equal"
                // elements. The dedicated finite `(Float, Float)` arm above
                // means this only runs with at least one ExtFloat operand.
                Value::ExtFloat(if result == 0.0 { 0.0 } else { result })
            }
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

    pub(super) fn compare(&mut self, pred: fn(std::cmp::Ordering) -> bool) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let ordering = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            // Collapsed from four byte-identical NaN/non-finite arms
            // (`(Float, Float)`, `(ExtFloat, ExtFloat)`, `(Float,
            // ExtFloat)`, `(ExtFloat, Float)`) that previously had two
            // divergent error wordings — a non-finite phrasing on the
            // Float/Float arm and a NaN-only phrasing on the three
            // Float/ExtFloat shapes — for the semantically identical
            // condition. Round 67 unified to ONE message — NaN IS
            // non-finite, so the broader phrasing covers every
            // partial_cmp failure. The typechecker permits the mixed
            // Float/ExtFloat pair for ordering comparisons; the
            // or-pattern dereference + f64 partial_cmp matches the
            // widening rule used elsewhere.
            (Value::Float(a) | Value::ExtFloat(a), Value::Float(b) | Value::ExtFloat(b)) => {
                a.partial_cmp(b).ok_or_else(|| {
                    VmError::new("cannot compare non-finite float values".to_string())
                })?
            }
            (Value::String(a), Value::String(b)) => a.cmp(b),
            // List vs List and the mixed List/Range pairings share the same
            // Silt type (`List(T)`), so must be ordered element-wise. The
            // `Value::cmp` impl already handles every pairing, including
            // Range vs List, so defer to it.
            (Value::List(_), Value::List(_))
            | (Value::List(_), Value::Range(..))
            | (Value::Range(..), Value::List(_))
            | (Value::Range(..), Value::Range(..)) => a.cmp(&b),
            // Round 85: mirror the `<anon>`-wildcard logic from
            // `Value::PartialEq`/`Ord` (src/value.rs ~1729, ~1875).
            // The typechecker normally rejects source-level ordering of
            // anon-shaped records, but this is defensive for cases
            // where a nominal flows through `unify_anon_nominal` and
            // ends up compared against an anon-typed value at runtime —
            // the `na == nb` guard alone would skip the dispatch and
            // fall to the catch-all error.
            (Value::Record(na, _), Value::Record(nb, _))
                if na == nb || na.as_str() == "<anon>" || nb.as_str() == "<anon>" =>
            {
                a.cmp(&b)
            }
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

    // ── Type compatibility ────────────────────────────────────────

    /// Returns a discriminant used by [`check_same_type`] to decide whether
    /// two values may be compared for equality. Silt types that the
    /// typechecker treats interchangeably share a discriminant:
    /// `Float`/`ExtFloat` (the typechecker permits mixed equality and
    /// ordering without unifying them) and `List`/`Range` (a range has
    /// type `List(Int)`).
    pub(super) fn value_disc(val: &Value) -> u8 {
        // These values are compared only for equality in `check_same_type`
        // (never as `Ord`) and are not persisted anywhere — they are a
        // compile-time-agreed label, not a stable serialization tag. So the
        // numbers may be renumbered freely. A historical gap at `2` used to
        // mark a now-removed variant; closed here since closing it is
        // semantically invisible to all current callers.
        match val {
            Value::Int(_) => 0,
            // Float and ExtFloat share a discriminant so `check_same_type`
            // accepts the mixed pair that the typechecker permits. The VM
            // falls through to `Value::eq`, which widens to f64 for both
            // variants.
            Value::Float(_) | Value::ExtFloat(_) => 1,
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
