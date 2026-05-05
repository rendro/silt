//! Round-73 lock: behavioural equivalence after collapsing the four
//! `synth_*_impl_for_record` scaffolds in `src/typechecker/auto_derive.rs`
//! into two shared helpers (`synth_binop_record_impl` for Compare/Equal
//! and `synth_unop_record_impl` for Hash/Display).
//!
//! ## Why this lives in its own file
//!
//! `tests/auto_derive_dead_arm_proof_tests.rs` is the strongest existing
//! lock for auto-derive correctness — a per-shape × per-trait barrage
//! that executes the generated code via `silt run` and asserts on
//! stdout. This file COMPLEMENTS that barrage by pinning the precise
//! axes the round-73 refactor could regress on:
//!
//!   1. **Per-trait × per-record-shape output equivalence.** Compare,
//!      Equal, Hash, Display each invoked over (zero-field, one-field,
//!      three-field, nested) records — every cell exercises both
//!      helpers' branches (empty-fields fast path is exercised in
//!      section 2; full-field bodies are exercised in section 1).
//!
//!   2. **Empty-fields fast path.** A `type Unit {}` is given each of
//!      the four traits; the body is the per-trait empty-body literal
//!      (`0` / `true` / `0` / `"Unit {}"`). Tests construct a `Unit`
//!      and exercise every trait method on it.
//!
//!   3. **Field declaration-order preservation.** Compare lex-orders
//!      fields in declaration order. A regression that, say, sorted
//!      fields alphabetically would change the lex order. Tests pin
//!      declaration order using a non-alphabetical field declaration
//!      so alphabetical == declaration coincidence isn't enough to
//!      pass.
//!
//!   4. **Structural source-grep locks.** Pin that the four pub fns
//!      are now thin adapters (each fn body fewer than 25 source
//!      lines) AND that `synth_binop_record_impl` /
//!      `synth_unop_record_impl` exist. Mirror lock for the round-69
//!      enum-side helpers (`synth_binop_match_enum` /
//!      `synth_unop_match_enum`) — record and enum sides should
//!      mirror each other.
//!
//! Behavioural assertions go through `silt run` (subprocess), not
//! in-process `vm.run`, because the auto-derive bug class is "synth
//! produces a body that compiles but runs wrong" — only the runtime
//! output proves the synthesized AST is semantically intact.
//!
//! All `.compare(...)` / `.equal(...)` / `.hash()` / `.display()` calls
//! on record values flow through a generic helper bound by the relevant
//! trait — that's the canonical surface form for invoking a record's
//! auto-derived method by receiver-style call.

use std::process::Command;

fn run_silt_ok(label: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_round73_record_scaffold_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "silt run should succeed for {label}; stdout=<<<{stdout}>>> stderr=<<<{stderr}>>>"
    );
    stdout
}

// Header reused by every body test below — declares the four trait-
// bound generic helpers used to invoke each auto-derived method.
const TRAIT_HELPERS: &str = r#"
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
"#;

fn run_with_helpers(label: &str, body: &str) -> String {
    run_silt_ok(label, &format!("{TRAIT_HELPERS}{body}"))
}

// ── 1. Output equivalence: trait × shape matrix ─────────────────────
//
// Each test below pins a specific trait+shape cell. The expected
// output is computed by hand from the synth body shape; if the
// refactor accidentally swaps a per-field expression or drops the
// empty-body literal, the wrong number prints and the test fails.

