//! Cross-package import resolution tests for the v0.7 package manager.
//!
//! These tests exercise the path-dependency feature added in PR 3:
//! a package may declare `dep = { path = "../dep" }` in its `silt.toml`
//! and then `import dep` in its sources. The import resolves against
//! the dep's `src/lib.silt` and only `pub` items are reachable.
//!
//! The compiler-side API under test is `Compiler::with_package_roots`,
//! which receives the resolved set of packages (local + transitive
//! deps) and the symbol naming the local package. PR 2 (CLI work)
//! will plumb manifest discovery into this API; here we wire it up
//! manually so the compiler change can ship and be verified
//! independently.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::intern::{self, Symbol};
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;

// ── Test scaffolding ────────────────────────────────────────────────

/// Description of a single package in a multi-package test setup.
///
/// Each `Pkg` translates to:
/// ```text
/// <tmp>/<name>/silt.toml      # not actually written; deps listed below
/// <tmp>/<name>/src/<file>     # one entry per `files`
/// ```
/// The compiler is wired up directly with `with_package_roots`, so the
/// `silt.toml` content only matters for tests that exercise the
/// manifest layer. PR 2 will write a real toml and parse it; here we
/// just need the on-disk source layout.
struct Pkg<'a> {
    name: &'a str,
    files: &'a [(&'a str, &'a str)],
}

/// Set up a multi-package workspace under a fresh temp directory and
/// compile + run `<main_pkg>/src/main.silt`. Returns whatever the
/// program's `main()` returned.
///
/// The setup writes only source files (no `silt.toml`) because the
/// compiler API under test takes pre-resolved package roots; the CLI
/// is responsible for manifest discovery in PR 2.
fn run_pkg_test(packages: &[Pkg], main_pkg: &str) -> Value {
    match try_run_pkg_test(packages, main_pkg) {
        Ok(v) => v,
        Err(e) => panic!("expected success, got error: {e}"),
    }
}

/// Like [`run_pkg_test`] but returns the (compile or runtime) error
/// message instead of panicking.
fn run_pkg_test_err(packages: &[Pkg], main_pkg: &str) -> String {
    match try_run_pkg_test(packages, main_pkg) {
        Ok(v) => panic!("expected error, got value: {v:?}"),
        Err(e) => e,
    }
}

fn try_run_pkg_test(packages: &[Pkg], main_pkg: &str) -> Result<Value, String> {
    let dir = tempdir();
    let mut roots: HashMap<Symbol, PathBuf> = HashMap::new();

    for pkg in packages {
        let pkg_root = dir.join(pkg.name);
        let src_dir = pkg_root.join("src");
        fs::create_dir_all(&src_dir).expect("failed to create src/");
        for (file, content) in pkg.files {
            fs::write(src_dir.join(file), content).expect("failed to write source file");
        }
        roots.insert(intern::intern(pkg.name), src_dir);
    }

    let local_sym = intern::intern(main_pkg);
    let main_path = dir.join(main_pkg).join("src").join("main.silt");
    let main_source = fs::read_to_string(&main_path)
        .map_err(|e| format!("missing main entry {}: {e}", main_path.display()))?;

    let tokens = Lexer::new(&main_source)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e.message))?;
    let mut program = Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {}", e.message))?;
    let _ = silt::typechecker::check(&mut program);

    let mut compiler = Compiler::with_package_roots(local_sym, roots);
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return Err(e.message),
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).map_err(|e| e.to_string())
}

fn tempdir() -> PathBuf {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "silt_pkg_test_{}_{}",
        std::process::id(),
        nanos as u64
    ));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

// ── Tests ────────────────────────────────────────────────────────────

/// Smoke test: an `app` that imports a local path dep `calc` and
/// invokes one of its `pub fn`s. Locks the basic happy path: the dep's
/// `lib.silt` is loaded, its public function is reachable as
/// `calc.add(...)`, and the returned value flows back out of `main`.
#[test]
fn test_single_path_dep() {
    let result = run_pkg_test(
        &[
            Pkg {
                name: "app",
                files: &[(
                    "main.silt",
                    r#"
import calc
fn main() = calc.add(2, 3)
"#,
                )],
            },
            Pkg {
                name: "calc",
                files: &[(
                    "lib.silt",
                    r#"
pub fn add(a, b) = a + b
"#,
                )],
            },
        ],
        "app",
    );
    assert_eq!(result, Value::Int(5));
}

