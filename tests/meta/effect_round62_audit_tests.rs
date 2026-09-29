//! Round 62 audit regression locks for the effect-rows pipeline.
//!
//! The five-bit `EffectSet::TOP` bitset (`0b00011111`, all five effects)
//! is identical at the value level to a hand-written
//! `!{io, fs, net, time, random}` annotation. Every site that pivoted
//! on `== EffectSet::TOP` collapsed those two semantically distinct
//! states (`"user wrote no annotation"` vs
//! `"user wrote all five effects"`) and produced one of:
//!
//!   - B1: formatter silently dropping the user's full annotation on
//!     fmt round-trip.
//!   - B2: --strict-effects flipping a fully-annotated fn to EMPTY,
//!     making every effectful body call an error.
//!   - B3: the strict-effects diagnostic emitting `!*` (the "no
//!     annotation" sigil) inside a `help: annotate as ...` line —
//!     unparseable; the user can't paste it back.
//!   - B4: pass-3 narrowing rebuilding the scheme via `generalize`
//!     (which hardcodes `effects: TOP`), so a callee declared `!{io}`
//!     looked like `!*` to its caller after narrowing.
//!   - B5: `remap_scheme` (cross-module import) hardcoding
//!     `effects: TOP`, dropping the producer's declared effects on
//!     the way to the consumer.
//!   - G7: strict-effects diagnostic single-quoting row syntax —
//!     `'!{IO}'` reads as a single oddly-named effect, not a row.
//!   - L1: inline `["Equal", "Compare", "Hash", "Display"]` literal
//!     in trait_impl_set seeding rather than the
//!     `BUILTIN_AUTO_DERIVED_TRAIT_NAMES` constant defined ten lines
//!     above.
//!
//! Each assertion below is a runtime/CLI lock (silt fmt, silt check,
//! silt check --strict-effects) — not just a typechecker-pass
//! assertion. The locks are intentionally narrow: they pin the user-
//! visible behaviour, not internal implementation choices.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_effect_round62_audit");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── B1: full five-effect annotation survives fmt round-trip ───────────

#[test]
fn fn_with_all_five_effects_annotation_survives_fmt_round_trip() {
    // The user wrote `!{io, fs, net, time, random}`. Bit-equality
    // would collapse this onto `EffectSet::TOP` (the gradual default),
    // and the formatter previously dropped TOP on the floor. With the
    // `is_annotated` pivot in place the user's annotation must round-
    // trip through `silt fmt`. The output is the alphabetised render
    // (matching `Display for EffectSet`'s alphabetic ordering).
    let path = temp_silt_file(
        "b1_full_five_effects",
        r#"fn doit() !{io, fs, net, time, random} {
  println("hi")
}
"#,
    );

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt fmt");

    assert!(
        output.status.success(),
        "silt fmt exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let formatted = fs::read_to_string(&path).expect("read formatted output");
    // The formatter emits effects in alphabetic order: fs, io, net,
    // random, time. A bare `fn doit()` with no `!{...}` is the broken
    // shape we are locking against.
    assert!(
        formatted.contains("!{fs, io, net, random, time}"),
        "expected the full five-effect annotation to survive fmt, got:\n{formatted}"
    );
    // Belt-and-suspenders: the pre-fix bug dropped the annotation
    // entirely. Make sure the formatted output still has *some* `!{`.
    assert!(
        formatted.contains("!{"),
        "fmt dropped the effect annotation entirely:\n{formatted}"
    );
}

// ── B2: --strict-effects honours an explicit five-effect annotation ──

