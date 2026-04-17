//! Regression tests for round-26 finding L6: `Value::PartialEq`
//! returned `false` for reflexive comparisons on `Handle`, `VmClosure`,
//! `BuiltinFn`, and `VariantConstructor` via the catch-all `_ => false`
//! arm. That violated the `Eq` reflexivity contract (`a == a` ≡ `true`)
//! AND desynchronized `PartialEq` from `Ord` (round-23 added explicit
//! arms to `Ord` returning identity-based `Equal`).
//!
//! The fix mirrors the `Ord` arms into `PartialEq`:
//!   - `Handle(a) == Handle(b)` iff `a.id == b.id`
//!   - `VmClosure(a) == VmClosure(b)` iff `Arc::ptr_eq(a, b)`
//!   - `BuiltinFn(a) == BuiltinFn(b)` iff `a == b` (by name)
//!   - `VariantConstructor(na, aa) == VariantConstructor(nb, ab)`
//!     iff `na == nb && aa == ab`
//!
//! Cross-kind pairs still fall through to `_ => false` as before.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::value::{TaskHandle, Value};

// ── End-to-end silt binary test ────────────────────────────────────

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

/// End-to-end: `let h = task.spawn(fn() { 42 }); println(h == h)` must
/// print `true`. Before the fix, the `_ => false` arm of `PartialEq`
/// returned `false` for reflexive `Handle` comparison, and the silt
/// `==` operator surfaced that as the user-visible `false`.
#[test]
fn test_handle_reflexivity_via_silt_eq_operator() {
    let src = r#"
import task

fn main() {
  let h = task.spawn { -> 42 }
  let r = task.join(h)
  -- `h == h` must be true. Before round-26 L6 fix, this printed false.
  println(h == h)
  -- Consume `r` so it's not flagged as unused.
  match r == 42 {
    true -> println("joined")
    false -> println("wrong")
  }
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "silt_vpe26_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("failed to run silt binary");
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.contains(&"true"),
        "expected `h == h` to print true; got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        lines.contains(&"joined"),
        "expected join to return 42; got stdout={stdout:?} stderr={stderr:?}"
    );
}

// ── Rust-level unit tests ──────────────────────────────────────────

// ── Handle ────────────────────────────────────────────────────────

/// Same handle id → equal. The primary reflexivity lock.
#[test]
fn partial_eq_handle_same_id() {
    let h = Value::Handle(Arc::new(TaskHandle::new(42)));
    // Reflexive on the Value itself (identical Value clone shares the Arc).
    assert_eq!(h, h.clone());
    // Two distinct Arcs with the same id also compare equal — the fix
    // is id-based, not Arc-ptr-based, matching `impl Ord`.
    let h1 = Value::Handle(Arc::new(TaskHandle::new(100)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(100)));
    assert_eq!(h1, h2);
}

/// Different handle ids → not equal.
#[test]
fn partial_eq_handle_different_ids() {
    let h1 = Value::Handle(Arc::new(TaskHandle::new(1)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(2)));
    assert_ne!(h1, h2);
}

// ── VmClosure ─────────────────────────────────────────────────────

fn mk_closure(name: &str) -> Arc<VmClosure> {
    Arc::new(VmClosure {
        function: Arc::new(Function::new(name.to_string(), 0)),
        upvalues: Vec::new(),
    })
}

/// Same Arc → equal (ptr_eq).
#[test]
fn partial_eq_vmclosure_same_arc() {
    let arc = mk_closure("f");
    let a = Value::VmClosure(arc.clone());
    let b = Value::VmClosure(arc);
    assert_eq!(a, b);
    // Reflexivity.
    assert_eq!(a, a.clone());
}

/// Different Arc allocations → not equal (even same name), matching
/// `impl Ord`'s identity-based semantics.
#[test]
fn partial_eq_vmclosure_distinct_arcs_not_equal() {
    let a = Value::VmClosure(mk_closure("f"));
    let b = Value::VmClosure(mk_closure("f"));
    assert_ne!(a, b);
    let c = Value::VmClosure(mk_closure("g"));
    assert_ne!(a, c);
}

// ── BuiltinFn ─────────────────────────────────────────────────────

/// Same name → equal.
#[test]
fn partial_eq_builtin_fn_same_name() {
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("println".into());
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
}

/// Different names → not equal.
#[test]
fn partial_eq_builtin_fn_different_names() {
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("print".into());
    assert_ne!(a, b);
}

// ── VariantConstructor ────────────────────────────────────────────

/// Same name + arity → equal.
#[test]
fn partial_eq_variant_constructor_same_name_and_arity() {
    let a = Value::VariantConstructor("Some".into(), 1);
    let b = Value::VariantConstructor("Some".into(), 1);
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
}

/// Different name or arity → not equal.
#[test]
fn partial_eq_variant_constructor_differences() {
    let some_1 = Value::VariantConstructor("Some".into(), 1);
    let none_0 = Value::VariantConstructor("None".into(), 0);
    let some_2 = Value::VariantConstructor("Some".into(), 2);
    assert_ne!(some_1, none_0);
    assert_ne!(some_1, some_2);
    assert_ne!(none_0, some_2);
}

// ── Reflexivity: every variant `v == v` ───────────────────────────
//
// The core Eq invariant. Before the fix, `Handle`, `VmClosure`,
// `BuiltinFn`, and `VariantConstructor` would violate this.

#[test]
fn partial_eq_reflexivity_every_variant() {
    let handle_arc = Arc::new(TaskHandle::new(7));
    let closure_arc = mk_closure("id");
    let values: Vec<Value> = vec![
        Value::Unit,
        Value::Bool(true),
        Value::Int(42),
        Value::Float(1.5),
        Value::ExtFloat(2.5),
        Value::String("hi".into()),
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)])),
        Value::Range(1, 5),
        Value::Tuple(vec![Value::Int(1), Value::String("x".into())]),
        Value::Variant("Ok".into(), vec![Value::Int(1)]),
        Value::VariantConstructor("Some".into(), 1),
        Value::BuiltinFn("println".into()),
        Value::VmClosure(closure_arc),
        Value::Handle(handle_arc),
        Value::RecordDescriptor("Point".into()),
        Value::PrimitiveDescriptor("Int".into()),
    ];
    for v in &values {
        assert_eq!(
            v,
            &v.clone(),
            "reflexivity violation: `{v:?}` is not equal to itself"
        );
    }
}

// ── Cross-kind comparisons still false ────────────────────────────

/// The new explicit arms must not accidentally cross-match. Example:
/// `Handle` vs `VmClosure` must still be `false` via the catch-all.
#[test]
fn partial_eq_cross_kind_still_false() {
    let h = Value::Handle(Arc::new(TaskHandle::new(1)));
    let c = Value::VmClosure(mk_closure("f"));
    let b = Value::BuiltinFn("println".into());
    let vc = Value::VariantConstructor("Some".into(), 1);

    assert_ne!(h, c);
    assert_ne!(h, b);
    assert_ne!(h, vc);
    assert_ne!(c, b);
    assert_ne!(c, vc);
    assert_ne!(b, vc);
}
