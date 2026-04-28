//! Phase A locks for effect-set inference.
//!
//! See `docs/proposals/effect-rows.md` (Part 7) for the rollout plan.
//! Phase A is plumbing-only: every existing program defaults to
//! `EffectSet::TOP` so legacy code keeps typechecking unchanged. These
//! tests exercise the inference helpers and the public accessor on
//! `TypeChecker` that future phases (LSP hover, `--strict-effects`)
//! will read from.
//!
//! Phase B adds parser surface syntax (`!{io}`); Phase C annotates
//! every stdlib builtin with its real effect set. Until then all
//! builtins carry `TOP`, so any function that calls a builtin infers
//! `TOP`. The pure-arithmetic / pattern-match / lambda-purity tests
//! below survive that limitation by avoiding builtin calls.

use silt::intern::intern;
use silt::typechecker::TypeChecker;
use silt::types::effects::{Effect, EffectSet};

/// Drive the type checker on a source string and return the populated
/// `TypeChecker` so the caller can introspect `fn_body_effects_for`.
/// Panics on lex/parse errors so the tests stay focused on inference.
fn check(src: &str) -> TypeChecker {
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let mut checker = TypeChecker::new();
    checker.check_program(&mut program);
    checker
}

// ── 1. Pure functions infer EMPTY ──────────────────────────────────

#[test]
fn pure_function_infers_empty_effects() {
    // Pure arithmetic — no calls, no builtins. The inferred set must
    // be exactly `EMPTY`, not `TOP`. If this flips to `TOP`, the
    // bottom-up walk is contaminating with the gradual-rollout default
    // somewhere it shouldn't.
    let checker = check("fn add(x: Int, y: Int) -> Int { x + y }");
    let got = checker.fn_body_effects_for(intern("add")).expect("registered");
    assert_eq!(got, EffectSet::EMPTY, "pure function must infer EMPTY");
}

// ── 2. Transitive purity ───────────────────────────────────────────

#[test]
fn function_calling_pure_function_infers_empty() {
    // `outer` calls only `inner`, which is pure. Transitivity means
    // `outer` should also infer EMPTY — the call-effect lookup goes
    // through `Scheme::effects`, but `inner` is registered with the
    // gradual-rollout `TOP` default at `Scheme` construction time, NOT
    // its inferred body. Phase A only stores inferred body effects in
    // the side-channel map; the scheme stays `TOP`. That's intentional
    // for backwards compatibility, but it means transitive purity
    // tracking through `Scheme::effects` doesn't kick in until Phase
    // D when the default flips to inference-derived.
    //
    // For this test: confirm the stored body effects are computed.
    // The transitive chain shows `outer` infers TOP because `inner`
    // looks up to TOP (gradual default). The body inference walks
    // both correctly — this test locks the Phase A behaviour, not
    // the Phase D behaviour.
    let checker = check(
        r#"
fn inner(x: Int) -> Int { x + 1 }
fn outer(x: Int) -> Int { inner(x) }
"#,
    );
    // inner's body uses no calls — pure.
    let inner_effects = checker
        .fn_body_effects_for(intern("inner"))
        .expect("inner registered");
    assert_eq!(
        inner_effects,
        EffectSet::EMPTY,
        "inner has only arithmetic — must infer EMPTY"
    );
    // outer calls inner. Phase A reads `inner`'s scheme effects (TOP
    // by default), so outer's inferred body effects becomes TOP.
    // Phase D will tighten this; for now it's the documented
    // backwards-compat behaviour.
    let outer_effects = checker
        .fn_body_effects_for(intern("outer"))
        .expect("outer registered");
    assert_eq!(
        outer_effects,
        EffectSet::TOP,
        "outer calls a TOP-default function — must propagate TOP"
    );
}

// ── 3. Calls into TOP-defaulted builtins infer TOP ─────────────────

#[test]
fn function_calling_top_builtin_infers_top() {
    // Phase A leaves every stdlib builtin at TOP; Phase C will narrow
    // them. So a function that calls `io.read_file` infers TOP — the
    // gradual rollout default. This is the load-bearing
    // backwards-compat invariant.
    let checker = check(
        r#"
fn read_thing() -> String { io.read_file("foo") }
"#,
    );
    let effects = checker
        .fn_body_effects_for(intern("read_thing"))
        .expect("read_thing registered");
    assert_eq!(
        effects,
        EffectSet::TOP,
        "call to TOP-default builtin must propagate TOP"
    );
}

