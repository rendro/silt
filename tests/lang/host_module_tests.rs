//! Host modules (design decision D7): an embedder declares modules of
//! Rust functions to the session, each function by its signature. A
//! program imports one like any module; calls are typechecked against
//! the declared signatures and run the Rust closure.

use silt::diagnostic::{Code, Diagnostic};
use silt::session::HostModule;
use silt::session::testing::{check_with_host, run_with_host};
use silt::typeinfo::bv;
use silt::value::{IntoValue, Value};
use silt::vm::VmError;

fn mylib() -> HostModule {
    HostModule::new("mylib")
        .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
        .fn2("fn add(a: Int, b: Int) -> Int", |a: i64, b: i64| a + b)
        .fn0("fn answer() -> Int", || 42_i64)
        .fn1("fn shout(s: String) -> String", |s: String| {
            s.to_uppercase()
        })
}

fn run(source: &str, host: HostModule) -> Result<Value, String> {
    run_with_host(&[("main.silt", source)], vec![host])
}

fn errors(source: &str, host: HostModule) -> Vec<Diagnostic> {
    check_with_host(&[("main.silt", source)], vec![host])
        .into_iter()
        .filter(Diagnostic::is_error)
        .collect()
}

#[test]
fn program_importing_a_host_module_runs() {
    let source = "import mylib\nfn main() { mylib.add(mylib.double(20), mylib.answer() - 40) }";
    assert_eq!(run(source, mylib()), Ok(Value::Int(42)));
}

#[test]
fn host_functions_imported_by_name_and_through_an_alias() {
    let source = "import mylib.{ shout }\nimport mylib as m\nfn main() { \"{shout(\"hi\")} {m.double(4)}\" }";
    assert_eq!(run(source, mylib()), Ok(Value::String("HI 8".into())));
}

/// Two host modules with functions of one name: each function has a
/// global slot of its own, read from the entry and from a file module.
#[test]
fn two_host_modules_with_functions_of_one_name() {
    let twice = HostModule::new("twice").fn1("fn apply(x: Int) -> Int", |x: i64| x * 2);
    let thrice = HostModule::new("thrice").fn1("fn apply(x: Int) -> Int", |x: i64| x * 3);
    let files = [
        (
            "main.silt",
            "import twice\nimport helper\nfn main() { \"{twice.apply(1)} {helper.via_thrice(1)}\" }",
        ),
        (
            "helper.silt",
            "import thrice\npub fn via_thrice(x: Int) -> Int { thrice.apply(x) }",
        ),
    ];
    assert_eq!(
        run_with_host(&files, vec![twice, thrice]),
        Ok(Value::String("2 3".into()))
    );
}

#[test]
fn host_function_is_a_value() {
    let source =
        "import list\nimport mylib\nfn main() { let f = mylib.double\n [1, 2, 3] |> list.map(f) }";
    assert_eq!(
        run(source, mylib()),
        Ok(Value::list(vec![
            Value::Int(2),
            Value::Int(4),
            Value::Int(6)
        ]))
    );
}

#[test]
fn wrong_argument_type_is_a_type_error_at_check_time() {
    let errors = errors(
        "import mylib\nfn main() { mylib.double(\"hello\") }",
        mylib(),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::TypeMismatch, "{errors:?}");
}

#[test]
fn wrong_arity_is_a_type_error_at_check_time() {
    let errors = errors("import mylib\nfn main() { mylib.add(1) }", mylib());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("argument"), "{errors:?}");
}

#[test]
fn undeclared_host_function_is_unknown_at_check_time() {
    let errors = errors("import mylib\nfn main() { mylib.triple(3) }", mylib());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::NotExported, "{errors:?}");
}

#[test]
fn host_module_is_not_in_scope_without_an_import() {
    let errors = errors("fn main() { mylib.double(3) }", mylib());
    assert!(!errors.is_empty(), "a host module must be imported");
}

