//! Numeric builtin functions (`int.*`, `float.*`, `math.*`).

use crate::value::Value;
use crate::vm::VmError;

/// Dispatch `int.<name>(args)`.
pub fn call_int(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 {
                return Err(VmError::new("int.parse takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("int.parse requires a string".into()));
            };
            match s.trim().parse::<i64>() {
                Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Int(n)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "abs" => {
            if args.len() != 1 {
                return Err(VmError::new("int.abs takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("int.abs requires an int".into()));
            };
            match n.checked_abs() {
                Some(v) => Ok(Value::Int(v)),
                None => Err(VmError::new(format!("integer overflow: abs({n})"))),
            }
        }
        "min" => {
            if args.len() != 2 {
                return Err(VmError::new("int.min takes 2 arguments".into()));
            }
            let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("int.min requires ints".into()));
            };
            Ok(Value::Int(*a.min(b)))
        }
        "max" => {
            if args.len() != 2 {
                return Err(VmError::new("int.max takes 2 arguments".into()));
            }
            let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("int.max requires ints".into()));
            };
            Ok(Value::Int(*a.max(b)))
        }
        "clamp" => {
            if args.len() != 3 {
                return Err(VmError::new("int.clamp takes 3 arguments".into()));
            }
            let (Value::Int(x), Value::Int(lo), Value::Int(hi)) = (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new("int.clamp requires ints".into()));
            };
            if lo > hi {
                return Err(VmError::new(format!(
                    "int.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(Value::Int((*x).clamp(*lo, *hi)))
        }
        "to_float" => {
            if args.len() != 1 {
                return Err(VmError::new("int.to_float takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("int.to_float requires an int".into()));
            };
            Ok(Value::Float(*n as f64))
        }
        "to_string" => {
            if args.len() != 1 {
                return Err(VmError::new("int.to_string takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("int.to_string requires an int".into()));
            };
            Ok(Value::String(n.to_string()))
        }
        _ => Err(VmError::new(format!("unknown int function: {name}"))),
    }
}

/// Extract an f64 from a Float, ExtFloat, or Int value.
fn extract_float(val: &Value, fn_name: &str) -> Result<f64, VmError> {
    match val {
        Value::Float(f) => Ok(*f),
        Value::ExtFloat(f) => Ok(*f),
        Value::Int(n) => Ok(*n as f64),
        _ => Err(VmError::new(format!("{fn_name} requires a number"))),
    }
}

/// Dispatch `float.<name>(args)`.
pub fn call_float(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 {
                return Err(VmError::new("float.parse takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("float.parse requires a string".into()));
            };
            match s.trim().parse::<f64>() {
                Ok(n) if n.is_nan() || n.is_infinite() => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String("parsed value is not a finite number".into())],
                )),
                Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Float(n)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
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
                    Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
                }
                Value::ExtFloat(f) => Ok(Value::ExtFloat(f.round())),
                _ => Err(VmError::new("float.round requires a float".into())),
            }
        }
        "ceil" => {
            if args.len() != 1 {
                return Err(VmError::new("float.ceil takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.ceil();
                    Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
                }
                Value::ExtFloat(f) => Ok(Value::ExtFloat(f.ceil())),
                _ => Err(VmError::new("float.ceil requires a float".into())),
            }
        }
        "floor" => {
            if args.len() != 1 {
                return Err(VmError::new("float.floor takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.floor();
                    Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
                }
                Value::ExtFloat(f) => Ok(Value::ExtFloat(f.floor())),
                _ => Err(VmError::new("float.floor requires a float".into())),
            }
        }
        "abs" => {
            if args.len() != 1 {
                return Err(VmError::new("float.abs takes 1 argument".into()));
            }
            match &args[0] {
                Value::Float(f) => {
                    let result = f.abs();
                    Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
                }
                Value::ExtFloat(f) => Ok(Value::ExtFloat(f.abs())),
                _ => Err(VmError::new("float.abs requires a float".into())),
            }
        }
        "to_string" => {
            // Accepts (Float) or (Float, Int). The documented 2-arg form
            // formats with a fixed number of decimal places; the 1-arg form
            // uses the shortest round-trippable representation (Rust's
            // default `Display` for `f64`). The 1-arg form exists because
            // the typechecker tolerates arity ±1 for module-qualified calls
            // via `FieldAccess` and some call sites rely on that, so the
            // runtime mirrors that tolerance rather than erroring.
            if args.is_empty() || args.len() > 2 {
                return Err(VmError::new(
                    "float.to_string takes 1 or 2 arguments".into(),
                ));
            }
            let f = match &args[0] {
                Value::Float(f) => *f,
                Value::ExtFloat(f) => *f,
                _ => return Err(VmError::new("float.to_string requires a float".into())),
            };
            if args.len() == 1 {
                // Shortest round-trippable representation. Force a decimal
                // point for whole-number floats so the result always parses
                // as a float (e.g. `3.0` instead of `3`).
                let s = if f.is_finite() && f.fract() == 0.0 && !f.is_nan() {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                };
                return Ok(Value::String(s));
            }
            let Value::Int(decimals) = &args[1] else {
                return Err(VmError::new(
                    "float.to_string requires an int for decimals".into(),
                ));
            };
            if *decimals < 0 {
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
            let prec = u16::try_from(*decimals).map_err(|_| {
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
                Value::ExtFloat(f) => *f,
                _ => return Err(VmError::new("float.to_int requires a float".into())),
            };
            if f.is_nan() || f.is_infinite() {
                return Err(VmError::new(
                    "float.to_int: cannot convert non-finite float to int".into(),
                ));
            }
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
            let result = a.min(b);
            if result.is_finite() {
                Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
            } else {
                Ok(Value::ExtFloat(result))
            }
        }
        "max" => {
            if args.len() != 2 {
                return Err(VmError::new("float.max takes 2 arguments".into()));
            }
            let a = extract_float(&args[0], "float.max")?;
            let b = extract_float(&args[1], "float.max")?;
            let result = a.max(b);
            if result.is_finite() {
                Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
            } else {
                Ok(Value::ExtFloat(result))
            }
        }
        "clamp" => {
            // float.clamp(x, lo, hi) -> Float
            // Panics if lo > hi (matches Rust's `f64::clamp` behavior).
            // NaN inputs are not expected (Float is guaranteed finite in
            // the silt type system); if one sneaks through, behavior is
            // unspecified (we delegate to `f64::clamp`, which itself
            // panics on NaN bounds and propagates NaN for a NaN `x`).
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
            let result = x.clamp(lo, hi);
            if result.is_finite() {
                Ok(Value::Float(if result == 0.0 { 0.0 } else { result }))
            } else {
                // Shouldn't happen for well-typed Float inputs, but if a
                // NaN leaks in we surface it as ExtFloat rather than
                // silently materializing a non-finite `Float`.
                Ok(Value::ExtFloat(result))
            }
        }
        "is_finite" => {
            if args.len() != 1 {
                return Err(VmError::new("float.is_finite takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "float.is_finite")?;
            Ok(Value::Bool(f.is_finite()))
        }
        "is_infinite" => {
            if args.len() != 1 {
                return Err(VmError::new("float.is_infinite takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "float.is_infinite")?;
            Ok(Value::Bool(f.is_infinite()))
        }
        "is_nan" => {
            if args.len() != 1 {
                return Err(VmError::new("float.is_nan takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "float.is_nan")?;
            Ok(Value::Bool(f.is_nan()))
        }
        _ => Err(VmError::new(format!("unknown float function: {name}"))),
    }
}

/// Dispatch `math.<name>(args)`.
pub fn call_math(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "sqrt" => {
            if args.len() != 1 {
                return Err(VmError::new("math.sqrt takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.sqrt")?;
            Ok(Value::ExtFloat(f.sqrt()))
        }
        "pow" => {
            if args.len() != 2 {
                return Err(VmError::new("math.pow takes 2 arguments".into()));
            }
            let base = extract_float(&args[0], "math.pow")?;
            let exp = extract_float(&args[1], "math.pow")?;
            Ok(Value::ExtFloat(base.powf(exp)))
        }
        "log" => {
            if args.len() != 1 {
                return Err(VmError::new("math.log takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.log")?;
            Ok(Value::ExtFloat(f.ln()))
        }
        "log10" => {
            if args.len() != 1 {
                return Err(VmError::new("math.log10 takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.log10")?;
            Ok(Value::ExtFloat(f.log10()))
        }
        "sin" => {
            if args.len() != 1 {
                return Err(VmError::new("math.sin takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.sin")?;
            if matches!(&args[0], Value::ExtFloat(_)) {
                Ok(Value::ExtFloat(f.sin()))
            } else {
                Ok(Value::Float(f.sin()))
            }
        }
        "cos" => {
            if args.len() != 1 {
                return Err(VmError::new("math.cos takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.cos")?;
            if matches!(&args[0], Value::ExtFloat(_)) {
                Ok(Value::ExtFloat(f.cos()))
            } else {
                Ok(Value::Float(f.cos()))
            }
        }
        "tan" => {
            if args.len() != 1 {
                return Err(VmError::new("math.tan takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.tan")?;
            if matches!(&args[0], Value::ExtFloat(_)) {
                Ok(Value::ExtFloat(f.tan()))
            } else {
                Ok(Value::Float(f.tan()))
            }
        }
        "asin" => {
            if args.len() != 1 {
                return Err(VmError::new("math.asin takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.asin")?;
            Ok(Value::ExtFloat(f.asin()))
        }
        "acos" => {
            if args.len() != 1 {
                return Err(VmError::new("math.acos takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.acos")?;
            Ok(Value::ExtFloat(f.acos()))
        }
        "atan" => {
            if args.len() != 1 {
                return Err(VmError::new("math.atan takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.atan")?;
            if matches!(&args[0], Value::ExtFloat(_)) {
                Ok(Value::ExtFloat(f.atan()))
            } else {
                Ok(Value::Float(f.atan()))
            }
        }
        "atan2" => {
            if args.len() != 2 {
                return Err(VmError::new("math.atan2 takes 2 arguments".into()));
            }
            let y = extract_float(&args[0], "math.atan2")?;
            let x = extract_float(&args[1], "math.atan2")?;
            if matches!(&args[0], Value::ExtFloat(_)) || matches!(&args[1], Value::ExtFloat(_)) {
                Ok(Value::ExtFloat(y.atan2(x)))
            } else {
                Ok(Value::Float(y.atan2(x)))
            }
        }
        "exp" => {
            if args.len() != 1 {
                return Err(VmError::new("math.exp takes 1 argument".into()));
            }
            let f = extract_float(&args[0], "math.exp")?;
            Ok(Value::ExtFloat(f.exp()))
        }
        "random" => {
            if !args.is_empty() {
                return Err(VmError::new("math.random takes 0 arguments".into()));
            }
            use std::cell::Cell;
            use std::time::SystemTime;
            thread_local! {
                static RNG_STATE: Cell<u64> = Cell::new({
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0x12345678_9abcdef0)
                });
            }
            let val = RNG_STATE.with(|state| {
                let mut s = state.get();
                // xorshift64
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                state.set(s);
                // Convert to [0.0, 1.0)
                (s >> 11) as f64 / ((1u64 << 53) as f64)
            });
            Ok(Value::Float(val))
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
    fn float_to_int_rejects_nan_and_infinity() {
        let err = to_int(f64::NAN).expect_err("NaN should error");
        assert!(err.message.contains("non-finite"), "got: {}", err.message);
        let err = to_int(f64::INFINITY).expect_err("+inf should error");
        assert!(err.message.contains("non-finite"), "got: {}", err.message);
        let err = to_int(f64::NEG_INFINITY).expect_err("-inf should error");
        assert!(err.message.contains("non-finite"), "got: {}", err.message);
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