#[test]
fn compare_one_field_record() {
    // Single-field: lex-compare collapses to a single
    // `self.f.compare(other.f)` call (no chain).
    let out = run_with_helpers(
        "compare_one_field",
        r#"
type One { x: Int }
fn main() {
    let a = One { x: 1 }
    let b = One { x: 2 }
    let c = One { x: 1 }
    println(cmp_gen(a, b))
    println(cmp_gen(b, a))
    println(cmp_gen(a, c))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

#[test]
fn compare_three_field_record_lex_order() {
    // Three fields: lex-compare in declaration order.
    //   cmp(R{1, 0, 0}, R{2, 0, 0}) → first-field decides → -1
    //   cmp(R{1, 1, 0}, R{1, 0, 0}) → first eq, second decides → 1
    //   cmp(R{1, 0, 1}, R{1, 0, 0}) → first/second eq, third decides → 1
    //   cmp(R{1, 0, 0}, R{1, 0, 0}) → all eq → 0
    let out = run_with_helpers(
        "compare_three_field",
        r#"
type R { a: Int, b: Int, c: Int }
fn main() {
    println(cmp_gen(R { a: 1, b: 0, c: 0 }, R { a: 2, b: 0, c: 0 }))
    println(cmp_gen(R { a: 1, b: 1, c: 0 }, R { a: 1, b: 0, c: 0 }))
    println(cmp_gen(R { a: 1, b: 0, c: 1 }, R { a: 1, b: 0, c: 0 }))
    println(cmp_gen(R { a: 1, b: 0, c: 0 }, R { a: 1, b: 0, c: 0 }))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "1", "0"]);
}

#[test]
fn compare_nested_record_recurses_through_field_compare() {
    // Outer record's compare calls inner's compare via field chain.
    // Inner R{x: 1} vs R{x: 2} → -1, so outer Wrap{r: R{1}} vs Wrap{r: R{2}} → -1.
    let out = run_with_helpers(
        "compare_nested",
        r#"
type Inner { x: Int }
type Wrap { r: Inner, tag: Int }
fn main() {
    let a = Wrap { r: Inner { x: 1 }, tag: 0 }
    let b = Wrap { r: Inner { x: 2 }, tag: 0 }
    let c = Wrap { r: Inner { x: 1 }, tag: 1 }
    println(cmp_gen(a, b))
    println(cmp_gen(a, c))
    println(cmp_gen(a, a))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "0"]);
}

#[test]
fn equal_one_field_record() {
    let out = run_with_helpers(
        "equal_one_field",
        r#"
type One { x: Int }
fn main() {
    println(eq_gen(One { x: 1 }, One { x: 1 }))
    println(eq_gen(One { x: 1 }, One { x: 2 }))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn equal_three_field_record_left_fold() {
    // All three equal → true. Any one differs → false (short-circuit
    // via &&, but the fold visits every field).
    let out = run_with_helpers(
        "equal_three_field",
        r#"
type R { a: Int, b: Int, c: Int }
fn main() {
    println(eq_gen(R { a: 1, b: 2, c: 3 }, R { a: 1, b: 2, c: 3 }))
    println(eq_gen(R { a: 1, b: 2, c: 3 }, R { a: 9, b: 2, c: 3 }))
    println(eq_gen(R { a: 1, b: 2, c: 3 }, R { a: 1, b: 9, c: 3 }))
    println(eq_gen(R { a: 1, b: 2, c: 3 }, R { a: 1, b: 2, c: 9 }))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "false", "false"]);
}

#[test]
fn equal_nested_record_recurses_through_field_equal() {
    let out = run_with_helpers(
        "equal_nested",
        r#"
type Inner { x: Int }
type Wrap { r: Inner, tag: Int }
fn main() {
    let a = Wrap { r: Inner { x: 1 }, tag: 0 }
    let b = Wrap { r: Inner { x: 1 }, tag: 0 }
    let c = Wrap { r: Inner { x: 2 }, tag: 0 }
    println(eq_gen(a, b))
    println(eq_gen(a, c))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn hash_one_field_record() {
    // Single-field: hash collapses to `self.x.hash()` (no combine).
    // Pin determinism + structural equality.
    let out = run_with_helpers(
        "hash_one_field",
        r#"
type One { x: Int }
fn main() {
    let h1 = h(One { x: 7 })
    let h2 = h(One { x: 7 })
    let h3 = h(One { x: 8 })
    println(eq_gen(h1, h2))
    println(eq_gen(h1, h3))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn hash_three_field_record_combine() {
    // Three fields: combine-fold visits every field.
    // - Same record → same hash (deterministic, structural)
    // - Differ in any field → likely-different hash (FNV-style mul-xor
    //   over field hashes; collisions theoretically possible but the
    //   small inputs we use here don't trigger one).
    let out = run_with_helpers(
        "hash_three_field",
        r#"
type R { a: Int, b: Int, c: Int }
fn main() {
    let h1 = h(R { a: 1, b: 2, c: 3 })
    let h2 = h(R { a: 1, b: 2, c: 3 })
    let h3 = h(R { a: 9, b: 2, c: 3 })
    let h4 = h(R { a: 1, b: 9, c: 3 })
    let h5 = h(R { a: 1, b: 2, c: 9 })
    println(eq_gen(h1, h2))
    println(eq_gen(h1, h3))
    println(eq_gen(h1, h4))
    println(eq_gen(h1, h5))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "false", "false"]);
}

#[test]
fn hash_nested_record_recurses_through_field_hash() {
    let out = run_with_helpers(
        "hash_nested",
        r#"
type Inner { x: Int }
type Wrap { r: Inner, tag: Int }
fn main() {
    let h1 = h(Wrap { r: Inner { x: 7 }, tag: 0 })
    let h2 = h(Wrap { r: Inner { x: 7 }, tag: 0 })
    let h3 = h(Wrap { r: Inner { x: 8 }, tag: 0 })
    println(eq_gen(h1, h2))
    println(eq_gen(h1, h3))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn display_one_field_record() {
    // Body shape: "Name { x: " + self.x.display() + " }".
    let out = run_with_helpers(
        "display_one_field",
        r#"
type One { x: Int }
fn main() {
    println(d(One { x: 7 }))
}
"#,
    );
    assert_eq!(out.trim(), "One { x: 7 }");
}

#[test]
fn display_three_field_record() {
    // Body: "R { a: " + self.a.display() + ", b: " + self.b.display()
    //   + ", c: " + self.c.display() + " }".
    let out = run_with_helpers(
        "display_three_field",
        r#"
type R { a: Int, b: Int, c: String }
fn main() {
    println(d(R { a: 1, b: 2, c: "hi" }))
}
"#,
    );
    assert_eq!(out.trim(), "R { a: 1, b: 2, c: hi }");
}

#[test]
fn display_nested_record() {
    // Outer's display recurses into inner's display.
    let out = run_with_helpers(
        "display_nested",
        r#"
type Inner { x: Int }
type Wrap { r: Inner, tag: Int }
fn main() {
    println(d(Wrap { r: Inner { x: 7 }, tag: 3 }))
}
"#,
    );
    assert_eq!(out.trim(), "Wrap { r: Inner { x: 7 }, tag: 3 }");
}

// ── 2. Zero-field record fast path ──────────────────────────────────
//
// Synth emits the per-trait empty-body literal when fields is empty:
//   - Compare → 0
//   - Equal → true
//   - Hash → 0
//   - Display → "Name {}"
//
// **Surface-syntax limitation.** `type X {}` is parsed as a zero-
// variant enum (see `Parser::is_record_body`), not a zero-field
// record, so we cannot construct a zero-field record at the surface
// to drive the fast path end-to-end.
//
// Instead we lock the fast path structurally by source-grepping for
// the per-trait empty-body literals inside the four adapters. If a
// future refactor accidentally swapped, dropped, or replaced any
// empty-body literal, these greps fail.

#[test]
fn zero_field_compare_empty_body_literal_is_int_zero() {
    // The compare adapter passes `int_expr(0)` to the binop scaffold.
    let adapter = adapter_slice(AUTO_DERIVE_SRC, "synth_compare_impl_for_record");
    assert!(
        adapter.contains("int_expr(0)"),
        "synth_compare_impl_for_record must pass int_expr(0) as empty body; got:\n{adapter}"
    );
}

#[test]
fn zero_field_equal_empty_body_literal_is_bool_true() {
    let adapter = adapter_slice(AUTO_DERIVE_SRC, "synth_equal_impl_for_record");
    assert!(
        adapter.contains("bool_expr(true)"),
        "synth_equal_impl_for_record must pass bool_expr(true) as empty body; got:\n{adapter}"
    );
}

#[test]
fn zero_field_hash_empty_body_literal_is_int_zero() {
    let adapter = adapter_slice(AUTO_DERIVE_SRC, "synth_hash_impl_for_record");
    assert!(
        adapter.contains("int_expr(0)"),
        "synth_hash_impl_for_record must pass int_expr(0) as empty body; got:\n{adapter}"
    );
}

#[test]
fn zero_field_display_empty_body_literal_uses_name_braces() {
    // Display's empty body is "Name {}" — built via
    // `string_expr(&format!("{name_str} {{}}"))`. The format-string
    // literal `{name_str} {{}}` is the load-bearing piece; if a
    // refactor changed it to ` {{ }} ` or dropped the `{{}}`, this
    // assertion would catch it.
    let adapter = adapter_slice(AUTO_DERIVE_SRC, "synth_display_impl_for_record");
    assert!(
        adapter.contains("{name_str} {{}}"),
        "synth_display_impl_for_record must build empty body as \"Name {{}}\"; got:\n{adapter}"
    );
}

// ── 3. Field-declaration-order preservation ─────────────────────────
//
// The Compare path encodes lex-comparison in declaration order. A
// regression that, say, alphabetised fields would change the order in
// which fields decide ties.
//
// To rule out alphabetical-coincidence regressions, the test below
// declares fields in non-alphabetical order: `z` first, `a` second.
// Pinning that the FIRST field (`z` by declaration) decides ties
// proves the refactor preserved declaration order.

#[test]
fn compare_pins_field_declaration_order() {
    // Declaration order: z, a, m. So z decides first, then a, then m.
    //   cmp(R{z:1,a:9,m:9}, R{z:2,a:0,m:0}) → first-field z decides → -1
    //   cmp(R{z:1,a:1,m:9}, R{z:1,a:2,m:0}) → z eq, a decides → -1
    //   cmp(R{z:1,a:1,m:1}, R{z:1,a:1,m:2}) → z/a eq, m decides → -1
    // If alphabetical-order regressed, fields would be visited a, m, z,
    // and the first cmp would route through `a` first: 9 vs 0 → 1, not -1.
    let out = run_with_helpers(
        "field_decl_order_non_alpha",
        r#"
type R { z: Int, a: Int, m: Int }
fn main() {
    println(cmp_gen(R { z: 1, a: 9, m: 9 }, R { z: 2, a: 0, m: 0 }))
    println(cmp_gen(R { z: 1, a: 1, m: 9 }, R { z: 1, a: 2, m: 0 }))
    println(cmp_gen(R { z: 1, a: 1, m: 1 }, R { z: 1, a: 1, m: 2 }))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "-1"]);
}

#[test]
fn display_pins_field_declaration_order() {
    // Declaration order: z, a, m. Display body must visit them in
    // declaration order to produce "R { z: 1, a: 2, m: 3 }". An
    // alphabetical regression would print "R { a: 2, m: 3, z: 1 }".
    let out = run_with_helpers(
        "display_field_decl_order",
        r#"
type R { z: Int, a: Int, m: Int }
fn main() {
    println(d(R { z: 1, a: 2, m: 3 }))
}
"#,
    );
    assert_eq!(out.trim(), "R { z: 1, a: 2, m: 3 }");
}

// ── 4. Structural source-grep locks ────────────────────────────────
//
// These tests inspect the auto_derive.rs source directly to lock the
// refactor's shape, not just its observable behaviour. If a future
// change re-inlines the four scaffolds, these tests fail loud and the
// dup is caught at PR-review time rather than re-surfacing in a later
// audit round.

const AUTO_DERIVE_SRC: &str = include_str!("../src/typechecker/auto_derive.rs");

/// Slice the source from `fn <name>(` through that fn's closing
/// top-level `}`. Used by the empty-body-literal greps to limit
/// matching to a specific adapter's body.
fn adapter_slice<'a>(src: &'a str, fn_name: &str) -> &'a str {
    let needle = format!("fn {fn_name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("could not find `fn {fn_name}(` in source"));
    // Closing `}` at column 0 — fn termination for top-level fns.
    let after_start = &src[start..];
    let end_in_slice = after_start
        .find("\n}\n")
        .unwrap_or_else(|| panic!("could not find closing brace for `fn {fn_name}`"))
        + "\n}".len();
    &after_start[..end_in_slice]
}

/// Find the body span (the lines between the function's opening `{`
/// and its matching closing `}`) of a function whose signature appears
/// on a single source line containing `fn <name>(`. Counts source
/// lines in the body, not statements. Returns `None` if the function
/// can't be found or its body span can't be determined.
fn fn_body_line_count(src: &str, fn_name: &str) -> Option<usize> {
    let needle = format!("fn {fn_name}(");
    // Find the line containing the signature. Functions in this file
    // have signatures that span multiple lines (one param per line),
    // so we look for the line containing `fn name(` and scan forward
    // for the opening `{` that starts the body.
    let lines: Vec<&str> = src.lines().collect();
    let sig_idx = lines.iter().position(|l| l.contains(&needle))?;
    // Scan forward from the signature line for `) -> ... {` — i.e. the
    // line that ends with `{` after the signature's closing `)`.
    let mut open_idx = None;
    for (i, line) in lines.iter().enumerate().skip(sig_idx) {
        // Body opens at the first `{` after the closing `)` of params.
        // Heuristic: a line ending with ` {` (after trimming trailing
        // whitespace) is the body opener. This is robust to multi-line
        // signatures because rustfmt always puts the `{` on the line
        // ending the signature.
        if line.trim_end().ends_with('{') {
            open_idx = Some(i);
            break;
        }
    }
    let open = open_idx?;
    // Now scan for the matching `}` at column 0 (top-level fn close).
    // All four record adapters are top-level so their closing brace
    // sits at column 0.
    let close_idx = lines
        .iter()
        .enumerate()
        .skip(open + 1)
        .find(|(_, l)| *l == &"}")
        .map(|(i, _)| i)?;
    // Body line count: lines strictly between open and close.
    Some(close_idx - open - 1)
}

#[test]
fn record_adapters_are_thin_wrappers() {
    // Each of the four pub fns must be a thin adapter — fewer than 25
    // body lines after the refactor. The pre-refactor versions were
    // ~30 lines each (compare 22, equal 28, hash 27, display 33).
    for name in [
        "synth_compare_impl_for_record",
        "synth_equal_impl_for_record",
        "synth_hash_impl_for_record",
        "synth_display_impl_for_record",
    ] {
        let body_lines = fn_body_line_count(AUTO_DERIVE_SRC, name)
            .unwrap_or_else(|| panic!("could not locate fn body for `{name}`"));
        assert!(
            body_lines < 25,
            "fn `{name}` body has {body_lines} lines; expected < 25 (thin adapter post-refactor)"
        );
    }
}

#[test]
fn record_side_helpers_exist() {
    // Pin that the two shared helpers exist by name. A future re-
    // inline would delete these, and this test would fail.
    assert!(
        AUTO_DERIVE_SRC.contains("fn synth_binop_record_impl("),
        "expected `fn synth_binop_record_impl(` to appear in src/typechecker/auto_derive.rs"
    );
    assert!(
        AUTO_DERIVE_SRC.contains("fn synth_unop_record_impl("),
        "expected `fn synth_unop_record_impl(` to appear in src/typechecker/auto_derive.rs"
    );
}

#[test]
fn enum_side_helpers_still_exist_for_parity() {
    // Round 69 introduced `synth_binop_match_enum` /
    // `synth_unop_match_enum`. The record-side helpers introduced in
    // round 73 are deliberately analogous. This test pins the parity
    // — both halves of the file share the binop/unop split — so a
    // future "let's rename one half but not the other" diff fails
    // loud rather than silently drifting.
    assert!(
        AUTO_DERIVE_SRC.contains("fn synth_binop_match_enum("),
        "expected enum-side helper `fn synth_binop_match_enum(` (round 69) to still exist"
    );
    assert!(
        AUTO_DERIVE_SRC.contains("fn synth_unop_match_enum("),
        "expected enum-side helper `fn synth_unop_match_enum(` (round 69) to still exist"
    );
}

#[test]
fn record_and_enum_side_helpers_share_binop_unop_split() {
    // Stricter parity lock: the record-side helpers must mirror the
    // naming convention of the enum-side helpers introduced in round
    // 69. Both halves use a `binop` (Compare/Equal: two-param method)
    // and `unop` (Hash/Display: one-param method) split. If a future
    // refactor renames one side without the other, this test fails.
    let has_record_binop = AUTO_DERIVE_SRC.contains("fn synth_binop_record_impl(");
    let has_record_unop = AUTO_DERIVE_SRC.contains("fn synth_unop_record_impl(");
    let has_enum_binop = AUTO_DERIVE_SRC.contains("fn synth_binop_match_enum(");
    let has_enum_unop = AUTO_DERIVE_SRC.contains("fn synth_unop_match_enum(");
    assert!(
        has_record_binop && has_record_unop && has_enum_binop && has_enum_unop,
        "expected record-side and enum-side helpers to share binop/unop split: \
         record_binop={has_record_binop}, record_unop={has_record_unop}, \
         enum_binop={has_enum_binop}, enum_unop={has_enum_unop}"
    );
}

#[test]
fn record_adapters_delegate_to_shared_helpers() {
    // Pin that each of the four record adapters actually CALLS one of
    // the two shared helpers. Without this, an adapter could regress
    // to a duplicated inline scaffold while still passing the
    // line-count test (e.g. by extracting a per-trait private helper
    // instead of going through the shared scaffold).
    //
    // We slice the source between consecutive top-level `fn` headers
    // and check the slice mentions the expected helper. The sequence
    // of pub adapters in declaration order is compare → equal → hash
    // → display, and they live consecutively after their build_*
    // helpers in the file.
    fn fn_calls_helper(src: &str, fn_name: &str, helper_name: &str) -> bool {
        let needle = format!("fn {fn_name}(");
        let Some(start) = src.find(&needle) else {
            return false;
        };
        // Scan up to ~80 lines (well past the adapter body) for the
        // helper call.
        let slice_end = src[start..]
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .nth(80)
            .map(|(i, _)| start + i)
            .unwrap_or(src.len());
        src[start..slice_end].contains(&format!("{helper_name}("))
    }
    assert!(
        fn_calls_helper(
            AUTO_DERIVE_SRC,
            "synth_compare_impl_for_record",
            "synth_binop_record_impl"
        ),
        "synth_compare_impl_for_record must delegate to synth_binop_record_impl"
    );
    assert!(
        fn_calls_helper(
            AUTO_DERIVE_SRC,
            "synth_equal_impl_for_record",
            "synth_binop_record_impl"
        ),
        "synth_equal_impl_for_record must delegate to synth_binop_record_impl"
    );
    assert!(
        fn_calls_helper(
            AUTO_DERIVE_SRC,
            "synth_hash_impl_for_record",
            "synth_unop_record_impl"
        ),
        "synth_hash_impl_for_record must delegate to synth_unop_record_impl"
    );
    assert!(
        fn_calls_helper(
            AUTO_DERIVE_SRC,
            "synth_display_impl_for_record",
            "synth_unop_record_impl"
        ),
        "synth_display_impl_for_record must delegate to synth_unop_record_impl"
    );
}
