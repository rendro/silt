//! Round-93 regression lock for `Hash` / `Compare` on user records
//! and variants whose fields are NOT auto-derive supportable
//! (Channel, Map, Tuple, Function, Bytes, Handle).
//!
//! HISTORY: round 62 restored the (Variant, Variant) / (Record,
//! Record) arms in `Vm::dispatch_trait_method` and this suite locked
//! them — `register_type_decl` unconditionally pre-stamped Equal /
//! Compare / Hash for every user type, the synth pass skipped
//! emission for unsupportable fields, and the call fell through to
//! the Value-level dispatch arms at runtime.
//!
//! ROUND-93 RETARGET: that pre-stamp laundering was the subject of an
//! audit finding (Value-level fallbacks are unsound for the gated
//! traits: closure ordering is Arc-pointer-address nondeterministic,
//! Channel has no trait-surface Hash, etc.). The field-aware gate
//! (`compute_auto_derive_field_negatives` in
//! `src/typechecker/mod.rs`) now UN-stamps `(trait, type)` pairs
//! whose fields cannot satisfy the trait, so these programs are
//! REJECTED at typecheck time. The tests below are retargeted to
//! lock the static rejection. The lone still-running case
//! (`hash_runs_on_user_record_with_tuple_field`) keeps its runtime
//! assertion: tuples DO support Hash (Equal/Hash/Display policy), so
//! `(Hash, Pair)` legitimately survives the gate.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_unsupportable_field_dispatch_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Run and assert success; return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── Hash on Record with non-hashable field ──────────────────────────

/// ROUND-93 RETARGET: record with a `Channel(Int)` field. Channels
/// have no `Hash` impl (their only trait-surface capability is
/// identity-based Equal, round 82), so the round-93 field-aware gate
/// UN-stamps `(Hash, Holder)` and the `where a: Hash` obligation
/// rejects the program statically — the call can no longer launder
/// into the Value-level structural hash at runtime.
#[test]
fn hash_on_user_record_with_channel_field_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "record_channel",
        r#"
import channel
type Holder { id: String, ch: Channel(Int) }
fn h(x: a) -> Int where a: Hash { x.hash() }
fn main() {
    let c = channel.new(1)
    let hol = Holder { id: "x", ch: c }
    println(h(hol))
}
"#,
    );
    assert!(
        !ok,
        "hash() on a Channel-field record must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'Holder' does not implement trait 'Hash'"),
        "diagnostic should name the missing Hash impl on Holder, got: {stderr}"
    );
}

/// ROUND-93 RETARGET: variant whose payload includes a
/// `Channel(Int)`. Same gating argument as above — the round-93 gate
/// UN-stamps `(Hash, V)` and the program is rejected statically.
#[test]
fn hash_on_user_variant_with_channel_field_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "variant_channel",
        r#"
import channel
type V { Box1(Channel(Int)), Other }
fn h(x: a) -> Int where a: Hash { x.hash() }
fn main() {
    let v = Box1(channel.new(1))
    println(h(v))
}
"#,
    );
    assert!(
        !ok,
        "hash() on a Channel-payload variant must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'V' does not implement trait 'Hash'"),
        "diagnostic should name the missing Hash impl on V, got: {stderr}"
    );
}

// ── Compare on Record with non-comparable field ─────────────────────

/// ROUND-93 RETARGET: record with a `Map(String, Int)` field. Maps
/// have no `Compare`, so the round-93 gate UN-stamps `(Compare, Doc)`
/// and the `where a: Compare` obligation rejects the program
/// statically instead of laundering into `Value::cmp` at runtime.
#[test]
fn compare_on_user_record_with_map_field_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "record_map_compare",
        r#"
type Doc { meta: Map(String, Int) }
fn cmp_gen(x: a, y: a) -> Int where a: Compare { x.compare(y) }
fn main() {
    let d = Doc { meta: #{"a": 1} }
    println(cmp_gen(d, d))
}
"#,
    );
    assert!(
        !ok,
        "compare on a Map-field record must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'Doc' does not implement trait 'Compare'"),
        "diagnostic should name the missing Compare impl on Doc, got: {stderr}"
    );
}

