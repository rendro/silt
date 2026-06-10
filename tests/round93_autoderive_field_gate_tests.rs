//! Round 93 regression locks — field-aware auto-derive gate.
//!
//! FINDING (BROKEN): `register_type_decl` unconditionally pre-stamped
//! all four built-in traits (Equal/Compare/Hash/Display) into
//! `trait_impl_set` for every user type. When a field could never
//! satisfy a trait, `synthesize_auto_derive_impls` honestly computed
//! `compare_ok`/`equal_ok`/... per field but on failure only SKIPPED
//! synthesis — no negative was recorded, so the pre-stamp stood and
//! `==` / `<` / `.compare()` / `.hash()` on records/enums wrapping
//! non-comparable fields laundered into nondeterministic Value-level
//! fallbacks:
//!   - closure ordering = Arc pointer address (`a < b` on a Fn-field
//!     record flipped between runs under ASLR),
//!   - closure equality = ptr_eq (`H{...} == H{...}` printed false),
//!   - closure hash = constant tag.
//!
//! FIX: `compute_auto_derive_field_negatives` (src/typechecker/mod.rs)
//! computes honest, recursive, field-aware eligibility for
//! Equal/Compare/Hash and UN-stamps ineligible pairs;
//! `operand_builtin_trait_violation` rejects `==`/`<` operands and
//! `method_auto_derive_violation` enriches `.compare()`-style
//! rejections. Hand-written impls still register and dispatch.
//!
//! Every rejection test below FAILS on the pre-fix code (the program
//! typechecked and ran via the Value-level fallback) and PASSES
//! post-fix. The still-works tests guard against over-rejection (the
//! gate's failure mode) and run end-to-end through the `silt` CLI.

use std::process::Command;

use silt::typechecker;
use silt::types::Severity;

/// Typecheck `input` in-process and return the Error-severity messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert that typecheck rejects `input` with a message containing all
/// of `needles`.
fn assert_rejected(label: &str, input: &str, needles: &[&str]) {
    let errs = type_errors(input);
    assert!(
        !errs.is_empty(),
        "{label}: expected a typecheck rejection, got none"
    );
    for needle in needles {
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "{label}: expected an error containing {needle:?}, got: {errs:?}"
        );
    }
}

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round93_field_gate_{label}.silt"));
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

// ── 1. Static rejection: Fn-field record ────────────────────────────

const FN_FIELD_PRELUDE: &str = r#"
type H { f: Fn(Int) -> Int }
fn mk() -> H { H{f: fn(x) { x }} }
"#;

/// Pre-fix: typechecked, printed `false` (ptr_eq on distinct Arcs).
#[test]
fn fn_field_record_equality_rejected() {
    assert_rejected(
        "H == H",
        &format!("{FN_FIELD_PRELUDE}fn main() {{ println(mk() == mk()) }}"),
        &[
            "type 'H' cannot derive 'Equal'",
            "field 'f'",
            "Fn(Int) -> Int",
            "not equatable",
        ],
    );
}

/// THE nondeterminism repro: pre-fix `a < b` ordered by Arc pointer
/// address — `true false` vs `false true` flipped between runs under
/// ASLR. Now rejected at check time.
#[test]
fn fn_field_record_ordering_rejected_nondeterminism_repro() {
    assert_rejected(
        "H < H",
        &format!(
            "{FN_FIELD_PRELUDE}fn main() {{
    let a = mk()
    let b = mk()
    println(a < b)
    println(b < a)
}}"
        ),
        &[
            "type 'H' cannot derive 'Compare'",
            "field 'f'",
            "Fn(Int) -> Int",
            "not comparable",
        ],
    );
}

/// Pre-fix: typechecked, fell through to `dispatch_trait_method` and
/// the Value-level compare at runtime.
#[test]
fn fn_field_record_compare_method_rejected() {
    assert_rejected(
        "H.compare",
        &format!("{FN_FIELD_PRELUDE}fn main() {{ println(mk().compare(mk())) }}"),
        &[
            "type 'H' cannot derive 'Compare'",
            "field 'f'",
            "Fn(Int) -> Int",
        ],
    );
}

/// Pre-fix: typechecked; the runtime closure hash is a constant tag.
#[test]
fn fn_field_record_hash_method_rejected() {
    assert_rejected(
        "H.hash",
        &format!("{FN_FIELD_PRELUDE}fn main() {{ println(mk().hash()) }}"),
        &[
            "type 'H' cannot derive 'Hash'",
            "field 'f'",
            "Fn(Int) -> Int",
            "not hashable",
        ],
    );
}

// ── 1b. Static rejection: Map-field record ──────────────────────────

