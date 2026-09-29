//! Trait-orphan rule: the REPL / scratch-package carve-out.
//!
//! The rule's positive and negative arms, the diagnostic wording and the
//! auto-derive exemption are golden cases
//! `tests/golden/lang/traits/orphan_rule__*`. The CLI always typechecks
//! under a package (`__local__` for a lone file), so the "no current
//! package" mode below is reachable only through
//! `typechecker::check_with_package(.., None)`.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Type-check `input` with no package context (REPL / ad-hoc script).
/// The orphan rule is disabled in this mode.
fn errors_no_pkg(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check_with_package(&mut program, None)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── REPL / scratch-package: rule disabled ─────────────────────────

/// With no current_package (REPL / ad-hoc script), every decl looks
/// local to the scratch package and the orphan rule is effectively
/// disabled. Locks the documented "playground" carve-out so the REPL
/// keeps working when the user types `trait Display for List(a)`
/// directly.
#[test]
fn orphan_rule_disabled_when_no_current_package() {
    let errs = errors_no_pkg(
        r#"
trait Display for List(a) {
  fn display(self) -> String { "scratch" }
}
"#,
    );
    let orphan: Vec<&String> = errs.iter().filter(|m| m.contains("orphan impl")).collect();
    assert!(
        orphan.is_empty(),
        "orphan rule must be disabled without current_package; got: {orphan:?}"
    );
}

// ── Direct typechecker probe (white-box) ───────────────────────────
//
// The pseudo-code in the implementation prompt suggests a white-box
// alternative when multi-package scaffolding is heavy: directly
// constructing the typechecker's package state. We exercise the same
// rule through the public `check_with_package` API instead — the test
// authoring is simpler and the rule's behaviour is identical.
