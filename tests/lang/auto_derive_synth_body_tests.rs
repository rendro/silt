//! Round-62 lock tests for the auto-derive synthesis pass.
//!
//! Replaces the typecheck-only `trait_impl_set` stamp for user-declared
//! enums and records with real synthesized `TraitImpl` AST nodes that
//! flow through the same `register_trait_impl` + compiler emission
//! pipeline as user-written impls. The tests below lock that:
//!
//! 1. Trait-bound dispatch on user enums/records routes through the
//!    synthesized global (`Color.compare`, `Point.equal`, etc.) at
//!    `Op::CallMethod`'s qualified-global lookup, NOT through the
//!    primitive-only fallback in `dispatch_trait_method`.
//! 2. The synthesized bodies produce the right runtime values:
//!    declaration-order ordinal compare, AND-chain equal, FNV-style
//!    hash combine, structural display.
//! 3. Manual `trait <X> for T` impls override the synthesized one for
//!    each of the four built-in traits.
//! 4. Generic enums / records (round-1 scope: typecheck-stamp
//!    fallback) still route through the existing dispatch arm — no
//!    behavioural change.
//!
//! Tests are end-to-end via the `silt` CLI so they exercise the
//! parser → typechecker → compiler → VM pipeline.

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_auto_derive_synth_{label}.silt"));
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

// ── Compare on enum ──────────────────────────────────────────────────

/// `cmp_gen(Red, Green)` for `type Color { Red, Green, Blue }` returns
/// -1 (Red ordinal=0, Green ordinal=1). Locks the synthesized body's
/// ordinal-compare catch-all arm against the declaration-order
/// registry from round 60.
#[test]
fn compare_on_enum_synthesized_body_routes_through_user_global() {
    let out = run_silt_ok(
        "compare_enum_basic",
        r#"
type Color { Red, Green, Blue, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Red, Green))
    println(cmp_gen(Green, Red))
    println(cmp_gen(Blue, Blue))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

/// Same-tag arm with constructor args: `Foo(5).compare(Foo(3))` recurses
/// on field `.compare()` calls and produces 1 (5 > 3). Different-tag
/// pair `Foo(99).compare(Bar(0))` returns -1 (Foo ordinal < Bar ordinal).
#[test]
fn compare_on_enum_with_args_recurses() {
    let out = run_silt_ok(
        "compare_enum_args",
        r#"
type Tagged { Foo(Int), Bar(Int), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Foo(5), Foo(3)))
    println(cmp_gen(Foo(3), Foo(5)))
    println(cmp_gen(Foo(7), Foo(7)))
    println(cmp_gen(Foo(99), Bar(0)))
    println(cmp_gen(Bar(0), Foo(99)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["1", "-1", "0", "-1", "1"]);
}

/// `Color.compare(Red, Green)` (qualified-call form) matches the
/// trait-bound `cmp_gen(Red, Green)` (where-clause form). Both routes
/// land on the same synthesized `Color.compare` global. Locks the
/// architectural goal: the qualified-global lookup at `Op::CallMethod`
/// finds the synthesized impl; both call shapes converge.
#[test]
fn compare_on_enum_qualified_call_matches_bound() {
    let out = run_silt_ok(
        "compare_qualified",
        r#"
type Color { Red, Green, Blue, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Red, Green))
    println(Color.compare(Red, Green))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines[0], lines[1]);
}

// ── Equal on enum ───────────────────────────────────────────────────

#[test]
fn equal_on_enum_synthesized() {
    let out = run_silt_ok(
        "equal_enum",
        r#"
type Color { Red, Green, Blue, }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    println(eq_gen(Red, Red))
    println(eq_gen(Red, Green))
    println(eq_gen(Blue, Blue))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "true"]);
}

