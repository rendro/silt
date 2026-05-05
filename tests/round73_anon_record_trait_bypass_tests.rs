//! Round 73 B2 (BROKEN, soundness): user-trait `where` constraints were
//! silently bypassed when the receiver was an anonymous (structural)
//! record.
//!
//! Repro: `fn use_it(x: a) -> Int where a: MyTrait { x.ident() }` called
//! with `r = {x: 1, y: 2}`. Pre-fix:
//!   - `verify_trait_obligation` resolved the type via
//!     `type_name_for_impl`, which returned `None` for `Type::AnonRecord`
//!     via the catch-all `_ => None`.
//!   - `None` was treated by callers (mod.rs:1916, inference.rs:1117 /
//!     3204 / 3557) as "still unresolved — defer", so the obligation
//!     never fired the "type X does not implement trait Y" diagnostic.
//!   - The program crashed at runtime with `no method 'ident' for type
//!     '<anon>'`.
//!
//! Post-fix: `type_name_for_impl` returns a synthetic name
//! `"AnonRecord"` for the AnonRecord arm. No user impl block can ever
//! target this synthetic key (impl blocks bind to nominal heads), so
//! the existing `trait_impl_set.contains(...)` check at
//! `verify_trait_obligation` correctly rejects the receiver.

use silt::typechecker;
use silt::types::Severity;

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

/// Lock test: passing an anon-record receiver to a fn whose `where`
/// constraint requires a user trait that anon records cannot implement
/// must produce a typecheck error.
///
/// Wording: the synthetic dispatch name for anon records is `<anon>`
/// (matching `canonical_name(Type::AnonRecord{..})` in
/// `src/types/canonical.rs:450` — the same key the rest of the
/// dispatch surface uses for these types). The diagnostic therefore
/// reads `type '<anon>' does not implement trait 'MyTrait'`.
#[test]
fn anon_record_does_not_implement_user_trait_at_typecheck() {
    let errs = type_errors(
        r#"
trait MyTrait { fn ident(self) -> Int }
trait MyTrait for Int { fn ident(self) = self }
fn use_it(x: a) -> Int where a: MyTrait { x.ident() }
fn main() {
    let r = {x: 1, y: 2}
    println(use_it(r))
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a typecheck error rejecting anon-record vs MyTrait; \
         got none — runtime crash slipped through"
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("does not implement trait")
                && e.contains("MyTrait")
                && e.contains("<anon>")
        }),
        "expected an error of the form \"type '<anon>' does not implement trait 'MyTrait'\"; \
         got: {errs:?}"
    );
}

/// Same shape but with a nominal record receiver that doesn't implement
/// the trait — the existing diagnostic must still fire (sanity check
/// that the AnonRecord-arm addition didn't shift the nominal path).
#[test]
fn nominal_record_without_impl_still_rejected() {
    let errs = type_errors(
        r#"
trait MyTrait { fn ident(self) -> Int }
trait MyTrait for Int { fn ident(self) = self }
type Foo { x: Int }
fn use_it(x: a) -> Int where a: MyTrait { x.ident() }
fn main() {
    let r = Foo { x: 1 }
    println(use_it(r))
}
"#,
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("does not implement trait") && e.contains("MyTrait") && e.contains("Foo")
        }),
        "expected a 'Foo does not implement MyTrait' error; got: {errs:?}"
    );
}

/// Sanity: anon record passed to a fn WITHOUT a where constraint must
/// still typecheck. Our fix must not over-reject.
#[test]
fn anon_record_to_unconstrained_fn_still_passes() {
    let errs = type_errors(
        r#"
fn pass_through(x: a) -> a { x }
fn main() {
    let r = {x: 1, y: 2}
    pass_through(r)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "anon record to unconstrained fn should typecheck; got: {errs:?}"
    );
}

/// End-to-end: confirm that the typecheck error surfaces before the
/// runtime would have crashed. We drive lex → parse → typecheck and
/// assert hard errors exist; the program in the OLD codebase reached
/// runtime and crashed with `no method 'ident' for type '<anon>'`.
#[test]
fn anon_record_trait_bypass_blocks_compile() {
    let src = r#"
trait MyTrait { fn ident(self) -> Int }
trait MyTrait for Int { fn ident(self) = self }
fn use_it(x: a) -> Int where a: MyTrait { x.ident() }
fn main() {
    let r = {x: 1, y: 2}
    println(use_it(r))
}
"#;
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let type_errs = typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        !hard_errors.is_empty(),
        "typecheck must produce at least one hard error for AnonRecord/MyTrait; got: {type_errs:?}"
    );
    assert!(
        hard_errors
            .iter()
            .any(|e| e.message.contains("<anon>") && e.message.contains("MyTrait")),
        "typecheck error must mention <anon> and MyTrait; got: {hard_errors:?}"
    );
}
