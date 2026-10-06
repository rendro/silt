//! Core builtin functions (`result.*`, `option.*`, `test.*`).

use crate::typeinfo::{BuiltinVariant, bv};
use crate::value::Value;
use crate::vm::{Step, Vm, VmError, call_then};

/// Shape of a two-variant ADT (Result or Option) for the dedup helpers
/// below. `ok_tag` is the "present/success" variant (carries one field);
/// `err_tag` is the "absent/failure" variant (0 or 1 fields depending
/// on the ADT).
struct AdtShape {
    module: &'static str,    // "result" | "option" — for error messages
    adt_name: &'static str,  // "Result" | "Option" — for error messages
    ok_tag: BuiltinVariant,  // Ok  | Some
    err_tag: BuiltinVariant, // Err | None
}

const RESULT_SHAPE: AdtShape = AdtShape {
    module: "result",
    adt_name: "Result",
    ok_tag: bv::OK,
    err_tag: bv::ERR,
};

const OPTION_SHAPE: AdtShape = AdtShape {
    module: "option",
    adt_name: "Option",
    ok_tag: bv::SOME,
    err_tag: bv::NONE,
};

/// Dispatch the shared two-variant ADT operations that call no function
/// (`unwrap_or`, `is_ok`/`is_some`, `is_err`/`is_none`). Returns
/// `Ok(Some(value))` if the name matched and we produced a value,
/// `Ok(None)` if the name was not one of the shared ops (caller should
/// try module-specific ops), or `Err(VmError)` for arity/type errors.
///
/// Why a helper: `call_result` and `call_option` previously duplicated
/// ~40 lines of arm template per shared op. Collapsing to one helper
/// honors silt's "one way to do things" principle (see MEMORY.md).
fn dispatch_shared_adt_op(
    shape: &AdtShape,
    name: &str,
    args: &[Value],
    is_ok_name: &str,
    is_err_name: &str,
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
            Value::Variant(tag, fields) if tag.is(ok_tag) && fields.len() == 1 => {
                Ok(Some(fields[0].clone()))
            }
            Value::Variant(tag, _) if tag.is(err_tag) => Ok(Some(args[1].clone())),
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
            matches!(&args[0], Value::Variant(tag, _) if tag.is(ok_tag)),
        )));
    }
    if name == is_err_name {
        if args.len() != 1 {
            return Err(VmError::new(format!(
                "{module}.{is_err_name} takes 1 argument"
            )));
        }
        return Ok(Some(Value::Bool(
            matches!(&args[0], Value::Variant(tag, _) if tag.is(err_tag)),
        )));
    }
    Ok(None)
}

/// The shared operations that call a function on the present value:
/// `map_ok`/`map`, which wraps the result again, and `flat_map`, which
/// does not. `None` if `name` is neither.
fn shared_adt_call(shape: &AdtShape, name: &str, args: &[Value]) -> Result<Option<Step>, VmError> {
    let (adt_name, ok_tag, err_tag) = (shape.adt_name, shape.ok_tag, shape.err_tag);
    let (full, wraps) = match (shape.module, name) {
        ("result", "map_ok") => ("result.map_ok", true),
        ("option", "map") => ("option.map", true),
        ("result", "flat_map") => ("result.flat_map", false),
        ("option", "flat_map") => ("option.flat_map", false),
        _ => return Ok(None),
    };
    if args.len() != 2 {
        return Err(VmError::new(format!("{full} takes 2 arguments")));
    }
    match &args[0] {
        Value::Variant(tag, fields) if tag.is(ok_tag) && fields.len() == 1 => Ok(Some(call_then(
            full,
            args[1].clone(),
            fields[0].clone(),
            move |result| match wraps {
                true => Ok(Value::variant(ok_tag, vec![result])),
                false => Ok(result),
            },
        ))),
        other @ Value::Variant(tag, _) if tag.is(err_tag) => Ok(Some(Step::Done(other.clone()))),
        _ => Err(VmError::new(format!(
            "{full} requires a{} {adt_name}",
            if adt_name == "Option" { "n" } else { "" }
        ))),
    }
}

