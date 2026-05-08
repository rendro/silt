//! Round-72 BLOAT-DUP cleanup locks.
//!
//! Three LATENT findings collapsed in this round:
//!
//! - **L3:** `compiler::format_module_source_error` re-implemented gutter
//!   width math via a hand-rolled `while n >= 10` loop instead of calling
//!   `errors::line_num_width`. The justification ("private helper") was
//!   stale — the helper is now `pub(crate)` and the loop is gone.
//!
//! - **L4:** A comment in `compiler/mod.rs` claimed
//!   `clamp_span_to_module_source` was a duplicate of a "private to
//!   errors.rs" helper. The helper was lifted to `pub(crate)` in a
//!   prior round; the wrapper became a one-line forwarder. This round
//!   inlines the forwarder at the single call site so the stale
//!   "private to errors.rs" wording cannot drift.
//!
//! - **L5:** `synth_span` was defined twice (in
//!   `typechecker::exhaustiveness` and `typechecker::auto_derive`),
//!   each producing the same `Span::new(0, 0)` shape. Collapsed into
//!   `Span::synthetic()` on `impl Span` in `src/lexer.rs`.
//!
//! Source-grep style: the round-72 audit prompt explicitly recommends
//! source-grep locks over tautological format-the-error tests. These
//! locks pin that the deletions/refactors were semantic no-ops by
//! checking the unique substrings that used to identify the duplicated
//! sites are gone, and that the canonical names are present.

const COMPILER_MOD_SRC: &str = include_str!("../src/compiler/mod.rs");
const ERRORS_SRC: &str = include_str!("../src/errors.rs");
const LEXER_SRC: &str = include_str!("../src/lexer.rs");
const EXHAUSTIVENESS_SRC: &str = include_str!("../src/typechecker/exhaustiveness.rs");
const AUTO_DERIVE_SRC: &str = include_str!("../src/typechecker/auto_derive.rs");

// ── L3: gutter-width math collapsed onto errors::line_num_width ──────

#[test]
fn l3_inline_gutter_loop_is_gone_from_compiler_mod() {
    // The hand-rolled `floor(log10) + 1` loop justification is gone.
    assert!(
        !COMPILER_MOD_SRC.contains("inlined to avoid pulling"),
        "src/compiler/mod.rs still contains the stale justification \
         comment for the inlined gutter-width loop. The loop was \
         supposed to be replaced by a call to `errors::line_num_width` \
         in round-72 (L3)."
    );
    // The body of the deleted loop is gone too. `n /= 10;` inside a
    // gutter-width context is the unique fingerprint of the loop.
    assert!(
        !COMPILER_MOD_SRC.contains("while n >= 10"),
        "src/compiler/mod.rs still contains the hand-rolled gutter-width \
         loop body (`while n >= 10`). Round-72 (L3) replaces it with a \
         call to `errors::line_num_width`."
    );
}

#[test]
fn l3_compiler_mod_calls_line_num_width_helper() {
    assert!(
        COMPILER_MOD_SRC.contains("crate::errors::line_num_width"),
        "src/compiler/mod.rs::format_module_source_error must call \
         `crate::errors::line_num_width` after the round-72 L3 fix \
         instead of re-implementing the math inline."
    );
}

#[test]
fn l3_line_num_width_is_pub_crate_in_errors() {
    assert!(
        ERRORS_SRC.contains("pub(crate) fn line_num_width"),
        "src/errors.rs::line_num_width must be `pub(crate)` after the \
         round-72 L3 fix so the compiler-side gutter math can delegate \
         to the shared helper."
    );
}

// ── L4: stale "private to errors.rs" wording is gone ─────────────────

#[test]
fn l4_stale_private_helper_comment_is_gone() {
    assert!(
        !COMPILER_MOD_SRC.contains("private to errors.rs"),
        "src/compiler/mod.rs still references a `private to errors.rs` \
         helper. That helper was lifted to `pub(crate)` in round-71; the \
         round-72 L4 fix removes the stale wording (and inlines the \
         forwarder at the call site)."
    );
}

#[test]
fn l4_call_site_uses_canonical_clamp_helper() {
    // Whether the wrapper was inlined or kept, the canonical
    // `crate::errors::clamp_span_to_source(span, source)` substring
    // must appear — that's the lock that guarantees we're using the
    // shared helper, not a re-copy.
    assert!(
        COMPILER_MOD_SRC.contains("crate::errors::clamp_span_to_source(span, source)"),
        "src/compiler/mod.rs must call `crate::errors::clamp_span_to_source` \
         directly. Round-72 L4 inlines the one-line forwarder so there is \
         no opportunity for the helper to drift back into a duplicate."
    );
}

// ── L5: synth_span collapsed onto Span::synthetic ────────────────────

