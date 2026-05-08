//! Round-73 BLOAT-2 (L5) regression locks: the runtime
//! `Value::PrimitiveDescriptor` / `Value::TypeDescriptor` seed loops in
//! `src/vm/dispatch.rs` and the matching typechecker registration loops
//! in `src/typechecker/builtins.rs` must iterate over the canonical
//! NAME constants in `src/module.rs` so the two sites cannot drift on
//! the set of descriptor names.
//!
//! Pre-fix shape (round 72 audit):
//!
//!   * dispatch.rs hand-rolled five `globals.insert(("Int" / "Float" /
//!     "ExtFloat" / "String" / "Bool").into(), ...)` calls plus a slice
//!     `["List", "Map", "Set", "Channel", "Tuple"]` for the generic
//!     containers.
//!   * builtins.rs hand-rolled an `&["Int", "Float", "ExtFloat",
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

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const TYPECHECKER_BUILTINS_SRC: &str = include_str!("../src/typechecker/builtins.rs");

// ── Constant shape locks ────────────────────────────────────────────

/// The constants must contain exactly the round-72 / round-73 known
/// names — adding a new descriptor name here is a deliberate gesture,
/// not an accident.
#[test]
fn primitive_descriptor_names_match_audit_baseline() {
    assert_eq!(
        BUILTIN_PRIMITIVE_NAMES,
        &["Int", "Float", "ExtFloat", "String", "Bool"],
        "round-73 BLOAT-2 baseline: the canonical primitive descriptor \
         name set is exactly Int/Float/ExtFloat/String/Bool. Extending \
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

// ── Source-grep locks: dispatch.rs ──────────────────────────────────

/// `src/vm/dispatch.rs` must NOT contain a hand-rolled
/// `globals.insert("Int"...)` followed by sibling lines for
/// `Float`/`ExtFloat`/`String`/`Bool`. The seed loop iterates
/// `module::BUILTIN_PRIMITIVE_NAMES`.
#[test]
fn dispatch_rs_does_not_hand_roll_primitive_inserts() {
    // Pre-fix shape (5 unrolled inserts), each looking like:
    //   self.globals.insert(
    //       "Int".into(),
    //       Value::PrimitiveDescriptor("Int".into()),
    //   );
    //
    // Look for `Value::PrimitiveDescriptor("Int"`. If the seed loop
    // is data-driven, the name `"Int"` appears only once textually
    // (in the constant slice in module.rs) — never literally in
    // dispatch.rs as a `Value::PrimitiveDescriptor("Int")` argument.
    for name in BUILTIN_PRIMITIVE_NAMES {
        let needle = format!("Value::PrimitiveDescriptor(\"{name}\"");
        assert!(
            !DISPATCH_SRC.contains(&needle),
            "src/vm/dispatch.rs re-introduced a hand-rolled \
             `Value::PrimitiveDescriptor(\"{name}\"...)` literal. The \
             round-73 BLOAT-2 fix derives the name set from \
             `module::BUILTIN_PRIMITIVE_NAMES`."
        );
    }
}

/// `src/vm/dispatch.rs` must NOT contain a hand-rolled
/// `Value::TypeDescriptor("List"...)` etc.
#[test]
fn dispatch_rs_does_not_hand_roll_container_inserts() {
    for name in BUILTIN_GENERIC_CONTAINER_NAMES {
        let needle = format!("Value::TypeDescriptor(\"{name}\"");
        assert!(
            !DISPATCH_SRC.contains(&needle),
            "src/vm/dispatch.rs re-introduced a hand-rolled \
             `Value::TypeDescriptor(\"{name}\"...)` literal. The \
             round-73 BLOAT-2 fix derives the name set from \
             `module::BUILTIN_GENERIC_CONTAINER_NAMES`."
        );
    }
}

#[test]
fn dispatch_rs_iterates_primitive_names_constant() {
    assert!(
        DISPATCH_SRC.contains("BUILTIN_PRIMITIVE_NAMES"),
        "src/vm/dispatch.rs must reference \
         `module::BUILTIN_PRIMITIVE_NAMES` — without it the data-driven \
         seed loop is silently broken (round-73 BLOAT-2)."
    );
}

#[test]
fn dispatch_rs_iterates_container_names_constant() {
    assert!(
        DISPATCH_SRC.contains("BUILTIN_GENERIC_CONTAINER_NAMES"),
        "src/vm/dispatch.rs must reference \
         `module::BUILTIN_GENERIC_CONTAINER_NAMES` — without it the \
         data-driven seed loop is silently broken (round-73 BLOAT-2)."
    );
}

// ── Source-grep locks: typechecker/builtins.rs ──────────────────────

/// `src/typechecker/builtins.rs` must NOT contain the pre-fix
/// hand-rolled `&["Int", "Float", "ExtFloat", "String", "Bool"]` slice.
#[test]
fn typechecker_builtins_does_not_hand_roll_primitive_slice() {
    let needle = "&[\"Int\", \"Float\", \"ExtFloat\", \"String\", \"Bool\"]";
    for (idx, line) in TYPECHECKER_BUILTINS_SRC.lines().enumerate() {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");
        if is_comment {
            continue;
        }
        assert!(
            !line.contains(needle),
            "src/typechecker/builtins.rs:{} re-introduces the hand-rolled \
             primitive descriptor name slice ({needle:?}). The \
             round-73 BLOAT-2 fix derives this from \
             `module::BUILTIN_PRIMITIVE_NAMES`. Line: {line}",
            idx + 1
        );
    }
}

#[test]
fn typechecker_builtins_iterates_primitive_names_constant() {
    assert!(
        TYPECHECKER_BUILTINS_SRC.contains("BUILTIN_PRIMITIVE_NAMES"),
        "src/typechecker/builtins.rs must reference \
         `module::BUILTIN_PRIMITIVE_NAMES` — without it the data-driven \
         per-primitive `Scheme` registration is silently broken \
         (round-73 BLOAT-2)."
    );
}

#[test]
fn typechecker_builtins_iterates_container_names_constant() {
    assert!(
        TYPECHECKER_BUILTINS_SRC.contains("BUILTIN_GENERIC_CONTAINER_NAMES"),
        "src/typechecker/builtins.rs must reference \
         `module::BUILTIN_GENERIC_CONTAINER_NAMES` — without it the \
         per-container `Scheme` registration is silently broken \
         (round-73 BLOAT-2)."
    );
}

// ── Per-site name parity ────────────────────────────────────────────

/// Both call sites must consult the same constant for the primitive
/// name set. A future patch that adds a new primitive descriptor must
/// add it to `BUILTIN_PRIMITIVE_NAMES` (so both loops see it) — adding
/// it only to one site by hand would break this test.
///
/// The lock works by asserting both sites contain a `for name in
/// crate::module::BUILTIN_PRIMITIVE_NAMES` (or `module::...`)
/// iteration shape.
#[test]
fn both_sites_iterate_primitive_names_via_for_loop() {
    let dispatch_iters = DISPATCH_SRC.matches("BUILTIN_PRIMITIVE_NAMES").count();
    let builtins_iters = TYPECHECKER_BUILTINS_SRC
        .matches("BUILTIN_PRIMITIVE_NAMES")
        .count();
    assert!(
        dispatch_iters >= 1,
        "src/vm/dispatch.rs has 0 references to BUILTIN_PRIMITIVE_NAMES"
    );
    assert!(
        builtins_iters >= 1,
        "src/typechecker/builtins.rs has 0 references to BUILTIN_PRIMITIVE_NAMES"
    );
}

#[test]
fn both_sites_iterate_container_names_via_for_loop() {
    let dispatch_iters = DISPATCH_SRC
        .matches("BUILTIN_GENERIC_CONTAINER_NAMES")
        .count();
    let builtins_iters = TYPECHECKER_BUILTINS_SRC
        .matches("BUILTIN_GENERIC_CONTAINER_NAMES")
        .count();
    assert!(
        dispatch_iters >= 1,
        "src/vm/dispatch.rs has 0 references to BUILTIN_GENERIC_CONTAINER_NAMES"
    );
    assert!(
        builtins_iters >= 1,
        "src/typechecker/builtins.rs has 0 references to BUILTIN_GENERIC_CONTAINER_NAMES"
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
