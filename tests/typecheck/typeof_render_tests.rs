use silt::intern::intern;
use silt::types::Type;

#[test]
fn typeof_renders_as_type() {
    let a = Type::Var(7);
    let typeof_a = Type::Generic(intern("TypeOf"), vec![a]);
    let rendered = format!("{}", typeof_a);
    // Unresolved TyVar renders as `_` — no leaking `?N`. The TypeOf
    // envelope renders as `type `.
    assert_eq!(rendered, "type _");
}

#[test]
fn typeof_in_fn_renders_correctly() {
    let a = Type::Var(3);
    let typeof_a = Type::Generic(intern("TypeOf"), vec![a]);
    let fn_ty = Type::Fun(vec![Type::String, typeof_a], Box::new(Type::Int));
    let rendered = format!("{}", fn_ty);
    assert!(rendered.contains("type _"), "got: {rendered}");
    assert!(!rendered.contains("TypeOf"), "leaked TypeOf: {rendered}");
    assert!(!rendered.contains("?"), "leaked TyVar id: {rendered}");
}

#[test]
fn type_error_renders_as_placeholder() {
    // Type::Error must never leak `<error>` into user-facing messages.
    let rendered = format!("{}", Type::Error);
    assert_eq!(rendered, "_");
    assert!(!rendered.contains("error"));
}
