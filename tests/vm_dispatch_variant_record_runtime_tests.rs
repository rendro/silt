//! Runtime-dispatch tests for `Compare` / `Hash` on `Variant` and `Record`.
//!
//! Three BROKEN findings (pre-fix, all in `src/vm/dispatch.rs`):
//!
//! 1. `cmp_gen(Red, Green)` where `cmp_gen` carries a `where a: Compare`
//!    bound fails at runtime:
//!      `error[runtime]: compare() not supported between Variant and Variant`.
//!    The `"compare"` arm in `dispatch_trait_method` had no
//!    `(Value::Variant, Value::Variant)` pair, so it fell into the
//!    catch-all error. Fix: add a `(Variant, Variant) => receiver.cmp(other)`
//!    arm; `Value::cmp` (`src/value.rs:1551`) already orders variants by
//!    name with a weekday-ordinal special case.
//!
//! 2. `cmp_gen(p, q)` where `p`/`q` are user-defined records fails:
//!      `error[runtime]: compare() not supported between Record and Record`.
//!    Same arm missing `(Value::Record, Value::Record)`. Fix: defer to
//!    `Value::cmp`, which orders records by name then field-wise
//!    (`src/value.rs:1537`) with canonical Date/Time/DateTime ordering.
//!
//! 3. `h(p)` where `p` is a user record and `h` has `where a: Hash` fails:
//!      `error[runtime]: no method 'hash' for type 'Point'`.
//!    The `"hash"` arm's allowlist omitted `Value::Record`, even though
//!    `impl Hash for Value` (`src/value.rs:1814`) already hashes records
//!    structurally. Fix: add `Value::Record(..)` to the allowlist.
//!
//! ROUND-62 STATUS (prose corrected round 100): After the auto-derive
//! synthesis pass extension to generic types, a fully-derivable USER
//! enum/record (every field supports the trait) routes through the
//! synthesized `<Type>.<method>` global emitted by the typechecker —
//! `Op::CallMethod`'s qualified-global lookup resolves the call
//! BEFORE falling through to `dispatch_trait_method`. The deadness
//! proof (`tests/auto_derive_dead_arm_proof_tests.rs`) confirms this
//! through an exhaustive behavioural barrage. Globals are synthesized
//! for BUILT-IN enums and records too: `synthesize_auto_derive_impls`
//! (src/typechecker/mod.rs) walks `self.enums` / `self.records` and
//! emits `Weekday.compare` and friends, so built-ins ALSO resolve
//! through their synth global ahead of `dispatch_trait_method`
//! (locked by `builtin_types_compile_and_run_through_synth_globals`
//! in the dead-arm proof and by
//! `tests/auto_derive_builtin_synth_tests.rs`). The Variant/Record
//! dispatch arms are therefore defensive-dead for every valid program.
//!
//! ROUND-89 RETARGET (this finding): the three user-typed cases below
//! were originally written over fully-derivable types (Color, Point),
//! which means they were served by the synth global, NOT the dispatch
//! arms their names claim to lock — deleting the
//! `(Variant,Variant)`/`(Record,Record)` compare arms or the
//! Variant/Record hash allowlist entry would NOT have turned them red.
//! They were misleading.
//!
//! The synth pass (`synthesize_auto_derive_impls` in
//! src/typechecker/mod.rs) gates emission on per-field trait support
//! (`compare_ok`/`hash_ok`): if ANY field does not support the trait,
//! NO `<Type>.<method>` global is emitted.
//!
//! ROUND-93 RETARGET: the round-89 form of the two `compare_*` tests
//! relied on the pre-93 laundering — a user type with a non-derivable
//! field kept its unconditional `(Compare, Type)` pre-stamp, so the
//! `where a: Compare` call typechecked and fell through to
//! `dispatch_trait_method`'s Value-level compare at runtime. The
//! round-93 field-aware auto-derive gate
//! (`compute_auto_derive_field_negatives` in `src/typechecker/mod.rs`)
//! now UN-stamps such pairs, so those programs are REJECTED statically
//! and the user-type path to the dispatch compare arms is dead. The
//! two tests are retargeted again to lock the static rejection. The
//! built-in Weekday case (`compare_runs_on_builtin_weekday`) locks the
//! builtin synth semantics — declaration-order ordinals through the
//! synth-emitted `Weekday.compare` global — NOT dispatch-arm
//! reachability: built-ins are synthesized too, so the compare arm is
//! never reached.
//!
//! The `hash_runs_on_user_record` case is kept as-is over the fully-
//! derivable `Point`: retargeting it onto a non-derivable field would
//! just duplicate the static-rejection locks in the unsupportable
//! suite (`hash_on_user_record_with_channel_field_rejected_statically`
//! and friends). Its comment is corrected to state plainly that it
//! locks the SYNTH path (not the dispatch arm). ROUND-100 CORRECTION:
//! no test positively locks the Variant/Record hash-allowlist entries
//! — post round 93 they are a defensive-dead fallback (see the
//! `"hash"`-arm comment in `src/vm/dispatch.rs`): derivable receivers
//! resolve via their synth-emitted `<Type>.hash` global first, and
//! non-derivable ones are rejected statically.
//!
//! These tests exercise the runtime end-to-end via the `silt` CLI
//! (mirroring `tests/vm_trait_dispatch_runtime_tests.rs`) so they fail
//! before the `dispatch.rs` fix and pass after.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_variant_record_dispatch_{label}.silt"));
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

// ── Bug 1: Variant-vs-Variant `.compare()` via `Compare` bound ──────

