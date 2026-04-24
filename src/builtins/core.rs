//! Core builtin functions (`result.*`, `option.*`, `test.*`).

use crate::value::Value;
use crate::vm::{Vm, VmError};

/// Shape of a two-variant ADT (Result or Option) for the dedup helpers
/// below. `ok_tag` is the "present/success" variant (carries one field);
/// `err_tag` is the "absent/failure" variant (0 or 1 fields depending
/// on the ADT).
struct AdtShape {
    module: &'static str,   // "result" | "option" — for error messages
    adt_name: &'static str, // "Result" | "Option" — for error messages
    ok_tag: &'static str,   // "Ok"     | "Some"
    err_tag: &'static str,  // "Err"    | "None"
}

const RESULT_SHAPE: AdtShape = AdtShape {
    module: "result",
    adt_name: "Result",
    ok_tag: "Ok",
    err_tag: "Err",
};

const OPTION_SHAPE: AdtShape = AdtShape {
    module: "option",
    adt_name: "Option",
    ok_tag: "Some",
    err_tag: "None",
};

/// Dispatch the shared two-variant ADT operations (`unwrap_or`, `is_ok`/
/// `is_some`, `is_err`/`is_none`, `map_ok`/`map`, `flat_map`). Returns
/// `Ok(Some(value))` if the name matched and we produced a value,
/// `Ok(None)` if the name was not one of the shared ops (caller should
/// try module-specific ops), or `Err(VmError)` for arity/type errors.
///
/// Why a helper: `call_result` and `call_option` previously duplicated
/// ~40 lines of arm template per shared op. Collapsing to one helper
/// honors silt's "one way to do things" principle (see MEMORY.md).
fn dispatch_shared_adt_op(
    vm: &mut Vm,
    shape: &AdtShape,
    name: &str,
    args: &[Value],
    is_ok_name: &str,
    is_err_name: &str,
    map_name: &str,
) -> Result<Option<Value>, VmError> {
    let module = shape.module;
    let adt_name = shape.adt_name;
    let ok_tag = shape.ok_tag;
    let err_tag = shape.err_tag;
    if name == "unwrap_or" {
        if args.len() != 2 {
            return Err(VmError::new(format!(
                "{module}.unwrap_or takes 2 arguments"
            )));
        }
        return match &args[0] {
            Value::Variant(tag, fields) if tag == ok_tag && fields.len() == 1 => {
                Ok(Some(fields[0].clone()))
            }
            Value::Variant(tag, _) if tag == err_tag => Ok(Some(args[1].clone())),
            _ => Err(VmError::new(format!(
                "{module}.unwrap_or requires a{} {adt_name}",
                if adt_name == "Option" { "n" } else { "" }
            ))),
        };
    }
    if name == is_ok_name {
        if args.len() != 1 {
            return Err(VmError::new(format!(
                "{module}.{is_ok_name} takes 1 argument"
            )));
        }
        return Ok(Some(Value::Bool(
            matches!(&args[0], Value::Variant(tag, _) if tag == ok_tag),
        )));
    }
    if name == is_err_name {
        if args.len() != 1 {
            return Err(VmError::new(format!(
                "{module}.{is_err_name} takes 1 argument"
            )));
        }
        return Ok(Some(Value::Bool(
            matches!(&args[0], Value::Variant(tag, _) if tag == err_tag),
        )));
    }
    if name == map_name {
        if args.len() != 2 {
            return Err(VmError::new(format!(
                "{module}.{map_name} takes 2 arguments"
            )));
        }
        return match &args[0] {
            Value::Variant(tag, fields) if tag == ok_tag && fields.len() == 1 => {
                let new_val =
                    vm.invoke_callable_resumable(&args[1], &[fields[0].clone()], args)?;
                Ok(Some(Value::Variant(ok_tag.into(), vec![new_val])))
            }
            other @ Value::Variant(tag, _) if tag == err_tag => Ok(Some(other.clone())),
            _ => Err(VmError::new(format!(
                "{module}.{map_name} requires a{} {adt_name}",
                if adt_name == "Option" { "n" } else { "" }
            ))),
        };
    }
    if name == "flat_map" {
        if args.len() != 2 {
            return Err(VmError::new(format!(
                "{module}.flat_map takes 2 arguments"
            )));
        }
        return match &args[0] {
            Value::Variant(tag, fields) if tag == ok_tag && fields.len() == 1 => {
                let v = vm.invoke_callable_resumable(&args[1], &[fields[0].clone()], args)?;
                Ok(Some(v))
            }
            other @ Value::Variant(tag, _) if tag == err_tag => Ok(Some(other.clone())),
            _ => Err(VmError::new(format!(
                "{module}.flat_map requires a{} {adt_name}",
                if adt_name == "Option" { "n" } else { "" }
            ))),
        };
    }
    Ok(None)
}

