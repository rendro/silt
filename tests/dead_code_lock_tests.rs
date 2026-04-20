//! Dead-code lock tests.
//!
//! After the round-N dead-code cleanup, several items were deleted
//! outright (unused fields, no-op helpers) and several had stale
//! `#[allow(dead_code)]` annotations scraped. These tests make the
//! cleanup stick: if someone revives a deleted symbol without also
//! restoring its read sites, grep-based assertions fail; if someone
//! re-applies a `#[allow(dead_code)]` to a live function, a pattern
//! assertion catches it.
//!
//! The tests are deliberately text-based because the deleted items
//! are no longer present in the AST — there's no symbol to poke at.

// ── Sources we lock against ─────────────────────────────────────────

const VM_MOD_RS: &str = include_str!("../src/vm/mod.rs");
const VM_RUNTIME_RS: &str = include_str!("../src/vm/runtime.rs");
const CLI_PIPELINE_RS: &str = include_str!("../src/cli/pipeline.rs");
const TYPECHECKER_MOD_RS: &str = include_str!("../src/typechecker/mod.rs");
const COMPILER_MOD_RS: &str = include_str!("../src/compiler/mod.rs");

// ── Deleted items stay deleted ──────────────────────────────────────

#[test]
fn vm_current_chunk_stays_deleted() {
    // `Vm::current_chunk` was a dead helper returning the current
    // frame's chunk. Zero callers at time of deletion.
    assert!(
        !VM_MOD_RS.contains("fn current_chunk"),
        "Vm::current_chunk was deleted — don't resurrect it without a real caller"
    );
}

#[test]
fn runtime_variant_types_field_stays_deleted() {
    // `Runtime::variant_types` was written once (empty HashMap at
    // construction) and never read. Deleted.
    assert!(
        !VM_RUNTIME_RS.contains("variant_types"),
        "Runtime::variant_types was deleted — don't resurrect it without a reader"
    );
    assert!(
        !VM_MOD_RS.contains("variant_types"),
        "the write site of Runtime::variant_types was deleted — don't resurrect it"
    );
}

#[test]
fn pipeline_has_hard_errors_stays_deleted() {
    // `CompilePipelineResult::has_hard_errors` was annotated
    // `#[allow(dead_code)]` with an explicit "not currently read"
    // comment. Deleted along with every write site.
    assert!(
        !CLI_PIPELINE_RS.contains("has_hard_errors"),
        "CompilePipelineResult::has_hard_errors was deleted — don't resurrect without a reader"
    );
}

// ── Stale `#[allow(dead_code)]` annotations removed ────────────────

#[test]
fn warning_fn_has_no_allow_dead_code() {
    // `warning` has 11 callers — the allow was stale, removed.
    // This test asserts the allow doesn't silently come back.
    let needle = "#[allow(dead_code)]\n    pub(super) fn warning(";
    assert!(
        !TYPECHECKER_MOD_RS.contains(needle),
        "TypeChecker::warning had a stale #[allow(dead_code)] removed — don't add it back"
    );
}

#[test]
fn method_entry_has_no_allow_dead_code() {
    // `MethodEntry` is constructed 12× and every field is read.
    // Stale allow was removed.
    let needle = "#[allow(dead_code)]\npub(super) struct MethodEntry";
    assert!(
        !TYPECHECKER_MOD_RS.contains(needle),
        "MethodEntry had a stale #[allow(dead_code)] removed — don't add it back"
    );
}

#[test]
fn loop_info_binding_count_has_no_allow_dead_code() {
    // `LoopInfo.binding_count` is read by the loop codegen —
    // the stale allow was removed.
    // Match the exact attribute + field pair with minimal surrounding context.
    let needle = "#[allow(dead_code)]\n    binding_count:";
    assert!(
        !COMPILER_MOD_RS.contains(needle),
        "LoopInfo.binding_count had a stale #[allow(dead_code)] removed — don't add it back"
    );
}

// ── Shared helper is the single source of truth ────────────────────

#[test]
fn trait_init_helper_is_defined_once() {
    // The built-in trait init logic was extracted into
    // `register_builtin_trait_impls`. It must exist (the two call
    // sites depend on it) but only one definition should exist.
    let occurrences = TYPECHECKER_MOD_RS
        .matches("fn register_builtin_trait_impls")
        .count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one definition of register_builtin_trait_impls, found {occurrences}"
    );
}

