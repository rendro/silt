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
//! ROUND-62 STATUS: After the auto-derive synthesis pass extension to
//! generic types, a fully-derivable USER enum/record (every field
//! supports the trait) routes through the synthesized `<Type>.<method>`
//! global emitted by the typechecker — `Op::CallMethod`'s qualified-
//! global lookup resolves the call BEFORE falling through to
//! `dispatch_trait_method`. The deadness proof
//! (`tests/auto_derive_dead_arm_proof_tests.rs`) confirms those user-
//! type counters stay zero through an exhaustive barrage. BUILT-IN
//! enums (Weekday and friends), however, are registered directly into
//! `self.enums` without a user `Decl::Type` and so are not processed
//! by the synth pass; they continue to hit the dispatch arms below.
//!
//! ROUND-89 RETARGET (this finding): the three user-typed cases below
//! were originally written over fully-derivable types (Color, Point),
//! which means they were served by the synth global, NOT the dispatch
//! arms their names claim to lock — deleting the
//! `(Variant,Variant)`/`(Record,Record)` compare arms or the
//! Variant/Record hash allowlist entry would NOT have turned them red.
//! They were misleading.
//!
//! The synth pass at `src/typechecker/mod.rs:5397-5567` gates emission
//! on per-field trait support (`compare_ok`/`hash_ok`): if ANY field
//! does not support the trait, NO `<Type>.<method>` global is emitted
//! and the call provably falls through to `dispatch_trait_method`. The
//! two `compare_*` tests are RETARGETED onto user types carrying a
//! non-derivable field (a `Channel` payload on the variant, a tuple
//! field on the record), so they now genuinely lock the compare arms
//! AND retain a meaningful ordering assertion (-1) that the
//! equal-only cases in `tests/vm_dispatch_unsupportable_field_runtime_tests.rs`
//! do not cover. The variant ordinal registry (registered
//! unconditionally in `register_type_decl`,
//! `src/typechecker/mod.rs:3784`, independent of synth gating) still
//! seeds declaration-order ordinals, so `Value::cmp`
//! (`src/value.rs:1875/1904`) orders the fall-through cases by ordinal /
//! field-wise.
//!
//! The `hash_runs_on_user_record` case is kept as-is over the fully-
//! derivable `Point`: retargeting it onto a non-derivable field would
//! exactly duplicate `hash_runs_on_user_record_with_channel_field` /
//! `hash_runs_on_user_record_with_tuple_field` in the unsupportable
//! suite. Its comment is corrected to state plainly that it locks the
//! SYNTH path (not the dispatch arm); the Variant/Record hash dispatch
//! arms are locked in `tests/vm_dispatch_unsupportable_field_runtime_tests.rs`.
//!
//! The Weekday case still serves as the reachability proof for built-in
//! variant dispatch.
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

/// `Compare` on a user-declared enum must dispatch through the runtime
/// `(Variant, Variant)` `"compare"` arm. To force fall-through (and so
/// make this test a genuine lock on that arm rather than on the synth
/// global), the enum carries a NON-DERIVABLE `Channel(Int)` payload on
/// one variant: `Channel` has no `Compare` derive, so the synth gate at
/// `src/typechecker/mod.rs:5478` leaves `compare_ok = false` and emits
/// NO `Color.compare` global. `Op::CallMethod`'s qualified-global lookup
/// therefore misses and execution falls through to
/// `dispatch_trait_method`'s `(Variant, Variant) => receiver.cmp(other)`
/// arm. `Value::cmp` (`src/value.rs:1904`) orders variants by their
/// declaration-order ordinal — registered unconditionally in
/// `register_type_decl` (`src/typechecker/mod.rs:3784`), independent of
/// synth gating — so for `type Color { Red, Green, Holding(Channel) }`
/// we have Red=0, Green=1, and `Red.compare(Green)` = Less = -1.
/// Deleting the `(Variant, Variant)` compare arm turns this red with
/// `error[runtime]: compare() not supported between Variant and Variant`.
#[test]
fn compare_runs_on_user_variant() {
    let out = run_silt_ok(
        "variant_user",
        r#"
import channel
type Color { Red, Green, Holding(Channel(Int)), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Red, Green)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

/// The built-in `Weekday` variant (from `import time`) is a Variant at
/// runtime. `Value::cmp` consults the global variant-ordinal registry,
/// which seeds Weekday with Monday=0..Sunday=6, so Monday < Friday and
/// `cmp_gen(Monday, Friday)` = Less = -1. (Pre-round-61 this used a
/// hand-rolled `weekday_ordinal` table; the new registry-based path
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

/// `Compare` on a user-declared record must dispatch through the
/// runtime `(Record, Record)` `"compare"` arm. To force fall-through
/// (so this test genuinely locks that arm and not the synth global),
/// the record carries a NON-DERIVABLE tuple field `t: (Int, Int)`:
/// tuples have no user-side `Compare` derive, so the synth gate at
/// `src/typechecker/mod.rs:5536` leaves `compare_ok = false` and emits
/// NO `Point.compare` global — `Op::CallMethod` falls through to
/// `dispatch_trait_method`'s `(Record, Record) => receiver.cmp(other)`
/// arm. `Value::cmp` (`src/value.rs:1875`) orders records by name then
/// field-wise; record fields are stored in a `BTreeMap` keyed by field
/// name, so iteration is alphabetical — field `n` (the Int) is compared
/// before field `t` (the tuple), which is identical on both sides. With
/// `n: 1` vs `n: 2`, the Int decides: Less = -1. (Unlike the equal-only
/// `cmp_gen(d, d) == 0` cases in the unsupportable suite, this asserts a
/// real ordering, so a broken field-wise comparison would also flag.)
/// Deleting the `(Record, Record)` compare arm turns this red with
/// `error[runtime]: compare() not supported between Record and Record`.
#[test]
fn compare_runs_on_user_record() {
    let out = run_silt_ok(
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
    assert_eq!(out.trim(), "-1");
}

// ── Bug 3: Record `.hash()` via `Hash` bound ────────────────────────

/// LOCKS THE SYNTH PATH, NOT THE DISPATCH ARM. `Point` is fully
/// derivable (both fields are `Int`), so the synth gate at
/// `src/typechecker/mod.rs:5560` emits a `Point.hash` global and
/// `Op::CallMethod`'s qualified-global lookup resolves the call there —
/// it never reaches the Variant/Record hash allowlist in
/// `dispatch_trait_method`. (Deleting that allowlist entry would NOT
/// turn this red.) This test therefore locks the auto-derive SYNTH
/// path; the Variant/Record HASH DISPATCH ARM is locked by
/// `hash_runs_on_user_record_with_channel_field` /
/// `hash_runs_on_user_record_with_tuple_field` in
/// `tests/vm_dispatch_unsupportable_field_runtime_tests.rs`, which use
/// non-derivable fields to force fall-through. (It is kept user-typed
/// here rather than retargeted onto a non-derivable field precisely to
/// avoid duplicating those.)
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
