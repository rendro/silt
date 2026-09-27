//! Round 101 BROKEN lock: impl-level where-clause obligations on ALIAS
//! targets must be verified against the correct positional slot of the
//! CANONICAL EXPANDED type.
//!
//! Pre-fix bug (`src/typechecker/mod.rs::register_trait_impl`, the
//! impl-level where-clause loop): obligations were stored as
//! `(idx, trait, args)` with `idx` = the param's position in
//! `ti.target_param_names`, but the consumer
//! (`verify_trait_obligation`) resolves them via
//! `type_args_of(resolved_receiver).get(idx)` — the positional args of
//! the canonical EXPANDED type. The two index spaces coincide for
//! direct targets (`Box(a)`, `Map(k, v)`) but NOT for alias targets:
//! with `type Named(a) = Map(String, a)`, param `a` is at param
//! position 0 but expanded slot 1, so `where a: Marked` verified the
//! KEY slot (`String`) instead of the value type. Both directions were
//! broken:
//!   - FALSE REJECT: a valid program (`a = Int`, `Marked for Int`
//!     exists) was rejected with "type 'String' does not implement
//!     trait 'Marked'".
//!   - FALSE ACCEPT: `where a: Display` with a `Fn` map value passed
//!     `silt check` (it checked `String: Display`) and only died at
//!     runtime.
//!
//! Post-fix: the registration side stores `idx` = the position of the
//! param's tyvar within the expanded self_type's positional args, so
//! both sides of the `impl_constraints` table agree on one index
//! space. Direct (non-alias) targets keep their old (correct) indices;
//! two control tests below pin that.

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_alias_where_slot_{label}.silt"));
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

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── 1. FALSE-REJECT direction (audit repro, execution path) ─────────

/// `type Named(a) = Map(String, a)` with `where a: Marked` and
/// `a = Int` (which implements Marked) must ACCEPT and RUN. Pre-fix
/// this was rejected at check time with "type 'String' does not
/// implement trait 'Marked'" — the obligation was verified against the
/// key slot (expanded slot 0) instead of `a` (expanded slot 1).
#[test]
fn alias_target_where_bound_checks_value_slot_accepts_and_runs() {
    let out = run_silt_ok(
        "false_reject",
        r#"
import map

type Named(a) = Map(String, a)

trait Marked { fn m(self) -> Int }
trait Marked for Int { fn m(self) -> Int = self }

trait Keys { fn k(self) -> Int }
trait Keys for Named(a) where a: Marked {
  fn k(self) -> Int = map.length(self)
}

fn go(x: b) -> Int where b: Keys { x.k() }

fn main() {
  println(go(#{ "a": 1 }))
}
"#,
    );
    assert_eq!(out.trim(), "1", "expected the map length to print");
}

// ── 2. FALSE-ACCEPT direction (soundness, check-time rejection) ─────

/// Same alias prelude with `where a: Display` and a `Fn` map value:
/// `a = Fn` has no Display impl, so this must be REJECTED at check
/// time, naming 'Fn' (not 'String'). Pre-fix it verified
/// `String: Display` (trivially true), exited check with 0, and only
/// the runtime Display gate caught it.
#[test]
fn alias_target_where_bound_rejects_fn_value_at_check_time() {
    let (stdout, stderr, ok) = run_silt_raw(
        "false_accept",
        r#"
import map

type Named(a) = Map(String, a)

trait Keys { fn k(self) -> Int }
trait Keys for Named(a) where a: Display {
  fn k(self) -> Int = map.length(self)
}

fn go(x: b) -> Int where b: Keys { x.k() }

fn main() {
  println(go(#{ "a": { x -> x + 1 } }))
}
"#,
    );
    assert!(
        !ok,
        "a Fn map value must not satisfy `where a: Display` on the alias's \
         value slot; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stderr.contains("type 'Fn' does not implement trait 'Display'"),
        "the rejection must name the VALUE type 'Fn'; stderr={stderr}"
    );
    assert!(
        !stderr.contains("'String' does not implement"),
        "the rejection must not blame the key slot 'String'; stderr={stderr}"
    );
    assert!(
        !stderr.contains("error[runtime]"),
        "the rejection must fire at check time, not at runtime; stderr={stderr}"
    );
}

// ── 3. Direct-target controls: expanded slots == param slots ────────

/// Direct (non-alias) `Map(k, v)` target with a bound on `v`: the
/// value slot must still be the one verified. `String` (the key) does
/// NOT implement Marked, so if the fix had regressed direct-target
/// indexing toward slot 0 this would false-reject.
#[test]
fn direct_map_target_value_bound_keeps_slot_accepts_and_runs() {
    let out = run_silt_ok(
        "direct_value_bound",
        r#"
import map

trait Marked { fn m(self) -> Int }
trait Marked for Int { fn m(self) -> Int = self }

trait Vals { fn total(self) -> Int }
trait Vals for Map(k, v) where v: Marked {
  fn total(self) -> Int = map.length(self)
}

fn go(x: b) -> Int where b: Vals { x.total() }

fn main() {
  println(go(#{ "a": 7 }))
}
"#,
    );
    assert_eq!(out.trim(), "1", "expected the map length to print");
}

/// Direct `Map(k, v)` target with a bound on `k`: a `String`-keyed map
/// must be rejected naming 'String' — proving the key obligation still
/// indexes expanded slot 0 after the fix.
#[test]
fn direct_map_target_key_bound_rejects_unmarked_key() {
    let (stdout, stderr, ok) = run_silt_raw(
        "direct_key_bound",
        r#"
import map

trait Marked { fn m(self) -> Int }
trait Marked for Int { fn m(self) -> Int = self }

trait Keyed { fn total(self) -> Int }
trait Keyed for Map(k, v) where k: Marked {
  fn total(self) -> Int = map.length(self)
}

fn go(x: b) -> Int where b: Keyed { x.total() }

fn main() {
  println(go(#{ "a": 7 }))
}
"#,
    );
    assert!(
        !ok,
        "a String key must not satisfy `where k: Marked`; \
         stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stderr.contains("type 'String' does not implement trait 'Marked'"),
        "the rejection must name the KEY type 'String'; stderr={stderr}"
    );
}
