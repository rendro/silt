//! Round 89 (BROKEN): accessing an unknown MEMBER of a valid imported
//! USER module reported a misleading "undefined variable '<module>'"
//! pointing at the module name, instead of telling the user the member
//! doesn't exist.
//!
//! Repro: `a.silt = pub fn divide(x, y) = x / y`; main does `import a`
//! then `a.nope()`. Before the fix the typechecker inferred the bare
//! module identifier `a` as a variable and emitted `undefined variable
//! 'a'`. The real fault is that `nope` is not a member of `a`.
//!
//! Root cause was in `src/typechecker/inference.rs` FieldAccess arm: the
//! "unknown function 'X' on module 'Y'" diagnostic only fired for builtin
//! modules (`is_builtin_module`). For a known imported USER module the
//! failed member lookup fell through to `infer_expr(obj)`, which inferred
//! the bare module name as an undefined variable.
//!
//! Fix: mirror the builtin branch for any module tracked in
//! `imported_modules` (plain name or alias), emitting `unknown function
//! '<field>' on module '<module>'` on the field span.
//!
//! These tests run the package typecheck path (matching `pipeline.rs`),
//! so they fail before the fix and pass after.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use silt::compiler::Compiler;
use silt::intern::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;

/// Create a fresh tempdir holding the supplied module files plus a
/// `main.silt` containing `main_source`. Returns the dir path.
fn setup_dir(files: &[(&str, &str)], main_source: &str) -> PathBuf {
    let raw_dir =
        std::env::temp_dir().join(format!("silt_r89_{}_{}", std::process::id(), rand_u64()));
    let _ = fs::remove_dir_all(&raw_dir);
    fs::create_dir_all(&raw_dir).expect("mkdir");
    let dir = raw_dir.canonicalize().expect("canonicalize tempdir");
    for (name, content) in files {
        fs::write(dir.join(name), content).expect("write module");
    }
    fs::write(dir.join("main.silt"), main_source).expect("write main");
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    dir
}

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Run typecheck-only against `main.silt` in `dir`. Mirrors what
/// `pipeline.rs` does for a package: pre-typecheck sibling imports, then
/// check with the accumulated exports.
fn typecheck_main_in(dir: &PathBuf) -> Vec<typechecker::TypeError> {
    let main_path = dir.join("main.silt");
    let source = fs::read_to_string(&main_path).expect("read main");
    let tokens = Lexer::new(&source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");

    let local_pkg = intern("__test__");
    let mut roots = HashMap::new();
    roots.insert(local_pkg, dir.clone());
    let mut compiler = Compiler::with_package_roots(local_pkg, roots);
    compiler.pre_typecheck_imports(&program);
    let exports = compiler.module_exports_snapshot();
    let (errors, _) =
        typechecker::check_with_package_and_imports(&mut program, Some(local_pkg), exports);
    errors
}

fn messages(errs: &[typechecker::TypeError]) -> Vec<String> {
    errs.iter().map(|e| e.message.clone()).collect()
}

const LIB: &str = "pub fn divide(x: Int, y: Int) -> Int = x / y\n";

/// (a) Plain `import lib`: accessing the unknown member `nope` must
/// report it as an unknown function on the module — NOT as an undefined
/// variable on the module name.
#[test]
fn plain_import_unknown_member_reports_unknown_function_on_module() {
    let dir = setup_dir(
        &[("lib.silt", LIB)],
        r#"
import lib

fn main() {
  println(lib.nope())
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let msgs = messages(&errs);

    assert!(
        msgs.iter()
            .any(|m| m.contains("unknown function 'nope' on module 'lib'")),
        "expected 'unknown function 'nope' on module 'lib''; got: {msgs:?}"
    );
    assert!(
        !msgs.iter().any(|m| m.contains("undefined variable 'lib'")),
        "must NOT emit the misleading 'undefined variable 'lib''; got: {msgs:?}"
    );
}

/// (a') Same misleading path for non-call field access (`let f = lib.nope`).
#[test]
fn plain_import_unknown_member_field_access_reports_unknown_function() {
    let dir = setup_dir(
        &[("lib.silt", LIB)],
        r#"
import lib

fn main() {
  let f = lib.nope
  f
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let msgs = messages(&errs);

    assert!(
        msgs.iter()
            .any(|m| m.contains("unknown function 'nope' on module 'lib'")),
        "expected 'unknown function 'nope' on module 'lib''; got: {msgs:?}"
    );
    assert!(
        !msgs.iter().any(|m| m.contains("undefined variable 'lib'")),
        "must NOT emit the misleading 'undefined variable 'lib''; got: {msgs:?}"
    );
}

/// (b) Aliased `import lib as mylib`: the diagnostic must use the alias
/// the user wrote (`mylib`), and must not be the misleading undefined
/// variable on the alias.
#[test]
fn aliased_import_unknown_member_reports_unknown_function_on_alias() {
    let dir = setup_dir(
        &[("lib.silt", LIB)],
        r#"
import lib as mylib

fn main() {
  println(mylib.nope())
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let msgs = messages(&errs);

    assert!(
        msgs.iter()
            .any(|m| m.contains("unknown function 'nope' on module 'mylib'")),
        "expected 'unknown function 'nope' on module 'mylib''; got: {msgs:?}"
    );
    assert!(
        !msgs
            .iter()
            .any(|m| m.contains("undefined variable 'mylib'")),
        "must NOT emit the misleading 'undefined variable 'mylib''; got: {msgs:?}"
    );
}

/// (c) The valid member `lib.divide(...)` must still typecheck clean —
/// the fix must not introduce a false error on real exports.
#[test]
fn valid_member_access_typechecks_clean() {
    let dir = setup_dir(
        &[("lib.silt", LIB)],
        r#"
import lib

fn main() {
  println(lib.divide(10, 2))
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors: Vec<&typechecker::TypeError> = errs
        .iter()
        .filter(|e| e.severity == typechecker::Severity::Error)
        .collect();
    assert!(
        real_errors.is_empty(),
        "valid member access must typecheck clean; got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