/// Dispatch `result.<name>(args)`.
pub fn call_result(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    if let Some(v) =
        dispatch_shared_adt_op(vm, &RESULT_SHAPE, name, args, "is_ok", "is_err", "map_ok")?
    {
        return Ok(v);
    }
    match name {
        "map_err" => {
            if args.len() != 2 {
                return Err(VmError::new("result.map_err takes 2 arguments".into()));
            }
            match &args[0] {
                other @ Value::Variant(tag, _) if tag == "Ok" => Ok(other.clone()),
                Value::Variant(tag, fields) if tag == "Err" && fields.len() == 1 => {
                    let new_val =
                        vm.invoke_callable_resumable(&args[1], &[fields[0].clone()], args)?;
                    Ok(Value::Variant("Err".into(), vec![new_val]))
                }
                _ => Err(VmError::new("result.map_err requires a Result".into())),
            }
        }
        "flatten" => {
            if args.len() != 1 {
                return Err(VmError::new("result.flatten takes 1 argument".into()));
            }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                    match &fields[0] {
                        ok @ Value::Variant(inner_tag, _)
                            if inner_tag == "Ok" || inner_tag == "Err" =>
                        {
                            Ok(ok.clone())
                        }
                        _ => Ok(args[0].clone()),
                    }
                }
                other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                _ => Err(VmError::new("result.flatten requires a Result".into())),
            }
        }
        _ => Err(VmError::new(format!("unknown result function: {name}"))),
    }
}

/// Dispatch `option.<name>(args)`.
pub fn call_option(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    if let Some(v) =
        dispatch_shared_adt_op(vm, &OPTION_SHAPE, name, args, "is_some", "is_none", "map")?
    {
        return Ok(v);
    }
    match name {
        "to_result" => {
            if args.len() != 2 {
                return Err(VmError::new("option.to_result takes 2 arguments".into()));
            }
            match &args[0] {
                Value::Variant(tag, fields) if tag == "Some" && fields.len() == 1 => {
                    Ok(Value::Variant("Ok".into(), vec![fields[0].clone()]))
                }
                Value::Variant(tag, _) if tag == "None" => {
                    Ok(Value::Variant("Err".into(), vec![args[1].clone()]))
                }
                _ => Err(VmError::new("option.to_result requires an Option".into())),
            }
        }
        _ => Err(VmError::new(format!("unknown option function: {name}"))),
    }
}

/// Dispatch `test.<name>(args)`.
pub fn call_test(vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "assert" => {
            if args.is_empty() || args.len() > 2 {
                return Err(VmError::new("test.assert takes 1-2 arguments".into()));
            }
            if vm.is_truthy(&args[0]) {
                Ok(Value::Unit)
            } else {
                let msg = if args.len() == 2 {
                    format!("assertion failed: {}", args[1])
                } else {
                    format!("assertion failed: {:?}", args[0])
                };
                Err(VmError::new(msg))
            }
        }
        "assert_eq" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(VmError::new("test.assert_eq takes 2-3 arguments".into()));
            }
            if args[0] == args[1] {
                Ok(Value::Unit)
            } else {
                let msg = if args.len() == 3 {
                    format!(
                        "assertion failed: {}: {:?} != {:?}",
                        args[2], args[0], args[1]
                    )
                } else {
                    format!("assertion failed: {:?} != {:?}", args[0], args[1])
                };
                Err(VmError::new(msg))
            }
        }
        "assert_ne" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(VmError::new("test.assert_ne takes 2-3 arguments".into()));
            }
            if args[0] != args[1] {
                Ok(Value::Unit)
            } else {
                let msg = if args.len() == 3 {
                    format!(
                        "assertion failed: {}: {:?} == {:?}",
                        args[2], args[0], args[1]
                    )
                } else {
                    format!("assertion failed: {:?} == {:?}", args[0], args[1])
                };
                Err(VmError::new(msg))
            }
        }
        _ => Err(VmError::new(format!("unknown test function: {name}"))),
    }
}
