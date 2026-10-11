//! Numeric builtin functions (`int.*`, `float.*`, `math.*`).

use super::typed::builtins;
use crate::typeinfo::bv;
use crate::value::{Float, Value};
use crate::vm::VmError;

/// Locate the byte offset of the first character in `s` that could not
/// plausibly be part of a numeric literal (base-10 int or decimal float).
/// Used to build `InvalidDigit(offset)` variants when Rust's
/// `ParseIntError` / `ParseFloatError` don't expose a native offset.
///
/// `allow_decimal` toggles float-only characters (`.`, `e`, `E`). The
/// scan tolerates a leading sign and underscore separators since Rust's
/// float parser allows neither — the float path re-validates — but the
/// helper is purposefully permissive so the returned offset is the
/// first byte the parser itself would have rejected, not a pre-filter
/// stricter than `parse::<f64>`. Falls through to `0` for strings that
/// look fully numeric; the caller is expected to treat `0` as a
/// "couldn't localize" sentinel.
fn find_first_invalid_digit(s: &str, allow_decimal: bool) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() {
        let b = bytes[i];
        let ok = b.is_ascii_digit()
            || b == b'_'
            || (allow_decimal && (b == b'.' || b == b'e' || b == b'E' || b == b'+' || b == b'-'));
        if !ok {
            return i;
        }
        i += 1;
    }
    0
}

/// Classify a `ParseIntError` into the matching `IntError` variant.
/// `s` is the original input, used to recover an offset for
/// `InvalidDigit` since `IntErrorKind::InvalidDigit` doesn't expose one.
fn classify_int_parse_error(err: &std::num::ParseIntError, s: &str) -> Value {
    use std::num::IntErrorKind;
    match err.kind() {
        IntErrorKind::Empty => Value::variant(bv::INT_EMPTY, vec![]),
        IntErrorKind::PosOverflow => Value::variant(bv::INT_OVERFLOW, vec![]),
        IntErrorKind::NegOverflow => Value::variant(bv::INT_UNDERFLOW, vec![]),
        // InvalidDigit, Zero, and any future-added kinds fall through
        // to `InvalidDigit(offset)`: std doesn't promise IntErrorKind
        // stays exhaustive, so the `_` arm is not dead code even today.
        _ => {
            let offset = find_first_invalid_digit(s.trim(), false) as i64;
            Value::variant(bv::INT_INVALID_DIGIT, vec![Value::Int(offset)])
        }
    }
}

/// Classify a `ParseFloatError` into the matching `FloatError` variant.
/// `ParseFloatError` doesn't expose a stable discriminant, so we
/// fall back to rescanning the input: empty → `Empty`, otherwise
/// `InvalidDigit(offset)`. (An out-of-range literal parses, to an
/// infinity: `float.parse` tells it apart itself.)
fn classify_float_parse_error(_err: &std::num::ParseFloatError, s: &str) -> Value {
    let trimmed = s.trim();
    // A string that's only a sign has no digits → treat as empty.
    if trimmed.is_empty() || trimmed == "+" || trimmed == "-" {
        return Value::variant(bv::FLOAT_EMPTY, vec![]);
    }
    let offset = find_first_invalid_digit(trimmed, true) as i64;
    Value::variant(bv::FLOAT_INVALID_DIGIT, vec![Value::Int(offset)])
}

/// What a variant of `IntError` or of `FloatError` reads as: the two
/// enums have the same variants.
fn parse_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Empty", []) => "cannot parse empty string".to_string(),
        ("InvalidDigit", [Value::Int(offset)]) => {
            format!("invalid digit at byte {offset}")
        }
        ("Overflow", []) => "number too large".to_string(),
        ("Underflow", []) => "number too small".to_string(),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

/// What `IntError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn int_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    parse_error_text(tag, fields)
}

/// What `FloatError`'s `message` says of the variant `tag` with
/// `fields`: `None` if they are no variant of it.
pub(crate) fn float_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    parse_error_text(tag, fields)
}

/// Build a `Float` from the result of a float operation, raising the
/// error `msg` names when the result is NaN or infinite: a `Float` is
/// always finite, so an operation with no finite result fails the way
/// integer overflow does.
pub(crate) fn checked_float(f: f64, msg: impl FnOnce() -> String) -> Result<Value, VmError> {
    Float::new(f)
        .map(Value::Float)
        .ok_or_else(|| VmError::new(msg()))
}

/// `int.*`
pub(crate) mod int {
    use super::*;

