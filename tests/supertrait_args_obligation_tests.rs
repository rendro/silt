//! Round 60 B1 regression lock.
//!
//! `validate_trait_impls`' supertrait-obligation check walked only the
//! supertrait names; `trait_info.supertrait_args` was ignored. So
//! declaring `trait Holds(b): Carry(b)` and then registering
//! `impl Holds(Int) for Bag` passed the obligation check as long as
//! any `impl Carry(_) for Bag` existed — even a mismatched
//! `impl Carry(String) for Bag`. The subtrait's runtime dispatch then
//! crashed because the Carry method had the wrong signature for the
//! type the subtrait was parameterised against.
//!
//! The fix resolves `supertrait_args` through the enclosing trait's
//! `params → impl-args` mapping and requires the matching
//! `impl_trait_args[(supertrait, target)]` to be positionally
//! compatible with the resolved supertrait-arg list.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Repro from the audit finding: parameterised supertrait whose arg-list
/// does not match the concrete supertrait impl. Pre-fix this program
/// typechecked clean and blew up at runtime. Post-fix the typechecker
/// rejects at the impl site.
#[test]
fn test_supertrait_impl_args_must_match_subtrait_args() {
    let errs = type_errors(
        r#"
trait Carry(b) { fn carry(self, x: b) -> b }
trait Holds(b): Carry(b) { fn take(self, x: b) -> b }
type Bag { v: Int }
trait Carry(String) for Bag { fn carry(self, x: String) -> String { x + "!" } }
trait Holds(Int) for Bag { fn take(self, x: Int) -> Int { x } }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("requires impl Carry(Int) for Bag"),
        "expected supertrait-arg obligation error mentioning 'requires impl Carry(Int) for Bag', got:\n{joined}"
    );
    assert!(
        joined.contains("found impl Carry(String) for Bag"),
        "error should also identify the mismatched impl, got:\n{joined}"
    );
}

/// Counterpart: when the supertrait impl's args match the subtrait's
/// args, no error. Locks that the new check doesn't over-reject.
#[test]
fn test_matching_supertrait_impl_accepted() {
    let errs = type_errors(
        r#"
trait Carry(b) { fn carry(self, x: b) -> b }
trait Holds(b): Carry(b) { fn take(self, x: b) -> b }
type Bag { v: Int }
trait Carry(Int) for Bag { fn carry(self, x: Int) -> Int { x + 1 } }
trait Holds(Int) for Bag { fn take(self, x: Int) -> Int { x } }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "matching supertrait impl args should typecheck, got:\n{}",
        errs.join("\n")
    );
}

/// Bare-supertrait (no args) continues to work — the round-58
/// supertrait-without-args chain-through path must not regress.
#[test]
fn test_bare_supertrait_unaffected() {
    let errs = type_errors(
        r#"
trait Eq2 { fn eq2(self, other: Self) -> Bool }
trait Ord2: Eq2 { fn lt2(self, other: Self) -> Bool }
type Foo { v: Int }
trait Eq2 for Foo { fn eq2(self, other: Foo) -> Bool { self.v == other.v } }
trait Ord2 for Foo { fn lt2(self, other: Foo) -> Bool { self.v < other.v } }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "bare supertrait with matching impl should typecheck, got:\n{}",
        errs.join("\n")
    );
}
