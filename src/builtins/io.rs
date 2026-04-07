//! IO and filesystem builtin functions (`io.*`, `fs.*`).

use std::sync::Arc;

use crate::value::Value;
use crate::vm::{Vm, VmError};

/// Dispatch `io.<name>(args)`.
pub fn call(_vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "inspect" => {
            if args.len() != 1 {
                return Err(VmError::new("io.inspect takes 1 argument".into()));
            }
            Ok(Value::String(args[0].format_silt()))
        }
        "read_file" => {
            if args.len() != 1 {
                return Err(VmError::new("io.read_file takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("io.read_file requires a string path".into()));
            };
            match std::fs::read_to_string(path) {
                Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "write_file" => {
            if args.len() != 2 {
                return Err(VmError::new("io.write_file takes 2 arguments".into()));
            }
            let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "io.write_file requires string arguments".into(),
                ));
            };
            match std::fs::write(path, content) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "read_line" => {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(_) => Ok(Value::Variant(
                    "Ok".into(),
                    vec![Value::String(line.trim_end().to_string())],
                )),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "args" => {
            let args_list: Vec<Value> = std::env::args().map(Value::String).collect();
            Ok(Value::List(Arc::new(args_list)))
        }
        _ => Err(VmError::new(format!("unknown io function: {name}"))),
    }
}

/// Dispatch `fs.<name>(args)`.
pub fn call_fs(_vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "exists" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.exists takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("fs.exists requires a string path".into()));
            };
            Ok(Value::Bool(std::path::Path::new(path).exists()))
        }
        _ => Err(VmError::new(format!("unknown fs function: {name}"))),
    }
}
