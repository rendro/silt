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
    let source = "import mylib\nfn main() { host.give(mylib.add(mylib.double(20), mylib.answer() - 40)) }\nimport host";
    assert_eq!(run(source, mylib()), Ok(Value::Int(42)));
}

#[test]
fn host_functions_imported_by_name_and_through_an_alias() {
    let source = "import mylib.{ shout }\nimport mylib as m\nfn main() { host.give(\"{shout(\"hi\")} {m.double(4)}\") }\nimport host";
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
            "import twice\nimport helper\nfn main() { host.give(\"{twice.apply(1)} {helper.via_thrice(1)}\") }\nimport host",
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
    let source = "import list\nimport mylib\nfn main() { let f = mylib.double\n host.give([1, 2, 3] |> list.map(f)) }\nimport host";
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
            xs.first().into_value().map_err(VmError::new)
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
  host.give((a, b, c, d))
}
import host
"#;
    assert_eq!(
        run(source, host.clone()),
        Ok(Value::tuple(vec![
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
    let err = run(
        "import h\nfn main() { host.give(h.fail(1)) }\nimport host",
        host,
    )
    .unwrap_err();
    assert!(err.contains("h.fail: no"), "got: {err}");
}

#[test]
fn host_function_panic_is_a_runtime_error() {
    let host = HostModule::new("h").fn1("fn boom(x: Int) -> Int", |_: i64| -> i64 {
        panic!("kaboom")
    });
    let err = run(
        "import h\nfn main() { host.give(h.boom(1)) }\nimport host",
        host,
    )
    .unwrap_err();
    assert!(
        err.contains("host function 'h.boom' panicked: kaboom"),
        "got: {err}"
    );
}

#[test]
fn host_function_returning_nan_raises() {
    let host = HostModule::new("h").fn0("fn get_nan() -> Float", || f64::NAN);
    let err = run(
        "import h\nfn main() { host.give(h.get_nan()) }\nimport host",
        host,
    )
    .unwrap_err();
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
  host.give(task.join(t))
}
import host
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
        ("fn f(x: Int) -> _", "unknown type '_'"),
        ("fn f(x: Int) -> Option(_a)", "unknown type '_a'"),
        // (A record without a name is no type.)
        ("fn f(p: {name: String}) -> Int", "expected a type, found {"),
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
        (
            "fn any() -> a",
            "type variable 'a' in the return type of the host function `any` is not introduced \
             by any parameter: name it in a parameter's type, or return a concrete type",
        ),
        ("fn f() -> List(a)", "type variable 'a' in the return type"),
        (
            "fn f(x: a) -> (a, b)",
            "type variable 'b' in the return type",
        ),
        (
            "fn f() -> Result(a, String) where a: Display",
            "type variable 'a' in the return type",
        ),
    ];
    for (signature, expected) in cases {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Unit));
        // Reported whether or not the program imports the module.
        let errors = errors("fn main() { let _ = 1 }", host);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{signature}: {errors:?}"
        );
        // `_` is no type variable, in the return type as in a parameter.
        if signature.contains("_") {
            let variable = errors.iter().any(|e| e.message.contains("variable '_"));
            assert!(!variable, "{signature}: {errors:?}");
        }
    }
}

/// A host function's result is not checked to be of the type a call
/// asks of a type variable: one that only its return type has is an
/// error, so that no program is checked against it.
#[test]
fn a_return_only_type_variable_is_refused() {
    let any = HostModule::new("h").function("fn any() -> a", |_: &[Value]| Ok(Value::Int(5)));
    let errors = errors(
        "import h\nfn main() { let s: String = h.any()\n let _ = s }",
        any,
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::HostSignature, "{errors:?}");
    assert!(
        errors[0]
            .message
            .contains("type variable 'a' in the return type of the host function `any`"),
        "{errors:?}"
    );
}

/// A variable that a parameter introduces stays a host function's to
/// return, by its type or as a `type a` parameter.
#[test]
fn type_variables_a_parameter_introduces_are_accepted() {
    for signature in [
        "fn give(x: a) -> ()",
        "fn first(xs: List(a)) -> Option(a)",
        "fn same(x: a) -> a",
        "fn pair(x: a, y: b) -> (b, a)",
        "fn pick(s: String, type a) -> Option(a)",
    ] {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Unit));
        let errors = errors("fn main() { let _ = 1 }", host);
        assert!(errors.is_empty(), "{signature}: {errors:?}");
    }
}

