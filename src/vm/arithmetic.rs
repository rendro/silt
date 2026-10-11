//! Arithmetic, comparison, and type-checking helpers for the VM.

use crate::bytecode::Op;
use crate::value::Value;

use super::{Vm, VmError, finite_float};

impl Vm {
    // ── Arithmetic helpers ────────────────────────────────────────

    pub(super) fn binary_arithmetic(&mut self, op: Op) -> Result<(), VmError> {
        let b = self.pop();
        let a = self.pop();
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
            (Value::Float(a), Value::Float(b)) => {
                let (a, b) = (a.get(), b.get());
                match op {
                    Op::Add => finite_float(a + b, &format!("{a} + {b}"))?,
                    Op::Sub => finite_float(a - b, &format!("{a} - {b}"))?,
                    Op::Mul => finite_float(a * b, &format!("{a} * {b}"))?,
                    Op::Div => {
                        if b == 0.0 {
                            return Err(VmError::new("float division by zero".to_string()));
                        }
                        finite_float(a / b, &format!("{a} / {b}"))?
                    }
                    Op::Mod => {
                        if b == 0.0 {
                            return Err(VmError::new("modulo by zero".to_string()));
                        }
                        finite_float(a % b, &format!("{a} % {b}"))?
                    }
                    _ => unreachable!(),
                }
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
                let a_type = a.kind();
                let b_type = b.kind();
                // Special error for Int/Float mixing
                if (a_type == "Int" && b_type == "Float") || (a_type == "Float" && b_type == "Int")
                {
                    return Err(VmError::type_confusion(
                        "cannot mix Int and Float — use int.to_float or float.to_int for explicit conversion",
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
                return Err(VmError::type_confusion(format!(
                    "cannot apply '{op_name}' to {a_type} and {b_type}{hint}",
                )));
            }
        };
        self.push(result);
        Ok(())
    }

    pub(super) fn compare(&mut self, pred: fn(std::cmp::Ordering) -> bool) -> Result<(), VmError> {
        let b = self.pop();
        let a = self.pop();
        let ordering = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.cmp(b),
            (Value::String(a), Value::String(b)) => a.cmp(b),
            // Lists are ordered element by element (`Value::cmp`), after
            // the function-leaf gate (see `ordering_with_fn_gate` below).
            (Value::List(_), Value::List(_)) => Self::ordering_with_fn_gate(&a, &b)?,
            // Records of one type are ordered by their fields.
            (Value::Record(ra), Value::Record(rb)) if ra.type_id() == rb.type_id() => {
                Self::ordering_with_fn_gate(&a, &b)?
            }
            (Value::Variant(..), Value::Variant(..)) => Self::ordering_with_fn_gate(&a, &b)?,
            // What has `Compare` is ordered: a tuple part by part, `false`
            // before `true`, `()` equal to itself.
            (Value::Tuple(_), Value::Tuple(_)) => Self::ordering_with_fn_gate(&a, &b)?,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Unit, Value::Unit) => std::cmp::Ordering::Equal,
            _ => {
                return Err(VmError::type_confusion(format!(
                    "unsupported operation: cannot compare {} and {}",
                    a.kind(),
                    b.kind()
                )));
            }
        };
        self.push(Value::Bool(pred(ordering)));
        Ok(())
    }

    /// Order two container-shaped operands element-wise via `Value::cmp`,
    /// first rejecting any operand that transitively contains a
    /// function-shaped leaf (`Value::contains_fn`, src/value/mod.rs).
    ///
    /// The checker rejects ordering a value that holds a function
    /// (`Compare` is decided by structure): this is the backstop at the
    /// execution site. Without it `Value::cmp` would order `VmClosure`
    /// leaves by `Arc::as_ptr` (src/value/key.rs), a result that depends
    /// on heap allocation order. Bare function-shaped operands never
    /// reach this helper: they fall to `compare()`'s catch-all arm and
    /// keep its "cannot compare Fn and Fn" wording.
    fn ordering_with_fn_gate(a: &Value, b: &Value) -> Result<std::cmp::Ordering, VmError> {
        if a.contains_fn() || b.contains_fn() {
            return Err(VmError::type_confusion(
                "type 'Fn' does not implement Compare",
            ));
        }
        Ok(a.cmp(b))
    }

    // ── Type compatibility ────────────────────────────────────────

    /// Check that two values are of one kind, as the operands of `==`
    /// and `!=` are.
    pub(super) fn check_same_type(&self, a: &Value, b: &Value) -> Result<(), VmError> {
        if std::mem::discriminant(a) != std::mem::discriminant(b) {
            return Err(VmError::type_confusion(format!(
                "unsupported operation: cannot compare {} and {}",
                a.kind(),
                b.kind()
            )));
        }
        Ok(())
    }
}
