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
        (
            "fn f(p: {name: String}) -> {name: String, ...r}",
            "row variable 'r' in the return type of the host function `f`",
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
        "fn keep(p: {name: String, ...r}) -> {name: String, ...r}",
        "fn pick(s: String, type a) -> Option(a)",
    ] {
        let host = HostModule::new("h").function(signature, |_: &[Value]| Ok(Value::Unit));
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

/// A closed record result has the fields its type writes, no more and
/// no fewer; an open one gives back the rest of the record it is given.
#[test]
fn a_record_result_is_checked_by_its_fields() {
    let host = HostModule::new("h")
        .function(
            "fn missing(n: Int) -> {name: String, age: Int}",
            |_: &[Value]| Ok(Value::anon_record([("name", Value::String("a".into()))])),
        )
        .function("fn extra(n: Int) -> {name: String}", |_: &[Value]| {
            Ok(Value::anon_record([
                ("name", Value::String("a".into())),
                ("age", Value::Int(3)),
            ]))
        })
        .function("fn wrong(n: Int) -> {name: String}", |_: &[Value]| {
            Ok(Value::anon_record([("name", Value::Int(1))]))
        })
        .function(
            "fn keep(p: {name: String, ...r}) -> {name: String, ...r}",
            |_: &[Value]| Ok(Value::anon_record([("name", Value::String("b".into()))])),
        );
    for (call, expected) in [
        (
            "h.missing(1)",
            "h.missing: its signature returns {name: String, age: Int}, but it returned Record",
        ),
        (
            "h.extra(1)",
            "h.extra: its signature returns {name: String}, but it returned Record",
        ),
        (
            "h.wrong(1)",
            "h.wrong: its signature returns {name: String}, but it returned Record",
        ),
        (
            r#"h.keep({name: "a", age: 3})"#,
            "h.keep: its signature returns {name: String, ...r}, where r is the rest of \
             {age: Int, name: String} (the type of its argument `p`), but it returned Record {name: \"b\"}",
        ),
    ] {
        let err = returned_error(host.clone(), call);
        assert!(err.contains(expected), "{call}: {err}");
    }
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
            "h.hetero: its signature returns a, here List (the type of its argument `x`), but \
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
            "h.same: its signature returns a, here Box (the type of its argument `x`), but it \
             returned Record Box {v: \"s\"}"
        ),
        "got: {err}"
    );
}

/// Host functions that return what their signatures say keep running,
/// of every shape that is checked: variables shown by an argument, its
/// element, a key or a value of a map, an option's value, a tuple's
/// part, a record's field and row, a `type a` parameter, records,
/// channels, and a variable bound to a record of a declared type.
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
        .function("fn name(p: {name: a}) -> a", |args: &[Value]| {
            let Value::Record(r) = &args[0] else {
                return Err(VmError::new("expected a record".into()));
            };
            Ok(r.get("name").cloned().unwrap_or(Value::Unit))
        })
        .function(
            "fn keep(p: {name: String, ...r}) -> {name: String, ...r}",
            |args: &[Value]| Ok(args[0].clone()),
        )
        .function("fn pick(s: String, type a) -> List(a)", |_: &[Value]| {
            Ok(Value::list(Vec::new()))
        })
        .function(
            "fn user(n: Int) -> {name: String, age: Int}",
            |args: &[Value]| {
                Ok(Value::anon_record([
                    ("age", args[0].clone()),
                    ("name", Value::String("a".into())),
                ]))
            },
        )
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
  let f = h.name({name: true})
  let g = h.keep({name: "a", age: 3})
  let p = h.keep(Person { name: "b", age: 4 })
  let q: List(Int) = h.pick("x", Int)
  let u = h.user(7)
  let ch = h.pass(channel.new(1))
  channel.close(ch)
  host.give("{a} {b} {c} {d} {e} {f} {g.age} {p.age} {q} {u.age}")
}
import host
"#;
    assert_eq!(
        run(source, host),
        Ok(Value::String(
            "[1, 2] Some(x) (s, 1) [k] 3 true 3 4 [] 7".into()
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