/// A type taken as `TypeOf(..)` around anything but a bare variable
/// shows no type for its variables that the result could be checked
/// against (`TypeOf(List(a))` gives `List(String)`, and `a` is not
/// read out of it): the signature is refused when the module is
/// registered, and `type a` / `t: TypeOf(a)` stay accepted.
#[test]
fn a_type_of_around_more_than_a_variable_is_refused() {
    for (signature, written) in [
        ("fn oft(v: b, t: TypeOf(List(a))) -> a", "TypeOf(List(a))"),
        ("fn opt(t: Option(TypeOf(List(a)))) -> a", "TypeOf(List(a))"),
        (
            "fn pair(t: (Int, TypeOf(Map(k, v)))) -> Int",
            "TypeOf(Map(k, v))",
        ),
    ] {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Int(1)));
        let errors = errors("fn main() { let _ = 1 }", host);
        assert_eq!(errors.len(), 1, "{signature}: {errors:?}");
        assert_eq!(
            errors[0].code,
            Code::HostSignature,
            "{signature}: {errors:?}"
        );
        assert!(
            errors[0]
                .message
                .contains(&format!("takes a type as `{written}`")),
            "{signature}: {errors:?}"
        );
        let help = errors[0].help.join("\n");
        assert!(help.contains("take `type a`"), "{signature}: {errors:?}");
    }
    for signature in [
        "fn tcast(v: b, t: TypeOf(a)) -> a",
        "fn otcast(v: b, t: Option(TypeOf(a))) -> a",
        "fn pick(v: b, type a) -> a",
    ] {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Int(1)));
        let errors = errors("fn main() { let _ = 1 }", host);
        assert!(errors.is_empty(), "{signature}: {errors:?}");
    }
}

#[test]
fn a_signature_that_does_not_parse_is_reported_once() {
    let host = HostModule::new("h").function("fn (", |_: &[Value]| Ok(Value::Unit));
    let errors = errors("fn main() { let _ = 1 }", host);
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
            Ok(Value::variant(bv::SOME, [Value::Int(1)]))
        })
        .function("fn pair() -> (Int, Bool)", |_: &[Value]| {
            Ok(Value::tuple(vec![Value::Int(1), Value::Bool(true)]))
        });
    let err = run(
        "import mylib\nfn main() { host.give(mylib.count() + 1) }\nimport host",
        host.clone(),
    )
    .unwrap_err();
    assert!(
        err.contains("mylib.count: its signature returns Int, but it returned String \"three\""),
        "got: {err}"
    );
    let err = run(
        "import mylib\nfn main() { host.give(mylib.evens()) }\nimport host",
        host.clone(),
    )
    .unwrap_err();
    assert!(
        err.contains("mylib.evens: its signature returns List(Int), but it returned List"),
        "got: {err}"
    );
    let err = run(
        "import mylib\nfn main() { host.give(mylib.maybe()) }\nimport host",
        host.clone(),
    )
    .unwrap_err();
    assert!(
        err.contains("mylib.maybe: its signature returns Option(String)"),
        "got: {err}"
    );
    // A result of the declared type passes.
    assert_eq!(
        run(
            "import mylib\nfn main() { host.give(mylib.pair()) }\nimport host",
            host
        ),
        Ok(Value::tuple(vec![Value::Int(1), Value::Bool(true)]))
    );
}

/// No value is of `Never`: a host function that says it returns it and
/// returns a value is an error, and failing is what it may do.
#[test]
fn a_host_function_returning_never_can_only_fail() {
    let host = HostModule::new("h")
        .function("fn f() -> Never", |_: &[Value]| {
            Ok(Value::String("x".into()))
        })
        .function("fn fail(msg: String) -> Never", |args: &[Value]| {
            Err(VmError::new(format!("failed: {}", args[0])))
        });
    let program = "import h\nfn main() { let b: Bool = h.f()\n match b { true -> host.give(1)\n false -> host.give(2) } }\nimport host";
    let err = run(program, host.clone()).unwrap_err();
    assert!(
        err.contains("h.f: its signature returns Never, but it returned String"),
        "got: {err}"
    );
    let err = run(
        "import h\nfn main() { let s: String = h.fail(\"no\")\n host.give(s) }\nimport host",
        host,
    )
    .unwrap_err();
    assert!(err.contains("h.fail: failed: no"), "got: {err}");
}