#[test]
fn generic_and_option_signatures() {
    let host = HostModule::new("h")
        .fn1("fn maybe_positive(x: Int) -> Option(Int)", |x: i64| {
            if x > 0 { Some(x) } else { None }
        })
        .fn1(
            "fn safe_reciprocal(x: Int) -> Result(Int, String)",
            |x: i64| -> Result<i64, String> {
                if x != 0 {
                    Ok(100 / x)
                } else {
                    Err("div by zero".to_string())
                }
            },
        )
        .fn1("fn range_up_to(n: Int) -> List(Int)", |n: i64| {
            (0..n).map(Value::Int).collect::<Vec<Value>>()
        })
        .function("fn first(xs: List(a)) -> Option(a)", |args: &[Value]| {
            let Value::List(xs) = &args[0] else {
                return Err(VmError::new("expected a list".into()));
            };
            xs.first()
                .map(|x| x.into_value())
                .into_value()
                .map_err(VmError::new)
        });
    let source = r#"
import h
fn main() {
  let a = match h.maybe_positive(7) {
    Some(n) -> n
    None -> 0
  }
  let b = match h.safe_reciprocal(0) {
    Ok(n) -> n
    Err(_) -> 1
  }
  let c = match h.first(h.range_up_to(4)) {
    Some(n) -> n
    None -> 100
  }
  let d = match h.first(["x"]) {
    Some(s) -> s
    None -> ""
  }
  (a, b, c, d)
}
"#;
    assert_eq!(
        run(source, host.clone()),
        Ok(Value::Tuple(vec![
            Value::Int(7),
            Value::Int(1),
            Value::Int(0),
            Value::String("x".into())
        ]))
    );
    // The generic signature is checked at each use.
    let errors = errors("import h\nfn main() { h.first(3) }", host);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn host_function_error_names_the_function() {
    let host = HostModule::new("h").function("fn fail(x: Int) -> Int", |_: &[Value]| {
        Err(VmError::new("no".into()))
    });
    let err = run("import h\nfn main() { h.fail(1) }", host).unwrap_err();
    assert!(err.contains("h.fail: no"), "got: {err}");
}

#[test]
fn host_function_panic_is_a_runtime_error() {
    let host = HostModule::new("h").fn1("fn boom(x: Int) -> Int", |_: i64| -> i64 {
        panic!("kaboom")
    });
    let err = run("import h\nfn main() { h.boom(1) }", host).unwrap_err();
    assert!(
        err.contains("host function 'h.boom' panicked: kaboom"),
        "got: {err}"
    );
}

#[test]
fn host_function_returning_nan_raises() {
    let host = HostModule::new("h").fn0("fn get_nan() -> Float", || f64::NAN);
    let err = run("import h\nfn main() { h.get_nan() }", host).unwrap_err();
    assert!(
        err.contains("h.get_nan: non-finite float result"),
        "got: {err}"
    );
}

#[test]
fn host_function_runs_in_a_spawned_task() {
    let source = r#"
import mylib
import task
fn main() {
  let t = task.spawn { -> mylib.double(21) }
  task.join(t)
}
"#;
    assert_eq!(run(source, mylib()), Ok(Value::Int(42)));
}

#[test]
fn bad_signatures_are_reported() {
    let cases = [
        ("fn f(x) -> Int", "must declare the type"),
        ("fn f(x: Int)", "must declare the type"),
        ("fn f(x: Int) -> Int { x }", "no body"),
        ("let x = 1", "no body"),
        (
            "fn f(x: Int) -> Int\nfn g() -> Int",
            "has 1 function but its signatures declare 2",
        ),
        ("fn f(x: Int) -> Nope", "Nope"),
        (
            "fn f(g: Fn(Int) -> Int) -> Int",
            "cannot take or return a function",
        ),
        (
            "fn f(gs: List(Fn(Int) -> Int)) -> Int",
            "cannot take or return a function",
        ),
        (
            "fn f() -> Fn(Int) -> Int",
            "cannot take or return a function",
        ),
    ];
    for (signature, expected) in cases {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Unit));
        // Reported whether or not the program imports the module.
        let errors = errors("fn main() { 1 }", host);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{signature}: {errors:?}"
        );
    }
}

#[test]
fn a_signature_that_does_not_parse_is_reported_once() {
    let host = HostModule::new("h").function("fn (", |_: &[Value]| Ok(Value::Unit));
    let errors = errors("fn main() { 1 }", host);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(!errors[0].message.contains("no body"), "{errors:?}");
}

#[test]
fn typed_function_arity_must_match_the_signature() {
    let host = HostModule::new("h").fn1("fn arity() -> Int", |x: i64| x);
    let errors = errors("import h\nfn main() { h.arity() }", host);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::HostSignature, "{errors:?}");
    assert!(
        errors[0].message.contains(
            "is given a Rust function of 1 argument, but its signature declares 0 parameters"
        ),
        "{errors:?}"
    );
}

#[test]
fn a_result_of_the_wrong_type_names_the_host_function() {
    let host = HostModule::new("mylib")
        .function("fn count() -> Int", |_: &[Value]| {
            Ok(Value::String("three".into()))
        })
        .function("fn evens() -> List(Int)", |_: &[Value]| {
            Ok(Value::list(vec![
                Value::Int(2),
                Value::String("four".into()),
            ]))
        })
        .function("fn maybe() -> Option(String)", |_: &[Value]| {
            Ok(Value::variant(bv::SOME, vec![Value::Int(1)]))
        })
        .function("fn pair() -> (Int, Bool)", |_: &[Value]| {
            Ok(Value::Tuple(vec![Value::Int(1), Value::Bool(true)]))
        });
    let err = run(
        "import mylib\nfn main() { mylib.count() + 1 }",
        host.clone(),
    )
    .unwrap_err();
    assert!(
        err.contains("mylib.count: its signature returns Int, but it returned String"),
        "got: {err}"
    );
    let err = run("import mylib\nfn main() { mylib.evens() }", host.clone()).unwrap_err();
    assert!(
        err.contains("mylib.evens: its signature returns List(Int), but it returned List"),
        "got: {err}"
    );
    let err = run("import mylib\nfn main() { mylib.maybe() }", host.clone()).unwrap_err();
    assert!(
        err.contains("mylib.maybe: its signature returns Option(String)"),
        "got: {err}"
    );
    // A result of the declared type passes.
    assert_eq!(
        run("import mylib\nfn main() { mylib.pair() }", host),
        Ok(Value::Tuple(vec![Value::Int(1), Value::Bool(true)]))
    );
}

