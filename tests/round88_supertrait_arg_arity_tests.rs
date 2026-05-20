//! Round 88 LATENT regression lock.
//!
//! `resolve_supertrait_arg` (in `src/typechecker/inference.rs`) used to
//! handle `TypeExprKind::Named(sym)` by:
//!   1. mapping `sym` through the enclosing trait's params (substitution),
//!   2. checking for builtin names (Int/Float/Bool/String),
//!   3. falling through to `Type::Generic(*sym, Vec::new())` for any
//!      unrecognised uppercase name.
//!
//! Step 3 silently accepted parametric type names used without arguments
//! in a supertrait bound, e.g.
//!
//!     type Box(a) { Boxed(a) }
//!     trait Carry(t) { fn carry(self, x: t) -> t }
//!     trait Holds: Carry(Box) { ... }   -- BUG: `Box` is parametric
//!
//! The resulting 0-arity `Generic("Box", [])` never matched any downstream
//! impl's actual args, so the obligation went unsatisfied without any
//! user-facing diagnostic. Post-fix the typechecker emits a clean arity
//! error attached to the supertrait declaration's span.
//!
//! Tests:
//! 1. Negative: `trait Sub: Super(Box)` where `Box(a)` is parametric →
//!    arity diagnostic IS produced (was silently accepted before).
//! 2. Positive: `trait Sub: Super(Box(Int))` → typechecks OK (the user
//!    supplied the missing arg).
//! 3. Negative: parametric record (no params supplied) in supertrait →
//!    same arity diagnostic.
//! 4. Negative: nested parametric name (`Super(Generic(Box))`) — diagnostic
//!    fires recursively.

use std::process;

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Touch a tmp path keyed on `process::id()` so parallel test runs do not
/// collide. Currently unused (the typechecker check runs in-memory) but
/// kept so the harness reflects the convention adopted by other round-NN
/// regression tests in this crate.
fn _tmp_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("silt-round88-supertrait-arity-{}", process::id()))
}

/// 1. Negative: parametric enum used as a bare supertrait arg must
///    produce an arity diagnostic. Pre-fix this silently typechecked.
#[test]
fn test_parametric_enum_bare_in_supertrait_emits_arity_error() {
    let _ = _tmp_path();
    let errs = type_errors(
        r#"
type Box(a) { Boxed(a) }
trait Carry(t) { fn carry(self, x: t) -> t }
trait Holds: Carry(Box) { fn hold(self) -> Int }

fn use_holds(x: t) -> Int where t: Holds {
  x.hold()
}

fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("'Box'")
            && joined.contains("supertrait bound")
            && (joined.contains("type argument") || joined.contains("type arguments")),
        "expected arity diagnostic mentioning 'Box', 'supertrait bound', and type arguments, got:\n{joined}"
    );
}

/// 2. Positive: supplying the missing arg makes the supertrait reference
///    well-formed. The arity diagnostic must NOT fire here. (Other
///    obligations may or may not be satisfied depending on impls — the
///    assertion only locks the absence of the arity message.)
#[test]
fn test_parametric_enum_with_args_in_supertrait_no_arity_error() {
    let _ = _tmp_path();
    let errs = type_errors(
        r#"
type Box(a) { Boxed(a) }
trait Carry(t) { fn carry(self, x: t) -> t }
trait Holds: Carry(Box(Int)) { fn hold(self) -> Int }

fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        !joined.contains("expects") || !joined.contains("supertrait bound"),
        "did not expect an arity-in-supertrait-bound diagnostic when Box's arg was supplied; got:\n{joined}"
    );
}

/// 3. Negative variant: parametric record used as a bare supertrait arg.
///    Same diagnostic path as enums — uses `record_param_var_ids`.
#[test]
fn test_parametric_record_bare_in_supertrait_emits_arity_error() {
    let _ = _tmp_path();
    let errs = type_errors(
        r#"
type Wrapper(a) { v: a }
trait Carry(t) { fn carry(self, x: t) -> t }
trait Holds: Carry(Wrapper) { fn hold(self) -> Int }

fn use_holds(x: t) -> Int where t: Holds {
  x.hold()
}

fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("'Wrapper'")
            && joined.contains("supertrait bound")
            && (joined.contains("type argument") || joined.contains("type arguments")),
        "expected arity diagnostic mentioning 'Wrapper' in supertrait bound, got:\n{joined}"
    );
}

/// 4. Negative: nested parametric name inside a Generic position.
///    `Super(List(Box))` should still flag `Box` even though the outer
///    spot is a Generic application. Locks that the recursive walk
///    traverses into Generic args.
#[test]
fn test_parametric_name_inside_generic_arg_still_flagged() {
    let _ = _tmp_path();
    let errs = type_errors(
        r#"
type Box(a) { Boxed(a) }
trait Carry(t) { fn carry(self, x: t) -> t }
trait Holds: Carry(List(Box)) { fn hold(self) -> Int }

fn use_holds(x: t) -> Int where t: Holds {
  x.hold()
}

fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("'Box'") && joined.contains("supertrait bound"),
        "expected arity diagnostic for nested 'Box' inside List(...), got:\n{joined}"
    );
}