/// The error of a program that gives what `call` returns to the host.
fn returned_error(host: HostModule, call: &str) -> String {
    let source =
        format!("import h\nimport channel\nfn main() {{ host.give({call}) }}\nimport host");
    run(&source, host).expect_err(call)
}

/// A host function whose result is of a type variable gives back a value
/// of the type its argument has at the call; another is an error.
#[test]
fn a_result_of_a_type_variable_is_of_its_argument_s_type() {
    let host = HostModule::new("h").function("fn same(x: a) -> a", |_: &[Value]| Ok(Value::Int(5)));
    let err = returned_error(host, r#"h.same("x")"#);
    assert!(
        err.contains(
            "h.same: its signature returns a, here String (the type of its argument `x`), \
             but it returned Int 5"
        ),
        "got: {err}"
    );
}

/// An element of a list argument shows the variable, for a result that
/// holds it.
#[test]
fn an_element_of_a_list_argument_shows_the_variable() {
    let host = HostModule::new("h")
        .function("fn first(xs: List(a)) -> Option(a)", |_: &[Value]| {
            Ok(Value::variant(bv::SOME, [Value::String("s".into())]))
        });
    let err = returned_error(host, "h.first([1, 2])");
    assert!(
        err.contains(
            "h.first: its signature returns Option(a), where a is Int (from its argument \
             `xs`), but it returned Variant Some(\"s\")"
        ),
        "got: {err}"
    );
}

/// An empty list shows no type: what the function returns of it is not
/// checked (no value could tell what the call gives the variable).
#[test]
fn a_variable_no_argument_shows_stays_unchecked() {
    let host = HostModule::new("h")
        .function("fn first(xs: List(a)) -> Option(a)", |_: &[Value]| {
            Ok(Value::variant(bv::SOME, [Value::String("s".into())]))
        });
    let source = "import h\nfn main() {\n  let xs: List(Int) = []\n  let _ = h.first(xs)\n  host.give(1)\n}\nimport host";
    assert_eq!(run(source, host), Ok(Value::Int(1)));
}

/// A `type a` parameter shows the variable as the type itself.
#[test]
fn a_type_parameter_shows_the_variable() {
    let host = HostModule::new("h").function(
        "fn pick(s: String, type a) -> Option(a)",
        |_: &[Value]| Ok(Value::variant(bv::SOME, [Value::Int(1)])),
    );
    let err = returned_error(host.clone(), r#"h.pick("x", String)"#);
    assert!(
        err.contains(
            "h.pick: its signature returns Option(a), where a is String (its type argument \
             `a`), but it returned Variant Some(1)"
        ),
        "got: {err}"
    );
    let source = "import h\nfn main() {\n  match h.pick(\"x\", Int) {\n    Some(n) -> host.give(n)\n    None -> host.give(0)\n  }\n}\nimport host";
    assert_eq!(run(source, host), Ok(Value::Int(1)));
}

/// A Channel result is a channel; what it carries is not shown.
#[test]
fn a_channel_result_is_a_channel() {
    let host = HostModule::new("h").function("fn open(n: Int) -> Channel(Int)", |_: &[Value]| {
        Ok(Value::Int(1))
    });
    let err = returned_error(host, "h.open(1)");
    assert!(
        err.contains("h.open: its signature returns Channel(Int), but it returned Int 1"),
        "got: {err}"
    );
}

/// A host function that gives back a deep value of a variable's type
/// runs, whatever the depth: the check compares a bounded part of the
/// value, and none of it on the stack. (It walked the whole value by
/// recursion: a list of 10,000 cells overflowed a debug build's stack
/// and aborted the process.) In a thread of the default stack of a
/// spawned thread, as an embedder's may be.
#[test]
fn a_deep_result_of_a_variable_s_type_is_checked_in_bounded_steps() {
    for cells in [30_000, 1_000_000] {
        let outcome = std::thread::spawn(move || {
            let host = HostModule::new("h")
                .function("fn same(x: a) -> a", |args: &[Value]| Ok(args[0].clone()))
                // (Two lists of the type, not one shared: their parts are
                // compared.)
                .function("fn second(x: a, y: a) -> a", |args: &[Value]| {
                    Ok(args[1].clone())
                });
            let source = format!(
                "import h\ntype L {{ Nil, Cons(Int, L) }}\n\
                 fn build(n: Int, acc: L) -> L {{\n  match n {{\n    0 -> acc\n    _ -> build(n - 1, Cons(n, acc))\n  }}\n}}\n\
                 fn main() {{\n  let l = build({cells}, Nil)\n  let _m = h.same(l)\n  let _r = h.second(l, build({cells}, Nil))\n  host.give(1)\n}}\nimport host"
            );
            run(&source, host)
        })
        .join()
        .expect("the check does not overflow the stack");
        assert_eq!(outcome, Ok(Value::Int(1)), "{cells} cells");
    }
}

/// Each element of a list or a map that a variable stands for is
/// compared with the first one of the argument's, within the bound: a
/// list of Ints given back with a String after the first element is
/// refused.
#[test]
fn later_elements_of_a_collection_are_compared() {
    let host = HostModule::new("h")
        .function("fn hetero(x: a) -> a", |_: &[Value]| {
            Ok(Value::list(vec![Value::Int(1), Value::String("x".into())]))
        })
        .function("fn same(x: a) -> a", |args: &[Value]| Ok(args[0].clone()));
    let err = returned_error(host.clone(), "h.hetero([7])");
    assert!(
        err.contains(
            "h.hetero: its signature returns a, here List(Int) (the type of its argument `x`), but \
             it returned List [1, \"x\"]"
        ),
        "got: {err}"
    );
    // (A list of the type, longer than the argument, passes.)
    let source =
        "import h\nfn main() {\n  let xs = h.same([1, 2, 3])\n  host.give(xs)\n}\nimport host";
    assert_eq!(
        run(source, host),
        Ok(Value::list(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3)
        ]))
    );
}