/// ROUND-93 RETARGET: record with a tuple field `(Int, Int)`. Tuples
/// have no `Compare`, so the round-93 gate UN-stamps `(Compare,
/// Pair)` and the program is rejected statically.
#[test]
fn compare_on_user_record_with_tuple_field_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "record_tuple_compare",
        r#"
type Pair { t: (Int, Int) }
fn cmp_gen(x: a, y: a) -> Int where a: Compare { x.compare(y) }
fn main() {
    let p = Pair { t: (1, 2) }
    println(cmp_gen(p, p))
}
"#,
    );
    assert!(
        !ok,
        "compare on a tuple-field record must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'Pair' does not implement trait 'Compare'"),
        "diagnostic should name the missing Compare impl on Pair, got: {stderr}"
    );
}

// ── Hash on Record with a hashable tuple field ──────────────────────

/// Record with a tuple field. Tuples DO support Hash — `(Hash,
/// Tuple)` is stamped by `register_auto_derived_impls_for` under the
/// Equal/Hash/Display policy (see this file's header), so
/// `field_type_supports_trait` passes, `hash_ok` stays true, and the
/// call resolves through a synth-emitted `Pair.hash` global that
/// `Op::CallMethod`'s qualified-global lookup finds BEFORE
/// `dispatch_trait_method` is ever consulted. The runtime dispatch
/// arm this program exercises is therefore the TUPLE entry of the
/// hash allowlist (reached by the synthesized body's `self.t.hash()`
/// field call), NOT the `Value::Record` entry — the Record/Variant
/// hash-allowlist entries remain a defensive-dead fallback with no
/// positive behavioral lock (see the comment block in the `"hash"`
/// arm of `src/vm/dispatch.rs`). Verify the call succeeds and prints
/// an Int. The mechanism claim is locked in-process by
/// `synth_pass_emits_pair_hash_trait_impl_for_tuple_field_record`
/// below.
#[test]
fn hash_runs_on_user_record_with_tuple_field() {
    let out = run_silt_ok(
        "record_tuple_hash",
        r#"
type Pair { t: (Int, Int) }
fn h(x: a) -> Int where a: Hash { x.hash() }
fn main() {
    let p = Pair { t: (1, 2) }
    println(h(p))
}
"#,
    );
    let line = out.lines().next().unwrap_or("");
    let parsed: Result<i64, _> = line.parse();
    assert!(
        parsed.is_ok(),
        "hash() on Pair must produce a parseable Int; got {line:?}"
    );
}

// ── Round-100 doc-drift locks ────────────────────────────────────────
//
// The doc on `hash_runs_on_user_record_with_tuple_field` previously
// claimed the synth gate skips `Pair.hash` emission for tuple fields
// and the call "falls through" to the Record hash dispatch arm. That
// contradicted this file's own header (tuples DO support Hash) and
// the mechanism: `(Hash, Tuple)` is stamped by
// `register_auto_derived_impls_for(checker, ["Tuple","Map","Set"],
// non_ordering_traits)` in `src/typechecker/mod.rs`, so
// `field_type_supports_trait` passes and the synth pass emits
// `Pair.hash`. Two locks below:
//   1. an in-process MECHANISM lock proving the synth pass emits an
//      auto-derived `Pair.hash` TraitImpl for a tuple-field record,
//      and
//   2. a source-grep lock keeping the corrected prose in place (here
//      and in the reciprocal claim in
//      `tests/vm_dispatch_variant_record_runtime_tests.rs`).

