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
            if args.len() != 2 {
                return Err(VmError::new("float.to_string takes 2 arguments".into()));
            }
            let f = match &args[0] {
                Value::Float(f) => *f,
                Value::ExtFloat(f) => *f,
                _ => return Err(VmError::new("float.to_string requires a float".into())),
            };
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
            Ok(Value::String(format!(
                "{:.prec$}",
                f,
                prec = *decimals as usize
            )))
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
