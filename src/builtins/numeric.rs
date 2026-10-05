//! Numeric builtin functions (`int.*`, `float.*`, `math.*`).

use super::common::{require_int, require_string, value_kind};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::Vm;
use crate::vm::VmError;

/// Locate the byte offset of the first character in `s` that could not
/// plausibly be part of a numeric literal (base-10 int or decimal float).
/// Used to build `ParseInvalidDigit(offset)` variants when Rust's
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

/// Classify a `ParseIntError` into the matching `ParseError` variant.
/// `s` is the original input, used to recover an offset for
/// `InvalidDigit` since `IntErrorKind::InvalidDigit` doesn't expose one.
fn classify_int_parse_error(err: &std::num::ParseIntError, s: &str) -> Value {
    use std::num::IntErrorKind;
    match err.kind() {
        IntErrorKind::Empty => Value::variant(bv::PARSE_EMPTY, vec![]),
        IntErrorKind::PosOverflow => Value::variant(bv::PARSE_OVERFLOW, vec![]),
        IntErrorKind::NegOverflow => Value::variant(bv::PARSE_UNDERFLOW, vec![]),
        // InvalidDigit, Zero, and any future-added kinds fall through
        // to `ParseInvalidDigit(offset)`: it's the only variant that
        // carries data, so it doubles as the "anything else" sink.
        // std doesn't promise IntErrorKind stays exhaustive, so the
        // `_` arm is not dead code even today.
        _ => {
            let offset = find_first_invalid_digit(s.trim(), false) as i64;
            Value::variant(bv::PARSE_INVALID_DIGIT, vec![Value::Int(offset)])
        }
    }
}

/// Classify a `ParseFloatError` into the matching `ParseError` variant.
/// `ParseFloatError` doesn't expose a stable discriminant, so we
/// fall back to rescanning the input: empty → `ParseEmpty`, otherwise
/// `ParseInvalidDigit(offset)`. Overflow/underflow aren't distinguishable
/// at this layer — `f64::from_str` silently saturates to `±inf`, and
/// `float.parse` rejects non-finite results upstream.
fn classify_float_parse_error(_err: &std::num::ParseFloatError, s: &str) -> Value {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Value::variant(bv::PARSE_EMPTY, vec![]);
    }
    // A string that's only a sign has no digits → treat as empty.
    if trimmed == "+" || trimmed == "-" {
        return Value::variant(bv::PARSE_EMPTY, vec![]);
    }
    let offset = find_first_invalid_digit(trimmed, true) as i64;
    Value::variant(bv::PARSE_INVALID_DIGIT, vec![Value::Int(offset)])
}

/// Dispatch the builtin `trait Error for ParseError` method table.
/// Routed through `dispatch_builtin`'s "ParseError" module arm, mirroring
/// `call_io_error_trait`. Scaffolding lives in
/// `super::dispatch_error_trait`; this site just supplies the
/// variant → message rendering.
pub fn call_parse_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("ParseError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("ParseEmpty", []) => "cannot parse empty string".to_string(),
            ("ParseInvalidDigit", [Value::Int(offset)]) => {
                format!("invalid digit at byte {offset}")
            }
            ("ParseOverflow", []) => "number too large".to_string(),
            ("ParseUnderflow", []) => "number too small".to_string(),
            _ => return None,
        })
    })
}