/// Three-package transitive chain: `app` → `mid` → `leaf`. `app`
/// imports `mid` and calls `mid.foo()`; `mid` itself imports `leaf`
/// and delegates to `leaf.bar()`. Locks that the compiler doesn't
/// flatten everything into the local package — the dep `mid` must
/// itself receive the full `package_roots` map so its own `import
/// leaf` resolves correctly.
#[test]
fn test_transitive_path_dep() {
    let result = run_pkg_test(
        &[
            Pkg {
                name: "app",
                files: &[(
                    "main.silt",
                    r#"
import mid
fn main() = mid.foo()
"#,
                )],
            },
            Pkg {
                name: "mid",
                files: &[(
                    "lib.silt",
                    r#"
import leaf
pub fn foo() = leaf.bar() + 1
"#,
                )],
            },
            Pkg {
                name: "leaf",
                files: &[(
                    "lib.silt",
                    r#"
pub fn bar() = 41
"#,
                )],
            },
        ],
        "app",
    );
    assert_eq!(result, Value::Int(42));
}

/// Cross-package cycle (`pkg_a` ↔ `pkg_b`) must be detected and
/// reported with the full chain so the user can see *which packages*
/// participate. The renderer uses qualified names (`pkg_a::lib ->
/// pkg_b::lib -> pkg_a::lib`) for cross-package cycles to make the
/// boundary explicit.
#[test]
fn test_cross_package_cycle_detected() {
    let err = run_pkg_test_err(
        &[
            Pkg {
                name: "app",
                files: &[(
                    "main.silt",
                    r#"
import pkg_a
fn main() = pkg_a.entry()
"#,
                )],
            },
            Pkg {
                name: "pkg_a",
                files: &[(
                    "lib.silt",
                    r#"
import pkg_b
pub fn entry() = pkg_b.go()
pub fn helper() = 1
"#,
                )],
            },
            Pkg {
                name: "pkg_b",
                files: &[(
                    "lib.silt",
                    r#"
import pkg_a
pub fn go() = pkg_a.helper()
"#,
                )],
            },
        ],
        "app",
    );
    assert!(
        err.contains("circular import"),
        "expected cycle diagnostic, got: {err}"
    );
    assert!(
        err.contains("pkg_a::lib") && err.contains("pkg_b::lib"),
        "cycle message should name both packages with qualified keys, got: {err}"
    );
}

