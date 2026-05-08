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

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

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

    // The mapping we rely on. Round-75 TYPE-3 flipped the Unit
    // canonical direction from `Unit → ()` to `() → Unit` so VM
    // dispatch (`dispatch_name_for_value(&Value::Unit) = "Unit"`)
    // and trait-impl registration agree on the same key. The
    // round-24 LATENT semantics are unchanged: type_name_for_impl
    // must return Some(_) for Type::Unit so the constraint check
    // does not silently no-op; only the canonical name is "Unit"
    // now (post-canonicalize, not "()").
    let expected = r#"Type::Unit => Some(intern("Unit"))"#;
    assert!(
        src.contains(expected),
        "src/typechecker/mod.rs no longer contains the expected \
         Type::Unit => Some(intern(\"Unit\")) mapping in \
         type_name_for_impl. Round-24 LATENT fix relies on this \
         mapping: once Unit falls through dispatch_method_entry's \
         match, the trait-constraint check for a concrete Unit \
         receiver uses type_name_for_impl to look up the impl. If \
         this ever returns None for Unit, the check silently no-ops."
    );
}

/// Behavioural lock for the round-24 dispatch_method_entry Unit-skip arm.
///
/// WHY THE SOURCE-GREP LOCKS ABOVE AREN'T ENOUGH (silent-revert pattern):
/// The two source-grep tests above pin the literal arm
/// `Type::Error | Type::Never => {}` and reject the broken form
/// `Type::Error | Type::Never | Type::Unit => {}`. They do NOT catch
/// semantically-equivalent reverts that use a different syntactic shape,
/// e.g. introducing a separate arm `Type::Unit => return,` (or `=> {}`,
/// or `=> continue,`) earlier in the match. Both source-grep assertions
/// would still pass against that revert because the literal "Error |
/// Never" arm is unchanged.
///
/// WHY THIS TEST CATCHES IT (behavioural counterpart):
/// The original justification at the top of the file said a behavioural
/// test would require either a user-reachable `trait X for ()` impl
/// (the grammar didn't permit it) or a constructed `MethodEntry` with
/// non-empty `method_constraints` on a Unit receiver. That justification
/// is now STALE: round 74 GAP-4 (commit c189e2e) made `trait T for Unit`
/// reachable end-to-end, and round 75 TYPE-3 cemented the canonical
/// "Unit" symbol. There is now a public, user-reachable path.
///
/// This test exercises that path: a parameterized trait `T(c)` is
/// implemented for `Unit` with `c = String`, and a method-level
/// `where a: T(Int)` clause forces dispatch_method_entry to validate
/// the trait-args constraint on a Unit-typed tyvar. With current
/// (correct) source, the Unit case falls through the match's `_` arm
/// and `verify_trait_obligation` rejects the program because Unit
/// impls only `T(String)`, not `T(Int)`. With the silent revert
/// `Type::Unit => return,`, the constraint check is skipped entirely,
/// the program typechecks clean, and the soundness hole is back.
///
/// This pattern mirrors the round-58/round-?? `BROKEN-2` test in
/// `tests/trait_args_where_clause_runtime_tests.rs::impl_method_where_clause_trait_args_runtime`,
/// adapted to substitute Unit for the inner type-arg so the relevant
/// branch in dispatch_method_entry is `Type::Unit`.
#[test]
fn dispatch_method_entry_validates_trait_args_when_inner_is_unit() {
    // The setup:
    //   - `T(c)` is a parameterized trait with one method `m(self) -> c`.
    //   - `Unit` impls only `T(String)`.
    //   - `Box(a)` is a generic carrier; `Foo for Box(a)` declares its
    //     `do_it` method with method-level `where a: T(Int)`.
    //   - `main` calls `b.do_it()` on `b: Box(Unit)`, so at dispatch
    //     time the inner tyvar `a` resolves to `Unit`. The constraint
    //     `a: T(Int)` must be checked against Unit's impls and rejected
    //     because Unit impls `T(String)`, not `T(Int)`.
    //
    // If `dispatch_method_entry`'s skip arm silently re-includes Unit
    // (e.g. `Type::Unit => return,`), the constraint is dropped on the
    // floor and the program typechecks clean — that is the regression
    // this test guards against.
    let src = r#"
trait T(c) { fn m(self) -> c }
trait T(String) for Unit { fn m(self) -> String = "u" }
type Box(a) { Wrap(a) }
trait Foo { fn do_it(self) -> Int }
trait Foo for Box(a) {
    fn do_it(self) -> Int where a: T(Int) = match self {
        Wrap(_) -> 0
    }
}
fn main() {
    let b: Box(Unit) = Wrap(())
    let r: Int = b.do_it()
    println(r)
}
"#;

    let tokens = Lexer::new(src).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let errs: Vec<String> = typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();

    // Primary assertion: typechecking MUST produce at least one error.
    // Pre-revert (current correct source): the constraint `a: T(Int)`
    // resolves with `a := Unit`, hits the `_` arm in dispatch_method_entry,
    // and `verify_trait_obligation` emits an error.
    // Post-revert (`Type::Unit => return`): no error is emitted, the
    // program typechecks clean, and this assertion fires.
    assert!(
        !errs.is_empty(),
        "round-24 behavioural lock: typechecking the `T(Int)` mismatch \
         on a `Box(Unit)` receiver MUST produce at least one error. \
         The constraint `a: T(Int)` is method-level on `Foo for Box(a)`; \
         when `b: Box(Unit)` is dispatched through `do_it`, the inner \
         tyvar `a` resolves to Unit and dispatch_method_entry's match \
         must validate the trait-args bound. If this assertion fires, \
         someone likely re-introduced a Unit-skip arm (e.g. \
         `Type::Unit => return,`) into dispatch_method_entry that \
         the source-grep locks above don't catch. Got no errors."
    );

    // Secondary assertion: the error wording must look like a trait /
    // trait-args mismatch diagnostic, not some unrelated error. We
    // accept either the bare "does not implement trait" form (the
    // verify_trait_obligation fallback) or the parameterized
    // "trait 'T(Int)'" / "T(String)" mismatch form. We're tolerant
    // here on purpose: the exact wording can shift between rounds
    // (and indeed already shifted between round-58 and the
    // parameterized-trait diagnostics), but any of these strings
    // anchors the failure to the trait-constraint path rather than
    // an unrelated diagnostic that happened to fire on the program.
    let trait_related = errs.iter().any(|e| {
        e.contains("does not implement")
            || e.contains("T(Int)")
            || (e.contains("trait") && e.contains("Int"))
            || e.contains("T(String)")
    });
    assert!(
        trait_related,
        "round-24 behavioural lock: at least one error must mention \
         the trait-constraint mismatch (either 'does not implement', \
         'T(Int)', 'trait' + 'Int', or 'T(String)'). Got: {errs:?}"
    );
}
