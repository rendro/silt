//! Regression tests for the impl-level / method-level `where` clause
//! trait-args soundness fix.
//!
//! Round 58 closed the same hole at the FnDecl level — `verify_trait_obligation`
//! takes `bound_trait_args` and rejects mismatched parameterized impls. The
//! impl-level path (`trait Use for List(a) where a: Conv(Int)`) and the
//! method-level path (`fn do_it(self) where a: Conv(Int)`) silently ignored
//! the bound's trait args (the `_trait_args` discard in
//! `register_trait_impl`), so a `Conv(Int)` bound would silently accept any
//! `Conv(*) for ...` impl and produce a runtime crash inside the method
//! body when the trait method returned the wrong shape.
//!
//! Each test below pins both:
//!   - the typecheck-time rejection (the bound's trait args MUST be checked
//!     against the matched impl's args);
//!   - the runtime path via subprocess `silt run` (sanity that the program
//!     compiles and runs as expected — used as a belt-and-braces guard).
//!
//! These tests run the fix end-to-end. Pre-fix, all four would FAIL: the
//! BROKEN-1 / BROKEN-2 reproductions would `silt check` clean and crash at
//! runtime; the GAP-1 reproduction would land its caret on the wrong line.

use std::process::Command;

use silt::lexer::Span;
use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors_full(input: &str) -> Vec<(String, Span)> {
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
        .map(|e| (e.message, e.span))
        .collect()
}

fn type_errors(input: &str) -> Vec<String> {
    type_errors_full(input)
        .into_iter()
        .map(|(m, _)| m)
        .collect()
}

/// Run a silt source program via the `silt run` subprocess and return
/// (stdout, stderr, success). Mirrors the helper in
/// `tests/vm_trait_dispatch_runtime_tests.rs`.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_trait_args_where_{label}.silt"));
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

// ── BROKEN-1: impl-level `where` clause with trait args ─────────────

/// `trait UseIt for List(a) where a: Conv(Int)` against `Conv(String) for Int`.
///
/// Pre-fix: typecheck passed (`_trait_args` was dropped at
/// `src/typechecker/mod.rs:5542` so the impl-level constraint was stored
/// without args; `verify_trait_obligation` recursed with hard-coded `&[]`,
/// which falls through to the bare "trait implemented" check). At runtime
/// the body would call `x.conv()` and bind it to `Int`, then add it to a
/// recursive `Int` — but the actual return is the impl's `String`,
/// producing `cannot apply '+' to String and Int`.
///
/// Post-fix: typecheck rejects with a diagnostic citing both the bound's
/// args (`Conv(Int)`) and the matched impl's args (`Conv(String)`).
#[test]
fn impl_level_where_clause_trait_args_runtime() {
    // Property the test pins: the typechecker MUST reject this
    // program (the soundness hole). We assert the diagnostic
    // mentions the trait-args mismatch using the same shape the
    // round-58 FnDecl-level test uses.
    let src = r#"
trait Conv(to) { fn conv(self) -> to }
trait Conv(String) for Int { fn conv(self) -> String = "an int" }
trait UseIt { fn use_it(self) -> Int }
trait UseIt for List(a) where a: Conv(Int) {
    fn use_it(self) -> Int = match self {
        [] -> 0
        [x, ..rest] -> { let n: Int = x.conv() n + rest.use_it() }
    }
}
fn main() {
    let xs: List(Int) = [1, 2, 3]
    let r: Int = xs.use_it()
    println(r)
}
"#;
    let errs = type_errors(src);
    assert!(
        !errs.is_empty(),
        "impl-level where-clause trait-args mismatch MUST reject; got no errors"
    );
    assert!(
        errs.iter().any(|e| {
            (e.contains("Conv(Int)") || (e.contains("Conv") && e.contains("Int")))
                && (e.contains("does not implement") || e.contains("Conv(String)"))
        }),
        "expected 'does not implement Conv(Int)' (or similar) citing both \
         the bound's args and the impl's args; got: {errs:?}"
    );

    // Belt-and-braces runtime guard: `silt run` MUST NOT succeed
    // (it should hit the typecheck-time rejection above, not produce
    // a clean run of the program). Pre-fix, this would print 0 and
    // exit 0 after typecheck silently passed and the runtime hit the
    // `String + Int` panic. Post-fix, typecheck fails cleanly so
    // `silt run` exits non-zero with the type error on stderr.
    let (stdout, stderr, ok) = run_silt_raw("impl_level", src);
    assert!(
        !ok,
        "silt run MUST NOT succeed for the soundness hole repro; \
         stdout={stdout}, stderr={stderr}"
    );
}

// ── BROKEN-2: method-level `where` clause with trait args ───────────

