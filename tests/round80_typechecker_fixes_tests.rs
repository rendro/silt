//! Round 80 typechecker-side audit fixes — B1+B2 (BROKEN diagnostic
//! formatting), B3 (BROKEN builtin-type shadow guard), and L1
//! (LATENT row-tail merge policy lock). Each fix ships with a
//! regression test here so a future refactor that reverts the fix
//! surfaces as a failing test.
//!
//! ── B1+B2 (BROKEN): Generic-vs-Generic mismatch dropped type args ──
//!
//! `unify`'s `(Type::Generic(n1, a1), Type::Generic(n2, a2))` arm
//! formatted the bare `Symbol` heads (`{n1}` / `{n2}`) on a name
//! mismatch. Two failure modes:
//!   B1: `Bag(String)` vs `Box(Int)` rendered as "expected Box, got
//!       Bag" — the type args were dropped, hiding the parametric
//!       distinction.
//!   B2: A `TypeOf(..)` head leaked through verbatim instead of
//!       rendering as `type X` (the surface form `Type::Display`
//!       special-cases for the user-facing type-descriptor syntax).
//!
//! Fix: format via `{t1}` / `{t2}` so `Type::Display`'s args rendering
//! and TypeOf special-casing apply (matches the catch-all arm at the
//! bottom of `unify`).
//!
//! ── B3 (BROKEN): register_type_decl missing builtin-type shadow guard ──
//!
//! `register_type_decl` had a guard for `TypeOf` and a guard for
//! shadows of *variant* constructors of builtin enums (`Empty`,
//! `Some`, `Ok`, …) but no guard for shadows of builtin scalar /
//! container *type names* (Int, Float, Bool, String, Unit, List,
//! Range, Map, Set, Channel, Tuple, Fn, Fun, Handle, Bytes, ExtFloat,
//! TcpListener, TcpStream — the authoritative list lives at
//! `src/types/builtins.rs::BUILTIN_TYPES`). A user writing
//! `type Int { x: String }` synthesised auto-derived
//! Equal/Compare/Hash/Display impls referencing `self.x` on the
//! *builtin* `Int`, producing 6 unspanned cascade errors of the form
//! "unknown method 'x' on type Int". Fix: add a single early-return
//! diagnostic at declaration time.
//!
//! ── L1 (LATENT): apply row-tail merge policy lock ──
//!
//! Round 76 LATENT T3 changed `apply`'s row-tail merge to existing-
//! wins (`new_fields.entry(n).or_insert(t)`). Round 79 source-grep
//! covers the `debug_assert!` in the *else* branch of the same match;
//! this is the missing source-grep covering the merge-policy line
//! itself. Reverting to `new_fields.insert(n, t)` keeps the suite
//! green today (no production caller hits overlap) but silently
//! diverges from `substitute_vars` again. The grep guards against
//! that drift.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── B1+B2: Generic-vs-Generic mismatch renders args + handles TypeOf ─

/// B1: a parametric type mismatch must include the type arguments
/// in the diagnostic (`Bag(String)` not just `Bag`). Pre-fix the
/// args were dropped because the arm formatted bare `Symbol` heads.
#[test]
fn generic_mismatch_includes_type_args_in_diagnostic() {
    let src = r#"
type Bag(a) { Wrap(a) }
type Box(a) { Pack(a) }
fn want_box(b: Box(Int)) -> Int { 0 }
fn main() -> Int {
  let b: Bag(String) = Wrap("x")
  want_box(b)
}
"#;
    let errs = type_errors(src);
    // Must contain the rendered type with args, not the bare head.
    assert!(
        errs.iter().any(|e| e.contains("Bag(String)") && e.contains("Box(Int)")),
        "round 80 B1 regression: Generic-vs-Generic mismatch should \
         render full parametric form `Bag(String)` and `Box(Int)`; \
         got: {errs:?}"
    );
    // Belt-and-braces: explicitly forbid the args-stripped form.
    assert!(
        !errs.iter().any(|e| {
            // The bug-shape is "expected Box, got Bag" with no args.
            // We look for the substring "expected Box, got Bag" with
            // no parenthesised args attached — tolerate any prefix /
            // suffix but require neither `Box(` nor `Bag(` in the
            // exact failing message.
            e.contains("expected Box, got Bag")
                && !e.contains("Box(")
                && !e.contains("Bag(")
        }),
        "round 80 B1 regression: diagnostic dropped type args; got: {errs:?}"
    );
}