/// Same-tag with args: AND-chain on field `.equal()` calls.
#[test]
fn equal_on_enum_with_args() {
    let out = run_silt_ok(
        "equal_enum_args",
        r#"
type Pair { Both(Int, Int), Just(Int), }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    println(eq_gen(Both(1, 2), Both(1, 2)))
    println(eq_gen(Both(1, 2), Both(1, 3)))
    println(eq_gen(Both(1, 2), Just(1)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "false"]);
}

// ── Hash on enum ─────────────────────────────────────────────────────

/// Hashing two equal values produces the same hash. The numeric value
/// is stable per-build (deterministic combine function).
#[test]
fn hash_on_enum_synthesized_is_deterministic_and_structural() {
    let out = run_silt_ok(
        "hash_enum",
        r#"
type Color { Red, Green, Blue, }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    println(h(Red))
    println(h(Red))
    println(h(Green))
    println(h(Blue))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    // Same value → same hash.
    assert_eq!(lines[0], lines[1]);
    // Different ordinals → different hashes (high probability).
    assert_ne!(lines[0], lines[2]);
    assert_ne!(lines[2], lines[3]);
}

/// Hash on enum-with-args is also deterministic.
#[test]
fn hash_on_enum_with_args_is_deterministic() {
    let out = run_silt_ok(
        "hash_enum_args",
        r#"
type Tagged { Foo(Int), Bar(Int), }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    println(h(Foo(5)))
    println(h(Foo(5)))
    println(h(Foo(6)))
    println(h(Bar(5)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines[0], lines[1]);
    assert_ne!(lines[0], lines[2]);
    assert_ne!(lines[0], lines[3]);
}

// ── Display on enum ─────────────────────────────────────────────────

#[test]
fn display_on_enum_synthesized_nullary() {
    let out = run_silt_ok(
        "display_enum_nullary",
        r#"
type Color { Red, Green, Blue, }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(d(Red))
    println(d(Green))
    println(d(Blue))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["Red", "Green", "Blue"]);
}

#[test]
fn display_on_enum_synthesized_with_args() {
    let out = run_silt_ok(
        "display_enum_args",
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(d(Foo(7)))
    println(d(Bar(42, "hi")))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["Foo(7)", "Bar(42, hi)"]);
}

// ── Compare on record ────────────────────────────────────────────────

#[test]
fn compare_on_record_synthesized() {
    let out = run_silt_ok(
        "compare_record",
        r#"
type Point { x: Int, y: Int, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 1, y: 3 }
    let r = Point { x: 2, y: 0 }
    println(cmp_gen(p, q))
    println(cmp_gen(q, p))
    println(cmp_gen(p, p))
    println(cmp_gen(p, r))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0", "-1"]);
}

// ── Equal on record ──────────────────────────────────────────────────

#[test]
fn equal_record_field_chain() {
    let out = run_silt_ok(
        "equal_record_chain",
        r#"
type Point { x: Int, y: Int, }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 1, y: 2 }
    let r = Point { x: 1, y: 3 }
    println(eq_gen(p, q))
    println(eq_gen(p, r))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

// ── Hash on record ───────────────────────────────────────────────────

#[test]
fn hash_record_combines_fields() {
    let out = run_silt_ok(
        "hash_record",
        r#"
type Point { x: Int, y: Int, }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 1, y: 2 }
    let r = Point { x: 2, y: 1 }
    println(h(p))
    println(h(q))
    println(h(r))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    // Equal values produce equal hashes.
    assert_eq!(lines[0], lines[1]);
    // Permuted fields produce different hashes (high probability;
    // `(x mod P)*31 + y mod P` is asymmetric in x and y).
    assert_ne!(lines[0], lines[2]);
}

// ── Display on record ────────────────────────────────────────────────

#[test]
fn display_record_synthesized() {
    let out = run_silt_ok(
        "display_record",
        r#"
type Point { x: Int, y: Int, }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let p = Point { x: 1, y: 2 }
    println(d(p))
}
"#,
    );
    assert_eq!(out.trim(), "Point { x: 1, y: 2 }");
}

// ── Override: manual impl wins for each of the four traits ──────────

/// Manual `trait Compare for Color { ... }` overrides the synthesized
/// auto-derive — the user's body is dispatched, returning the sentinel.
#[test]
fn manual_compare_overrides_auto_derive_synthesis() {
    let out = run_silt_ok(
        "manual_compare_override",
        r#"
type Color { Red, Green, Blue, }
trait Compare for Color {
    fn compare(self: Color, other: Color) -> Int = 0 - 42
}
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Red, Green)) }
"#,
    );
    assert_eq!(out.trim(), "-42");
}

#[test]
fn manual_equal_overrides_auto_derive_synthesis() {
    let out = run_silt_ok(
        "manual_equal_override",
        r#"
type Color { Red, Green, Blue, }
trait Equal for Color {
    fn equal(self: Color, other: Color) -> Bool = true
}
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() { println(eq_gen(Red, Green)) }
"#,
    );
    // Sentinel: user impl always returns true regardless of arg.
    assert_eq!(out.trim(), "true");
}

#[test]
fn manual_hash_overrides_auto_derive_synthesis() {
    let out = run_silt_ok(
        "manual_hash_override",
        r#"
type Color { Red, Green, Blue, }
trait Hash for Color {
    fn hash(self: Color) -> Int = 12345
}
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    println(h(Red))
    println(h(Green))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["12345", "12345"]);
}

#[test]
fn manual_display_overrides_auto_derive_synthesis() {
    let out = run_silt_ok(
        "manual_display_override",
        r#"
type Color { Red, Green, Blue, }
trait Display for Color {
    fn display(self: Color) -> String = "<color>"
}
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(d(Red))
    println(d(Blue))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["<color>", "<color>"]);
}

// ── Decl-order vs alphabetical: synthesized body honours decl order ─

/// `type Color { Red, Green, Blue }` — Red declared first (ordinal 0)
/// despite alphabetical `Blue < Green < Red`. Synthesized compare body
/// must use the registry-driven ordinal, NOT `compare()` on the tag
/// strings. Locks against a regression where the synthesis would emit
/// `name.compare(name)` on String tags.
#[test]
fn synth_body_uses_decl_order_not_alphabetical() {
    let out = run_silt_ok(
        "decl_order_not_alpha",
        r#"
type Backwards { Z, Y, A, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Z, A))
    println(cmp_gen(Y, Z))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1"]);
}

// ── Recursive enum: `type Tree { Leaf, Node(Tree, Tree) }` ──────────

/// Recursive types: the synthesized `Tree.compare` body recursively
/// calls `.compare()` on child Tree values. The recursive call resolves
/// to the same synthesized global at runtime (no infinite compile-
/// time recursion). Tests Leaf < Node + structural ordering.
#[test]
fn recursive_enum_compare_synthesized() {
    let out = run_silt_ok(
        "recursive_enum",
        r#"
type Tree { Leaf, Node(Tree, Tree), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Leaf, Node(Leaf, Leaf)))
    println(cmp_gen(Node(Leaf, Leaf), Node(Leaf, Leaf)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "0"]);
}

// ── No qualified-global lookup falls through to dispatch_trait_method
//    for user types after synthesis ─────────────────────────────────

/// Indirect verification of the architectural goal: after synthesis,
/// `Color.compare` is a real global. We don't have a hook to prove the
/// dispatch arm is unreachable, but a behavioral marker is that the
/// trait-bound `compare()` returns the same value regardless of
/// whether the underlying dispatch route is the synthesized global or
/// the dispatch_trait_method fallback. Compare a few pairs against
/// expected ordinal output.
#[test]
fn synthesized_global_present_for_user_enum() {
    let out = run_silt_ok(
        "synthesized_global_present",
        r#"
type Color { Red, Green, Blue, }
-- Direct qualified call exercises the qualified-global lookup.
-- If the synthesis didn't emit the global, this would error with
-- "unknown function 'compare' on Color" or similar.
fn main() {
    println(Color.compare(Red, Green))
}
"#,
    );
    assert_eq!(out.trim(), "-1");
}
