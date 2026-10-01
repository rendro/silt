//! Round-73 BLOAT-2 (L5) regression locks: the runtime
//! `Value::PrimitiveDescriptor` / `Value::TypeDescriptor` seed loops in
//! `src/vm/dispatch.rs` and the matching typechecker registration loops
//! in `src/typechecker/builtins.rs` must iterate over the canonical
//! NAME constants in `src/module.rs` so the two sites cannot drift on
//! the set of descriptor names.
//!
//! Pre-fix shape (round 72 audit):
//!
//!   * dispatch.rs hand-rolled one `globals.insert(("Int" / "Float" /
//!     "String" / "Bool").into(), ...)` call per primitive plus a slice
//!     `["List", "Map", "Set", "Channel", "Tuple"]` for the generic
//!     containers.
//!   * builtins.rs hand-rolled an `&["Int", "Float",
//!     "String", "Bool"]` slice and per-container blocks for `List` /
//!     `Set` / `Channel` / `Map` (Tuple is VM-only on the typechecker
//!     side — there is no polymorphic descriptor scheme for it today).
//!
//! The collapse hoists the NAME sets to two `pub const` slices in
//! `src/module.rs`:
//!
//!   * `BUILTIN_PRIMITIVE_NAMES`
//!   * `BUILTIN_GENERIC_CONTAINER_NAMES`
//!
//! The per-name behaviour stays at the call sites because each name has
//! a distinct mapping to a `Type` (typechecker) or a value-shape
//! constructor (VM); only the NAME set is centralised.

use silt::module::{BUILTIN_GENERIC_CONTAINER_NAMES, BUILTIN_PRIMITIVE_NAMES};

// ── Constant shape locks ────────────────────────────────────────────

/// The constants must contain exactly the round-72 / round-73 known
/// names — adding a new descriptor name here is a deliberate gesture,
/// not an accident.
#[test]
fn primitive_descriptor_names_match_audit_baseline() {
    assert_eq!(
        BUILTIN_PRIMITIVE_NAMES,
        &["Int", "Float", "String", "Bool"],
        "round-73 BLOAT-2 baseline: the canonical primitive descriptor \
         name set is exactly Int/Float/String/Bool. Extending \
         this list is fine — but the new entry must also acquire the \
         matching per-name `Type` mapping in \
         `src/typechecker/builtins.rs::register_builtins` and the \
         matching `Value::PrimitiveDescriptor` shape in \
         `src/vm/dispatch.rs::register_builtins`. Update this test \
         alongside the addition."
    );
}

#[test]
fn container_descriptor_names_match_audit_baseline() {
    assert_eq!(
        BUILTIN_GENERIC_CONTAINER_NAMES,
        &["List", "Map", "Set", "Channel", "Tuple"],
        "round-73 BLOAT-2 baseline: the canonical generic container \
         descriptor name set is exactly List/Map/Set/Channel/Tuple. \
         Extending this list is fine — but the new entry must also \
         acquire the matching per-name `Type` builder in \
         `src/typechecker/builtins.rs::register_builtins` (or be \
         skipped explicitly, like Tuple) and the matching \
         `Value::TypeDescriptor` shape in \
         `src/vm/dispatch.rs::register_builtins`. Update this test \
         alongside the addition."
    );
}

// ── Behavioural lock ────────────────────────────────────────────────

/// Every name in `BUILTIN_PRIMITIVE_NAMES` must resolve as a bare
/// global at runtime to a primitive descriptor — i.e. `println(Int)` /
/// `println(Float)` etc. must succeed. The shape of the printed value
/// is the descriptor's display, but the test only requires that the
/// program type-checks and runs to completion without an "unknown
/// global" or "type error" surfacing for the bare name.
#[test]
fn every_primitive_descriptor_name_is_a_runtime_global() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("silt_round73_descriptor_runtime");
        fs::create_dir_all(&dir).unwrap();
        let name = format!("{prefix}_{n}.silt");
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    for name in BUILTIN_PRIMITIVE_NAMES {
        // The simplest end-to-end exercise is to bind the bare
        // descriptor name to a local. If the seed loop doesn't register
        // the name, the typechecker emits "unknown identifier" — which
        // is exactly the regression shape the dedup must prevent.
        // We deliberately don't try to `.display()` the descriptor here
        // because typed-descriptor values are typechecker-only carriers
        // (`TypeOf(<inner>)`) and don't have user-callable methods
        // beyond `Type::parse` / `Type::empty` style statics.
        let src = format!("fn main() {{\n    let _d = {name}\n    println(\"ok\")\n}}\n");
        let path = temp_silt_file(&format!("primdesc_{name}"), &src);

        let output = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("run")
            .arg(&path)
            .output()
            .expect("failed to run silt");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "`silt run` failed for primitive descriptor `{name}` — the \
             round-73 BLOAT-2 dedup must keep every name in \
             BUILTIN_PRIMITIVE_NAMES wired as a runtime global.\n\
             src:\n{src}\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "primitive descriptor `{name}` script ran but produced \
             unexpected stdout — likely a panic-then-recover.\n\
             stdout: {stdout:?}\nstderr: {stderr:?}"
        );
    }
}