/// Dispatch `int.<name>(args)`.
pub fn call_int(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 {
                return Err(VmError::new("int.parse takes 1 argument".into()));
            }
            let s = require_string(&args[0], "int.parse")?;
            match s.trim().parse::<i64>() {
                Ok(n) => Ok(Value::variant(bv::OK, vec![Value::Int(n)])),
                Err(e) => Ok(Value::variant(
                    bv::ERR,
                    vec![classify_int_parse_error(&e, &s)],
                )),
            }
        }
        "abs" => {
            if args.len() != 1 {
                return Err(VmError::new("int.abs takes 1 argument".into()));
            }
            let n = require_int(&args[0], "int.abs")?;
            match n.checked_abs() {
                Some(v) => Ok(Value::Int(v)),
                None => Err(VmError::new(format!("integer overflow: abs({n})"))),
            }
        }
        "min" => {
            if args.len() != 2 {
                return Err(VmError::new("int.min takes 2 arguments".into()));
            }
            let a = require_int(&args[0], "int.min")?;
            let b = require_int(&args[1], "int.min")?;
            Ok(Value::Int(a.min(b)))
        }
        "max" => {
            if args.len() != 2 {
                return Err(VmError::new("int.max takes 2 arguments".into()));
            }
            let a = require_int(&args[0], "int.max")?;
            let b = require_int(&args[1], "int.max")?;
            Ok(Value::Int(a.max(b)))
        }
        "clamp" => {
            if args.len() != 3 {
                return Err(VmError::new("int.clamp takes 3 arguments".into()));
            }
            let x = require_int(&args[0], "int.clamp")?;
            let lo = require_int(&args[1], "int.clamp")?;
            let hi = require_int(&args[2], "int.clamp")?;
            if lo > hi {
                return Err(VmError::new(format!(
                    "int.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(Value::Int(x.clamp(lo, hi)))
        }
        "to_float" => {
            if args.len() != 1 {
                return Err(VmError::new("int.to_float takes 1 argument".into()));
            }
            let n = require_int(&args[0], "int.to_float")?;
            Ok(float_value(n as f64))
        }
        "to_string" => {
            if args.len() != 1 {
                return Err(VmError::new("int.to_string takes 1 argument".into()));
            }
            let n = require_int(&args[0], "int.to_string")?;
            Ok(Value::String(n.to_string()))
        }
        _ => Err(VmError::new(format!("unknown int function: {name}"))),
    }
}

/// Extract an f64 from a Float or Int value.
fn extract_float(val: &Value, fn_name: &str) -> Result<f64, VmError> {
    match val {
        Value::Float(f) => Ok(*f),
        Value::Int(n) => Ok(*n as f64),
        other => Err(VmError::new(format!(
            "{fn_name} requires a number, got {}",
            value_kind(other)
        ))),
    }
}

/// Build a `Float` from a finite `f64`. `-0.0` becomes `0.0`: every
/// `Float` comparison (equality, ordering, hashing, sets, maps) treats
/// the two zeros as one value, so no `Float` may carry a negative zero
/// that would print or format differently. Every `Float` this module
/// produces goes through here.
pub(crate) fn float_value(f: f64) -> Value {
    Value::Float(if f == 0.0 { 0.0 } else { f })
}

/// Build a `Float` from the result of a float operation, raising the
/// error `msg` names when the result is NaN or infinite: a `Float` is
/// always finite, so an operation with no finite result fails the way
/// integer overflow does.
pub(crate) fn checked_float(f: f64, msg: impl FnOnce() -> String) -> Result<Value, VmError> {
    if f.is_finite() {
        Ok(float_value(f))
    } else {
        Err(VmError::new(msg()))
    }
}

/// Dispatch `float.<name>(args)`.
pub fn call_float(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 {
                return Err(VmError::new("float.parse takes 1 argument".into()));
            }
            let s = require_string(&args[0], "float.parse")?;
            match s.trim().parse::<f64>() {
                // Rust also parses `"inf"`, `"NaN"` and out-of-range
                // literals such as `"1e400"` (to ±infinity). None of them
                // is a `Float`: a spelled-out infinity or NaN is not a
                // number silt reads, and an out-of-range literal
                // overflows the way `int.parse` does (`ParseUnderflow`
                // below the range, `ParseOverflow` above it).
                Ok(n) if !n.is_finite() => {
                    let spelled = s.trim().trim_start_matches(['+', '-']);
                    let err = if !spelled.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
                        Value::variant(bv::PARSE_INVALID_DIGIT, vec![Value::Int(0)])
                    } else if n < 0.0 {
                        Value::variant(bv::PARSE_UNDERFLOW, vec![])
                    } else {
                        Value::variant(bv::PARSE_OVERFLOW, vec![])
                    };
                    Ok(Value::variant(bv::ERR, vec![err]))
                }
                Ok(n) => Ok(Value::variant(bv::OK, vec![float_value(n)])),
                Err(e) => Ok(Value::variant(
                    bv::ERR,
                    vec![classify_float_parse_error(&e, &s)],
                )),
            }
        }
        "round" => {
            if args.len() != 1 {
                return Err(VmError::new("float.round takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.round();
                    Ok(float_value(result))
                }
                other => Err(VmError::new(format!(
                    "float.round requires Float, got {}",
                    value_kind(other)
                ))),
            }
        }
        "ceil" => {
            if args.len() != 1 {
                return Err(VmError::new("float.ceil takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.ceil();
                    Ok(float_value(result))
                }
                other => Err(VmError::new(format!(
                    "float.ceil requires Float, got {}",
                    value_kind(other)
                ))),
            }
        }
        "floor" => {
            if args.len() != 1 {
                return Err(VmError::new("float.floor takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.floor();
                    Ok(float_value(result))
                }
                other => Err(VmError::new(format!(
                    "float.floor requires Float, got {}",
                    value_kind(other)
                ))),
            }
        }
        "abs" => {
            if args.len() != 1 {
                return Err(VmError::new("float.abs takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.abs();
                    Ok(float_value(result))
                }
                other => Err(VmError::new(format!(
                    "float.abs requires Float, got {}",
                    value_kind(other)
                ))),
            }
        }
        "to_string" => {
            // Accepts (Float) or (Float, Int). The documented 2-arg form
            // formats with a fixed number of decimal places; the 1-arg form
            // uses the shortest round-trippable representation (Rust's
            // default `Display` for `f64`). The typechecker signature
            // declares the `decimals` parameter optional
            // (`with_optional_last_param` in
            // `typechecker/builtins/float.rs`), so both forms reach here.
            if args.is_empty() || args.len() > 2 {
                return Err(VmError::new(
                    "float.to_string takes 1 or 2 arguments".into(),
                ));
            }
            let f = match &args[0] {
                Value::Float(f) => *f,
                other => {
                    return Err(VmError::new(format!(
                        "float.to_string requires Float, got {}",
                        value_kind(other)
                    )));
                }
            };
            if args.len() == 1 {
                // Shortest round-trippable representation. Force a decimal
                // point for whole-number floats so the result always parses
                // as a float (e.g. `3.0` instead of `3`).
                let s = if f.fract() == 0.0 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                };
                return Ok(Value::String(s));
            }
            let decimals = require_int(&args[1], "float.to_string")?;
            if decimals < 0 {
                return Err(VmError::new(
                    "float.to_string: decimals must be non-negative".into(),
                ));
            }
            // Rust's `{:.prec$}` formatter backs precision with a u16 and
            // panics with "Formatting argument out of range" for any value
            // above `u16::MAX` (65535). `catch_builtin_panic` would turn
            // that panic into a VmError, but std's panic handler still
            // prints a noisy `thread 'main' panicked at ...` line to
            // stderr, and the surfaced message is opaque to silt users.
            // Reject out-of-range precision up front with a clean error.
            let prec = u16::try_from(decimals).map_err(|_| {
                VmError::new(format!(
                    "float.to_string: decimals {decimals} exceeds maximum precision of 65535"
                ))
            })?;
            Ok(Value::String(format!("{:.prec$}", f, prec = prec as usize)))
        }
        "to_int" => {
            if args.len() != 1 {
                return Err(VmError::new("float.to_int takes 1 argument".into()));
            }
            let f = match &args[0] {
                Value::Float(f) => *f,
                other => {
                    return Err(VmError::new(format!(
                        "float.to_int requires Float, got {}",
                        value_kind(other)
                    )));
                }
            };
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
            Ok(Value::Int(f as i64))
        }
        "min" => {
            if args.len() != 2 {
                return Err(VmError::new("float.min takes 2 arguments".into()));
            }
            let a = extract_float(&args[0], "float.min")?;
            let b = extract_float(&args[1], "float.min")?;
            Ok(float_value(a.min(b)))
        }
        "max" => {
            if args.len() != 2 {
                return Err(VmError::new("float.max takes 2 arguments".into()));
            }
            let a = extract_float(&args[0], "float.max")?;
            let b = extract_float(&args[1], "float.max")?;
            Ok(float_value(a.max(b)))
        }
        "clamp" => {
            // float.clamp(x, lo, hi) -> Float. Raises if lo > hi.
            if args.len() != 3 {
                return Err(VmError::new("float.clamp takes 3 arguments".into()));
            }
            let x = extract_float(&args[0], "float.clamp")?;
            let lo = extract_float(&args[1], "float.clamp")?;
            let hi = extract_float(&args[2], "float.clamp")?;
            if lo > hi {
                return Err(VmError::new(format!(
                    "float.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(float_value(x.clamp(lo, hi)))
        }
        _ => Err(VmError::new(format!("unknown float function: {name}"))),
    }
}

/// Dispatch `math.<name>(args)`.
pub fn call_math(vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "sqrt" => {
            if args.len() != 1 {
                return Err(VmError::new("math.sqrt takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.sqrt")?;
            if f < 0.0 {
                return Err(VmError::new(format!("math.sqrt of a negative number: {f}")));
            }
            Ok(float_value(f.sqrt()))
        }
        "pow" => {
            if args.len() != 2 {
                return Err(VmError::new("math.pow takes 2 arguments".into()));
            }
            let base = extract_float(&args[0], "math.pow")?;
            let exp = extract_float(&args[1], "math.pow")?;
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
        "log" => {
            if args.len() != 1 {
                return Err(VmError::new("math.log takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.log")?;
            if f <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log of a number that is not positive: {f}"
                )));
            }
            Ok(float_value(f.ln()))
        }
        "log10" => {
            if args.len() != 1 {
                return Err(VmError::new("math.log10 takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.log10")?;
            if f <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log10 of a number that is not positive: {f}"
                )));
            }
            Ok(float_value(f.log10()))
        }
        "sin" => {
            if args.len() != 1 {
                return Err(VmError::new("math.sin takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.sin")?;
            Ok(float_value(f.sin()))
        }
        "cos" => {
            if args.len() != 1 {
                return Err(VmError::new("math.cos takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.cos")?;
            Ok(float_value(f.cos()))
        }
        "tan" => {
            if args.len() != 1 {
                return Err(VmError::new("math.tan takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.tan")?;
            checked_float(f.tan(), || format!("math.tan overflow: {f}"))
        }
        "asin" => {
            if args.len() != 1 {
                return Err(VmError::new("math.asin takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.asin")?;
            if !(-1.0..=1.0).contains(&f) {
                return Err(VmError::new(format!(
                    "math.asin of a number outside -1..1: {f}"
                )));
            }
            Ok(float_value(f.asin()))
        }
        "acos" => {
            if args.len() != 1 {
                return Err(VmError::new("math.acos takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.acos")?;
            if !(-1.0..=1.0).contains(&f) {
                return Err(VmError::new(format!(
                    "math.acos of a number outside -1..1: {f}"
                )));
            }
            Ok(float_value(f.acos()))
        }
        "atan" => {
            if args.len() != 1 {
                return Err(VmError::new("math.atan takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.atan")?;
            Ok(float_value(f.atan()))
        }
        "atan2" => {
            if args.len() != 2 {
                return Err(VmError::new("math.atan2 takes 2 arguments".into()));
            }
            let y = extract_float(&args[0], "math.atan2")?;
            let x = extract_float(&args[1], "math.atan2")?;
            Ok(float_value(y.atan2(x)))
        }
        "exp" => {
            if args.len() != 1 {
                return Err(VmError::new("math.exp takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.exp")?;
            checked_float(f.exp(), || format!("math.exp overflow: {f}"))
        }
        "random" => {
            if !args.is_empty() {
                return Err(VmError::new("math.random takes 0 arguments".into()));
            }
            use std::cell::Cell;
            thread_local! {
                // 0 until the first call on the thread seeds it.
                static RNG_STATE: Cell<u64> = const { Cell::new(0) };
            }
            let val = RNG_STATE.with(|state| {
                let mut s = state.get();
                if s == 0 {
                    // Seeded from the host clock; xorshift64 must not
                    // be seeded with 0.
                    s = vm.runtime.io.now().as_nanos() as u64 | 1;
                }
                // xorshift64
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                state.set(s);
                // Convert to [0.0, 1.0)
                (s >> 11) as f64 / ((1u64 << 53) as f64)
            });
            Ok(float_value(val))
        }
        _ => Err(VmError::new(format!("unknown math function: {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_int(f: f64) -> Result<Value, VmError> {
        call_float("to_int", &[Value::Float(f)])
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
