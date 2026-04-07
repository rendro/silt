//! Core builtin functions (`result.*`, `option.*`, `test.*`).

use crate::value::Value;
use crate::vm::{Vm, VmError};

/// Dispatch `result.<name>(args)`.
pub fn call_result(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "unwrap_or" => {
            if args.len() != 2 { return Err(VmError::new("result.unwrap_or takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Ok" => Ok(fields[0].clone()),
                Value::Variant(tag, _) if tag == "Err" => Ok(args[1].clone()),
                _ => Err(VmError::new("result.unwrap_or requires a Result".into())),
            }
        }
        "is_ok" => {
            if args.len() != 1 { return Err(VmError::new("result.is_ok takes 1 argument".into())); }
            Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Ok")))
        }
        "is_err" => {
            if args.len() != 1 { return Err(VmError::new("result.is_err takes 1 argument".into())); }
            Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Err")))
        }
        "map_ok" => {
            if args.len() != 2 { return Err(VmError::new("result.map_ok takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                    let new_val = vm.invoke_callable(&args[1], &[fields[0].clone()])?;
                    Ok(Value::Variant("Ok".into(), vec![new_val]))
                }
                other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                _ => Err(VmError::new("result.map_ok requires a Result".into())),
            }
        }
        "map_err" => {
            if args.len() != 2 { return Err(VmError::new("result.map_err takes 2 arguments".into())); }
            match &args[0] {
                other @ Value::Variant(tag, _) if tag == "Ok" => Ok(other.clone()),
                Value::Variant(tag, fields) if tag == "Err" && fields.len() == 1 => {
                    let new_val = vm.invoke_callable(&args[1], &[fields[0].clone()])?;
                    Ok(Value::Variant("Err".into(), vec![new_val]))
                }
                _ => Err(VmError::new("result.map_err requires a Result".into())),
            }
        }
        "flatten" => {
            if args.len() != 1 { return Err(VmError::new("result.flatten takes 1 argument".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                    match &fields[0] {
                        ok @ Value::Variant(inner_tag, _) if inner_tag == "Ok" || inner_tag == "Err" => Ok(ok.clone()),
                        _ => Ok(args[0].clone()),
                    }
                }
                other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                _ => Err(VmError::new("result.flatten requires a Result".into())),
            }
        }
        "flat_map" => {
            if args.len() != 2 { return Err(VmError::new("result.flat_map takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                    vm.invoke_callable(&args[1], &[fields[0].clone()])
                }
                other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                _ => Err(VmError::new("result.flat_map requires a Result".into())),
            }
        }
        _ => Err(VmError::new(format!("unknown result function: {name}"))),
    }
}

/// Dispatch `option.<name>(args)`.
pub fn call_option(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "unwrap_or" => {
            if args.len() != 2 { return Err(VmError::new("option.unwrap_or takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Some" => Ok(fields[0].clone()),
                Value::Variant(tag, _) if tag == "None" => Ok(args[1].clone()),
                _ => Err(VmError::new("option.unwrap_or requires an Option".into())),
            }
        }
        "is_some" => {
            if args.len() != 1 { return Err(VmError::new("option.is_some takes 1 argument".into())); }
            Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Some")))
        }
        "is_none" => {
            if args.len() != 1 { return Err(VmError::new("option.is_none takes 1 argument".into())); }
            Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "None")))
        }
        "to_result" => {
            if args.len() != 2 { return Err(VmError::new("option.to_result takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Some" => Ok(Value::Variant("Ok".into(), vec![fields[0].clone()])),
                Value::Variant(tag, _) if tag == "None" => Ok(Value::Variant("Err".into(), vec![args[1].clone()])),
                _ => Err(VmError::new("option.to_result requires an Option".into())),
            }
        }
        "map" => {
            if args.len() != 2 { return Err(VmError::new("option.map takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Some" && fields.len() == 1 => {
                    let new_val = vm.invoke_callable(&args[1], &[fields[0].clone()])?;
                    Ok(Value::Variant("Some".into(), vec![new_val]))
                }
                other @ Value::Variant(tag, _) if tag == "None" => Ok(other.clone()),
                _ => Err(VmError::new("option.map requires an Option".into())),
            }
        }
        "flat_map" => {
            if args.len() != 2 { return Err(VmError::new("option.flat_map takes 2 arguments".into())); }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Some" && fields.len() == 1 => {
                    vm.invoke_callable(&args[1], &[fields[0].clone()])
                }
                other @ Value::Variant(tag, _) if tag == "None" => Ok(other.clone()),
                _ => Err(VmError::new("option.flat_map requires an Option".into())),
            }
        }
        _ => Err(VmError::new(format!("unknown option function: {name}"))),
    }
}

/// Dispatch `test.<name>(args)`.
pub fn call_test(vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "assert" => {
            if args.is_empty() || args.len() > 2 { return Err(VmError::new("test.assert takes 1-2 arguments".into())); }
            if vm.is_truthy(&args[0]) { Ok(Value::Unit) }
            else {
                let msg = if args.len() == 2 { format!("assertion failed: {}", args[1]) }
                else { format!("assertion failed: {:?}", args[0]) };
                Err(VmError::new(msg))
            }
        }
        "assert_eq" => {
            if args.len() < 2 || args.len() > 3 { return Err(VmError::new("test.assert_eq takes 2-3 arguments".into())); }
            if args[0] == args[1] { Ok(Value::Unit) }
            else {
                let msg = if args.len() == 3 { format!("assertion failed: {}: {:?} != {:?}", args[2], args[0], args[1]) }
                else { format!("assertion failed: {:?} != {:?}", args[0], args[1]) };
                Err(VmError::new(msg))
            }
        }
        "assert_ne" => {
            if args.len() < 2 || args.len() > 3 { return Err(VmError::new("test.assert_ne takes 2-3 arguments".into())); }
            if args[0] != args[1] { Ok(Value::Unit) }
            else {
                let msg = if args.len() == 3 { format!("assertion failed: {}: {:?} == {:?}", args[2], args[0], args[1]) }
                else { format!("assertion failed: {:?} == {:?}", args[0], args[1]) };
                Err(VmError::new(msg))
            }
        }
        _ => Err(VmError::new(format!("unknown test function: {name}"))),
    }
}