#[test]
fn strict_effects_does_not_mistake_full_annotation_for_pure() {
    // The user wrote `!{io, fs, net, time, random}` — the full set —
    // and the body uses io. The strict-effects flip pass previously
    // tested `declared_effects == EffectSet::TOP` and saw the user's
    // bitset matching, then flipped it to EMPTY. Body inference then
    // produced "fn body uses io but is declared pure" even though the
    // user explicitly opted into every effect. The fix pivots on
    // `is_annotated` instead, which is true for any literal `!{...}`.
    let path = temp_silt_file(
        "b2_full_annotation_strict",
        r#"fn doit() !{io, fs, net, time, random} {
  println("hi")
}
fn main() !{io, fs, net, time, random} {
  doit()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    assert!(
        output.status.success(),
        "silt check --strict-effects rejected a fully-annotated fn:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── B3: strict-effects suggestion uses parseable `!{...}`, never `!*` ──

#[test]
fn strict_effects_suggestion_uses_parseable_brace_syntax() {
    // Under --strict-effects, an unannotated fn whose body infers a
    // wide effect set used to emit `help: annotate as `fn doit()
    // !*``. The `!*` token is the "no annotation" sigil — the parser
    // does not accept it as an annotation. The user pasted it back
    // and silt rejected it. Lock that the help line carries `!{`
    // (the parseable brace syntax) rather than `!*`.
    let path = temp_silt_file(
        "b3_no_star_in_help",
        r#"fn doit() {
  println("hi")
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    // We expect a non-zero exit (the body uses io but the fn is
    // unannotated → strict-mode flips it to pure → body exceeds).
    assert!(
        !output.status.success(),
        "silt check --strict-effects accepted a body whose effects exceed implicit pure"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    // Shape lock: the diagnostic must contain at least one `!{` (the
    // parseable annotation form). The pre-fix bug was that the
    // suggestion line contained `!*` and no `!{` at all.
    assert!(
        combined.contains("!{"),
        "expected diagnostic to mention `!{{` somewhere; got:\n{combined}"
    );
    // Stronger lock: the help-line phrase ` !*` (with leading space,
    // i.e. `... !*` as it would appear after the fn signature) must
    // not appear inside the suggestion. Other `!*` mentions inside
    // long-form documentation links are not a concern; in practice the
    // current diagnostic does not include `!*` anywhere in its body
    // text after the fix, so we check for the suggestion-shape
    // substring directly.
    assert!(
        !combined.contains("annotate as `") || !combined.contains("!*`"),
        "the `annotate as ...` help line still contains the unparseable `!*`:\n{combined}"
    );
}

// ── B3b: source-grep lock that format_suggested_fn_header avoids `!*` ──

#[test]
fn format_suggested_fn_header_does_not_emit_top_star_literal() {
    // Read the inference module and confirm the helper that builds
    // the `annotate as ...` suggestion explicitly special-cases
    // `EffectSet::TOP` to avoid the `!*` sigil. The audit's worry is
    // that a future edit reintroduces `header.push_str(&format!(" {}",
    // effects))` without the guard. We lock the presence of the
    // explicit-set fallback string — a rough but durable shape check.
    let src = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/typechecker/inference.rs"),
    )
    .expect("read inference.rs");
    assert!(
        src.contains("!{fs, io, net, random, time}"),
        "format_suggested_fn_header lost its TOP→explicit-set fallback; \
         a `!*` sigil could leak back into the help line"
    );
}

// ── B4: narrowing path preserves a callee's declared !{io} ────────────

#[test]
fn narrowed_fn_preserves_declared_effects() {
    // The reproducer from the audit. `doit` is declared `!{io}` with
    // no return type (so pass-3 narrowing kicks in to refine its
    // scheme). Before the fix, `generalize` rebuilt the scheme with
    // `effects: TOP` and `env.define` clobbered the original `!{io}`.
    // `main`'s body subset check then saw the call as carrying
    // every effect, and reported `!{fs}` (or any other) as
    // undeclared — even though `main` declared `!{io}` and `doit`'s
    // declared set was a subset.
    let path = temp_silt_file(
        "b4_narrowed_preserves_io",
        r#"fn doit() !{io} {
  println("hi")
}
fn main() !{io} {
  doit()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt check");

    assert!(
        output.status.success(),
        "silt check rejected a !{{io}} caller of a narrowed !{{io}} callee:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── B4b: same setup under --strict-effects ────────────────────────────

#[test]
fn narrowed_fn_strict_effects_preserves_inferred() {
    // Even with --strict-effects on, the narrowing path must still
    // copy the original scheme's `effects` field across. The flag
    // toggles UNANNOTATED fns to pure — it does not change how the
    // narrowing rebuilds existing schemes.
    let path = temp_silt_file(
        "b4_narrowed_strict_io",
        r#"fn doit() !{io} {
  println("hi")
}
fn main() !{io} {
  doit()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    assert!(
        output.status.success(),
        "--strict-effects rejected a !{{io}} caller of a narrowed !{{io}} callee:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── G7: strict-effects diagnostic does not single-quote row syntax ───

#[test]
fn strict_effects_diagnostic_does_not_single_quote_row_syntax() {
    // The strict-effects diagnostic used to contain
    // `function 'main' uses effect '!{IO}' ...` (mentally
    // misreading the row as a single oddly-named effect).
    // Mirror the non-strict branch's shape: name the effect
    // unquoted inside the body sentence. We grep the diagnostic
    // for the `'!{` substring (single-quote followed by `!{`,
    // the broken shape) and assert it does not appear.
    let path = temp_silt_file(
        "g7_no_quoted_row",
        r#"fn doit() {
  println("hi")
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    assert!(
        !combined.contains("'!{"),
        "strict-effects diagnostic still single-quotes the row syntax:\n{combined}"
    );
}

// ── L1: trait_impl_set seeding uses the named constant ────────────────

#[test]
fn trait_impl_set_seeding_uses_builtin_auto_derived_constant() {
    // The audit caught an inline `for trait_name in &["Equal",
    // "Compare", "Hash", "Display"]` that should reference the
    // `BUILTIN_AUTO_DERIVED_TRAIT_NAMES` constant defined in the
    // same file. Drift between the two lists would silently break
    // auto-derive seeding when a future trait is added or removed.
    // Source-grep lock: the `for trait_name in` line near the trait-
    // impl-seeding block must reference the named constant, not a
    // bare array.
    let src = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/typechecker/mod.rs"),
    )
    .expect("read typechecker/mod.rs");

    // The broken shape is the inline array literal: a `for trait_name
    // in` line that ALSO carries `"Equal"` and `"Display"` literally
    // on the same line. The audit's other `for trait_name in
    // trait_names` loop iterates a function parameter, not a bare
    // array, so it doesn't match this filter.
    let bad_lines: Vec<&str> = src
        .lines()
        .filter(|l| l.contains("for trait_name in"))
        .filter(|l| l.contains("\"Equal\"") || l.contains("\"Display\""))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "found inline trait literal in `for trait_name in &[\"...\"]` form; \
         use BUILTIN_AUTO_DERIVED_TRAIT_NAMES instead. Offending line(s):\n  {}",
        bad_lines.join("\n  ")
    );
    // Positive lock: the constant must actually be referenced
    // somewhere — otherwise a future edit could swap the loop for
    // the inline literal and the negative grep above would miss it
    // (e.g. if the literal got split across lines).
    assert!(
        src.contains("for trait_name in BUILTIN_AUTO_DERIVED_TRAIT_NAMES"),
        "expected at least one `for trait_name in BUILTIN_AUTO_DERIVED_TRAIT_NAMES` \
         loop in typechecker/mod.rs"
    );
}
