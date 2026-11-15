//! The type a run-time value dispatches its methods on, and the names
//! messages give it. Here, not in `crate::types`, so that the type layer
//! does not know the run-time value.

use crate::defs::TypeId;
use crate::types::TypeRef;
use crate::types::canonical::{is_opaque_value_type, kind_types};
use crate::value::Value;

/// The type a runtime [`Value`] dispatches its methods on: the type the
/// methods of its impls are keyed by. A record, a variant and a type
/// descriptor carry their type, so `Int.default()` and
/// `Todo.decode(...)` route to impls of `Int` / `Todo`.
///
/// The mapping mirrors [`crate::types::canonical::canonical_name`]
/// applied to each `Value` variant's corresponding
/// [`crate::types::Type`]. Every function-shaped value (a closure, a
/// builtin, a host function, a variant constructor) is an `Fn`: the typechecker types each as
/// `Type::Fun(..)`, so a `trait T for Fn` impl serves them all.
pub(crate) fn dispatch_type_for_value(val: &Value) -> TypeId {
    let kinds = kind_types();
    match val {
        Value::Variant(variant) => variant.type_id(),
        Value::Record(record) => record.type_id(),
        Value::TypeDescriptor(ty) => ty.id,
        Value::PrimitiveDescriptor(name) => TypeRef::builtin(name).id,
        Value::Int(_) => kinds.int,
        Value::Float(_) => kinds.float,
        Value::Bool(_) => kinds.bool,
        Value::String(_, _) => kinds.string,
        Value::List(_) => kinds.list,
        Value::Map(_) => kinds.map,
        Value::Set(_) => kinds.set,
        Value::Tuple(_) => kinds.tuple,
        Value::Channel(_) => kinds.channel,
        Value::VmClosure(_)
        | Value::BuiltinFn(_)
        | Value::HostFn(_)
        | Value::VariantConstructor(..) => kinds.function,
        Value::Unit => kinds.unit,
        Value::Bytes(_) => kinds.bytes,
        Value::Handle(_) => kinds.handle,
        Value::TcpListener(_) => kinds.tcp_listener,
        Value::TcpStream(_) => kinds.tcp_stream,
        Value::Regex(_) => kinds.regex,
        // (No method is found by a dictionary.)
        Value::Dict(_) => kinds.unit,
    }
}

/// The kind of `val` as a message names it, beside the type `written`
/// that it was to be of: [`Value::kind`], but for a value of an opaque
/// type of a builtin module its type with its module (`regex.Regex`),
/// which a program's own `Regex` is not, and the same for a record or a
/// variant of a type of a builtin module that `written` does not name
/// (`json.Value` where a host's own `Value` was to be).
pub(crate) fn kind_name(val: &Value, written: &str) -> String {
    let id = dispatch_type_for_value(val);
    if let Some((name, Some(module))) = crate::defs::builtin_types().get(id.0.0 as usize) {
        let qualified = format!("{module}.{name}");
        let named =
            matches!(val, Value::Record(_) | Value::Variant(_)) && !written.contains(&qualified);
        if is_opaque_value_type(id) || named {
            return qualified;
        }
    }
    val.kind().to_string()
}

/// The name of the type [`dispatch_type_for_value`] gives, as messages
/// show it.
pub(crate) fn dispatch_type_name(val: &Value) -> String {
    match val {
        Value::Variant(variant) => variant.ty().name.clone(),
        Value::Record(record) => record.ty().name.clone(),
        Value::TypeDescriptor(ty) => ty.name.clone(),
        _ => crate::typeinfo::builtin_type(dispatch_type_for_value(val))
            .name
            .clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dispatch_type_for_value ────────────────────────────────────

    fn builtin_id(name: &str) -> TypeId {
        TypeRef::builtin(name).id
    }

    #[test]
    fn dispatch_type_for_value_list_is_list() {
        let v = Value::list(vec![]);
        assert_eq!(dispatch_type_for_value(&v), builtin_id("List"));
    }

    #[test]
    fn dispatch_type_for_value_primitives() {
        assert_eq!(dispatch_type_for_value(&Value::Int(0)), builtin_id("Int"));
        assert_eq!(
            dispatch_type_for_value(&Value::Float(crate::value::Float::from(0))),
            builtin_id("Float")
        );
        assert_eq!(
            dispatch_type_for_value(&Value::Bool(false)),
            builtin_id("Bool")
        );
        assert_eq!(
            dispatch_type_for_value(&Value::string(String::new())),
            builtin_id("String")
        );
        assert_eq!(dispatch_type_for_value(&Value::Unit), builtin_id("()"));
    }

    #[test]
    fn dispatch_type_for_value_record_uses_carried_type() {
        let v = Value::builtin_record(
            crate::typeinfo::ty::DATE,
            [
                ("year", Value::Int(2024)),
                ("month", Value::Int(1)),
                ("day", Value::Int(2)),
            ],
        );
        assert_eq!(dispatch_type_for_value(&v), crate::typeinfo::ty::DATE);
        assert_eq!(dispatch_type_name(&v), "Date");
    }

    #[test]
    fn dispatch_type_for_value_descriptors_use_carried_type() {
        assert_eq!(
            dispatch_type_for_value(&Value::TypeDescriptor(
                crate::typeinfo::builtin_type(crate::typeinfo::ty::WEEKDAY).clone()
            )),
            crate::typeinfo::ty::WEEKDAY
        );
        assert_eq!(
            dispatch_type_for_value(&Value::PrimitiveDescriptor("Int")),
            builtin_id("Int")
        );
    }

    #[test]
    fn dispatch_type_for_value_variant_uses_its_type() {
        let v = Value::variant(crate::typeinfo::bv::SOME, [Value::Int(7)]);
        assert_eq!(dispatch_type_for_value(&v), crate::typeinfo::ty::OPTION);
        assert_eq!(dispatch_type_name(&v), "Option");
    }
}
