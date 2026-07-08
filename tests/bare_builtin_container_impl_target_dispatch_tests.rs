//! Lock tests for BARE builtin-container trait-impl targets:
//! `trait T for List` (no type args), plus Map/Set/Channel/Tuple/Range.
//!
//! Pre-fix state (round 101 finding): `register_trait_impl`'s
//! zero-target-args branch fell into the `user_arity == 0` fallback
//! `type_from_name(target_type)`, producing `Generic("List", [])` as
//! the self_type. `unify` has a bare-name wildcard arm only for `"Fn"`,
//! so a concrete receiver `Type::List(Int)` hit the catch-all mismatch
//! and DIRECT dispatch (`[1, 2].pretty()`) failed with the
//! self-contradictory "type mismatch: expected List, got List(Int)" —
//! while the SAME impl dispatched fine through a where-bound fn
//! (head-keyed obligation + runtime dispatch). The impl was
//! half-working.
//!
//! Post-fix: the `user_arity == 0` branch mirrors `resolve_type_expr`'s
//! bare-name annotation semantics — bare `List`/`Set`/`Channel` get one
//! fresh element var, `Map` gets two — so the self_type unifies with any
//! concrete receiver exactly as an annotation would. Variadic `Tuple`
//! has no fresh-var shape; it keeps `Generic("Tuple", [])` and the
//! unifier gained a bare-`Tuple` wildcard arm mirroring the round-71
//! follow-up `Fn` arm.
//!
//! Every test here RUNS the program (weak-gate rule): direct receiver
//! dispatch must print the impl's output end-to-end, not merely
//! typecheck.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_bare_container_{label}.silt"));
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

// ── The finding's exact repro: List, direct AND where-bound ─────────