/// A `[dependencies]` entry whose `path` doesn't exist on disk should
/// surface a clear, actionable error at first import. We simulate this
/// by registering a `package_roots` entry pointing at a non-existent
/// directory — the same condition the CLI will produce when the
/// manifest's path is wrong.
#[test]
fn test_missing_dep_path_is_error() {
    let dir = tempdir();
    let app_src = dir.join("app").join("src");
    fs::create_dir_all(&app_src).unwrap();
    fs::write(
        app_src.join("main.silt"),
        r#"
import gone
fn main() = gone.hello()
"#,
    )
    .unwrap();

    let mut roots = HashMap::new();
    roots.insert(intern::intern("app"), app_src);
    // Pretend the manifest declared `gone = { path = "../gone" }` but
    // the directory was deleted.
    roots.insert(
        intern::intern("gone"),
        dir.join("does_not_exist").join("src"),
    );

    let main_source = fs::read_to_string(dir.join("app/src/main.silt")).unwrap();
    let tokens = Lexer::new(&main_source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::with_package_roots(intern::intern("app"), roots);
    let err = compiler
        .compile_program(&program)
        .expect_err("expected err");

    // Either the lib-entry-point error (if the parent dir doesn't
    // exist `lib.silt` clearly doesn't either) OR a load-failure
    // error mentioning the missing path. Both are acceptable; the
    // user is told the dep is unreachable.
    let m = err.message;
    assert!(
        m.contains("gone") && (m.contains("library entry point") || m.contains("cannot load")),
        "expected error naming the missing dep `gone`, got: {m}"
    );
}

/// A dep package directory exists but has no `src/lib.silt`. This is
/// likely the most common bad-dep mistake (someone forgot to create
/// the entry point) so the message must be specific. We check for
/// `library entry point` to lock the wording.
#[test]
fn test_dep_without_lib_silt_is_error() {
    let dir = tempdir();
    let app_src = dir.join("app").join("src");
    let calc_src = dir.join("calc").join("src");
    fs::create_dir_all(&app_src).unwrap();
    fs::create_dir_all(&calc_src).unwrap();
    // Note: no lib.silt under calc/src, but the dir exists. A
    // helper.silt exists to prove the directory itself is fine.
    fs::write(calc_src.join("helper.silt"), "pub fn h() = 1").unwrap();
    fs::write(
        app_src.join("main.silt"),
        r#"
import calc
fn main() = calc.h()
"#,
    )
    .unwrap();

    let mut roots = HashMap::new();
    roots.insert(intern::intern("app"), app_src);
    roots.insert(intern::intern("calc"), calc_src);

    let main_source = fs::read_to_string(dir.join("app/src/main.silt")).unwrap();
    let tokens = Lexer::new(&main_source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::with_package_roots(intern::intern("app"), roots);
    let err = compiler
        .compile_program(&program)
        .expect_err("expected err");

    assert!(
        err.message.contains("library entry point"),
        "expected `library entry point` wording, got: {}",
        err.message
    );
    assert!(
        err.message.contains("calc"),
        "expected error to name the dep `calc`, got: {}",
        err.message
    );
}

/// Cross-package privacy: a non-`pub` function in a dep's lib.silt
/// must NOT be importable. The visibility check runs at compile time
/// (not just runtime), so the user gets a `not pub` diagnostic
/// pointing at the export site rather than a generic "undefined
/// global" at runtime.
#[test]
fn test_private_item_in_dep_not_importable() {
    let err = run_pkg_test_err(
        &[
            Pkg {
                name: "app",
                files: &[(
                    "main.silt",
                    r#"
import calc
fn main() = calc.secret()
"#,
                )],
            },
            Pkg {
                name: "calc",
                files: &[(
                    "lib.silt",
                    r#"
fn secret() = 42
pub fn public() = secret()
"#,
                )],
            },
        ],
        "app",
    );
    assert!(
        err.contains("secret") && err.contains("not `pub`"),
        "expected `secret ... not pub` diagnostic, got: {err}"
    );
}

/// Local sub-module imports inside a package keep working: `import
/// helpers` from `app/src/main.silt` resolves to `app/src/helpers.silt`
/// because no dep is named `helpers`. This is the regression check
/// that the new resolution logic doesn't break the single-package
/// case.
#[test]
fn test_local_imports_still_work() {
    let result = run_pkg_test(
        &[Pkg {
            name: "app",
            files: &[
                (
                    "main.silt",
                    r#"
import helpers
fn main() = helpers.shout(7)
"#,
                ),
                (
                    "helpers.silt",
                    r#"
pub fn shout(x) = x * 10
"#,
                ),
            ],
        }],
        "app",
    );
    assert_eq!(result, Value::Int(70));
}

/// Multi-segment cross-package imports (`import dep.internal`) are
/// not part of silt's import grammar — the parser only accepts a
/// single bare identifier after `import`. So a user can't even type
/// `import calc.internal` to attempt to reach into a dep's internals.
///
/// This test documents that absence by verifying the parser rejects
/// the construct cleanly (so PR 2/4 can rely on it). If multi-segment
/// imports are ever added, the rejection here will fail loudly and
/// cross-package access rules will need an explicit gate at that
/// point.
#[test]
fn test_cross_package_multi_segment_rejected() {
    // The parser treats `import calc.{ ... }` as the *items* form
    // (selective import). A truly multi-segment form like `import
    // calc.internal` (no braces) is not legal silt syntax. We assert
    // both shapes here so the contract is explicit.
    let bare_multi = "import calc.internal\n";
    let tokens = Lexer::new(bare_multi).tokenize().expect("lex");
    let result = Parser::new(tokens).parse_program();
    assert!(
        result.is_err(),
        "bare `import dep.module` must be a parse error; if multi-segment \
         imports are added later, cross-package access needs a new gate"
    );
}

// ── Sanity checks for the resolver itself ────────────────────────────

/// A tiny direct test that `with_package_roots` panics on a
/// programming error (local symbol not in the map). Locks the API
/// contract so the CLI in PR 2 can rely on it.
#[test]
#[should_panic(expected = "local_package symbol must appear in package_roots")]
fn test_with_package_roots_requires_local_in_map() {
    let mut roots: HashMap<Symbol, PathBuf> = HashMap::new();
    roots.insert(intern::intern("other"), Path::new("/tmp").to_path_buf());
    let _ = Compiler::with_package_roots(intern::intern("missing"), roots);
}