    builtins! {
        fn parse(s: &str) -> Value {
            match s.trim().parse::<i64>() {
                Ok(n) => Value::variant(bv::OK, vec![Value::Int(n)]),
                Err(e) => Value::variant(bv::ERR, vec![classify_int_parse_error(&e, s)]),
            }
        }

        fn abs(n: i64) -> Result<i64, VmError> {
            n.checked_abs()
                .ok_or_else(|| VmError::new(format!("integer overflow: abs({n})")))
        }

        fn min(a: i64, b: i64) -> i64 {
            a.min(b)
        }

        fn max(a: i64, b: i64) -> i64 {
            a.max(b)
        }

        fn clamp(x: i64, lo: i64, hi: i64) -> Result<i64, VmError> {
            if lo > hi {
                return Err(VmError::new(format!(
                    "int.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(x.clamp(lo, hi))
        }

        fn to_float(n: i64) -> f64 {
            n as f64
        }
    }
}

/// `float.*`
pub(crate) mod float {
    use super::*;

    builtins! {
        fn parse(s: &str) -> Value {
            match s.trim().parse::<f64>() {
                // Rust also parses `"inf"`, `"NaN"` and out-of-range
                // literals such as `"1e400"` (to ±infinity). None of them
                // is a `Float`: a spelled-out infinity or NaN is not a
                // number silt reads, and an out-of-range literal
                // overflows the way `int.parse` does (`Underflow` below
                // the range, `Overflow` above it).
                Ok(n) => match Float::new(n) {
                    Some(n) => Value::variant(bv::OK, vec![Value::Float(n)]),
                    None => {
                        let spelled = s.trim().trim_start_matches(['+', '-']);
                        let err = if !spelled.starts_with(|c: char| c.is_ascii_digit() || c == '.')
                        {
                            Value::variant(bv::FLOAT_INVALID_DIGIT, vec![Value::Int(0)])
                        } else if n < 0.0 {
                            Value::variant(bv::FLOAT_UNDERFLOW, vec![])
                        } else {
                            Value::variant(bv::FLOAT_OVERFLOW, vec![])
                        };
                        Value::variant(bv::ERR, vec![err])
                    }
                },
                Err(e) => Value::variant(bv::ERR, vec![classify_float_parse_error(&e, s)]),
            }
        }

        fn round(f: f64) -> f64 {
            f.round()
        }

        fn ceil(f: f64) -> f64 {
            f.ceil()
        }

        fn floor(f: f64) -> f64 {
            f.floor()
        }

        fn abs(f: f64) -> f64 {
            f.abs()
        }

        fn to_int(f: f64) -> Result<i64, VmError> {
            // B7 fix: `as i64` saturates for out-of-range finite floats,
            // silently clamping e.g. 1e20 to i64::MAX. Reject such values
            // explicitly so callers see a clear runtime error.
            //
            // The f64 representation of `i64::MIN` (-9223372036854775808) is
            // exact, so `f >= i64::MIN as f64` correctly accepts values
            // down to and including `i64::MIN`. `i64::MAX` is NOT exactly
            // representable (rounds up to 9223372036854775808.0), so we
            // compare strictly less than `(i64::MAX as f64) + 1.0` to
            // reject everything that would round to or past i64::MAX+1.
            const I64_MIN_AS_F64: f64 = i64::MIN as f64;
            const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact
            if !(I64_MIN_AS_F64..I64_MAX_PLUS_ONE).contains(&f) {
                return Err(VmError::new(format!(
                    "float.to_int: value out of i64 range: {f}"
                )));
            }
            Ok(f as i64)
        }

        fn min(a: f64, b: f64) -> f64 {
            a.min(b)
        }

        fn max(a: f64, b: f64) -> f64 {
            a.max(b)
        }

        fn clamp(x: f64, lo: f64, hi: f64) -> Result<f64, VmError> {
            if lo > hi {
                return Err(VmError::new(format!(
                    "float.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(x.clamp(lo, hi))
        }

        // Rust's `{:.prec$}` formatter backs precision with a u16 and
        // panics with "Formatting argument out of range" for any value
        // above `u16::MAX` (65535): such a precision is an error here.
        fn format(f: f64, decimals: i64) -> Result<String, VmError> {
            if decimals < 0 {
                return Err(VmError::new(
                    "float.format: decimals must be non-negative".into(),
                ));
            }
            let Ok(prec) = u16::try_from(decimals) else {
                return Err(VmError::new(format!(
                    "float.format: decimals {decimals} exceeds maximum precision of 65535"
                )));
            };
            Ok(format!("{:.prec$}", f, prec = prec as usize))
        }
    }
}

/// `math.*`
pub(crate) mod math {
    use super::*;

    builtins! {
        fn sqrt(f: f64) -> Result<f64, VmError> {
            if f < 0.0 {
                return Err(VmError::new(format!("math.sqrt of a negative number: {f}")));
            }
            Ok(f.sqrt())
        }

        fn pow(base: f64, exp: f64) -> Result<Value, VmError> {
            let result = base.powf(exp);
            if result.is_nan() {
                return Err(VmError::new(format!(
                    "math.pow of a negative number to a fractional power: {base} ^ {exp}"
                )));
            }
            if base == 0.0 && exp < 0.0 {
                return Err(VmError::new(format!(
                    "math.pow of zero to a negative power: {base} ^ {exp}"
                )));
            }
            checked_float(result, || format!("math.pow overflow: {base} ^ {exp}"))
        }

        fn log(f: f64) -> Result<f64, VmError> {
            if f <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log of a number that is not positive: {f}"
                )));
            }
            Ok(f.ln())
        }

        fn log10(f: f64) -> Result<f64, VmError> {
            if f <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log10 of a number that is not positive: {f}"
                )));
            }
            Ok(f.log10())
        }

        fn sin(f: f64) -> f64 {
            f.sin()
        }

        fn cos(f: f64) -> f64 {
            f.cos()
        }

        fn tan(f: f64) -> Result<Value, VmError> {
            checked_float(f.tan(), || format!("math.tan overflow: {f}"))
        }

        fn asin(f: f64) -> Result<f64, VmError> {
            if !(-1.0..=1.0).contains(&f) {
                return Err(VmError::new(format!(
                    "math.asin of a number outside -1..1: {f}"
                )));
            }
            Ok(f.asin())
        }

        fn acos(f: f64) -> Result<f64, VmError> {
            if !(-1.0..=1.0).contains(&f) {
                return Err(VmError::new(format!(
                    "math.acos of a number outside -1..1: {f}"
                )));
            }
            Ok(f.acos())
        }

        fn atan(f: f64) -> f64 {
            f.atan()
        }

        fn atan2(y: f64, x: f64) -> f64 {
            y.atan2(x)
        }

        fn exp(f: f64) -> Result<Value, VmError> {
            checked_float(f.exp(), || format!("math.exp overflow: {f}"))
        }

        fn random(vm) -> f64 {
            vm.runtime.random()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{Step, Vm};

    fn to_int(f: f64) -> Result<Value, VmError> {
        let mut vm = Vm::new(crate::vm::HostIo::process());
        let f = Value::Float(Float::new(f).expect("a finite number"));
        match float::to_int(&mut vm, &[f]).expect("a Float is the argument")? {
            Step::Done(value) => Ok(value),
            _ => panic!("float.to_int gives a value"),
        }
    }

    // ── float.to_int: B7 out-of-range regression ──────────────────

    #[test]
    fn float_to_int_rejects_positive_out_of_range() {
        let err = to_int(1.0e20).expect_err("expected out-of-range error");
        assert!(
            err.message.contains("out of i64 range"),
            "error should mention out-of-range, got: {}",
            err.message
        );
    }

    #[test]
    fn float_to_int_rejects_negative_out_of_range() {
        let err = to_int(-1.0e20).expect_err("expected out-of-range error");
        assert!(
            err.message.contains("out of i64 range"),
            "error should mention out-of-range, got: {}",
            err.message
        );
    }

    #[test]
    fn float_to_int_rejects_exactly_i64_max_plus_one() {
        // `i64::MAX + 1 == 9223372036854775808` is exactly representable in
        // f64 (it's 2^63) and is the first value strictly outside the i64
        // range. It must be rejected.
        let err = to_int(9_223_372_036_854_775_808.0)
            .expect_err("expected out-of-range for i64::MAX + 1");
        assert!(
            err.message.contains("out of i64 range"),
            "error should mention out-of-range, got: {}",
            err.message
        );
    }

    #[test]
    fn float_to_int_accepts_near_i64_max_after_rounding() {
        // f64 cannot exactly represent 9_223_372_036_854_775_000; it rounds
        // *down* to 9_223_372_036_854_774_784 (the nearest representable
        // value below i64::MAX), which IS within range and must convert
        // successfully. This pins the boundary so a future tightening of
        // the range check doesn't accidentally reject it.
        let v = to_int(9_223_372_036_854_775_000.0).expect("in-range after rounding");
        assert!(matches!(v, Value::Int(_)));
    }

    #[test]
    fn float_to_int_truncates_positive_fraction() {
        let v = to_int(42.5).expect("42.5 should convert");
        assert!(matches!(v, Value::Int(42)), "expected Int(42), got {v:?}");
    }

    #[test]
    fn float_to_int_truncates_negative_fraction() {
        let v = to_int(-42.5).expect("-42.5 should convert");
        assert!(matches!(v, Value::Int(-42)), "expected Int(-42), got {v:?}");
    }

    #[test]
    fn float_to_int_accepts_zero() {
        let v = to_int(0.0).expect("0.0 should convert");
        assert!(matches!(v, Value::Int(0)), "expected Int(0), got {v:?}");
        let v = to_int(-0.0).expect("-0.0 should convert");
        assert!(matches!(v, Value::Int(0)), "expected Int(0), got {v:?}");
    }

    #[test]
    fn float_to_int_accepts_i64_min_exact() {
        // i64::MIN is exactly representable as f64.
        let v = to_int(i64::MIN as f64).expect("i64::MIN as f64 should convert");
        assert!(
            matches!(v, Value::Int(n) if n == i64::MIN),
            "expected Int(i64::MIN), got {v:?}"
        );
    }
}
