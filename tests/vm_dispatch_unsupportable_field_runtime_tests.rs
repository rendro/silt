//! Round-62 regression lock for `Hash` / `Compare` dispatch on user
//! records and variants whose fields are NOT auto-derive supportable
//! (Channel, Map, Tuple, Function, Bytes, Handle).
//!
//! Background: `register_type_decl` (`src/typechecker/mod.rs:3612`)
//! unconditionally pre-stamps the `trait_impl_set` with `Compare` /
//! `Hash` / `Equal` for every user-declared record / enum, but the
//! synthesis pass at `src/typechecker/mod.rs:4856-4877` is gated on
//! per-field trait support (`compare_ok`, `hash_ok`). When a field
//! is unsupportable (e.g. `Channel(Int)` has no `Hash` impl), the
//! synth pass emits NOTHING — so no `<Type>.<method>` global is
//! created. At runtime, `Op::CallMethod` does the qualified-global
//! lookup, misses, then falls through to
//! `Vm::dispatch_trait_method`. That's the arm under test here.
//!
//! Round-62 (commit `cb64e67`) deleted the (Variant, Variant) and
//! (Record, Record) arms from `compare`, and dropped Variant /
//! Record from the `hash` allowlist, on the false claim that "every
//! (trait, type) pair stamped in `trait_impl_set` receives a
//! synthesized global." That claim ignores the gating in the synth
//! pass. The arms are restored and these tests are the lock.
//!
//! Each test below compiles and runs a silt program where the
//! receiver type carries a non-supportable field, so the only path
//! to a non-error result is through the runtime dispatch arms in
//! `src/vm/dispatch.rs`. We assert the call succeeds and prints a
//! deterministic value.

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

/// Record with a `Channel(Int)` field. Channels have no `Hash`
/// derive, so the synth pass at typechecker/mod.rs:4856-4877
/// emits NO `Holder.hash` global. At runtime, `Op::CallMethod`
/// falls through to `dispatch_trait_method`, which must hash
/// the record structurally via `impl Hash for Value`
/// (`src/value.rs:2020`). The exact value isn't important — what
/// matters is that the call succeeds and prints an Int.
#[test]
fn hash_runs_on_user_record_with_channel_field() {
    let out = run_silt_ok(
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
    let line = out.lines().next().unwrap_or("");
    let parsed: Result<i64, _> = line.parse();
    assert!(
        parsed.is_ok(),
        "hash() on Holder must produce a parseable Int; got {line:?}"
    );
}

/// Variant whose payload includes a `Channel(Int)`. Same gating
/// argument as above: the variant decl pre-stamps `Hash` in
/// `trait_impl_set`, the synth pass skips emission because Channel
/// is non-hashable, and the runtime path must serve the call via
/// the (Variant, ..) arm in `dispatch_trait_method`'s `hash`
/// allowlist.
#[test]
fn hash_runs_on_user_variant_with_channel_field() {
    let out = run_silt_ok(
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
    let line = out.lines().next().unwrap_or("");
    let parsed: Result<i64, _> = line.parse();
    assert!(
        parsed.is_ok(),
        "hash() on V must produce a parseable Int; got {line:?}"
    );
}

// ── Compare on Record with non-comparable field ─────────────────────

/// Record with a `Map(String, Int)` field. Maps have no `Compare`
/// derive, so the synth pass skips `Doc.compare`. The runtime
/// `(Record, Record)` arm in `dispatch_trait_method`'s `compare`
/// match must defer to `Value::cmp` (`src/value.rs:1728`), which
/// orders records by name then field-wise. `cmp(d, d) == 0`
/// because the record is identical to itself.
#[test]
fn compare_runs_on_user_record_with_map_field() {
    let out = run_silt_ok(
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
    assert_eq!(out.trim(), "0");
}

/// Record with a tuple field `(Int, Int)`. Tuples have no auto-
/// derived `Compare` impl on the user side — the synth pass skips
/// emission, and the runtime arm must serve the call. Same `d` on
/// both sides, so `cmp_gen(d, d) == 0`.
#[test]
fn compare_runs_on_user_record_with_tuple_field() {
    let out = run_silt_ok(
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
    assert_eq!(out.trim(), "0");
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
