//! Numeric builtin functions (`int.*`, `float.*`, `math.*`).

use crate::value::Value;
use crate::vm::VmError;

/// Dispatch `int.<name>(args)`.
pub fn call_int(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 { return Err(VmError::new("int.parse takes 1 argument".into())); }
            let Value::String(s) = &args[0] else { return Err(VmError::new("int.parse requires a string".into())); };
            match s.trim().parse::<i64>() {
                Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Int(n)])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        "abs" => {
            if args.len() != 1 { return Err(VmError::new("int.abs takes 1 argument".into())); }
            let Value::Int(n) = &args[0] else { return Err(VmError::new("int.abs requires an int".into())); };
            Ok(Value::Int(n.abs()))
        }
        "min" => {
            if args.len() != 2 { return Err(VmError::new("int.min takes 2 arguments".into())); }
            let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else { return Err(VmError::new("int.min requires ints".into())); };
            Ok(Value::Int(*a.min(b)))
        }
        "max" => {
            if args.len() != 2 { return Err(VmError::new("int.max takes 2 arguments".into())); }
            let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else { return Err(VmError::new("int.max requires ints".into())); };
            Ok(Value::Int(*a.max(b)))
        }
        "to_float" => {
            if args.len() != 1 { return Err(VmError::new("int.to_float takes 1 argument".into())); }
            let Value::Int(n) = &args[0] else { return Err(VmError::new("int.to_float requires an int".into())); };
            Ok(Value::Float(*n as f64))
        }
        "to_string" => {
            if args.len() != 1 { return Err(VmError::new("int.to_string takes 1 argument".into())); }
            let Value::Int(n) = &args[0] else { return Err(VmError::new("int.to_string requires an int".into())); };
            Ok(Value::String(n.to_string()))
        }
        _ => Err(VmError::new(format!("unknown int function: {name}"))),
    }
}

/// Dispatch `float.<name>(args)`.
pub fn call_float(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 1 { return Err(VmError::new("float.parse takes 1 argument".into())); }
            let Value::String(s) = &args[0] else { return Err(VmError::new("float.parse requires a string".into())); };
            match s.trim().parse::<f64>() {
                Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Float(n)])),
                Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
            }
        }
        "round" => {
            if args.len() != 1 { return Err(VmError::new("float.round takes 1 argument".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.round requires a float".into())); };
            Ok(Value::Float(f.round()))
        }
        "ceil" => {
            if args.len() != 1 { return Err(VmError::new("float.ceil takes 1 argument".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.ceil requires a float".into())); };
            Ok(Value::Float(f.ceil()))
        }
        "floor" => {
            if args.len() != 1 { return Err(VmError::new("float.floor takes 1 argument".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.floor requires a float".into())); };
            Ok(Value::Float(f.floor()))
        }
        "abs" => {
            if args.len() != 1 { return Err(VmError::new("float.abs takes 1 argument".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.abs requires a float".into())); };
            Ok(Value::Float(f.abs()))
        }
        "to_string" => {
            if args.len() != 2 { return Err(VmError::new("float.to_string takes 2 arguments".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.to_string requires a float".into())); };
            let Value::Int(decimals) = &args[1] else { return Err(VmError::new("float.to_string requires an int for decimals".into())); };
            Ok(Value::String(format!("{:.prec$}", f, prec = *decimals as usize)))
        }
        "to_int" => {
            if args.len() != 1 { return Err(VmError::new("float.to_int takes 1 argument".into())); }
            let Value::Float(f) = &args[0] else { return Err(VmError::new("float.to_int requires a float".into())); };
            Ok(Value::Int(*f as i64))
        }
        "min" => {
            if args.len() != 2 { return Err(VmError::new("float.min takes 2 arguments".into())); }
            let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else { return Err(VmError::new("float.min requires floats".into())); };
            Ok(Value::Float(a.min(*b)))
        }
        "max" => {
            if args.len() != 2 { return Err(VmError::new("float.max takes 2 arguments".into())); }
            let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else { return Err(VmError::new("float.max requires floats".into())); };
            Ok(Value::Float(a.max(*b)))
        }
        _ => Err(VmError::new(format!("unknown float function: {name}"))),
    }
}

/// Dispatch `math.<name>(args)`.
pub fn call_math(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "sqrt" => {
            if args.len() != 1 { return Err(VmError::new("math.sqrt takes 1 argument".into())); }
            let f = match &args[0] {
                Value::Float(f) => *f,
                Value::Int(n) => *n as f64,
                _ => return Err(VmError::new("math.sqrt requires a number".into())),
            };
            Ok(Value::Float(f.sqrt()))
        }
        "pow" => {
            if args.len() != 2 { return Err(VmError::new("math.pow takes 2 arguments".into())); }
            let base = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.pow requires numbers".into())) };
            let exp = match &args[1] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.pow requires numbers".into())) };
            Ok(Value::Float(base.powf(exp)))
        }
        "log" => {
            if args.len() != 1 { return Err(VmError::new("math.log takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.log requires a number".into())) };
            Ok(Value::Float(f.ln()))
        }
        "log10" => {
            if args.len() != 1 { return Err(VmError::new("math.log10 takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.log10 requires a number".into())) };
            Ok(Value::Float(f.log10()))
        }
        "sin" => {
            if args.len() != 1 { return Err(VmError::new("math.sin takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.sin requires a number".into())) };
            Ok(Value::Float(f.sin()))
        }
        "cos" => {
            if args.len() != 1 { return Err(VmError::new("math.cos takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.cos requires a number".into())) };
            Ok(Value::Float(f.cos()))
        }
        "tan" => {
            if args.len() != 1 { return Err(VmError::new("math.tan takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.tan requires a number".into())) };
            Ok(Value::Float(f.tan()))
        }
        "asin" => {
            if args.len() != 1 { return Err(VmError::new("math.asin takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.asin requires a number".into())) };
            Ok(Value::Float(f.asin()))
        }
        "acos" => {
            if args.len() != 1 { return Err(VmError::new("math.acos takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.acos requires a number".into())) };
            Ok(Value::Float(f.acos()))
        }
        "atan" => {
            if args.len() != 1 { return Err(VmError::new("math.atan takes 1 argument".into())); }
            let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan requires a number".into())) };
            Ok(Value::Float(f.atan()))
        }
        "atan2" => {
            if args.len() != 2 { return Err(VmError::new("math.atan2 takes 2 arguments".into())); }
            let y = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan2 requires numbers".into())) };
            let x = match &args[1] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan2 requires numbers".into())) };
            Ok(Value::Float(y.atan2(x)))
        }
        _ => Err(VmError::new(format!("unknown math function: {name}"))),
    }
}