/// Dispatch `result.<name>(args)`.
pub(crate) fn call_result(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    if let Some(step) = shared_adt_call(&RESULT_SHAPE, name, args)? {
        return Ok(step);
    }
    if name == "map_err" {
        if args.len() != 2 {
            return Err(VmError::new("result.map_err takes 2 arguments".into()));
        }
        return match &args[0] {
            other @ Value::Variant(tag, _) if tag.is(bv::OK) => Ok(Step::Done(other.clone())),
            Value::Variant(tag, fields) if tag.is(bv::ERR) && fields.len() == 1 => Ok(call_then(
                "result.map_err",
                args[1].clone(),
                fields[0].clone(),
                |new_val| Ok(Value::variant(bv::ERR, vec![new_val])),
            )),
            _ => Err(VmError::new("result.map_err requires a Result".into())),
        };
    }
    result_plain(name, args).map(Step::Done)
}

/// The `result` functions that call no function.
fn result_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    if let Some(v) = dispatch_shared_adt_op(&RESULT_SHAPE, name, args, "is_ok", "is_err")? {
        return Ok(v);
    }
    match name {
        "flatten" => {
            if args.len() != 1 {
                return Err(VmError::new("result.flatten takes 1 argument".into()));
            }
            match &args[0] {
                Value::Variant(tag, fields) if tag.is(bv::OK) && fields.len() == 1 => {
                    match &fields[0] {
                        ok @ Value::Variant(inner_tag, _)
                            if inner_tag.is(bv::OK) || inner_tag.is(bv::ERR) =>
                        {
                            Ok(ok.clone())
                        }
                        _ => Ok(args[0].clone()),
                    }
                }
                other @ Value::Variant(tag, _) if tag.is(bv::ERR) => Ok(other.clone()),
                _ => Err(VmError::new("result.flatten requires a Result".into())),
            }
        }
        _ => Err(VmError::new(format!("unknown result function: {name}"))),
    }
}

/// Dispatch `option.<name>(args)`.
pub(crate) fn call_option(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    if let Some(step) = shared_adt_call(&OPTION_SHAPE, name, args)? {
        return Ok(step);
    }
    option_plain(name, args).map(Step::Done)
}

/// The `option` functions that call no function.
fn option_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    if let Some(v) = dispatch_shared_adt_op(&OPTION_SHAPE, name, args, "is_some", "is_none")? {
        return Ok(v);
    }
    match name {
        "to_result" => {
            if args.len() != 2 {
                return Err(VmError::new("option.to_result takes 2 arguments".into()));
            }
            match &args[0] {
                Value::Variant(tag, fields) if tag.is(bv::SOME) && fields.len() == 1 => {
                    Ok(Value::variant(bv::OK, vec![fields[0].clone()]))
                }
                Value::Variant(tag, _) if tag.is(bv::NONE) => {
                    Ok(Value::variant(bv::ERR, vec![args[1].clone()]))
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
                // Use `format_silt` (not Debug) so failure messages render in
                // silt-source syntax. Debug leaks Rust variant names (see
                // `Value::format_silt` doc comment).
                let msg = if args.len() == 2 {
                    format!("assertion failed: {}", args[1])
                } else {
                    format!("assertion failed: {}", args[0].format_silt())
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
                        "assertion failed: {}: {} != {}",
                        args[2],
                        args[0].format_silt(),
                        args[1].format_silt()
                    )
                } else {
                    format!(
                        "assertion failed: {} != {}",
                        args[0].format_silt(),
                        args[1].format_silt()
                    )
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
                        "assertion failed: {}: {} == {}",
                        args[2],
                        args[0].format_silt(),
                        args[1].format_silt()
                    )
                } else {
                    format!(
                        "assertion failed: {} == {}",
                        args[0].format_silt(),
                        args[1].format_silt()
                    )
                };
                Err(VmError::new(msg))
            }
        }
        _ => Err(VmError::new(format!("unknown test function: {name}"))),
    }
}
