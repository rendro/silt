//! Arithmetic, comparison, and type-checking helpers for the VM.

use std::cmp::Ordering;

use crate::bytecode::Op;
use crate::value::{Float, Value};

use super::VmError;

// ── Arithmetic helpers ────────────────────────────────────────

/// `a op b` of two Ints, for the arithmetic instruction `op`: computed
/// in the caller, the instruction loop. (Operands that are not two
/// Ints are [`float_arithmetic`]'s, a call.)
#[inline(always)]
pub(super) fn int_arithmetic(op: Op, a: i64, b: i64) -> Result<i64, VmError> {
    let result = match op {
        Op::Add => a.checked_add(b),
        Op::Sub => a.checked_sub(b),
        Op::Mul => a.checked_mul(b),
        Op::Div if b == 0 => return Err(VmError::new("division by zero".to_string())),
        Op::Div => a.checked_div(b),
        Op::Mod if b == 0 => return Err(VmError::new("modulo by zero".to_string())),
        Op::Mod => a.checked_rem(b),
        _ => unreachable!(),
    };
    match result {
        Some(v) => Ok(v),
        None => Err(int_overflow(op, a, b)),
    }
}

/// How the arithmetic instruction `op` is written.
fn symbol(op: Op) -> &'static str {
    match op {
        Op::Add => "+",
        Op::Sub => "-",
        Op::Mul => "*",
        Op::Div => "/",
        Op::Mod => "%",
        _ => unreachable!(),
    }
}

#[cold]
fn int_overflow(op: Op, a: i64, b: i64) -> VmError {
    VmError::new(format!("integer overflow: {a} {} {b}", symbol(op)))
}

/// The error of `a op b` that is no Float: a NaN or an infinity.
#[cold]
fn float_overflow(op: Op, a: Float, b: Float) -> VmError {
    VmError::new(format!("float overflow: {a} {} {b}", symbol(op)))
}

/// `a op b` of operands that are not two Ints: two Floats give a
/// Float, anything else is refused.
pub(super) fn float_arithmetic(op: Op, a: &Value, b: &Value) -> Result<Float, VmError> {
    match (a, b) {
        (Value::Float(a), Value::Float(b)) => {
            let (x, y) = (a.get(), b.get());
            let result = match op {
                Op::Add => x + y,
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div if y == 0.0 => {
                    return Err(VmError::new("float division by zero".to_string()));
                }
                Op::Div => x / y,
                Op::Mod if y == 0.0 => return Err(VmError::new("modulo by zero".to_string())),
                Op::Mod => x % y,
                _ => unreachable!(),
            };
            // (The text of the error is made where there is one.)
            match Float::new(result) {
                Some(float) => Ok(float),
                None => Err(float_overflow(op, *a, *b)),
            }
        }
        _ => {
            let op_name = symbol(op);
            let a_type = a.kind();
            let b_type = b.kind();
            // Special error for Int/Float mixing
            if (a_type == "Int" && b_type == "Float") || (a_type == "Float" && b_type == "Int") {
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
            Err(VmError::type_confusion(format!(
                "cannot apply '{op_name}' to {a_type} and {b_type}{hint}",
            )))
        }
    }
}

/// How `a` and `b` are ordered, for the comparison instructions. Two
/// Ints are compared in the caller, the instruction loop; anything
/// else is a call.
#[inline(always)]
pub(super) fn ordering(a: &Value, b: &Value) -> Result<Ordering, VmError> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        _ => other_ordering(a, b),
    }
}

/// [`ordering`] of operands that are not two Ints.
fn other_ordering(a: &Value, b: &Value) -> Result<Ordering, VmError> {
    Ok(match (a, b) {
        (Value::Float(a), Value::Float(b)) => a.cmp(b),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        // Lists are ordered element by element (`Value::cmp`), after
        // the function-leaf gate (see `ordering_with_fn_gate` below).
        (Value::List(_), Value::List(_)) => ordering_with_fn_gate(a, b)?,
        // Records of one type are ordered by their fields.
        (Value::Record(ra), Value::Record(rb)) if ra.type_id() == rb.type_id() => {
            ordering_with_fn_gate(a, b)?
        }
        (Value::Variant(..), Value::Variant(..)) => ordering_with_fn_gate(a, b)?,
        // What has `Compare` is ordered: a tuple part by part, `false`
        // before `true`, `()` equal to itself.
        (Value::Tuple(_), Value::Tuple(_)) => ordering_with_fn_gate(a, b)?,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Unit, Value::Unit) => Ordering::Equal,
        // Two functions, of whatever kinds: to a program each is an
        // `Fn`.
        _ if a.is_fn() && b.is_fn() => ordering_with_fn_gate(a, b)?,
        _ => {
            return Err(VmError::type_confusion(format!(
                "unsupported operation: cannot compare {} and {}",
                a.kind(),
                b.kind()
            )));
        }
    })
}

/// Order two container-shaped operands element-wise via `Value::cmp`,
/// first rejecting any operand that transitively contains a
/// function-shaped leaf (`Value::contains_fn`, src/value/mod.rs).
///
/// The checker rejects ordering a value that holds a function
/// (`Compare` is decided by structure): this is the backstop at the
/// execution site. Without it `Value::cmp` would order the `VmClosure`
/// leaves of two functions by where the functions are (src/value/key.rs),
/// a result that depends on heap allocation order.
fn ordering_with_fn_gate(a: &Value, b: &Value) -> Result<Ordering, VmError> {
    if a.contains_fn() || b.contains_fn() {
        return Err(VmError::type_confusion(
            "type 'Fn' does not implement Compare",
        ));
    }
    Ok(a.cmp(b))
}

// ── Type compatibility ────────────────────────────────────────

/// Check that two values are of one kind, as the operands of `==`
/// and `!=` are. Two functions are of one kind, whatever made them
/// (a closure, a builtin, a host function, a constructor): to a
/// program each is an `Fn`, and the caller's gate says that an `Fn`
/// has no `Equal`.
pub(super) fn check_same_type(a: &Value, b: &Value) -> Result<(), VmError> {
    if std::mem::discriminant(a) != std::mem::discriminant(b) && !(a.is_fn() && b.is_fn()) {
        return Err(VmError::type_confusion(format!(
            "unsupported operation: cannot compare {} and {}",
            a.kind(),
            b.kind()
        )));
    }
    Ok(())
}
