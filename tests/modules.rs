use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;

/// Build a Compiler whose only package is the synthetic `__local__`,
/// rooted at `root`. Equivalent to the pre-PR-4 `with_project_root`
/// shim (which the compiler module rightly dropped) — preserved here
/// so the legacy module tests don't have to be rewritten to stage a
/// real `silt.toml`/`src/` layout.
fn compiler_for_root(root: PathBuf) -> Compiler {
    let local = intern::intern("__local__");
    let mut roots = HashMap::new();
    roots.insert(local, root);
    Compiler::with_package_roots(local, roots)
}

/// Helper: create a temp directory with module files, parse and run the main program.
fn run_module_test(files: &[(&str, &str)], main_source: &str) -> Value {
    let dir = tempdir();

    // Write all module files
    for (name, content) in files {
        let path = dir.join(name);
        fs::write(&path, content).expect("failed to write module file");
    }

    // Parse and compile the main source with project root set to the temp dir
    let tokens = Lexer::new(main_source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_module_test_err(files: &[(&str, &str)], main_source: &str) -> String {
    let dir = tempdir();

    for (name, content) in files {
        let path = dir.join(name);
        fs::write(&path, content).expect("failed to write module file");
    }

    let tokens = Lexer::new(main_source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    match compiler.compile_program(&program) {
        Ok(functions) => {
            let script = Arc::new(functions.into_iter().next().unwrap());
            let mut vm = Vm::new();
            match vm.run(script) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("expected error but got success"),
            }
        }
        Err(e) => e.message,
    }
}

/// Helper to run a simple program via the VM (no temp dir needed).
fn run_vm(source: &str) -> Value {
    let tokens = Lexer::new(source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Create a temporary directory for test module files.
fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_test_{}", std::process::id()));
    // Use a sub-directory with a random-ish name to avoid collisions
    let sub = dir.join(format!("{}", rand_u64()));
    fs::create_dir_all(&sub).expect("failed to create temp dir");
    sub
}

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    d.as_nanos() as u64
}

// ── Basic module import ─────────────────────────────────────────────

#[test]
fn test_import_module_qualified() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
fn internal_helper(x) = x * 2
        "#,
        )],
        r#"
import calc

fn main() {
  calc.add(3, 4)
}
        "#,
    );
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_import_module_multiple_functions() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#,
        )],
        r#"
import calc

fn main() {
  calc.add(calc.square(3), calc.square(4))
}
        "#,
    );
    assert_eq!(result, Value::Int(25));
}

// ── Selective import ────────────────────────────────────────────────

#[test]
fn test_import_specific_items() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#,
        )],
        r#"
import calc.{ add, square }

fn main() {
  add(square(3), square(4))
}
        "#,
    );
    assert_eq!(result, Value::Int(25));
}

#[test]
fn test_import_single_item() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#,
        )],
        r#"
import calc.{ add }

fn main() {
  add(10, 20)
}
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

// ── Alias import ────────────────────────────────────────────────────

#[test]
fn test_import_module_with_alias() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#,
        )],
        r#"
import calc as m

fn main() {
  m.add(3, 4)
}
        "#,
    );
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_import_alias_multiple_calls() {
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
pub fn mul(a, b) = a * b
        "#,
        )],
        r#"
import calc as m

fn main() {
  m.add(m.mul(2, 3), m.mul(4, 5))
}
        "#,
    );
    assert_eq!(result, Value::Int(26));
}

// ── Pub visibility enforcement ──────────────────────────────────────

#[test]
fn test_private_function_not_importable_qualified() {
    let err = run_module_test_err(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
fn secret(x) = x * 2
        "#,
        )],
        r#"
import calc

fn main() {
  calc.secret(5)
}
        "#,
    );
    // The compiler detects "exists-but-private" at compile time and emits
    // a visibility error naming the function, the module, and the fix.
    assert!(
        err.contains("`secret` exists in module `calc` but is not `pub`"),
        "expected visibility error about `secret`, got: {err}"
    );
}