const MAP_FIELD_PRELUDE: &str = r#"
type M { m: Map(String, Int) }
fn mk() -> M { M{m: #{"a": 1}} }
"#;

/// Pre-fix: `a < b` typechecked and ran even though bare `map1 < map2`
/// is statically rejected (Map has no Compare).
#[test]
fn map_field_record_ordering_rejected() {
    assert_rejected(
        "M < M",
        &format!("{MAP_FIELD_PRELUDE}fn main() {{ println(mk() < mk()) }}"),
        &[
            "type 'M' cannot derive 'Compare'",
            "field 'm'",
            "Map(String, Int)",
            "not comparable",
        ],
    );
}

#[test]
fn map_field_record_compare_method_rejected() {
    assert_rejected(
        "M.compare",
        &format!("{MAP_FIELD_PRELUDE}fn main() {{ println(mk().compare(mk())) }}"),
        &[
            "type 'M' cannot derive 'Compare'",
            "field 'm'",
            "Map(String, Int)",
        ],
    );
}

/// `where a: Compare` obligations reject through the (now-honest)
/// trait_impl_set with the pre-existing diagnostic.
#[test]
fn map_field_record_where_clause_obligation_rejected() {
    assert_rejected(
        "where a: Compare with M",
        &format!(
            "{MAP_FIELD_PRELUDE}fn cmp_gen(x: a, y: a) -> Int where a: Compare {{ x.compare(y) }}
fn main() {{ println(cmp_gen(mk(), mk())) }}"
        ),
        &["type 'M' does not implement trait 'Compare'"],
    );
}

/// OVER-REJECTION GUARD: Map supports Equal and Hash (honest policy),
/// so `==` and `.hash()` on the Map-field record must KEEP working —
/// at runtime, not just typecheck.
#[test]
fn map_field_record_equality_and_hash_still_run() {
    let out = run_silt_ok(
        "map_eq_hash",
        r#"
type M { m: Map(String, Int) }
fn main() {
    let a = M{m: #{"a": 1}}
    let b = M{m: #{"a": 1}}
    let c = M{m: #{"a": 2}}
    println(a == b)
    println(a == c)
    println(a.hash() == b.hash())
}
"#,
    );
    assert_eq!(out.trim(), "true\nfalse\ntrue");
}

// ── 1c. Static rejection: enum variant wrapping a Map ───────────────

const MAP_ENUM_PRELUDE: &str = r#"
type W { Wrap(Map(String, Int)), Empty }
fn mk() -> W { Wrap(#{"a": 1}) }
"#;

/// Pre-fix: `Wrap(..) < Wrap(..)` typechecked and ran via the
/// Value-level variant compare.
#[test]
fn enum_wrapping_map_ordering_rejected() {
    assert_rejected(
        "W < W",
        &format!("{MAP_ENUM_PRELUDE}fn main() {{ println(mk() < mk()) }}"),
        &[
            "type 'W' cannot derive 'Compare'",
            "variant 'Wrap' payload #1",
            "Map(String, Int)",
            "not comparable",
        ],
    );
}

#[test]
fn enum_wrapping_map_compare_method_rejected() {
    assert_rejected(
        "W.compare",
        &format!("{MAP_ENUM_PRELUDE}fn main() {{ println(mk().compare(mk())) }}"),
        &["type 'W' cannot derive 'Compare'", "variant 'Wrap'"],
    );
}

// ── 1d. Recursive laundering: nested user type ───────────────────────

/// A record wrapping ANOTHER record that wraps a Fn must also be
/// rejected (the gate is a fixpoint, not a one-level check).
#[test]
fn nested_fn_field_record_equality_rejected() {
    assert_rejected(
        "Outer == Outer",
        r#"
type Inner { f: Fn(Int) -> Int }
type Outer { i: Inner }
fn main() {
    let a = Outer{i: Inner{f: fn(x) { x }}}
    let b = Outer{i: Inner{f: fn(x) { x }}}
    println(a == b)
}
"#,
        &["type 'Outer' cannot derive 'Equal'", "field 'i'", "'Inner'"],
    );
}

/// Generic instantiation laundering: `type Box(a) { v: a }` is
/// conditionally eligible, but instantiating `a = Fn(..)` must reject
/// at the use site.
#[test]
fn generic_record_fn_instantiation_equality_rejected() {
    assert_rejected(
        "Box(Fn) == Box(Fn)",
        r#"
type Box(a) { v: a }
fn main() {
    let c = Box{v: fn(x) { x }}
    let d = Box{v: fn(x) { x }}
    println(c == d)
}
"#,
        &[
            "type 'Box' cannot derive 'Equal'",
            "field 'v'",
            "not equatable",
        ],
    );
}

// ── 2. Still-works runtime coverage (over-rejection guards) ─────────

/// Int/String-field records keep compare/equal/hash, end-to-end.
#[test]
fn derivable_record_compare_equal_hash_run() {
    let out = run_silt_ok(
        "derivable_record",
        r#"
type P { x: Int, name: String }
fn main() {
    let a = P{x: 1, name: "a"}
    let b = P{x: 2, name: "b"}
    println(a < b)
    println(a == b)
    println(a.compare(b))
    println(a.hash() == a.hash())
}
"#,
    );
    assert_eq!(out.trim(), "true\nfalse\n-1\ntrue");
}

/// Enum with derivable payloads keeps all gated traits, end-to-end.
#[test]
fn derivable_enum_with_payload_runs() {
    let out = run_silt_ok(
        "derivable_enum",
        r#"
type Shape { Dot, Line(Int), Rect(Int, Int) }
fn main() {
    println(Dot < Line(1))
    println(Line(1) == Line(1))
    println(Line(1).compare(Rect(1, 2)))
    println(Rect(1, 2).hash() == Rect(1, 2).hash())
}
"#,
    );
    assert_eq!(out.trim(), "true\ntrue\n-1\ntrue");
}

/// A HAND-WRITTEN Compare impl on a Fn-field record must still
/// register and dispatch at runtime — the gate only removes the
/// PROVISIONAL auto-derive entries.
#[test]
fn handwritten_compare_impl_on_fn_field_record_dispatches() {
    let out = run_silt_ok(
        "handwritten_compare",
        r#"
type H { f: Fn(Int) -> Int }
trait Compare for H {
    fn compare(a: H, b: H) -> Int { 42 }
}
fn main() {
    let a = H{f: fn(x) { x }}
    let b = H{f: fn(x) { x }}
    println(a.compare(b))
}
"#,
    );
    assert_eq!(out.trim(), "42");
}

/// Self-recursive types must neither loop the gate nor get spuriously
/// rejected (coinductive fixpoint: a clean cycle stays eligible).
#[test]
fn recursive_type_still_derives_and_runs() {
    let out = run_silt_ok(
        "recursive_tree",
        r#"
type Tree { leaf: Int, kids: List(Tree) }
fn main() {
    let t1 = Tree{leaf: 1, kids: []}
    let t2 = Tree{leaf: 2, kids: [Tree{leaf: 3, kids: []}]}
    println(t1 < t2)
    println(t1 == t1)
    println(t1.compare(t2))
    println(t2.hash() == t2.hash())
}
"#,
    );
    assert_eq!(out.trim(), "true\ntrue\n-1\ntrue");
}

/// Generic record instantiated with a derivable arg keeps everything.
#[test]
fn generic_record_int_instantiation_still_runs() {
    let out = run_silt_ok(
        "generic_box_int",
        r#"
type Box(a) { v: a }
fn main() {
    let a = Box{v: 1}
    let b = Box{v: 2}
    println(a < b)
    println(a == b)
    println(a.compare(b))
    println(a.hash() == a.hash())
}
"#,
    );
    assert_eq!(out.trim(), "true\nfalse\n-1\ntrue");
}

/// Phantom type params are unpunished: only types that actually appear
/// in fields are walked at the use site.
#[test]
fn phantom_param_instantiated_with_fn_still_runs() {
    let out = run_silt_ok(
        "phantom_param",
        r#"
type Tag(a) { name: String }
fn main() {
    let c: Tag(Fn(Int) -> Int) = Tag{name: "a"}
    let d: Tag(Fn(Int) -> Int) = Tag{name: "b"}
    println(c < d)
    println(c == d)
}
"#,
    );
    assert_eq!(out.trim(), "true\nfalse");
}

/// Round-82 adjacency: a record wrapping a Channel keeps identity-
/// based `==` (Channel's one trait-surface capability), even though
/// Compare/Hash on the same record are now rejected.
#[test]
fn channel_field_record_equality_still_runs() {
    let out = run_silt_ok(
        "channel_field_eq",
        r#"
import channel
type C { ch: Channel(Int) }
fn main() {
    let a = C{ch: channel.new(1)}
    let b = C{ch: channel.new(1)}
    println(a == b)
    println(a == a)
}
"#,
    );
    assert_eq!(out.trim(), "false\ntrue");
}

/// Round-85 adjacency: closed-row anon-record fields are honest
/// element-wise at the Value level — the wrapping record keeps `==`.
#[test]
fn anon_record_field_equality_still_runs() {
    let out = run_silt_ok(
        "anon_field_eq",
        r#"
type R { x: {a: Int} }
fn main() {
    let r1 = R{x: {a: 1}}
    let r2 = R{x: {a: 1}}
    let r3 = R{x: {a: 2}}
    println(r1 == r2)
    println(r1 == r3)
}
"#,
    );
    assert_eq!(out.trim(), "true\nfalse");
}