#[test]
fn l5_synth_span_definition_gone_from_exhaustiveness() {
    assert!(
        !EXHAUSTIVENESS_SRC.contains("fn synth_span"),
        "src/typechecker/exhaustiveness.rs still defines a local \
         `fn synth_span`. Round-72 L5 collapses both copies onto \
         `Span::synthetic()` on `impl Span` in src/lexer.rs."
    );
}

#[test]
fn l5_synth_span_definition_gone_from_auto_derive() {
    assert!(
        !AUTO_DERIVE_SRC.contains("fn synth_span"),
        "src/typechecker/auto_derive.rs still defines a local \
         `fn synth_span`. Round-72 L5 collapses both copies onto \
         `Span::synthetic()` on `impl Span` in src/lexer.rs."
    );
}

#[test]
fn l5_no_synth_span_call_sites_left_in_typechecker() {
    // Belt-and-suspenders: even ignoring definitions, no caller should
    // still be referencing the dead helper name. If anything still says
    // `synth_span(`, the rename was incomplete.
    assert!(
        !EXHAUSTIVENESS_SRC.contains("synth_span("),
        "src/typechecker/exhaustiveness.rs still contains call sites of \
         the dead `synth_span` helper. Round-72 L5 must rewrite all \
         call sites to `Span::synthetic()`."
    );
    assert!(
        !AUTO_DERIVE_SRC.contains("synth_span("),
        "src/typechecker/auto_derive.rs still contains call sites of \
         the dead `synth_span` helper. Round-72 L5 must rewrite all \
         call sites to `Span::synthetic()`."
    );
}

#[test]
fn l5_span_synthetic_constructor_present_in_lexer() {
    assert!(
        LEXER_SRC.contains("pub fn synthetic") || LEXER_SRC.contains("pub const SYNTHETIC"),
        "src/lexer.rs::impl Span must expose a `synthetic()` constructor \
         (or `SYNTHETIC` const) after the round-72 L5 fix. This is the \
         single source of truth that replaces the two `synth_span` \
         helpers in the typechecker."
    );
}

#[test]
fn l5_typechecker_files_use_span_synthetic() {
    assert!(
        EXHAUSTIVENESS_SRC.contains("Span::synthetic()"),
        "src/typechecker/exhaustiveness.rs must call `Span::synthetic()` \
         after the round-72 L5 fix."
    );
    assert!(
        AUTO_DERIVE_SRC.contains("Span::synthetic()"),
        "src/typechecker/auto_derive.rs must call `Span::synthetic()` \
         after the round-72 L5 fix."
    );
}

// ── Runtime: exhaustiveness diagnostics still produce sane output ────
//
// Semantic-no-op proof for L5. The synthetic-span shape is consumed by
// the diagnostic renderer (a 0-line span suppresses the source-line
// snippet). If the rename had broken the shape, exhaustiveness errors
// would either crash or render a bogus snippet. This test exercises a
// non-exhaustive match end-to-end and asserts the diagnostic still
// contains the expected wording without panicking.