#[test]
fn trait_init_helper_is_called_from_both_entrypoints() {
    // If either entrypoint stops calling the helper, the other will
    // silently diverge. Assert both call sites remain.
    let calls = TYPECHECKER_MOD_RS
        .matches("register_builtin_trait_impls(")
        .count();
    // 1 definition + 2 call sites = 3 textual occurrences.
    assert!(
        calls >= 3,
        "expected at least 3 occurrences of register_builtin_trait_impls(...) \
         (1 definition + 2 call sites), found {calls}"
    );
}

#[test]
fn auto_derived_impls_helper_is_called_by_time_builtins() {
    // `time.rs` uses the shared helper so its derive set stays in
    // sync with the primitive init path.
    let time_rs = include_str!("../src/typechecker/builtins/time.rs");
    assert!(
        time_rs.contains("register_auto_derived_impls_for"),
        "time builtin should call the shared register_auto_derived_impls_for helper"
    );
}

// ── Closure / upvalue invariant locks ──────────────────────────────
//
// Silt is fully immutable, so upvalues are captured by value at
// `Op::MakeClosure` time. There is no "open" vs "closed" upvalue
// distinction — no Op::CloseUpvalue, no Op::SetUpvalue — because
// there's no mutation to track. The `Local.captured` flag was a
// vestigial field from a Lua/clox-style design that was never
// implemented. Below tests lock the invariants that make the flag
// unneeded.

#[test]
fn local_captured_field_stays_deleted() {
    // Struct was `struct Local { name, depth, slot, captured: bool }`.
    // Captured was a write-only flag — no reader anywhere in the
    // codebase. Deleted because silt's immutability makes it
    // impossible to need.
    let local_struct_start = COMPILER_MOD_RS
        .find("struct Local {")
        .expect("struct Local exists");
    // Accept either `}\n` (Unix) or `}\r\n` (Windows autocrlf checkout).
    let local_struct_end = COMPILER_MOD_RS[local_struct_start..]
        .find('}')
        .map(|i| local_struct_start + i)
        .expect("struct Local is closed");
    let struct_body = &COMPILER_MOD_RS[local_struct_start..local_struct_end];
    assert!(
        !struct_body.contains("captured"),
        "Local.captured was deleted — don't resurrect it (silt is immutable; \
         upvalues are captured by value, not by reference)"
    );
}

#[test]
fn no_op_close_upvalue_opcode_exists() {
    // If someone ever introduces a CloseUpvalue opcode, they're
    // either reintroducing Lua-style open upvalues (requires
    // rewiring the entire closure path and adding mutation) or
    // they've confused themselves. Either way, the audit needs
    // to see it before it lands.
    let bytecode_rs = include_str!("../src/bytecode.rs");
    assert!(
        !bytecode_rs.contains("CloseUpvalue"),
        "Silt has no Op::CloseUpvalue because upvalues are captured by value. \
         If you added one, revisit why — the closure path is by-value on purpose."
    );
}

#[test]
fn no_op_set_upvalue_opcode_exists() {
    // Same reasoning: SetUpvalue would imply mutation of a captured
    // value, which silt's immutability forbids at the surface.
    let bytecode_rs = include_str!("../src/bytecode.rs");
    assert!(
        !bytecode_rs.contains("SetUpvalue"),
        "Silt has no Op::SetUpvalue because captured values are immutable. \
         If you added one, the language gained mutation — revisit invariants."
    );
}

// ── F18: Colors.dim stayed deleted ─────────────────────────────────────
//
// `Colors.dim` was written in both COLORS_ON ("\x1b[2m") and COLORS_OFF
// ("") but never read by the Display impl. The struct-level
// `#[allow(dead_code)]` that silenced it was removed along with the
// field and both initializer lines. Since the field had zero readers,
// deletion is behaviour-identical — this test locks against
// resurrection without a corresponding reader.

#[test]
fn colors_dim_field_stays_deleted() {
    let errors_rs = include_str!("../src/errors.rs");
    assert!(
        !errors_rs.contains("    dim: &'static str"),
        "Colors.dim field was deleted — don't resurrect without a reader"
    );
    assert!(
        !errors_rs.contains("pub dim:"),
        "Colors.dim field was deleted — don't resurrect (pub form) without a reader"
    );
    assert!(
        !errors_rs.contains(r#"dim: "\x1b[2m""#),
        "Colors.dim ON-initializer was deleted — don't resurrect without a reader"
    );
}