/// Method-level `where` clause variant of the same hole. The discard is
/// at `src/typechecker/mod.rs:5886`. Pre-fix this also typechecked clean
/// and crashed at runtime; post-fix typecheck rejects.
#[test]
fn impl_method_where_clause_trait_args_runtime() {
    let src = r#"
trait Conv(to) { fn conv(self) -> to }
trait Conv(String) for Int { fn conv(self) -> String = "an int" }
type Box(a) { Wrap(a) }
trait Foo { fn do_it(self) -> Int }
trait Foo for Box(a) {
    fn do_it(self) -> Int where a: Conv(Int) = match self {
        Wrap(x) -> { let n: Int = x.conv() n + 100 }
    }
}
fn main() {
    let b: Box(Int) = Wrap(5)
    let r: Int = b.do_it()
    println(r)
}
"#;
    let errs = type_errors(src);
    assert!(
        !errs.is_empty(),
        "impl-method where-clause trait-args mismatch MUST reject; got no errors"
    );
    assert!(
        errs.iter().any(|e| {
            (e.contains("Conv(Int)") || (e.contains("Conv") && e.contains("Int")))
                && (e.contains("does not implement") || e.contains("Conv(String)"))
        }),
        "expected 'does not implement Conv(Int)' (or similar) citing both \
         the bound's args and the impl's args; got: {errs:?}"
    );

    let (stdout, stderr, ok) = run_silt_raw("impl_method", src);
    assert!(
        !ok,
        "silt run MUST NOT succeed for the soundness hole repro; \
         stdout={stdout}, stderr={stderr}"
    );
}

// ── GAP-1: trait-impl method signature-mismatch span ─────────────────

/// The diagnostic for a trait-impl method whose signature differs from
/// the trait declaration MUST point at the offending method's signature
/// line — NOT at the impl-block header. Pre-fix, `MethodEntry.span` was
/// set to `ti.span` (the impl block) at `src/typechecker/mod.rs:5934`,
/// and the unify-based mismatch error at `mod.rs:3272` reported through
/// that span — landing the caret at `trait Foo(String) for String {`.
///
/// Post-fix, `MethodEntry.span` is set to `method.span`, so the caret
/// lands on the `fn bar(self) -> String { ... }` line.
#[test]
fn trait_impl_method_signature_mismatch_span_points_at_method() {
    // The trait declares `fn bar(self: a) -> Int`; the impl declares
    // `fn bar(self) -> String { ... }` — a return-type mismatch. The
    // body returns a String, the trait expects Int. The mismatch error
    // should land on line 4 (the `fn bar` line), not line 3 (the impl
    // block opening).
    let src = "\
trait Foo(a) { fn bar(self: a) -> Int }
trait Foo(String) for String {
  fn bar(self) -> String { let x = \"hello\" x }
}
fn main() { println(\"x\".bar()) }
";
    let errs = type_errors_full(src);
    let mismatches: Vec<_> = errs
        .iter()
        .filter(|(m, _)| m.contains("type mismatch") || m.contains("expected") && m.contains("got"))
        .collect();
    assert!(
        !mismatches.is_empty(),
        "expected at least one 'type mismatch' error from the bar() \
         signature mismatch, got: {errs:?}"
    );
    // Pin the exact line: the impl-method row, line 3 in the source
    // above (the `fn bar(self) -> String { ... }` line). `ti.span`
    // would point at line 2 (the `trait Foo(String) for String {` row).
    let on_method_line: Vec<_> = mismatches.iter().filter(|(_, s)| s.line == 3).collect();
    assert!(
        !on_method_line.is_empty(),
        "expected mismatch error on line 3 (fn bar) — pre-fix it lands \
         on line 2 (impl block header). Got mismatches: {mismatches:?}"
    );
    for (_, span) in &mismatches {
        assert_ne!(
            span.line, 2,
            "mismatch error MUST NOT land on the impl-block header (line 2); \
             span={span:?}, mismatches={mismatches:?}"
        );
    }
}

// ── LATENT-1: source-grep lock for the (TBD) comment removal ────────

/// Lock test pinning the LATENT-1 cleanup. Round 62 closed long ago; the
/// comment at `src/typechecker/mod.rs:4761` (`See the round-62 audit (TBD)`)
/// was a stale follow-up marker. The fix replaces `(TBD)` with a clean
/// reference. This test pins the literal string is gone from the source
/// so a future re-introduction of the marker (or a copy-paste from old
/// notes) trips immediately.
#[test]
fn round_62_audit_tbd_marker_is_removed() {
    let src = std::fs::read_to_string("src/typechecker/mod.rs")
        .expect("typechecker/mod.rs must be readable from the test working dir");
    assert!(
        !src.contains("round-62 audit (TBD)"),
        "src/typechecker/mod.rs still contains the stale 'round-62 audit \
         (TBD)' marker — replace `(TBD)` with the resolved reference"
    );
}