#[test]
fn compile_without_analyze_returns_the_analysis_errors() {
    use silt::session::{Config, Entry, LockPolicy, ProjectSetup, Session};
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![mylib()],
    });
    let file = session.set_overlay(
        std::path::Path::new("main.silt"),
        "import mylib\nfn main() { mylib.double(\"x\") }".to_string(),
    );
    let errors = match session.compile(file, Entry::Main) {
        Ok(_) => panic!("the program has a type error"),
        Err(errors) => errors,
    };
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::TypeMismatch, "{errors:?}");
}

#[test]
fn host_module_name_must_be_an_identifier() {
    for name in ["my-lib", "", "fn", "a b"] {
        let host = HostModule::new(name).fn0("fn answer() -> Int", || 42_i64);
        let errors = errors("fn main() { 1 }", host);
        assert!(
            errors
                .iter()
                .any(|e| e.code == Code::HostModuleCollision
                    && e.message.contains("not an identifier")),
            "{name:?}: {errors:?}"
        );
    }
}

#[test]
fn host_module_named_like_a_dependency_key_is_rejected_at_the_import() {
    use silt::session::{Config, LockPolicy, ProjectSetup, Session};
    let dir = std::env::temp_dir().join(format!("silt_host_dep_collision_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dep = dir.join("dep");
    std::fs::create_dir_all(dep.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmylib = { path = \"dep\" }\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("silt.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("src").join("lib.silt"),
        "pub fn double(x: Int) -> Int { x }\n",
    )
    .unwrap();
    let main = dir.join("src").join("main.silt");
    std::fs::write(&main, "import mylib\nfn main() { mylib.double(1) }\n").unwrap();
    let mut session = Session::new(Config {
        project: ProjectSetup::Discover(dir.join("src")),
        lock: LockPolicy::Update,
        host: vec![mylib()],
    });
    let file = session.open(&main).unwrap();
    let errors: Vec<Diagnostic> = session
        .analyze(file)
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .cloned()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::HostModuleCollision, "{errors:?}");
    assert!(
        errors[0]
            .help
            .iter()
            .any(|h| h.contains("rename the dependency key `mylib` in silt.toml")),
        "{errors:?}"
    );
}

#[test]
fn host_module_named_like_a_builtin_module_is_rejected() {
    let host = HostModule::new("list").fn0("fn answer() -> Int", || 42_i64);
    let errors = errors("fn main() { 1 }", host);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::HostModuleCollision, "{errors:?}");
}

#[test]
fn host_module_named_like_a_module_file_is_rejected_at_the_import() {
    let errors: Vec<Diagnostic> = check_with_host(
        &[
            ("main.silt", "import mylib\nfn main() { mylib.double(1) }"),
            ("mylib.silt", "pub fn double(x: Int) -> Int { x }"),
        ],
        vec![mylib()],
    )
    .into_iter()
    .filter(Diagnostic::is_error)
    .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::HostModuleCollision, "{errors:?}");
}

/// The Quick Start of docs/ffi.md, as written there.
#[test]
fn docs_ffi_quick_start() {
    use silt::session::{Config, Entry, LockPolicy, ProjectSetup, Session};
    use silt::{Buffer, HostIo, Value, Vm};
    use std::path::Path;

    // 1. Declare the host module: each function by its silt signature.
    let mylib = HostModule::new("mylib").fn1("fn double(x: Int) -> Int", |x: i64| x * 2);

    // 2. Open the program in a session that knows the module.
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![mylib],
    });
    let source = "import mylib\nfn main() { mylib.double(21) }";
    let file = session.set_overlay(Path::new("main.silt"), source.to_string());

    // 3. Check it. Every static error is in the analysis.
    let analysis = session.analyze(file);
    for d in &analysis.diagnostics {
        eprintln!("{}", d.message);
    }
    assert!(!analysis.has_errors());

    // 4. Compile it and run it. What the program prints is collected in `out`.
    let program = session.compile(file, Entry::Main).expect("compiles");
    let out = Buffer::new();
    let result = Vm::new(HostIo::buffer(&out)).run_program(&program).unwrap();
    assert_eq!(result, Value::Int(42));
}
