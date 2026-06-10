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

// ── Hash on Record with non-hashable tuple field ────────────────────

/// Record with a tuple field. The tuple is hashable by the runtime
/// `impl Hash for Value`, but the typechecker's synth gate doesn't
/// emit `Pair.hash` for tuple fields, so the call falls through to
/// the runtime dispatch arm. Verify the call succeeds and prints
/// an Int.
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