/// MECHANISM LOCK: run the exact `Pair { t: (Int, Int) }` program
/// from `hash_runs_on_user_record_with_tuple_field` through the
/// typechecker and assert its auto-derive synth pass pushed a
/// `trait Hash for Pair` TraitImpl (with a `hash` method) into
/// `program.decls`. This is the decl the compiler lowers to the
/// `Pair.hash` qualified global that `Op::CallMethod` resolves ahead
/// of `dispatch_trait_method` — i.e. the runtime test above locks the
/// SYNTH path plus the Tuple hash-allowlist entry, not the
/// `Value::Record` entry. If this ever fails, the doc claims on both
/// hash tests must be re-audited.
#[test]
fn synth_pass_emits_pair_hash_trait_impl_for_tuple_field_record() {
    let src = r#"
type Pair { t: (Int, Int) }
fn h(x: a) -> Int where a: Hash { x.hash() }
fn main() {
    let p = Pair { t: (1, 2) }
    println(h(p))
}
"#;
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    assert!(
        errors
            .iter()
            .all(|e| e.severity != silt::types::Severity::Error),
        "tuple-field record Hash program must typecheck cleanly: {errors:?}"
    );

    let hash_trait = silt::intern::intern("Hash");
    let pair_type = silt::intern::intern("Pair");
    let synth_impl = program.decls.iter().find_map(|d| match d {
        silt::ast::Decl::TraitImpl(ti)
            if ti.is_auto_derived && ti.trait_name == hash_trait && ti.target_type == pair_type =>
        {
            Some(ti)
        }
        _ => None,
    });
    let ti = synth_impl.expect(
        "the auto-derive synth pass must emit an is_auto_derived `trait Hash for \
         Pair` impl for a tuple-field record — (Hash, Tuple) is stamped, so the \
         field-aware gate passes and the call resolves via the synth global, \
         never the Value::Record hash dispatch arm",
    );
    let hash_method = silt::intern::intern("hash");
    assert!(
        ti.methods.iter().any(|m| m.name == hash_method),
        "synthesized `trait Hash for Pair` impl must contain a `hash` method"
    );
}

/// SOURCE-GREP LOCK: the false pre-round-100 prose must stay gone and
/// the corrected prose must stay present, in both this file and the
/// reciprocal doc in `tests/vm_dispatch_variant_record_runtime_tests.rs`.
/// Needles that would otherwise match this test's own literals are
/// assembled by concatenation so only real doc text can match.
#[test]
fn tuple_field_hash_doc_claims_stay_corrected() {
    let this_src = include_str!("vm_dispatch_unsupportable_field_runtime_tests.rs");
    let sibling_src = include_str!("vm_dispatch_variant_record_runtime_tests.rs");

    // Old false claim in this file: the synth gate skips Pair.hash and
    // the call "falls through" to the Record dispatch arm.
    let bad_gate_claim = ["the typechecker's synth gate", " doesn't"].concat();
    assert!(
        !this_src.contains(&bad_gate_claim),
        "stale claim resurfaced: the synth gate DOES emit `Pair.hash` for \
         tuple fields ((Hash, Tuple) is stamped; see file header)"
    );
    let bad_fallthrough_claim = ["so the call falls", " through to"].concat();
    assert!(
        !this_src.contains(&bad_fallthrough_claim),
        "stale claim resurfaced: the tuple-field record hash call resolves \
         via the synth-emitted global, it does not fall through to the \
         Record dispatch arm"
    );
    // Positive control: the corrected mechanism prose is present.
    let good_claim = ["synth-emitted `Pair.hash`", " global"].concat();
    assert!(
        this_src.contains(&good_claim),
        "corrected doc on hash_runs_on_user_record_with_tuple_field went \
         missing; keep the synth-global mechanism description"
    );

    // Reciprocal doc in the variant/record suite: it must not claim the
    // Record/Variant hash DISPATCH ARM is positively locked by the
    // unsupportable-field tests, nor cite the long-renamed
    // `hash_runs_..._with_channel_field` runtime test.
    let stale_test_name = ["hash_runs_on_user_record", "_with_channel_field"].concat();
    assert!(
        !sibling_src.contains(&stale_test_name),
        "vm_dispatch_variant_record_runtime_tests.rs cites a test renamed in \
         round 93 (now hash_on_user_record_with_channel_field_rejected_statically)"
    );
    let bad_force_fallthrough = ["non-derivable fields to force", " fall-through"].concat();
    assert!(
        !sibling_src.contains(&bad_force_fallthrough),
        "vm_dispatch_variant_record_runtime_tests.rs re-acquired the false \
         claim that the unsupportable-field suite forces dispatch-arm \
         fall-through for hash"
    );
    // Positive control: the corrected reciprocal prose is present.
    assert!(
        sibling_src.contains("defensive-dead"),
        "vm_dispatch_variant_record_runtime_tests.rs must state the \
         Record/Variant hash-allowlist entries are a defensive-dead \
         fallback with no positive behavioral lock"
    );
}