/// Each place that may show a variable is searched as far as the bound
/// on its own: a long argument that shows nothing (200 `None`s) leaves
/// a later argument that shows the variable its search, so a wrong
/// result is refused.
#[test]
fn a_later_argument_shows_the_variable_past_a_long_one() {
    let wrong = |_: &[Value]| Ok(Value::String("s".into()));
    let host = HostModule::new("h")
        .function("fn f(xs: List(Option(a)), w: a) -> a", wrong)
        .function(
            "fn g(xs: List(Option(a)), ys: List(Option(a))) -> Option(a)",
            |_: &[Value]| Ok(Value::variant(bv::SOME, [Value::String("s".into())])),
        );
    let nones = vec!["None"; 200].join(", ");
    let err = returned_error(host.clone(), &format!("h.f([Some(1), {nones}], 1)"));
    assert!(
        err.contains("h.f: its signature returns a, here Int"),
        "got: {err}"
    );
    let err = returned_error(host.clone(), &format!("h.f([{nones}], 1)"));
    assert!(
        err.contains("h.f: its signature returns a, here Int (the type of its argument `w`)"),
        "got: {err}"
    );
    let err = returned_error(host, &format!("h.g([{nones}], [Some(1)])"));
    assert!(
        err.contains("h.g: its signature returns Option(a), where a is Int"),
        "got: {err}"
    );
}