/// ROUND-93 RETARGET: this test used to assert that `Compare` on a
/// user enum carrying a NON-DERIVABLE `Channel(Int)` payload ran at
/// runtime through `dispatch_trait_method`'s `(Variant, Variant)`
/// compare arm. The round-93 field-aware auto-derive gate
/// (`compute_auto_derive_field_negatives`,
/// `src/typechecker/mod.rs`) now UN-stamps `(Compare, Color)` because
/// the `Holding` payload can never satisfy `Compare` — the program is
/// rejected statically by the `where a: Compare` obligation instead
/// of laundering into the Value-level fallback. For user types the
/// dispatch arm is therefore statically unreachable via auto-derive;
/// built-ins bypass it too (their synth-emitted globals resolve
/// first), so the arm is defensive-dead. `compare_runs_on_builtin_weekday`
/// below locks the builtin synth semantics.
#[test]
fn compare_on_user_variant_with_channel_payload_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "variant_user",
        r#"
import channel
type Color { Red, Green, Holding(Channel(Int)), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Red, Green)) }
"#,
    );
    assert!(
        !ok,
        "compare on a Channel-payload enum must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'Color' does not implement trait 'Compare'"),
        "diagnostic should name the missing Compare impl on Color, got: {stderr}"
    );
}

/// LOCKS THE BUILTIN SYNTH SEMANTICS, NOT THE DISPATCH ARM. Built-in
/// enums are synthesized like user types: `synthesize_auto_derive_impls`
/// (src/typechecker/mod.rs) walks `self.enums` and emits a
/// `Weekday.compare` global, which `Op::CallMethod` resolves BEFORE
/// `dispatch_trait_method` — the `(Variant, Variant)` compare arm is
/// never reached, and deleting it would NOT turn this test red. What
/// this test locks is the declaration-order ordinal semantics of the
/// synth path: Weekday seeds Monday=0..Sunday=6, so Monday < Friday
/// and `cmp_gen(Monday, Friday)` = Less = -1. (Pre-round-61 this used
/// a hand-rolled `weekday_ordinal` table; the ordinal-based synth path
/// preserves the same semantics.)
#[test]
fn compare_runs_on_builtin_weekday() {
    let out = run_silt_ok(
        "variant_weekday",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Monday, Friday)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

// ── Bug 2: Record-vs-Record `.compare()` via `Compare` bound ────────

/// ROUND-93 RETARGET: this test used to assert that `Compare` on a
/// user record carrying a NON-DERIVABLE tuple field `t: (Int, Int)`
/// ran at runtime through `dispatch_trait_method`'s `(Record,
/// Record)` compare arm. The round-93 field-aware auto-derive gate
/// now UN-stamps `(Compare, Point)` because tuples have no `Compare`
/// — the `where a: Compare` obligation rejects the program statically
/// instead of laundering into the Value-level fallback. For user
/// types the dispatch arm is therefore statically unreachable via
/// auto-derive.
#[test]
fn compare_on_user_record_with_tuple_field_rejected_statically() {
    let (stdout, stderr, ok) = run_silt_raw(
        "record_user",
        r#"
type Point { n: Int, t: (Int, Int), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let p = Point { n: 1, t: (0, 0) }
    let q = Point { n: 2, t: (0, 0) }
    println(cmp_gen(p, q))
}
"#,
    );
    assert!(
        !ok,
        "compare on a tuple-field record must be rejected statically; stdout={stdout}"
    );
    assert!(
        stderr.contains("type 'Point' does not implement trait 'Compare'"),
        "diagnostic should name the missing Compare impl on Point, got: {stderr}"
    );
}

// ── Bug 3: Record `.hash()` via `Hash` bound ────────────────────────

/// LOCKS THE SYNTH PATH, NOT THE DISPATCH ARM. `Point` is fully
/// derivable (both fields are `Int`), so the per-field synth gate
/// (`compare_ok`/`hash_ok` in `synthesize_auto_derive_impls`,
/// src/typechecker/mod.rs) emits a `Point.hash` global and
/// `Op::CallMethod`'s qualified-global lookup resolves the call there —
/// it never reaches the Variant/Record hash allowlist in
/// `dispatch_trait_method`. (Deleting that allowlist entry would NOT
/// turn this red.) This test therefore locks the auto-derive SYNTH
/// path. ROUND-100 CORRECTION: the Variant/Record HASH DISPATCH ARM
/// entries have NO positive behavioral lock — they are a
/// defensive-dead fallback (see the `"hash"`-arm comment in
/// `src/vm/dispatch.rs`). The unsupportable-field suite does not
/// reach them either: its channel-field case is rejected statically
/// by the round-93 gate, and its tuple-field case
/// (`hash_runs_on_user_record_with_tuple_field`) resolves through its
/// own synth-emitted `Pair.hash` global, exercising the TUPLE
/// allowlist entry via the synthesized body's field `.hash()` call —
/// see the mechanism lock
/// `synth_pass_emits_pair_hash_trait_impl_for_tuple_field_record` in
/// `tests/vm_dispatch_unsupportable_field_runtime_tests.rs`.
///
/// The synthesized body combines field hashes with a mod-prime FNV-
/// style combine to avoid integer-overflow under silt's checked
/// arithmetic — see `combine_hash_expr` in
/// `src/typechecker/auto_derive.rs`. The concrete value observed for
/// `Point { x: 1, y: 2 }` on this build is locked below; any change
/// to the combine function or the per-variant tag in
/// `impl Hash for Value` (round 74) will flag here.
#[test]
fn hash_runs_on_user_record() {
    let out = run_silt_ok(
        "record_hash",
        r#"
type Point { x: Int, y: Int, }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    let p = Point { x: 1, y: 2 }
    println(h(p))
}
"#,
    );
    assert_eq!(out.trim(), "-18907436");
}