#[test]
fn test_private_function_not_selectively_importable() {
    let err = run_module_test_err(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
fn secret(x) = x * 2
        "#,
        )],
        r#"
import calc.{ secret }

fn main() {
  secret(5)
}
        "#,
    );
    assert!(
        err.contains("undefined global: calc.secret"),
        "expected 'undefined global: calc.secret', got: {err}"
    );
}

// ── Module caching ──────────────────────────────────────────────────

#[test]
fn test_module_loaded_only_once() {
    // Importing the same module twice should work (cached)
    let result = run_module_test(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
        "#,
        )],
        r#"
import calc
import calc.{ add }

fn main() {
  add(calc.add(1, 2), 3)
}
        "#,
    );
    assert_eq!(result, Value::Int(6));
}

// ── Module not found ────────────────────────────────────────────────

#[test]
fn test_module_not_found() {
    let err = run_module_test_err(
        &[],
        r#"
import nonexistent

fn main() {
  nonexistent.foo()
}
        "#,
    );
    assert!(
        err.contains("cannot load module"),
        "expected file-not-found error, got: {err}"
    );
}

// ── Builtin module imports ──────────────────────────────────────────

#[test]
fn test_import_builtin_string_module() {
    // `import string` should be a no-op (builtins already registered)
    // and string.split should still work
    let result = run_vm(
        r#"
import string

fn main() {
  let parts = "a,b,c" |> string.split(",")
  parts
}
    "#,
    );
    assert_eq!(
        result,
        Value::List(std::sync::Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_items() {
    // `import string.{ split }` should bring split into scope directly
    let result = run_vm(
        r#"
import string.{ split }

fn main() {
  "a,b,c" |> split(",")
}
    "#,
    );
    assert_eq!(
        result,
        Value::List(std::sync::Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_with_alias() {
    // `import string as s` should make s.split available
    let result = run_vm(
        r#"
import string as s

fn main() {
  "hello world" |> s.split(" ")
}
    "#,
    );
    assert_eq!(
        result,
        Value::List(std::sync::Arc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_io_module() {
    let result = run_vm(
        r#"
import io
import list

fn main() {
  let args = io.args()
  -- just verify it returns a list
  list.length(args)
}
    "#,
    );
    // Should return some Int (the number of args)
    match result {
        Value::Int(_) => {} // ok
        other => panic!("expected Int, got {other}"),
    }
}

// ── Module with types ───────────────────────────────────────────────

#[test]
fn test_module_with_pub_type() {
    let result = run_module_test(
        &[(
            "shapes.silt",
            r#"
pub type Shape {
  Circle(Float)
  Rect(Float, Float)
}

pub fn area(shape) {
  match shape {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
  }
}
        "#,
        )],
        r#"
import shapes.{ area, Shape }

fn main() {
  area(Rect(3.0, 4.0))
}
        "#,
    );
    assert_eq!(result, Value::Float(12.0));
}

// ── Circular import detection ───────────────────────────────────────

#[test]
fn test_circular_import_detected() {
    let err = run_module_test_err(
        &[
            (
                "a.silt",
                r#"
import b
pub fn fa() = 1
            "#,
            ),
            (
                "b.silt",
                r#"
import a
pub fn fb() = 2
            "#,
            ),
        ],
        r#"
import a

fn main() {
  a.fa()
}
        "#,
    );
    assert!(
        err.contains("circular import"),
        "expected circular import error, got: {err}"
    );
}

// ── Multi-module example ────────────────────────────────────────────

#[test]
fn test_multi_module_example() {
    let result = run_module_test(
        &[
            (
                "calc.silt",
                r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
fn internal_helper(x) = x * 2
            "#,
            ),
            (
                "utils.silt",
                r#"
pub fn double(x) = x * 2
pub fn triple(x) = x * 3
            "#,
            ),
        ],
        r#"
import calc
import utils.{ double }

fn main() {
  let x = calc.add(3, 4)
  let y = calc.square(x)
  double(y)
}
        "#,
    );
    // x = 7, y = 49, double(49) = 98
    assert_eq!(result, Value::Int(98));
}

// ── Regression tests for pipe/module bugs ────────────────────────────

#[test]
fn test_pipe_inside_record_in_module() {
    // Regression: pipe inside a list literal inside a record literal in a module
    // used to create a ghost stack slot (hidden __pipe_val__ local) that shifted
    // all subsequent record field values.
    let result = run_module_test(
        &[(
            "shapes.silt",
            r#"
pub type Point { x: Int, y: Int }
pub fn make(n) { Point { x: n, y: n } }
pub fn double_x(p) { p.{ x: p.x * 2 } }

pub type Box { name: String, label: String, points: List(Point) }

pub fn create(name, label) {
    Box { name: name, label: label, points: [make(1) |> double_x] }
}
            "#,
        )],
        r#"
import shapes

fn main() {
    let b = shapes.create("mybox", "mylabel")
    -- Before the fix: name="mylabel", label=Point{...} (shifted!)
    -- After the fix:  name="mybox",   label="mylabel"
    b.name
}
        "#,
    );
    assert_eq!(result, Value::String("mybox".into()));
}

#[test]
fn test_intra_module_private_fn_calls() {
    // Regression: private functions in modules were registered under mangled
    // names (__module__fn) but intra-module calls used bare names, causing
    // "undefined global" at runtime.
    let result = run_module_test(
        &[(
            "helpers.silt",
            r#"
fn internal_double(x) { x * 2 }
pub fn quadruple(x) { internal_double(internal_double(x)) }
            "#,
        )],
        r#"
import helpers
fn main() { helpers.quadruple(5) }
        "#,
    );
    assert_eq!(result, Value::Int(20));
}

#[test]
fn test_transitive_module_scope() {
    // Regression: when module A imports module B which imports module C,
    // compiling C cleared A's module scope (not saved/restored), breaking
    // intra-module calls in A.
    let result = run_module_test(
        &[
            (
                "base.silt",
                r#"
pub fn add(a, b) { a + b }
            "#,
            ),
            (
                "mid.silt",
                r#"
import base
fn helper(x) { base.add(x, 10) }
pub fn process(x) { helper(x) }
            "#,
            ),
        ],
        r#"
import mid
fn main() { mid.process(5) }
        "#,
    );
    assert_eq!(result, Value::Int(15));
}

// ── Private module function visibility error ───────────────────────

/// Calling a private function across a module boundary used to surface
/// as a VM-level "undefined global: mymod.helper" at runtime, which is
/// indistinguishable from a typo. The compiler now detects the
/// "exists but not `pub`" case and raises a compile-time visibility
/// error that names the function, the module, the source file, and the
/// exact syntactic fix.
#[test]
fn test_private_module_function_reference_emits_visibility_error() {
    let err = run_module_test_err(
        &[(
            "mymod.silt",
            r#"
fn helper() = 1
pub fn x() = helper()
            "#,
        )],
        r#"
import mymod

fn main() {
  mymod.helper()
}
        "#,
    );
    assert!(
        err.contains("helper"),
        "error should name the private function, got: {err}"
    );
    assert!(
        err.contains("mymod"),
        "error should name the module, got: {err}"
    );
    assert!(
        err.contains("pub"),
        "error should suggest `pub` as the fix, got: {err}"
    );
    assert!(
        !err.contains("undefined global"),
        "new visibility path should fire instead of the generic runtime \
         error, got: {err}"
    );
}

/// Control: calling a name that is NOT in the imported module at all
/// (neither public nor private) should still fall through to the
/// existing "undefined" error path, not the new visibility-specific
/// message.
#[test]
fn test_truly_unknown_module_function_still_emits_undefined_error() {
    let err = run_module_test_err(
        &[(
            "mymod.silt",
            r#"
pub fn x() = 1
            "#,
        )],
        r#"
import mymod

fn main() {
  mymod.genuinely_missing()
}
        "#,
    );
    // `mymod.genuinely_missing` is not a known private fn, so the visibility
    // check in src/compiler/mod.rs passes through; the emitted GetGlobal for
    // `mymod.genuinely_missing` fails at runtime with the exact phrase from
    // src/vm/execute.rs:1126.
    assert!(
        err.contains("undefined global: mymod.genuinely_missing"),
        "unknown module-qualified names must surface the VM's undefined-global error, got: {err}"
    );
    assert!(
        !err.contains("but is not `pub`"),
        "visibility error must not fire for a name that doesn't exist at all, got: {err}"
    );
}

// ── G3 (round 15): module parse errors must include a source snippet ──
//
// Before the fix, `format_module_source_error` in src/compiler/mod.rs
// flattened the inner (module-file) parse error into a single-line
// message "module 'bad': parse error at bad.silt:3:1 — ..." and the
// outer renderer's caret landed at the `import bad` line in main.silt.
// Users had no way to see where the actual parse error was inside the
// imported module. The fix reproduces the offending source line from
// the module file plus a caret marker inline in the error message.
//
// Mutation reasoning: reverting the `format_module_source_error` body
// back to the flat one-line format would make this test fail because
// (a) the `-->` marker pointing at the module file wouldn't appear in
// the message, and (b) the actual line of module source would not be
// rendered.
#[test]
fn test_module_parse_error_renders_with_module_source_snippet() {
    let dir = tempdir();
    // A deliberately broken module: unclosed `(` in a function
    // declaration. The parse error will surface inside bad.silt at
    // the `}` on line 4, not at the outer `import bad` line.
    let bad_src = "pub fn hello(x,\n  y,\n  z\n}\n";
    fs::write(dir.join("bad.silt"), bad_src).expect("failed to write bad.silt");

    let main_src = r#"
import bad

fn main() {
  bad.hello(1, 2, 3)
}
"#;
    let tokens = Lexer::new(main_src).tokenize().expect("main lex error");
    let mut program = Parser::new(tokens)
        .parse_program()
        .expect("main parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    let err_msg = match compiler.compile_program(&program) {
        Ok(_) => panic!("expected compile error from broken module"),
        Err(e) => e.message,
    };

    // The flattened summary header must still name the module by file
    // path and describe the error kind so a caret-free terminal still
    // gets the original information.
    assert!(
        err_msg.contains("bad.silt"),
        "error must name the broken module file, got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("parse error"),
        "error must describe the error kind as parse error, got:\n{err_msg}"
    );

    // The inline snippet must include an arrow line pointing at the
    // module file (not the main file), so the user can distinguish the
    // inner error location from the outer import site.
    assert!(
        err_msg.contains("-->") && err_msg.contains("bad.silt:"),
        "error message must include a `--> bad.silt:LINE:COL` location, got:\n{err_msg}"
    );

    // The actual offending line from the module source must be
    // reproduced. The parse error lands at bad.silt line 4 (the lone
    // `}`) because the parser only notices the mismatch when it hits
    // the next token after the unclosed `(`. Pin to the EXACT
    // formatted gutter+source line so a regression that drops the
    // snippet body (but leaves the `-->` locator intact) is caught
    // immediately. No single-character fallbacks — those match too
    // loosely and were the L5 audit finding.
    assert!(
        err_msg.contains(" 4 | }"),
        "error must quote the exact offending line from bad.silt \
         (\" 4 | }}\"), got:\n{err_msg}"
    );

    // The caret glyph must appear inside the message body to mark the
    // offending column. This is what differentiates the fix from the
    // previous flat-string rendering.
    assert!(
        err_msg.contains("^"),
        "error must include a caret marker pointing at the parse failure, \
         got:\n{err_msg}"
    );
}

/// Companion test: a *lex* error inside an imported module must also
/// render a module-source snippet. format_module_source_error is the
/// single code path for both the `lex error` and `parse error` kinds,
/// so this locks the shared helper against a regression that only
/// touches one of the two callers.
#[test]
fn test_module_lex_error_renders_with_module_source_snippet() {
    let dir = tempdir();
    // An illegal character `@` that the lexer rejects — not a parser
    // failure. Must still produce a snippet with caret.
    let bad_src = "pub fn hi() = 1\n@@@\npub fn bye() = 2\n";
    fs::write(dir.join("badlex.silt"), bad_src).expect("failed to write badlex.silt");

    let main_src = r#"
import badlex

fn main() {
  badlex.hi()
}
"#;
    let tokens = Lexer::new(main_src).tokenize().expect("main lex error");
    let mut program = Parser::new(tokens)
        .parse_program()
        .expect("main parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    let err_msg = match compiler.compile_program(&program) {
        Ok(_) => panic!("expected compile error from broken module"),
        Err(e) => e.message,
    };

    assert!(
        err_msg.contains("badlex.silt"),
        "error must name the broken module file, got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("lex error"),
        "error must describe the error kind as lex error, got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("-->") && err_msg.contains("badlex.silt:"),
        "error must include a `--> badlex.silt:LINE:COL` location, got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("^"),
        "error must include a caret marker, got:\n{err_msg}"
    );
}

// ── B6: module parse-error snippet must not render twice ───────────
//
// When a parse error inside an imported module is rendered via the
// full `SourceError::Display` path (as main.rs/repl.rs do), the
// inner module snippet must appear EXACTLY ONCE, not twice. The
// audit finding (B6) was that round 15 embedded a multi-line
// `--> ... | ^` snippet into `CompileError.message`; `SourceError::
// Display` then echoed that entire blob both in the header
// (`error[compile]: {msg}`) AND on the caret line (`^ {msg}`),
// duplicating the inner snippet. The fix (errors.rs) truncates
// `msg` at the first newline when rendering the caret line. This
// test counts occurrences of the unique inner source line and
// pins to exactly 1.
#[test]
fn test_module_parse_error_inner_snippet_rendered_once() {
    use silt::errors::SourceError;

    let dir = tempdir();
    // Inner module with a parse error mid-line: unclosed param list
    // followed by `{`. The parse error lands at 1:16 — the open brace.
    let inner_src = "pub fn broken( {\n";
    fs::write(dir.join("m_inner.silt"), inner_src).expect("failed to write m_inner.silt");

    // Top-level module that imports the broken one. This ensures the
    // import chain is nontrivial (m_top -> m_inner) and the outer
    // caret lands in m_top.silt at its own `import m_inner`.
    let top_src = "import m_inner\npub fn top() = 1\n";
    fs::write(dir.join("m_top.silt"), top_src).expect("failed to write m_top.silt");

    let main_src = "import m_top\n\nfn main() {\n  m_top.top()\n}\n";
    let tokens = Lexer::new(main_src).tokenize().expect("main lex error");
    let mut program = Parser::new(tokens)
        .parse_program()
        .expect("main parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    let compile_err = match compiler.compile_program(&program) {
        Ok(_) => panic!("expected compile error"),
        Err(e) => e,
    };

    // Render through the same path main.rs uses so we exercise the
    // full `SourceError::Display` output — this is where the double
    // rendering bug surfaced.
    let source_err = SourceError::from_compile_error(&compile_err, main_src, "main.silt");
    let rendered = format!("{source_err}");

    // The inner file's source line body must appear EXACTLY once.
    // The B6 bug manifested as the embedded snippet block (gutter +
    // source line + caret line) appearing twice, once in the header
    // and once after the outer caret line. `SourceError::Display`
    // now truncates `msg` at the first newline for the caret-line
    // echo, so the multi-line embedded snippet only appears once.
    let snippet_occurrences = rendered.matches("pub fn broken( {").count();
    assert_eq!(
        snippet_occurrences, 1,
        "inner module source line must appear exactly once (was {snippet_occurrences}); \
         full rendered error:\n{rendered}"
    );

    // And the formatted gutter `1 | pub fn broken( {` — the actual
    // snippet body — must also appear exactly once. Pinning the
    // formatted gutter catches a regression where the source text
    // is printed bare (unformatted) a second time.
    let gutter_occurrences = rendered.matches(" 1 | pub fn broken( {").count();
    assert_eq!(
        gutter_occurrences, 1,
        "formatted gutter `1 | pub fn broken( {{` must appear exactly \
         once (was {gutter_occurrences}); full rendered error:\n{rendered}"
    );
}

// ── G1: module parse error with EOF-past-end span must keep snippet ──
//
// A truncated inner module file (e.g. `pub fn broken(\n` with an
// unexpected EOF) parses with a span pointing at line 2, column 1 —
// one line past the end of the file. Before the fix,
// `format_module_source_error` silently dropped the snippet because
// `source.lines().nth(span.line - 1)` returned `None`. The fix
// clamps the span back onto the last real line. This test locks
// the rendered error message to include both the `pub fn broken(`
// line and a caret marker.
#[test]
fn test_module_parse_error_eof_renders_snippet() {
    let dir = tempdir();
    // Deliberately truncated: open-paren on line 1, then EOF.
    let inner_src = "pub fn broken(\n";
    fs::write(dir.join("inner.silt"), inner_src).expect("failed to write inner.silt");

    let main_src = "import inner\n\nfn main() {\n  inner.broken()\n}\n";
    let tokens = Lexer::new(main_src).tokenize().expect("main lex error");
    let mut program = Parser::new(tokens)
        .parse_program()
        .expect("main parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    let err_msg = match compiler.compile_program(&program) {
        Ok(_) => panic!("expected compile error from truncated module"),
        Err(e) => e.message,
    };

    // The snippet containing the actual truncated line must be in
    // the error message, not just the header. Exact pin — no OR.
    assert!(
        err_msg.contains("pub fn broken("),
        "error must quote the truncated line from inner.silt, got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("^"),
        "error must include a caret marker pointing at the EOF, got:\n{err_msg}"
    );
    // The `-->` locator should point at inner.silt on a real line
    // (1, not 2 — the clamp moves it back). This locks the clamp.
    assert!(
        err_msg.contains("--> ") && err_msg.contains("inner.silt:1:"),
        "error must include a `--> inner.silt:1:COL` locator clamped onto the \
         last real line, got:\n{err_msg}"
    );
}

// ── G4: circular-import error must render the full chain ───────────
//
// A 3-cycle `c_a -> c_b -> c_c -> c_a` must produce an error message
// that includes the exact arrow chain as a substring, not just a
// bare "module 'c_a' imports itself" line. This lets the user see
// the path through which the cycle was reached.
#[test]
fn test_circular_import_error_includes_full_chain() {
    let err = run_module_test_err(
        &[
            (
                "c_a.silt",
                r#"
import c_b
pub fn fa() = 1
                "#,
            ),
            (
                "c_b.silt",
                r#"
import c_c
pub fn fb() = 2
                "#,
            ),
            (
                "c_c.silt",
                r#"
import c_a
pub fn fc() = 3
                "#,
            ),
        ],
        r#"
import c_a

fn main() {
  c_a.fa()
}
        "#,
    );
    // Exact arrow-chain substring — not an OR chain.
    assert!(
        err.contains("c_a -> c_b -> c_c -> c_a"),
        "circular-import error must render the full arrow chain \
         `c_a -> c_b -> c_c -> c_a`, got: {err}"
    );
}