/// B2 (source-level lock): the `(Generic, Generic)` mismatch arm of
/// `unify` in `src/typechecker/mod.rs` must format via the parent
/// `Type` values (`{t1}` / `{t2}`), NOT via the bare `Symbol` heads
/// (`{n1}` / `{n2}`). Pre-fix the arm leaked the internal `TypeOf`
/// head verbatim whenever a TypeOf-vs-other-Generic mismatch landed
/// on this arm (the catch-all sister arm at the bottom of `unify`
/// already used `{t1}` / `{t2}` correctly — this lock guards that
/// the two diagnostic shapes stay aligned).
///
/// A pure behavioural test would require a program shape that fires
/// Generic-vs-Generic with `n1 != n2` where one head is `TypeOf`;
/// such shapes exist only in narrow corners (e.g. type-descriptor
/// arguments being unified against a non-descriptor Generic), so a
/// source-level lock is the load-bearing assertion. The B1 test
/// above is the behavioural counterpart that exercises the same arm.
#[test]
fn generic_mismatch_arm_uses_type_display_not_symbol_display() {
    let mod_src = include_str!("../src/typechecker/mod.rs");
    // Locate the Generic-vs-Generic arm by its uniquely identifying
    // pattern. Then assert the formatter inside it uses `{t1}` /
    // `{t2}` (Type::Display, special-cases TypeOf) rather than
    // `{n1}` / `{n2}` (Symbol::Display, which drops args and leaks
    // TypeOf).
    let arm_anchor = "(Type::Generic(n1, a1), Type::Generic(n2, a2)) =>";
    let arm_pos = mod_src
        .find(arm_anchor)
        .expect("Generic-vs-Generic unify arm not found");
    // Scan a short window after the arm header for the format!
    // call. The arm-body for the n1 != n2 branch is well under 500
    // bytes from the anchor.
    let window = &mod_src[arm_pos..arm_pos + 800];
    assert!(
        window.contains("\"type mismatch: expected {t2}, got {t1}\""),
        "round 80 B2 regression: Generic-vs-Generic mismatch arm \
         must format via parent Type values `{{t2}} / {{t1}}` so \
         Type::Display's args rendering and TypeOf special-casing \
         apply; window: {window:?}"
    );
    // Belt-and-braces: forbid the pre-fix Symbol-formatted shape.
    assert!(
        !window.contains("\"type mismatch: expected {n2}, got {n1}\""),
        "round 80 B2 regression: forbidden pre-fix Symbol-formatted \
         diagnostic `expected {{n2}}, got {{n1}}` is back; this drops \
         type args and leaks `TypeOf`. Window: {window:?}"
    );
}

/// B2 (Display lock): companion to the source-grep above. Confirms
/// that `Type::Display` does in fact render `Generic(TypeOf, [Person])`
/// as `type Person` — the property the source-grep above relies on.
/// If `Type::Display` ever loses the TypeOf special-case, this test
/// breaks loudly even if the source-grep still passes.
#[test]
fn typeof_head_renders_as_surface_type_form() {
    use silt::intern::intern;
    use silt::types::Type;

    // Build `Type::Generic("TypeOf", [Generic("Person", [])])` —
    // exactly what flows through the `(Generic, Generic)` arm when a
    // user-declared record `Person` is used as a type descriptor.
    let person = Type::Generic(intern("Person"), vec![]);
    let typeof_person = Type::Generic(intern("TypeOf"), vec![person]);
    let rendered = format!("{typeof_person}");
    assert_eq!(
        rendered, "type Person",
        "round 80 B2 regression: TypeOf head must render as `type X`; \
         got `{rendered}`"
    );
    assert!(
        !rendered.contains("TypeOf"),
        "round 80 B2 regression: internal `TypeOf` head leaked into \
         Display output; got `{rendered}`"
    );
}