/// `trait Pretty for List` must dispatch DIRECTLY on a concrete
/// receiver (`[1, 2].pretty()`) and produce the same result as the
/// where-bound route (`fn show(x: a) -> String where a: Pretty`).
/// Pre-fix the direct call errored "type mismatch: expected List, got
/// List(Int)" while the where-bound call ran fine.
#[test]
fn bare_list_impl_direct_and_where_bound_dispatch_agree() {
    let out = run_silt_ok(
        "list_both_paths",
        r#"
trait Pretty { fn pretty(self) -> String }
trait Pretty for List { fn pretty(self) -> String = "list" }
fn show(x: a) -> String where a: Pretty { x.pretty() }
fn main() {
  println([1, 2].pretty())
  println(show([1, 2]))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "list\nlist",
        "direct and where-bound dispatch for bare `for List` must both print 'list'; got {out:?}"
    );
}

/// The self_type's fresh element var must be instantiated per call
/// site: dispatching on `List(Int)` first must not pin the impl to Int
/// and break a later `List(String)` receiver.
#[test]
fn bare_list_impl_dispatches_across_distinct_element_types() {
    let out = run_silt_ok(
        "list_two_elem_types",
        r#"
trait Pretty { fn pretty(self) -> String }
trait Pretty for List { fn pretty(self) -> String = "list" }
fn main() {
  println([1, 2].pretty())
  println(["a", "b"].pretty())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "list\nlist",
        "bare `for List` must dispatch for List(Int) and List(String) in one program; got {out:?}"
    );
}

// ── Map / Set / Channel siblings ─────────────────────────────────────

/// Bare `Map` target: self_type gets TWO fresh vars (key, value) so a
/// concrete `Map(String, Int)` literal receiver dispatches directly.
#[test]
fn bare_map_impl_direct_and_where_bound_dispatch_agree() {
    let out = run_silt_ok(
        "map_both_paths",
        r#"
trait Pretty { fn pretty(self) -> String }
trait Pretty for Map { fn pretty(self) -> String = "map" }
fn show(x: a) -> String where a: Pretty { x.pretty() }
fn main() {
  let m = #{"a": 1}
  println(m.pretty())
  println(show(m))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "map\nmap",
        "direct and where-bound dispatch for bare `for Map` must both print 'map'; got {out:?}"
    );
}

/// Bare `Set` target with a concrete `Set(Int)` receiver.
#[test]
fn bare_set_impl_direct_and_where_bound_dispatch_agree() {
    let out = run_silt_ok(
        "set_both_paths",
        r#"
import set
trait Pretty { fn pretty(self) -> String }
trait Pretty for Set { fn pretty(self) -> String = "set" }
fn show(x: a) -> String where a: Pretty { x.pretty() }
fn main() {
  let s = set.from_list([1, 2])
  println(s.pretty())
  println(show(s))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "set\nset",
        "direct and where-bound dispatch for bare `for Set` must both print 'set'; got {out:?}"
    );
}

/// Bare `Channel` target with a concrete `Channel(Int)` receiver.
#[test]
fn bare_channel_impl_direct_dispatches() {
    let out = run_silt_ok(
        "channel_direct",
        r#"
import channel
trait Pretty { fn pretty(self) -> String }
trait Pretty for Channel { fn pretty(self) -> String = "channel" }
fn main() {
  let ch: Channel(Int) = channel.new(1)
  println(ch.pretty())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "channel",
        "direct dispatch for bare `for Channel` must print 'channel'; got {out:?}"
    );
}

// ── Tuple: variadic, handled by the unify wildcard arm ───────────────

/// Bare `Tuple` target: no fresh-var shape exists (tuples are
/// variadic), so the self_type stays `Generic("Tuple", [])` and the
/// new bare-`Tuple` wildcard arm in `unify` (mirroring the `Fn` arm)
/// accepts any concrete tuple receiver. Pre-fix: "type mismatch:
/// expected Tuple, got (Int, Int)".
#[test]
fn bare_tuple_impl_direct_and_where_bound_dispatch_agree() {
    let out = run_silt_ok(
        "tuple_both_paths",
        r#"
trait Pretty { fn pretty(self) -> String }
trait Pretty for Tuple { fn pretty(self) -> String = "tuple" }
fn show(x: a) -> String where a: Pretty { x.pretty() }
fn main() {
  println((1, 2).pretty())
  println(show((1, "mixed", true)))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "tuple\ntuple",
        "direct and where-bound dispatch for bare `for Tuple` must both print 'tuple'; got {out:?}"
    );
}

// ── Range: canonicalises onto List at registration ───────────────────

/// Bare `Range` target canonicalises to `List` at registration
/// (`canonicalize_type_name`), so the synthesized self_type is
/// `List(fresh)` — and a `Range(Int)` receiver unifies through the
/// bidirectional Range/List arm. Both a range receiver and a list
/// receiver must dispatch into the one impl.
#[test]
fn bare_range_impl_dispatches_for_range_and_list_receivers() {
    let out = run_silt_ok(
        "range_direct",
        r#"
trait Pretty { fn pretty(self) -> String }
trait Pretty for Range { fn pretty(self) -> String = "range-or-list" }
fn main() {
  println((1..3).pretty())
  println([1, 2].pretty())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "range-or-list\nrange-or-list",
        "bare `for Range` registers under 'List' and must dispatch for both receivers; got {out:?}"
    );
}

// ── Source-grep lock on the contradictory diagnostic path ────────────

/// The `user_arity == 0` fallback in `register_trait_impl` must route
/// bare builtin containers through fresh-var shapes, not
/// `type_from_name`'s `Generic(name, [])`. Lock the match arms so a
/// refactor that drops them (regressing to the contradictory
/// "expected List, got List(Int)" mismatch) is caught by grep even if
/// the runtime tests above are reorganised.
#[test]
fn register_trait_impl_bare_container_arms_present() {
    let src = include_str!("../src/typechecker/mod.rs");
    for needle in [
        r#""List" => Type::List(Box::new(self.fresh_var()))"#,
        r#""Set" => Type::Set(Box::new(self.fresh_var()))"#,
        r#""Channel" => Type::Channel(Box::new(self.fresh_var()))"#,
        r#"resolve(*name) == "Tuple""#,
    ] {
        assert!(
            src.contains(needle),
            "src/typechecker/mod.rs lost the bare builtin-container \
             self_type arm `{needle}` — bare `trait T for List`-style \
             impls would regress to the non-unifying Generic(name, []) \
             self_type (round 101 finding)"
        );
    }
}
