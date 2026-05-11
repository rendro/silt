//! Round 83 regression tests.
//!
//! ── Finding 1 (LATENT, soundness): AnonRecord `==` divergence ────────
//!
//! Before the fix, a spread of a nominal record (e.g. `{...person}`)
//! compiled down to `Op::RecordUpdate`, which preserves the base's
//! `type_name` in the resulting `Value::Record`. The typechecker
//! reports the static type of `{...person}` as `Type::AnonRecord`
//! though (see `src/typechecker/inference.rs` ~4099), so the result is
//! statically interchangeable with an anon-record literal of the same
//! shape — but `Value::Record::eq` checks the name first
//! (`src/value.rs` ~1714: `na == nb && fa == fb`). The runtime answer
//! diverged from the type-level answer: `{...p} == {x: 1}` returned
//! `false` despite identical fields and identical static types.
//!
//! Fix (option A): added a new opcode `Op::RecordUpdateAnon` with the
//! same operand layout as `RecordUpdate`, but the resulting
//! `Value::Record`'s `type_name` is overridden to `"<anon>"`. The
//! compiler now emits `RecordUpdateAnon` from the `ExprKind::AnonRecord
//! { spread: Some(_), .. }` site at `src/compiler/mod.rs` ~2647 — that
//! site is reached only when the typechecker reported the result as
//! `Type::AnonRecord`. Nominal `.{...}` updates (different `ExprKind`)
//! still emit `Op::RecordUpdate` and preserve the base's name.
//!
//! ── Finding 2 (STYLE/BLOAT): dead generic lex-error message ──────────
//!
//! `pre_typecheck_user_module` at `src/compiler/mod.rs` ~1394 used to
//! construct `CompileError { message: format!("lex error in
//! '{module_name}'"), ... }`, discarding the underlying `LexError`'s
//! span+message. The only caller drops the Result entirely (`let _ =
//! self.pre_typecheck_user_module(...)`), so this generic wording was
//! unreachable to end users — pure noise. Fix: route through the same
//! `format_module_source_error` helper the live path uses
//! (`compile_file_module_inner` ~1466), so the diagnostic carries a
//! real file path, line, column, and snippet if the error is ever
//! propagated.
//!
//! Source-level grep lock asserts the old wording never returns.

use std::process::Command;

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round83_{label}.silt"));
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

// ── Finding 1: the exact audit repro ─────────────────────────────────

/// The audit-supplied repro: a spread of a nominal `Person` value and
/// an anon-record literal of the same shape must compare equal.
///
/// This exercises the FULL execution path (compile + VM), not just
/// typecheck. The unfixed bytecode emitted `Op::RecordUpdate` for the
/// spread, preserving `"Person"` in the resulting `Value::Record`,
/// while the anon literal carried `"<anon>"` — so
/// `Value::Record::eq`'s `na == nb` check returned `false`.
#[test]
fn anonrec_spread_of_nominal_eq_anon_literal_prints_true() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let q = {...p}
  let r: {x: Int} = {x: 1}
  println(q == r)
}
"#;
    let out = run_silt_ok("spread_eq_anon", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "spread-of-nominal must compare equal to an anon-record literal of the same shape; got: {out:?}"
    );
}

/// `!=` is the dual of `==`. Same shape ⇒ `!=` returns `false`.
#[test]
fn anonrec_spread_of_nominal_neq_anon_literal_prints_false() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let q = {...p}
  let r: {x: Int} = {x: 1}
  println(q != r)
}
"#;
    let out = run_silt_ok("spread_neq_anon", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "false",
        "spread-of-nominal must be `!=`-equal to a matching anon literal; got: {out:?}"
    );
}

/// Distinct shapes (different field values) ⇒ `==` returns `false`.
/// Guards against an overzealous fix that collapses all anon records to
/// equal regardless of fields.
#[test]
fn anonrec_spread_different_values_eq_false() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let q = {...p}
  let r: {x: Int} = {x: 2}
  println(q == r)
}
"#;
    let out = run_silt_ok("spread_eq_diff_vals", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "false",
        "distinct field values must remain unequal; got: {out:?}"
    );
}

/// Spreading and extending with a new field still produces an
/// AnonRecord (with the wider shape) — comparing it with a matching
/// anon literal must succeed. Exercises the more interesting code path
/// where new fields are added on top of the spread.
#[test]
fn anonrec_spread_with_extension_eq_anon_literal() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let q = {...p, y: 7}
  let r: {x: Int, y: Int} = {x: 1, y: 7}
  println(q == r)
}
"#;
    let out = run_silt_ok("spread_extend_eq", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "spread-with-extension must compare equal to a matching anon literal; got: {out:?}"
    );
}

// ── Finding 1 sibling: don't break nominal-vs-nominal equality ───────

/// Two `Person` values with the same fields must still compare equal
/// — the spread fix must NOT regress nominal record equality.
#[test]
fn nominal_record_equality_preserved() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let s: Person = Person { x: 1 }
  println(p == s)
}
"#;
    let out = run_silt_ok("nominal_eq", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "nominal-vs-nominal record equality must be preserved; got: {out:?}"
    );
}

/// Nominal `.{...}` (record update, NOT a spread expression) still
/// returns the same nominal type — its `==` against another nominal
/// instance of the same shape must work. The compiler keeps emitting
/// `Op::RecordUpdate` for this site; only the spread site switched to
/// `RecordUpdateAnon`.
#[test]
fn nominal_dot_update_equality_preserved() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let q: Person = p.{ x: 2 }
  let r: Person = Person { x: 2 }
  println(q == r)
}
"#;
    let out = run_silt_ok("nominal_dot_update_eq", src);
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "nominal `.{{...}}` update must preserve nominal-eq semantics; got: {out:?}"
    );
}

// ── Finding 2: source-level grep lock for the dead lex-error wording ─

/// Source-level lock: the dead, generic
/// `"lex error in '<module>'"` wording must never reappear in
/// `src/compiler/mod.rs`. The live path uses the rich
/// `format_module_source_error` helper instead.
///
/// We grep with `include_str!` because the message was unreachable at
/// runtime (the only caller discarded the Result), so the only way to
/// lock against regression is a source-level check.
#[test]
fn dead_lex_error_message_does_not_reappear() {
    let src = include_str!("../src/compiler/mod.rs");
    assert!(
        !src.contains("lex error in '"),
        "src/compiler/mod.rs reintroduces the dead generic 'lex error in <module>' \
         message. Use format_module_source_error like the live path at line ~1466."
    );
}