// ── 4. Union of two TOP builtins is still TOP ──────────────────────

#[test]
fn effect_set_union_two_top_calls_is_top() {
    let checker = check(
        r#"
fn read_two() -> String {
    let a = io.read_file("a")
    let b = io.read_file("b")
    a
}
"#,
    );
    let effects = checker
        .fn_body_effects_for(intern("read_two"))
        .expect("read_two registered");
    assert_eq!(
        effects,
        EffectSet::TOP,
        "two TOP-default calls must union to TOP"
    );
}

// ── 5. Lambda body's effects don't propagate to outer scope ────────

#[test]
fn lambda_body_does_not_leak_effects_to_outer_scope() {
    // `outer` constructs a lambda whose body calls a TOP-default
    // builtin, but `outer` never *invokes* the lambda. So `outer`'s
    // inferred set must stay EMPTY — the lambda is a value, not an
    // executed effect. Calling the lambda would propagate the
    // effects; constructing it does not.
    //
    // Phase A documented limitation: if the lambda is later passed
    // to a function that calls it, we conservatively widen to TOP
    // because we don't track lambda-effect rows through values yet.
    // Here `outer` doesn't even bind it — the lambda is the entire
    // body and is returned.
    let checker = check(
        r#"
fn outer() -> Fn() -> String {
    { -> io.read_file("x") }
}
"#,
    );
    let effects = checker
        .fn_body_effects_for(intern("outer"))
        .expect("outer registered");
    assert_eq!(
        effects,
        EffectSet::EMPTY,
        "constructing a lambda must not leak the body's effects upward"
    );
}

// ── 6. Pattern match arms unify effects ────────────────────────────

#[test]
fn pattern_match_arms_unify_effects() {
    // Two arms, both pure → union is EMPTY. The earlier arms-as-source
    // bug would have inferred TOP if any arm-walking path silently
    // bailed; this test locks the union.
    let checker = check(
        r#"
fn pick(x: Int) -> Int {
    match x {
        0 -> 1
        n -> n + 1
    }
}
"#,
    );
    let effects = checker
        .fn_body_effects_for(intern("pick"))
        .expect("pick registered");
    assert_eq!(
        effects,
        EffectSet::EMPTY,
        "two pure match arms must union to EMPTY"
    );
}

// ── 7. Display: io + fs renders alphabetically ─────────────────────

#[test]
fn effect_display_renders_io_fs_form() {
    // Insertion order is `io` then `fs` — output must come out
    // alphabetically as `!{fs, io}` so hover tips and diagnostics
    // are stable across inference paths.
    let s = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
    assert_eq!(s.to_string(), "!{fs, io}");
}

// ── 8. Display: TOP renders as `!*` ────────────────────────────────

#[test]
fn effect_top_renders_star() {
    assert_eq!(EffectSet::TOP.to_string(), "!*");
}

// ── 9. Display: EMPTY renders as `!{}` ─────────────────────────────

#[test]
fn effect_empty_renders_empty_braces() {
    assert_eq!(EffectSet::EMPTY.to_string(), "!{}");
}

// ── 10. Subset semantics ───────────────────────────────────────────

#[test]
fn effect_set_subset_check() {
    // Singleton ⊆ TOP.
    assert!(EffectSet::singleton(Effect::Io).is_subset(EffectSet::TOP));
    // TOP ⊄ EMPTY.
    assert!(!EffectSet::TOP.is_subset(EffectSet::EMPTY));
    // EMPTY ⊆ singleton.
    assert!(EffectSet::EMPTY.is_subset(EffectSet::singleton(Effect::Fs)));
    // io ⊄ {fs} (different bit positions).
    assert!(!EffectSet::singleton(Effect::Io).is_subset(EffectSet::singleton(Effect::Fs)));
    // {io, fs} ⊆ {io, fs, net}.
    let small = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
    let big = small.insert(Effect::Net);
    assert!(small.is_subset(big));
    assert!(!big.is_subset(small));
}