/// Within the parts the check compares, a result of another
/// instantiation of the argument's type is refused: `Box(String)` for a
/// `Box(Int)`.
#[test]
fn a_wrong_part_within_the_bound_is_refused() {
    let host = HostModule::new("h").function("fn same(x: a) -> a", |args: &[Value]| {
        let Value::Record(record) = &args[0] else {
            return Err(VmError::new("expected a record".into()));
        };
        Ok(Value::record(
            record.ty().clone(),
            vec![Value::String("s".into())],
        ))
    });
    let source = "import h\ntype Box(t) { v: t }\nfn main() {\n  let b = h.same(Box { v: 1 })\n  host.give(b.v)\n}\nimport host";
    let err = run(source, host).expect_err("a Box(String) for a Box(Int)");
    assert!(
        err.contains(
            "h.same: its signature returns a, here Box(Int) (the type of its argument `x`), but it \
             returned Record Box {v: \"s\"}"
        ),
        "got: {err}"
    );
}

/// Host functions that return what their signatures say keep running,
/// of every shape that is checked: variables shown by an argument, its
/// element, a key or a value of a map, an option's value, a tuple's
/// part, a `type a` parameter, channels, and a variable bound to a
/// record of a declared type.
#[test]
fn results_of_the_type_the_signature_returns_pass() {
    let host = HostModule::new("h")
        .function("fn same(x: a) -> a", |args: &[Value]| Ok(args[0].clone()))
        .function("fn first(xs: List(a)) -> Option(a)", |args: &[Value]| {
            let Value::List(xs) = &args[0] else {
                return Err(VmError::new("expected a list".into()));
            };
            xs.first().into_value().map_err(VmError::new)
        })
        .function("fn swap(p: (a, b)) -> (b, a)", |args: &[Value]| {
            let Value::Tuple(parts) = &args[0] else {
                return Err(VmError::new("expected a tuple".into()));
            };
            Ok(Value::tuple(vec![parts[1].clone(), parts[0].clone()]))
        })
        .function("fn keys(m: Map(k, v)) -> List(k)", |args: &[Value]| {
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("expected a map".into()));
            };
            Ok(Value::list(m.iter().map(|(k, _)| k.clone()).collect()))
        })
        .function("fn get(o: Option(a), d: a) -> a", |args: &[Value]| {
            Ok(match &args[0] {
                Value::Variant(v) if v.is(bv::SOME) => v.fields()[0].clone(),
                _ => args[1].clone(),
            })
        })
        .function("fn pick(s: String, type a) -> List(a)", |_: &[Value]| {
            Ok(Value::list(Vec::new()))
        })
        .function(
            "fn pass(c: Channel(Int)) -> Channel(Int)",
            |args: &[Value]| Ok(args[0].clone()),
        );
    let source = r#"
import h
import channel
type Person { name: String, age: Int }
fn main() {
  let a = h.same([1, 2])
  let b = h.first(["x", "y"])
  let c = h.swap((1, "s"))
  let d = h.keys(#{"k": 1})
  let e = h.get(None, 3)
  let p = h.same(Person { name: "b", age: 4 })
  let q: List(Int) = h.pick("x", Int)
  let ch = h.pass(channel.new(1))
  channel.close(ch)
  host.give("{a} {b} {c} {d} {e} {p.age} {q}")
}
import host
"#;
    assert_eq!(
        run(source, host),
        Ok(Value::String(
            "[1, 2] Some(\"x\") (\"s\", 1) [\"k\"] 3 4 []".into()
        ))
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
        let errors = errors("fn main() { let _ = 1 }", host);
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
    let errors = errors("fn main() { let _ = 1 }", host);
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
    use silt::{Buffer, HostIo, Vm};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    // 1. Declare the host module: each function by its silt signature. A
    //    program's `main` returns nothing: it hands its result to the host
    //    through a function of the host's, `mylib.give` here.
    let given = Arc::new(Mutex::new(0));
    let slot = given.clone();
    let mylib = HostModule::new("mylib")
        .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
        .fn1("fn give(x: Int) -> ()", move |x: i64| {
            *slot.lock().unwrap() = x
        });

    // 2. Open the program in a session that knows the module.
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![mylib],
    });
    let source = "import mylib\nfn main() { mylib.give(mylib.double(21)) }";
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
    Vm::new(HostIo::buffer(&out)).run_program(&program).unwrap();
    assert_eq!(*given.lock().unwrap(), 42);
}

