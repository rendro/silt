//! Regression test for Round-24 LATENT finding:
//! `dispatch_method_entry` in src/typechecker/inference.rs previously
//! skipped trait-constraint validation for `Type::Unit` via the arm
//! `Type::Error | Type::Never | Type::Unit => {}`.
//!
//! The `Type::Unit` branch was vacuously correct given the current
//! trait-impl grammar (no user syntactic path inserts a method into
//! `method_table[(Unit_sym, ...)]` with a non-empty `method_constraints`
//! list), but it was a latent soundness hole: if the grammar ever grows
//! `trait X for ()` on a user-accessible path, the branch would silently
//! accept call sites that fail the trait constraint instead of erroring.
//!
//! The Error/Never branches are the genuinely correct skips — they
//! propagate prior errors / divergence. Unit should fall through to the
//! normal validation path (which already handles it: `type_name_for_impl`
//! at src/typechecker/mod.rs:724 maps `Type::Unit` to the symbol `"()"`,
//! so the generic `_` arm's `trait_impl_set.contains((trait, "()"))`
//! check works without special-casing).
//!
//! The test below is a source-level assertion: it reads the arm in
//! inference.rs and pins the exact membership (Error, Never; NO Unit).
//! A behavioral test would require either a user-reachable `trait X
//! for ()` impl (grammar doesn't permit it yet) or a constructed
//! `MethodEntry` whose `method_constraints` list is non-empty on a
//! Unit receiver — neither is reachable through the public API. The
//! source-level lock prevents anyone from silently re-adding `Unit` to
//! the skip arm (which would be a silent regression of this audit fix).
//!
//! Prior-fix log: /tmp/prior_fix_log_r24.txt has no prior round
//! touching this arm. Round 23 touched `dispatch_method_entry`'s
//! doc-comments but not the match body. Round 22 added the
//! `instantiate_method_type` companion used to allocate fresh TyVars
//! per call site (src/typechecker/mod.rs:615), which is the mechanism
//! that makes the Unit fallthrough safe — constraints on Unit-typed
//! receivers get checked the same way as constraints on any other
//! monomorphic receiver.
//!
//! Prior rounds 15/17/19/22 all touched adjacent code in
//! `dispatch_method_entry` / `instantiate_method_entry` / the
//! `pending_where_constraints` machinery. This test is orthogonal to
//! those — it pins the specific arm, not the broader dispatch flow.

use std::fs;
use std::path::PathBuf;

/// Find src/typechecker/inference.rs relative to CARGO_MANIFEST_DIR.
fn inference_source() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    path.push("typechecker");
    path.push("inference.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Pin the exact membership of the `dispatch_method_entry` skip arm.
/// The arm must skip Error and Never (prior-errors / divergence
/// propagation) but NOT Unit.
#[test]
fn dispatch_method_entry_skip_arm_does_not_include_unit() {
    let src = inference_source();

    // The arm we're pinning. Searching for the exact byte string is
    // fragile to whitespace changes but the file uses a consistent
    // style so this is stable; if formatting ever shifts, the test
    // will fail loudly — that's the correct behavior (forces the
    // author to re-verify the fix is still in place).
    let broken_arm = "Type::Error | Type::Never | Type::Unit => {}";
    assert!(
        !src.contains(broken_arm),
        "src/typechecker/inference.rs still contains the pre-fix skip arm '{}'. \
         Round-24 LATENT fix requires that `Type::Unit` NOT be in the \
         dispatch_method_entry skip arm — Unit must fall through to \
         the normal trait-constraint validation path. \
         (Error and Never remain in the skip arm; they propagate prior \
         errors / divergence.)",
        broken_arm
    );

    // Positively verify the fixed arm is present. Also serves as a
    // sanity check that the test is reading the right file (i.e. the
    // arm itself still exists in some form).
    let fixed_arm = "Type::Error | Type::Never => {}";
    assert!(
        src.contains(fixed_arm),
        "src/typechecker/inference.rs no longer contains the expected \
         skip arm '{}'. If the match structure was legitimately \
         refactored, update this test; otherwise the Round-24 fix may \
         have been reverted.",
        fixed_arm
    );
}

/// Second lock: the skip arm must live inside `dispatch_method_entry`.
/// Ensures that if the function is ever renamed/split, the test catches
/// it rather than silently passing against an unrelated match somewhere
/// else in the file.
#[test]
fn dispatch_method_entry_function_still_contains_the_skip_arm() {
    let src = inference_source();

    // Locate the function header.
    let fn_header_idx = src.find("pub(super) fn dispatch_method_entry(").expect(
        "dispatch_method_entry function not found in \
             src/typechecker/inference.rs — if it was renamed or \
             moved, update this regression test and confirm the \
             Round-24 fix is still applied at the new location.",
    );

    // Slice from the header to end-of-file; the skip arm must appear
    // inside this slice. We don't try to find the function's exact
    // closing brace — any match before the next `pub(super) fn` or
    // `pub fn` is safe because match bodies nest.
    let tail = &src[fn_header_idx..];
    let next_fn_idx = tail[1..]
        .find("\n    pub(super) fn ")
        .map(|i| i + 1)
        .or_else(|| tail[1..].find("\n    pub fn ").map(|i| i + 1))
        .unwrap_or(tail.len());
    let fn_body = &tail[..next_fn_idx];

    let fixed_arm = "Type::Error | Type::Never => {}";
    assert!(
        fn_body.contains(fixed_arm),
        "dispatch_method_entry function body no longer contains the \
         expected skip arm '{}'. The Round-24 fix may have been \
         reverted or the match structure refactored in a way that \
         needs re-verification.",
        fixed_arm
    );

    // And the broken form must NOT reappear inside the function.
    let broken_arm = "Type::Error | Type::Never | Type::Unit => {}";
    assert!(
        !fn_body.contains(broken_arm),
        "dispatch_method_entry function body has regressed to the \
         pre-fix skip arm '{}'. Round-24 LATENT fix requires Unit to \
         fall through to normal trait-constraint validation.",
        broken_arm
    );
}

/// Third lock: `type_name_for_impl` in src/typechecker/mod.rs must
/// still map Type::Unit to a non-None symbol, otherwise the fallthrough
/// path silently drops Unit on the floor (Type::Var match arm only
/// fires for unresolved tyvars; the generic `_` arm relies on
/// `type_name_for_impl` returning Some). If that mapping ever regresses
/// to None, the fallthrough would no-op on Unit — re-introducing the
/// same latent hole by a different route.
#[test]
fn type_name_for_impl_maps_unit_to_some_symbol() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    path.push("typechecker");
    path.push("mod.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));

    // The mapping we rely on.
    let expected = r#"Type::Unit => Some(intern("()"))"#;
    assert!(
        src.contains(expected),
        "src/typechecker/mod.rs no longer contains the expected \
         Type::Unit => Some(intern(\"()\")) mapping in \
         type_name_for_impl. Round-24 LATENT fix relies on this \
         mapping: once Unit falls through dispatch_method_entry's \
         match, the trait-constraint check for a concrete Unit \
         receiver uses type_name_for_impl to look up the impl. If \
         this ever returns None for Unit, the check silently no-ops."
    );
}
