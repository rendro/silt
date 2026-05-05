//! Round-73 BLOAT-2 parity locks for builtin primitive / generic
//! container descriptor name lists.
//!
//! Pre-round-73 the names `["Int","Float","ExtFloat","String","Bool"]`
//! and `["List","Map","Set","Channel","Tuple"]` were hand-rolled at
//! both `src/vm/dispatch.rs::register_builtins` (for `Value::Primitive
//! Descriptor` and `Value::TypeDescriptor` globals) and
//! `src/typechecker/builtins.rs::register_builtins` (for the matching
//! polymorphic `TypeOf(...)` schemes). That's a textbook
//! parallel-array drift surface — adding a new primitive descriptor or
//! container required edits in two independent files.
//!
//! Round-73 BLOAT-2 hoists both lists onto:
//!   * `module::BUILTIN_PRIMITIVE_NAMES`
//!   * `module::BUILTIN_GENERIC_CONTAINER_NAMES`
//!
//! Per-name behaviour stays at the call sites (the typechecker still
//! has to map `"Int"` → `Type::Int` and `Map` to a 2-arg scheme), but
//! the NAME set is centralised. These locks pin both that the constants
//! exist AND that both call sites reference them — preventing a future
//! edit from re-introducing a literal `&["Int", ...]` array.

use silt::module::{BUILTIN_GENERIC_CONTAINER_NAMES, BUILTIN_PRIMITIVE_NAMES};

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const TYPECHECKER_BUILTINS_SRC: &str = include_str!("../src/typechecker/builtins.rs");
const MODULE_SRC: &str = include_str!("../src/module.rs");

// ── Constants exist in module.rs ──

#[test]
fn module_defines_primitive_names_constant() {
    assert!(
        MODULE_SRC.contains("pub const BUILTIN_PRIMITIVE_NAMES"),
        "src/module.rs must define `BUILTIN_PRIMITIVE_NAMES` as the \
         single source of truth for the primitive descriptor names."
    );
    // Sanity: at minimum the five canonical primitives must be present.
    let actual: std::collections::BTreeSet<&str> =
        BUILTIN_PRIMITIVE_NAMES.iter().copied().collect();
    let expected: std::collections::BTreeSet<&str> =
        ["Int", "Float", "ExtFloat", "String", "Bool"].into_iter().collect();
    assert!(
        expected.is_subset(&actual),
        "BUILTIN_PRIMITIVE_NAMES must contain {expected:?}; got {actual:?}"
    );
}

#[test]
fn module_defines_generic_container_names_constant() {
    assert!(
        MODULE_SRC.contains("pub const BUILTIN_GENERIC_CONTAINER_NAMES"),
        "src/module.rs must define `BUILTIN_GENERIC_CONTAINER_NAMES` as \
         the single source of truth for the generic container \
         descriptor names."
    );
    let actual: std::collections::BTreeSet<&str> =
        BUILTIN_GENERIC_CONTAINER_NAMES.iter().copied().collect();
    let expected: std::collections::BTreeSet<&str> =
        ["List", "Map", "Set", "Channel", "Tuple"].into_iter().collect();
    assert!(
        expected.is_subset(&actual),
        "BUILTIN_GENERIC_CONTAINER_NAMES must contain {expected:?}; \
         got {actual:?}"
    );
}

// ── Both call sites reference the constants ──

#[test]
fn dispatch_references_primitive_names_constant() {
    assert!(
        DISPATCH_SRC.contains("BUILTIN_PRIMITIVE_NAMES"),
        "src/vm/dispatch.rs::register_builtins must iterate over \
         `module::BUILTIN_PRIMITIVE_NAMES` to register \
         `Value::PrimitiveDescriptor` globals. Round-73 BLOAT-2 \
         hoisted the name set out of dispatch.rs to prevent drift \
         with the typechecker side."
    );
}

#[test]
fn dispatch_references_generic_container_names_constant() {
    assert!(
        DISPATCH_SRC.contains("BUILTIN_GENERIC_CONTAINER_NAMES"),
        "src/vm/dispatch.rs::register_builtins must iterate over \
         `module::BUILTIN_GENERIC_CONTAINER_NAMES` to register \
         `Value::TypeDescriptor` globals."
    );
}

