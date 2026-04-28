//! Phase B locks for the parser surface syntax of effect annotations.
//!
//! See `docs/proposals/effect-rows.md` (Part 7) for the rollout plan.
//! Phase B introduces the `!{set}` annotation slot between the return
//! type and the function body. Absent → `EffectSet::TOP` (the gradual-
//! rollout permissive default), so every existing program parses
//! identically. These tests lock the surface grammar — the parser
//! recognises the five baked-in effects, rejects unknowns with a
//! diagnostic that names the valid set, and tolerates whitespace
//! variations.

use silt::ast::Decl;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::types::effects::{Effect, EffectSet};

fn parse(src: &str) -> silt::ast::Program {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    Parser::new(tokens).parse_program().expect("parse error")
}

fn first_fn_effects(src: &str) -> EffectSet {
    let prog = parse(src);
    for d in &prog.decls {
        if let Decl::Fn(f) = d {
            return f.declared_effects;
        }
    }
    panic!("no fn decl in source");
}

// ── 1. Empty annotation parses to EMPTY ────────────────────────────

#[test]
fn parse_pure_annotation() {
    // `!{}` is the "fully pure declared" form. Distinct from `!*`
    // (the gradual-rollout default) at the Display layer; here the
    // parser produces the bitset-equivalent `EffectSet::EMPTY`.
    let got = first_fn_effects("fn f() -> Int !{} = 0");
    assert_eq!(got, EffectSet::EMPTY);
}

// ── 2. Single-effect annotation ────────────────────────────────────

#[test]
fn parse_single_effect() {
    let got = first_fn_effects("fn f() -> Int !{io} = 0");
    assert_eq!(got, EffectSet::singleton(Effect::Io));
}

// ── 3. Multi-effect annotation ─────────────────────────────────────

#[test]
fn parse_multi_effect() {
    let got = first_fn_effects("fn f() -> Int !{io, fs} = 0");
    let expected = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
    assert_eq!(got, expected);
}

// ── 4. Unknown effect emits a diagnostic ───────────────────────────

#[test]
fn parse_unknown_effect_emits_diagnostic() {
    // The parser should reject `xyzzy` and produce a message that lists
    // the valid effect set so users can correct the typo.
    let src = "fn f() -> Int !{xyzzy} = 0";
    let tokens = Lexer::new(src).tokenize().expect("lexer ok");
    let result = Parser::new(tokens).parse_program();
    let err = result.expect_err("expected parse error for unknown effect");
    let msg = err.message;
    assert!(
        msg.contains("xyzzy"),
        "diagnostic should name the offending token; got: {msg}"
    );
    // The list of valid effects must appear so users have a fix-it.
    assert!(msg.contains("io"), "diagnostic should list 'io': {msg}");
    assert!(msg.contains("fs"), "diagnostic should list 'fs': {msg}");
    assert!(msg.contains("net"), "diagnostic should list 'net': {msg}");
    assert!(msg.contains("time"), "diagnostic should list 'time': {msg}");
    assert!(
        msg.contains("random"),
        "diagnostic should list 'random': {msg}"
    );
}

// ── 5. Absent annotation → TOP ─────────────────────────────────────

#[test]
fn absent_annotation_defaults_to_top() {
    // The whole point of the gradual rollout: every existing legacy
    // function — no annotation — defaults to TOP, i.e. "any effect
    // permitted, no enforcement".
    let got = first_fn_effects("fn f() -> Int = 0");
    assert_eq!(got, EffectSet::TOP);
}

// ── 6. Whitespace tolerance ────────────────────────────────────────

#[test]
fn whitespace_tolerated() {
    // Three formattings of the same set must produce the same parse.
    let a = first_fn_effects("fn f() -> Int !{io,fs} = 0");
    let b = first_fn_effects("fn f() -> Int !{ io , fs } = 0");
    let c = first_fn_effects("fn f() -> Int !{io, fs} = 0");
    assert_eq!(a, b);
    assert_eq!(b, c);
    let expected = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
    assert_eq!(a, expected);
}

// ── 7. Block-body fn with annotation ───────────────────────────────

#[test]
fn parse_annotation_with_block_body() {
    // The annotation slot must work for both the `= expr` form and the
    // `{ block }` form, since the body parser branches on which one
    // appears.
    let got = first_fn_effects("fn f() -> Int !{io} { 0 }");
    assert_eq!(got, EffectSet::singleton(Effect::Io));
}

// ── 7b. Annotation without a return type is also accepted ─────────

#[test]
fn annotation_attaches_without_return_type() {
    // The grammar puts the annotation slot after the optional
    // return type arrow, but if the user omits the return type
    // entirely and writes the annotation directly after the param
    // list (`fn f() !{io} = 0`), we accept it — symmetry with the
    // lambda form `fn() !{io} { ... }` and matches the practical
    // use case of "function returns Unit but performs IO".
    let got = first_fn_effects("fn f() !{io} = 0");
    assert_eq!(got, EffectSet::singleton(Effect::Io));
}