/// The host module the result checks are probed with: functions that
/// give back an argument (`same`, `second`, `other` its second, of
/// another type), an element of one (`pick`), or the value of a type
/// argument (`cast`), and functions of maps, sets, channels.
fn probe() -> HostModule {
    let at = |i: usize| move |args: &[Value]| Ok(args[i].clone());
    HostModule::new("h")
        .function("fn same(x: a) -> a", at(0))
        .function("fn second(x: a, y: a) -> a", at(1))
        .function("fn other(x: a, y: b) -> a", at(1))
        .function("fn pick(xs: List(a), i: Int) -> a", |args: &[Value]| {
            let (Value::List(xs), Value::Int(i)) = (&args[0], &args[1]) else {
                return Err(VmError::new("expected a list and an index".into()));
            };
            Ok(xs.get(*i as usize).unwrap_or(Value::Unit))
        })
        .function("fn cast(v: b, type a) -> a", at(0))
        .function("fn tcast(v: b, t: TypeOf(a)) -> a", at(0))
        .function("fn otcast(v: b, t: Option(TypeOf(a))) -> a", at(0))
        .function("fn tyval(x: a, type a) -> a", at(0))
        .function(
            "fn chan2(c: Channel(a), d: Channel(a)) -> Channel(a)",
            at(1),
        )
        .function("fn swap(p: (a, b)) -> (b, a)", |args: &[Value]| {
            let Value::Tuple(parts) = &args[0] else {
                return Err(VmError::new("expected a tuple".into()));
            };
            Ok(Value::tuple(vec![parts[1].clone(), parts[0].clone()]))
        })
        .function("fn noswap(p: (a, b)) -> (b, a)", at(0))
        .function(
            "fn wrapres(x: a) -> Result(a, String)",
            |args: &[Value]| Ok(Value::variant(bv::OK, [args[0].clone()])),
        )
        .function("fn rep(x: a, n: Int) -> List(a)", |args: &[Value]| {
            let Value::Int(n) = args[1] else {
                return Err(VmError::new("expected a count".into()));
            };
            Ok(Value::list(vec![args[0].clone(); n as usize]))
        })
        .function("fn otherl(xs: List(a), ys: List(b)) -> List(a)", at(1))
        .function("fn vals(m: Map(k, v)) -> List(v)", |args: &[Value]| {
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("expected a map".into()));
            };
            Ok(Value::list(m.iter().map(|(_, v)| v.clone()).collect()))
        })
        .function("fn keys2(m: Map(k, v), n: Map(k, v)) -> Map(k, v)", at(1))
        .function("fn sets(s: Set(a), t: Set(a)) -> Set(a)", at(1))
}

/// The declarations the programs of `probe` calls are written against.
const PROBE_PRELUDE: &str = "import h
import channel
import task
import json
import toml
import list
import string
import result
type Shape { Circle(Float), Rect(Int, Int), Dot }
type Box(t) { v: t }
type Person { name: String, age: Int }
type Pet { name: String, age: Int }
type Id = Int
type Pair = (Int, String)
type B = Box(Int)
fn inc(x: Int) -> Int { x + 1 }
";

/// Run `body`, the statements of `main`, against `probe` with the
/// module `g` of the one function `signature` (which gives back its
/// argument `ret`): the runtime error, or what `main` gave last.
fn run_probe(body: &str, g: Option<(&str, usize)>) -> Result<Value, String> {
    let mut hosts = vec![probe()];
    if let Some((signature, ret)) = g {
        hosts.push(
            HostModule::new("g").function(signature, move |args: &[Value]| Ok(args[ret].clone())),
        );
    }
    let import_g = if g.is_some() { "import g\n" } else { "" };
    let source = format!(
        "{import_g}{PROBE_PRELUDE}fn main() -> Result((), String) {{\n{body}\n  Ok(())\n}}\nimport host"
    );
    run_with_host(&[("main.silt", &source)], hosts)
}