#[test]
fn typechecker_builtins_references_primitive_names_constant() {
    assert!(
        TYPECHECKER_BUILTINS_SRC.contains("BUILTIN_PRIMITIVE_NAMES"),
        "src/typechecker/builtins.rs::register_builtins must iterate \
         over `module::BUILTIN_PRIMITIVE_NAMES` to register the \
         polymorphic `TypeOf(<inner>)` schemes for primitive \
         descriptors."
    );
}

#[test]
fn typechecker_builtins_references_generic_container_names_constant() {
    assert!(
        TYPECHECKER_BUILTINS_SRC.contains("BUILTIN_GENERIC_CONTAINER_NAMES"),
        "src/typechecker/builtins.rs::register_builtins must iterate \
         over `module::BUILTIN_GENERIC_CONTAINER_NAMES` to register \
         the polymorphic container descriptor schemes."
    );
}

// ── Hand-rolled literals are gone ──

/// Locks against a regression where someone re-introduces the literal
/// `&["Int", "Float", "ExtFloat", "String", "Bool"]` array at either
/// site. The unique fingerprint is the exact 5-name array slice form.
#[test]
fn no_hand_rolled_primitive_name_array_in_dispatch() {
    let pre_fix = "&[\"Int\", \"Float\", \"ExtFloat\", \"String\", \"Bool\"]";
    let bad: Vec<(usize, &str)> = DISPATCH_SRC
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && l.contains(pre_fix)
        })
        .map(|(i, l)| (i + 1, l))
        .collect();
    assert!(
        bad.is_empty(),
        "src/vm/dispatch.rs still contains the hand-rolled primitive \
         name array. Round-73 BLOAT-2 hoisted this to \
         `module::BUILTIN_PRIMITIVE_NAMES`. Found: {bad:?}"
    );
}

#[test]
fn no_hand_rolled_primitive_name_array_in_typechecker_builtins() {
    let pre_fix = "&[\"Int\", \"Float\", \"ExtFloat\", \"String\", \"Bool\"]";
    let bad: Vec<(usize, &str)> = TYPECHECKER_BUILTINS_SRC
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && l.contains(pre_fix)
        })
        .map(|(i, l)| (i + 1, l))
        .collect();
    assert!(
        bad.is_empty(),
        "src/typechecker/builtins.rs still contains the hand-rolled \
         primitive name array. Round-73 BLOAT-2 hoisted this to \
         `module::BUILTIN_PRIMITIVE_NAMES`. Found: {bad:?}"
    );
}

#[test]
fn no_hand_rolled_container_name_array_in_dispatch() {
    let pre_fix = "&[\"List\", \"Map\", \"Set\", \"Channel\", \"Tuple\"]";
    let bad: Vec<(usize, &str)> = DISPATCH_SRC
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && l.contains(pre_fix)
        })
        .map(|(i, l)| (i + 1, l))
        .collect();
    assert!(
        bad.is_empty(),
        "src/vm/dispatch.rs still contains the hand-rolled container \
         name array. Round-73 BLOAT-2 hoisted this to \
         `module::BUILTIN_GENERIC_CONTAINER_NAMES`. Found: {bad:?}"
    );
}

// ── End-to-end runtime parity ──

/// Constants must match the lengths of the literal name sets that the
/// dispatch site previously used. The pre-fix array literal in
/// dispatch.rs had exactly 5 primitive names and 5 container names.
/// If the constants drift in size we lock the regression here.
#[test]
fn primitive_constant_size_matches_pre_fix_literal() {
    assert_eq!(
        BUILTIN_PRIMITIVE_NAMES.len(),
        5,
        "BUILTIN_PRIMITIVE_NAMES had 5 entries pre-round-73 \
         (Int/Float/ExtFloat/String/Bool). A change in size means a \
         primitive descriptor was added or removed — re-verify the \
         per-name mapping in src/typechecker/builtins.rs."
    );
}

#[test]
fn container_constant_size_matches_pre_fix_literal() {
    assert_eq!(
        BUILTIN_GENERIC_CONTAINER_NAMES.len(),
        5,
        "BUILTIN_GENERIC_CONTAINER_NAMES had 5 entries pre-round-73 \
         (List/Map/Set/Channel/Tuple). A change in size means a \
         container descriptor was added or removed — re-verify the \
         per-name mapping in src/typechecker/builtins.rs."
    );
}