// ── B3: builtin-type-name shadow rejected at declaration ────────────

/// B3 case 1: shadowing a builtin scalar `Int`. Pre-fix: cascade of
/// unspanned "unknown method 'x' on type Int" errors from auto-derive.
/// Post-fix: a single clear diagnostic at the declaration site.
#[test]
fn type_decl_shadowing_builtin_int_is_rejected_at_declaration() {
    let src = r#"
type Int { x: String }
fn main() {}
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("shadows builtin type 'Int'")),
        "round 80 B3 regression: `type Int {{ ... }}` should be \
         rejected with a builtin-shadow diagnostic at declaration \
         time; got: {errs:?}"
    );
    // Post-fix the cascade must be suppressed (the declaration
    // never lands in the type table, so no auto-derive runs against
    // it). Pre-fix this assertion fails because 6 cascade errors
    // referencing `unknown method 'x' on type Int` fire.
    assert!(
        !errs.iter().any(|e| e.contains("unknown method 'x' on type Int")),
        "round 80 B3 regression: builtin-shadow guard should suppress \
         the auto-derive cascade; got: {errs:?}"
    );
}

/// B3 case 2: shadowing a builtin scalar `String`.
#[test]
fn type_decl_shadowing_builtin_string_is_rejected() {
    let src = r#"
type String { x: Int }
fn main() {}
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("shadows builtin type 'String'")),
        "round 80 B3 regression: `type String {{ ... }}` should be \
         rejected; got: {errs:?}"
    );
}

/// B3 case 3: shadowing a builtin container `List`.
#[test]
fn type_decl_shadowing_builtin_list_is_rejected() {
    let src = r#"
type List { x: Int }
fn main() {}
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("shadows builtin type 'List'")),
        "round 80 B3 regression: `type List {{ ... }}` should be \
         rejected; got: {errs:?}"
    );
}

// ── L1: row-tail merge policy source-grep lock ──────────────────────

/// L1: `apply` in `src/typechecker/mod.rs` must use the existing-wins
/// merge policy `new_fields.entry(n).or_insert(t)` for row-tail
/// merging. Reverting to `new_fields.insert(n, t)` keeps the test
/// suite green today (no production caller exercises overlap) but
/// silently diverges from `substitute_vars` again — the divergence
/// that round 76 LATENT T3 fixed. Round 79's source-grep only
/// covered the `debug_assert!` in the else branch, not this line.
#[test]
fn apply_row_tail_merge_uses_existing_wins_policy() {
    let mod_src = include_str!("../src/typechecker/mod.rs");
    // The exact form on line 923 (round 80 baseline). If the line
    // gets reformatted (e.g. extra spaces, line break), update the
    // needle to match — the load-bearing assertion is the
    // `entry(...).or_insert(...)` shape.
    let needle = "new_fields.entry(n).or_insert(t)";
    assert!(
        mod_src.contains(needle),
        "round 80 L1 regression: apply's row-tail merge must use \
         the existing-wins policy `{needle}`; pre-round-76 wording \
         `new_fields.insert(n, t)` is forbidden. This drift would \
         silently diverge `apply` from `substitute_vars` (see round \
         76 LATENT T3 + round 79 TS-L2 for context)."
    );
    // Belt-and-braces: explicitly reject the pre-fix wording so a
    // half-finished revert that leaves both forms present still
    // trips this lock.
    let forbidden = "new_fields.insert(n, t)";
    assert!(
        !mod_src.contains(forbidden),
        "round 80 L1 regression: forbidden pre-round-76 merge form \
         `{forbidden}` is back in src/typechecker/mod.rs"
    );
}