/// A correct host function is never refused: each of these gives back a
/// value of the type its signature returns at the call, though it differs
/// in shape from the one its arguments show (another variant, an empty
/// list, fields written in another order, a range for a list, a record
/// made another way, a function of another kind).
#[test]
fn correct_host_results_of_every_shape_pass() {
    let calls = [
        "h.second(None, Some(1))",
        "h.second(Some(1), None)",
        "h.second([], [1, 2])",
        "h.second([[]], [[1], [2, 3]])",
        "h.second(Circle(1.0), Rect(1, 2))",
        "h.second(Dot, Rect(1, 2))",
        "h.second(Box { v: [] }, Box { v: [1] })",
        "h.second(Box { v: None }, Box { v: Some(\"x\") })",
        "h.second(#{}, #{\"a\": 1})",
        "h.second(#{\"a\": []}, #{\"b\": [1]})",
        "h.second(Ok(1), Err(\"e\"))",
        "h.second((1, []), (2, [\"x\"]))",
        "h.second(Person { name: \"x\", age: 1 }, Person { age: 2, name: \"y\" })",
        "h.second(1..3, [5, 6])",
        "h.second((), ())",
        "h.second(1.5, 2.5)",
        "h.second(task.spawn({ -> 1 }), task.spawn({ -> 2 }))",
        "h.second(json.parse(\"1\", json.Value), json.parse(\"[1]\", json.Value))",
        "h.cast(5, Int)",
        "h.tcast(5, Int)",
        "h.otcast(5, Some(Int))",
        "h.cast([1], List(Int))",
        "h.cast((1, \"x\"), (Int, String))",
        "h.cast(Box { v: 1 }, Box(Int))",
        "h.cast(None, Option(Int))",
        "h.cast(1.0, Float)",
        "h.cast(channel.new(0), Channel(Int))",
        "h.cast(5, Id)",
        "h.cast((5, \"x\"), Pair)",
        "h.cast(#{1: 2}, Map(Int, Int))",
        "h.cast(Ok(1), Result(Int, String))",
        "h.cast(Box { v: 1 }, B)",
        "h.cast(1..4, List(Int))",
        "h.cast(json.parse(\"[1]\", json.Value), Result(json.Value, json.JsonError))",
        "h.cast(Dot, Shape)",
        "h.cast(Person { name: \"a\", age: 1 }, Person)",
        "h.tyval(5, Id)",
        "h.tyval((5, \"x\"), Pair)",
        "h.chan2(channel.new(0), channel.new(5))",
        "h.vals(#{\"a\": [1], \"b\": []})",
        "h.keys2(#{\"a\": []}, #{\"b\": [1]})",
        "h.sets(#[1], #[2, 3])",
        "h.swap((1, \"x\"))",
        "h.wrapres([1])",
        "h.rep(Some(1), 3)",
        "h.pick([Person { name: \"a\", age: 1 }, Person { name: \"b\", age: 2 }], 1)",
    ];
    let refused: Vec<String> = (calls.iter())
        .filter_map(|call| {
            let body = format!("  host.give({call})");
            run_probe(&body, None).err().map(|e| format!("{call}: {e}"))
        })
        .collect();
    assert!(refused.is_empty(), "{refused:#?}");
    // Function values of every kind, and records made in other ways.
    let body = r#"
  let f = h.second({ x -> x + 1 }, inc)
  let g = h.second(inc, { x -> x * 2 })
  let k = h.second(Circle, Circle)
  let s = h.second(string.length, { s -> 0 })
  let fs = h.second([inc], [{ x -> x }, inc])
  let (_, p) = h.second((inc, 1), ({ x -> x }, 2))
  let m = h.second(Some, { x -> Some(x) })
  let lit = Person { name: "a", age: 1 }
  let fromjson = json.parse(json.stringify(#{"age": json.Int(2), "name": json.String("b")}), Person) |> result.map_err({ e -> "{e}" })?
  let fromtoml = toml.parse("age = 3\nname = 'c'", Person) |> result.map_err({ e -> "{e}" })?
  let rs = [h.second(lit, fromjson), h.second(fromjson, lit), h.second(lit, fromtoml), h.second(lit, lit.{ age: 9 })]
  host.give("{f(1)} {g(1)} {k(1.0)} {s("abc")} {list.length(fs)} {p} {m(3)} {rs |> list.map({ r -> r.age })}")"#;
    assert_eq!(
        run_probe(body, None),
        Ok(Value::String(
            "2 2 Circle(1.0) 0 2 2 Some(3) [2, 1, 3, 9]".into()
        ))
    );
}

/// The type a variable has at the call is named in full, as far as the
/// value that shows it shows it: `List(Int)`, not `List`; `_` for what
/// it does not show; `Fn` for a function of any kind; `type Int` for a
/// type given as a value. A `type a` parameter's type is the type it is
/// given.
#[test]
fn the_type_an_argument_shows_is_named_in_full() {
    let cases = [
        (
            "h.other([1], [\"s\"])",
            "here List(Int) (the type of its argument `x`), but it returned List [\"s\"]",
        ),
        (
            "h.other([[[1]]], [[[\"x\"]]])",
            "here List(List(List(Int)))",
        ),
        ("h.other([None], 1)", "here List(Option(_))"),
        (
            "h.other(#{\"a\": 1}, #{\"a\": \"x\"})",
            "here Map(String, Int)",
        ),
        ("h.other(#[1], 1)", "here Set(Int)"),
        ("h.other(Some(1), Some(\"x\"))", "here Option(Int)"),
        ("h.other(Ok(1), 1)", "here Result(Int, _)"),
        ("h.other((1, 2), (1, 2, 3))", "here (Int, Int)"),
        ("h.other(Box { v: 1 }, Box { v: \"s\" })", "here Box(Int)"),
        ("h.other(Box { v: [\"s\"] }, 1)", "here Box(List(String))"),
        ("h.other(Circle(1.0), Some(1))", "here Shape"),
        (
            "h.other(Person { name: \"a\", age: 1 }, Pet { name: \"b\", age: 2 })",
            "here Person",
        ),
        ("h.other((), 1)", "here ()"),
        ("h.other(channel.new(0), 1)", "here Channel(_)"),
        ("h.other(inc, 1)", "here Fn"),
        ("h.other(string.length, 1)", "here Fn"),
        ("h.other(Some, 1)", "here Fn"),
        ("h.other(h.same, 1)", "here Fn"),
        (
            "h.other(Int, 1)",
            "here type Int (the type of its argument `x`), but it returned Int 1",
        ),
        (
            "h.other(Person, Person { name: \"a\", age: 1 })",
            "here type Person",
        ),
        (
            "h.cast(1, String)",
            "here String (its type argument `a`), but it returned Int 1",
        ),
        (
            "h.cast([1], Set(Int))",
            "here Set(Int) (its type argument `a`)",
        ),
        (
            "h.cast([1], Box(Int))",
            "here Box(Int) (its type argument `a`)",
        ),
        // (`t: TypeOf(a)` is `type a` written out: the same check.)
        (
            "h.tcast(1, String)",
            "here String (its type argument `a`), but it returned Int 1",
        ),
        ("h.tcast([1], Set(Int))", "here Set(Int)"),
        // (And so is one inside the value of a parameter.)
        (
            "h.otcast(1, Some(String))",
            "here String (its type argument `a`), but it returned Int 1",
        ),
        (
            "h.otherl([1], [\"x\"])",
            "returns List(a), where a is Int (from its argument `xs`)",
        ),
    ];
    let wrong: Vec<String> = (cases.iter())
        .filter_map(|(call, expected)| {
            let err = run_probe(&format!("  host.give({call})"), None).expect_err(call);
            (!err.contains(expected)).then(|| format!("{call}: {err}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A parameter written as a pattern is named by its place.
#[test]
fn a_parameter_written_as_a_pattern_is_named_by_its_place() {
    for (signature, ret, call, expected) in [
        (
            "fn f((x, y): (a, Int)) -> a",
            0,
            "g.f((1, 2))",
            "here Int (from its first argument)",
        ),
        (
            "fn f(s: String, (x, y): (a, Int)) -> a",
            1,
            "g.f(\"s\", (1, 2))",
            "here Int (from its second argument)",
        ),
        (
            "fn f(s: String, _: a) -> a",
            0,
            "g.f(\"s\", 1)",
            "here Int (the type of its second argument)",
        ),
    ] {
        let err =
            run_probe(&format!("  host.give({call})"), Some((signature, ret))).expect_err(call);
        assert!(err.contains(expected), "{signature}: {err}");
    }
}
