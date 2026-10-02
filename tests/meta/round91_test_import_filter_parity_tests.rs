//! Round 91 GAP regression: `silt test` must apply the SAME user-import
//! diagnostic filter as `silt run`/`silt check`/LSP.
//!
//! The filter leaves out BOTH the "unknown module" warning AND the
//! follow-on undefined-name / trait cascade that the compiler resolves at
//! link time. Every front door routes through the one function
//! `silt::typechecker::without_import_cascade`, which decides by code,
//! never by message text. The wiring at the bug site (`silt test`) is
//! locked behaviourally by tests/lang/round92_test_import_filter_e2e_tests.rs.

use silt::diagnostic::{Code, Diagnostic};
use silt::source::{FileId, Span};
use silt::typechecker::without_import_cascade;

fn at() -> Span {
    Span::point(FileId::default(), 0)
}

fn unknown_module() -> Diagnostic {
    Diagnostic::warning(Code::UnknownModule, at(), "unknown module 'foo'")
}

fn codes(ds: Vec<Diagnostic>) -> Vec<Code> {
    ds.into_iter().map(|d| d.code).collect()
}

/// The "unknown module" warning itself is always left out.
#[test]
fn unknown_module_warning_is_always_suppressed() {
    assert!(without_import_cascade(vec![unknown_module()]).is_empty());
}

/// With the warning present, the undefined-name and trait cascade is left
/// out.
#[test]
fn undefined_name_cascade_suppressed_when_user_import_warning_present() {
    let cascade = [
        Code::UndefinedVariable,
        Code::UndefinedConstructor,
        Code::UndefinedType,
        Code::UnknownField,
        Code::MissingTraitImpl,
    ];
    let mut ds = vec![unknown_module()];
    ds.extend(cascade.iter().map(|&c| Diagnostic::error(c, at(), "x")));
    assert!(without_import_cascade(ds).is_empty());
}

/// Without the warning, the same errors are real and stay.
#[test]
fn undefined_name_not_suppressed_without_user_import_warning() {
    let ds = vec![Diagnostic::error(
        Code::UndefinedVariable,
        at(),
        "undefined variable 'x'",
    )];
    assert_eq!(
        codes(without_import_cascade(ds)),
        vec![Code::UndefinedVariable]
    );
}

/// A real type mismatch is never left out.
#[test]
fn real_type_mismatch_never_suppressed() {
    let ds = vec![
        unknown_module(),
        Diagnostic::error(
            Code::TypeMismatch,
            at(),
            "type mismatch: expected Int, got String",
        ),
    ];
    assert_eq!(codes(without_import_cascade(ds)), vec![Code::TypeMismatch]);
}