/// Source string for the L5 runtime exhaustiveness diagnostic test.
/// Promoted to a module-level constant so the structural-syntax lock
/// (`l5_exhaustiveness_test_source_is_valid_silt_syntax`) can grep it
/// without re-declaring the literal.
const L5_NONEXHAUSTIVE_SRC: &str = "\
type Color { Red, Green, Blue }
fn main() {
  let c = Red
  match c {
    Red -> println(\"red\")
    Green -> println(\"green\")
  }
}
";

#[test]
fn l5_exhaustiveness_runtime_still_renders_diagnostic() {
    use std::process::Command;

    // No `tempfile` dep — match the convention used by other CLI
    // integration tests in this suite (e.g. cli_strict_effects_flag_tests).
    //
    // Path basename intentionally NEUTRAL ("match.silt" — not
    // "nonexhaustive.silt"). The diagnostic renderer echoes the
    // source-file path back into the output, so any substring that the
    // assertion below matches against MUST NOT appear in the path —
    // otherwise the assertion is satisfied tautologically by the path
    // echo even if the exhaustiveness diagnostic never runs (round-73
    // L1 finding).
    let dir = std::env::temp_dir().join("silt_round72_bloat_l5");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join("match.silt");
    std::fs::write(&path, L5_NONEXHAUSTIVE_SRC).expect("write temp source");

    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("invoke silt run");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Path-echo guard: if the path basename ever drifts back to
    // something that contains one of the substrings the assertion
    // below grep for, the test would be tautological again. Pin
    // explicitly that the path components do NOT contain any of the
    // diagnostic substrings.
    let path_str = path.to_string_lossy().to_lowercase();
    for needle in ["non-exhaustive", "nonexhaustive", "not exhaustive", "blue"] {
        assert!(
            !path_str.contains(needle),
            "tempfile path {path_str:?} contains substring {needle:?} \
             that the diagnostic assertion grep for; that would make \
             the assertion satisfied by the path echo alone (round-73 \
             L1 lock)."
        );
    }

    let lower = combined.to_lowercase();
    // Tightened assertion (round-73 L1 fix): require BOTH a phrase
    // that names the exhaustiveness failure AND the missing variant
    // name (`Blue`). Combined with `&&` so a generic lex error like
    // `error[lex]: missing semicolon` cannot satisfy the test.
    let mentions_exhaustiveness = lower.contains("non-exhaustive")
        || lower.contains("nonexhaustive")
        || lower.contains("not exhaustive");
    let mentions_missing_variant = lower.contains("blue");
    assert!(
        mentions_exhaustiveness && mentions_missing_variant,
        "Expected an exhaustiveness diagnostic that BOTH names the \
         failure (e.g. `non-exhaustive`) AND names the missing variant \
         (`Blue`). exhaustiveness={mentions_exhaustiveness} \
         missing_variant={mentions_missing_variant}. Got: {combined}"
    );
}

// ── Round-73 L1 structural lock ──────────────────────────────────────
//
// The runtime test above (`l5_exhaustiveness_runtime_still_renders_diagnostic`)
// can only do its job if the silt source it feeds the CLI is VALID
// silt — not Rust. The original round-72 author wrote the source in
// Rust syntax (`enum Color`, `Color::Red`, `;` terminators, `pub fn
// main`), so `silt run` exited at the LEX stage and the exhaustiveness
// renderer was never exercised. The round-73 L1 fix rewrote the
// source as valid silt; this lock pins it so a future edit cannot
// silently drift it back to Rust.

#[test]
fn l5_exhaustiveness_test_source_is_valid_silt_syntax() {
    // Positive markers — silt-specific tokens that MUST be present.
    assert!(
        L5_NONEXHAUSTIVE_SRC.contains("type Color {"),
        "L5 test source must use silt's `type Color {{` ADT declaration, \
         not Rust's `enum Color`. Round-73 L1 fix."
    );
    assert!(
        L5_NONEXHAUSTIVE_SRC.contains("fn main()"),
        "L5 test source must declare `fn main()` (silt has no `pub fn`). \
         Round-73 L1 fix."
    );
    assert!(
        L5_NONEXHAUSTIVE_SRC.contains("Red ->"),
        "L5 test source must use bare variant name `Red ->` in match \
         arms (silt has no `Color::Red` path syntax). Round-73 L1 fix."
    );

    // Negative markers — Rust-isms that MUST NOT creep back in.
    assert!(
        !L5_NONEXHAUSTIVE_SRC.contains("enum Color"),
        "L5 test source must not use Rust's `enum Color` keyword \
         (silt uses `type`). The original round-72 source did, which \
         caused `silt run` to exit at the LEX stage and bypass the \
         exhaustiveness renderer entirely. Round-73 L1 lock."
    );
    assert!(
        !L5_NONEXHAUSTIVE_SRC.contains("Color::"),
        "L5 test source must not use Rust's `Color::Variant` path \
         syntax (silt uses bare variant names). Round-73 L1 lock."
    );
    assert!(
        !L5_NONEXHAUSTIVE_SRC.contains(";"),
        "L5 test source must not use `;` statement terminators \
         (silt rejects them at lex time, which would cause the test \
         to bypass the exhaustiveness renderer). Round-73 L1 lock."
    );
    assert!(
        !L5_NONEXHAUSTIVE_SRC.contains("pub fn"),
        "L5 test source must not use `pub fn` (silt's main is `fn main`). \
         Round-73 L1 lock."
    );

    // Structural: the match must actually be non-exhaustive — i.e.
    // it must list `Red` and `Green` arms but NOT a `Blue` arm. If
    // someone "fixes" the match to be exhaustive, the runtime test
    // becomes meaningless (no diagnostic to assert on).
    assert!(
        L5_NONEXHAUSTIVE_SRC.contains("Red ->"),
        "L5 source must have a `Red ->` arm. Round-73 L1 lock."
    );
    assert!(
        L5_NONEXHAUSTIVE_SRC.contains("Green ->"),
        "L5 source must have a `Green ->` arm. Round-73 L1 lock."
    );
    assert!(
        !L5_NONEXHAUSTIVE_SRC.contains("Blue ->"),
        "L5 source must NOT have a `Blue ->` arm — the whole point of \
         the test is that the match is non-exhaustive in the `Blue` \
         case so the diagnostic mentions the missing variant. \
         Round-73 L1 lock."
    );
}
