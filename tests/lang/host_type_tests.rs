//! Host types: a host module declares record and enum types, each by
//! its silt declaration (`declare_type("type Point { x: Int, y: Int }")`).
//! Its signatures name them, a program names them as `geo.Point` and
//! uses them like any module's types, and a value of one crosses the
//! boundary checked: a host function's result against the type its
//! signature returns, the fields and payloads too.

use silt::diagnostic::{Code, Diagnostic};
use silt::session::testing::{check_with_host, run_with_host};
use silt::session::{HostCall, HostModule};
use silt::value::Value;
use silt::vm::VmError;

/// The field `name` of the record `value`, as an `Int`.
fn int_field(value: &Value, name: &str) -> Result<i64, VmError> {
    match value {
        Value::Record(record) => match record.get(name) {
            Some(Value::Int(n)) => Ok(*n),
            _ => Err(VmError::new(format!("no Int field `{name}`"))),
        },
        _ => Err(VmError::new("expected a record".into())),
    }
}

/// A host module with a record type, an enum type and a generic record
/// type, and functions that take and give values of them.
fn geo() -> HostModule {
    HostModule::new("geo")
        .declare_type("type Point { x: Int, y: Int }")
        .declare_type("type Shape { Circle(Int), Rect(Point, Point) }")
        .declare_type("type Box(a) { value: a, tag: String }")
        .function("fn origin() -> Point", |call: &HostCall| {
            // (The fields by name, in any order.)
            call.record("Point", [("y", Value::Int(0)), ("x", Value::Int(0))])
        })
        .function("fn norm1(p: Point) -> Int", |call: &HostCall| {
            let p = &call.args[0];
            Ok(Value::Int(
                int_field(p, "x")?.abs() + int_field(p, "y")?.abs(),
            ))
        })
        .function("fn shift(p: Point, d: Int) -> Point", |call: &HostCall| {
            let Value::Int(d) = call.args[1] else {
                return Err(VmError::new("expected an Int".into()));
            };
            let p = &call.args[0];
            call.record(
                "Point",
                [
                    ("x", Value::Int(int_field(p, "x")? + d)),
                    ("y", Value::Int(int_field(p, "y")? + d)),
                ],
            )
        })
        .function("fn circle(r: Int) -> Shape", |call: &HostCall| {
            call.variant("Shape", "Circle", vec![call.args[0].clone()])
        })
        .function("fn boxed(n: Int) -> Box(Int)", |call: &HostCall| {
            call.record(
                "Box",
                [("value", call.args[0].clone()), ("tag", Value::string("n"))],
            )
        })
        .function(
            "fn tag(b: Box(a)) -> String",
            |call: &HostCall| match &call.args[0] {
                Value::Record(record) => Ok(record.get("tag").cloned().unwrap_or(Value::Unit)),
                _ => Err(VmError::new("expected a record".into())),
            },
        )
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

/// A program constructs, reads, updates and matches a host record and
/// a host enum like a module's own, annotates with them, and passes
/// them to and from the host.
#[test]
fn a_program_uses_host_types_like_a_modules_types() {
    let source = r#"
import geo
import host

fn far(p: geo.Point) -> Bool { geo.norm1(p) > 5 }

fn describe(s: geo.Shape) -> String {
  match s {
    geo.Circle(r) -> "circle {r}"
    geo.Shape.Rect(a, b) -> "rect {a.x},{a.y} {b.x},{b.y}"
  }
}

fn main() {
  let p = geo.Point { x: 3, y: -4 }
  let q = p.{ x: 10 }
  let o = geo.origin()
  let at_zero = match o {
    geo.Point { x: 0, y } -> "zero {y}"
    _ -> "elsewhere"
  }
  let r = geo.Rect(geo.origin(), geo.shift(p, 1))
  let b = geo.boxed(7)
  host.give("{geo.norm1(p)} {q.x} {at_zero} {far(q)} {describe(geo.circle(2))} {describe(r)} {b.value} {geo.tag(b)} {o == geo.Point { x: 0, y: 0 }}")
}
"#;
    assert_eq!(
        run(source, geo()),
        Ok(Value::string(
            "7 10 zero 0 true circle 2 rect 0,0 4,-3 7 n true"
        ))
    );
}

/// The types are imported by name too, and print by their name.
#[test]
fn host_types_are_imported_by_name_and_print_by_their_name() {
    let source = r#"
import geo.{ Point, Shape, Circle }
import host
fn main() {
  let p: Point = Point { x: 1, y: 2 }
  let s: Shape = Circle(3)
  host.give("{p} {s}")
}
"#;
    assert_eq!(
        run(source, geo()),
        Ok(Value::string("Point {x: 1, y: 2} Circle(3)"))
    );
}

/// A host type is checked like any type: a wrong field, a missing
/// field, and a host value used as another type are type errors.
#[test]
fn a_host_type_is_checked_in_the_program() {
    for (source, expected) in [
        (
            "import geo\nfn main() { let _ = geo.Point { x: 1, y: \"2\" } }",
            Code::TypeMismatch,
        ),
        (
            "import geo\nfn main() { let _ = geo.Point { x: 1 } }",
            Code::MissingField,
        ),
        (
            "import geo\nfn main() { let _ = geo.norm1(geo.circle(1)) }",
            Code::TypeMismatch,
        ),
        (
            "import geo\nfn main() { let _ = geo.Point { x: 1, y: 2 }.z }",
            Code::UnknownField,
        ),
    ] {
        let errors = errors(source, geo());
        assert_eq!(errors.len(), 1, "{source}: {errors:?}");
        assert_eq!(errors[0].code, expected, "{source}: {errors:?}");
    }
}

/// A result of a host type is checked against the type the signature
/// returns: its type, the types of its fields, of a variant's payload,
/// and of a generic type's fields at the signature's arguments.
#[test]
fn a_host_types_result_is_checked() {
    let host = HostModule::new("g")
        .declare_type("type Point { x: Int, y: Int }")
        .declare_type("type Size { w: Int, h: Int }")
        .declare_type("type Shape { Circle(Int), Dot }")
        .declare_type("type Box(a) { value: a }")
        .declare_type("type Tree { Leaf, Node(Tree, Int, Tree) }")
        .function("fn size() -> Size", |call: &HostCall| {
            call.record("Point", [("x", Value::Int(1)), ("y", Value::Int(2))])
        })
        .function("fn point() -> Point", |call: &HostCall| {
            call.record("Point", [("x", Value::Int(1)), ("y", Value::string("2"))])
        })
        .function("fn circle() -> Shape", |call: &HostCall| {
            call.variant("Shape", "Circle", vec![Value::Bool(true)])
        })
        .function("fn boxed() -> Box(Int)", |call: &HostCall| {
            call.record("Box", [("value", Value::string("s"))])
        })
        .function("fn tree() -> Tree", |call: &HostCall| {
            let leaf = call.variant("Tree", "Leaf", vec![])?;
            // (Wrong inside: a tree is checked as deep as its parts go.)
            let bad = call.variant(
                "Tree",
                "Node",
                vec![leaf.clone(), Value::Unit, leaf.clone()],
            )?;
            call.variant("Tree", "Node", vec![bad, Value::Int(1), leaf])
        })
        .function("fn points() -> List(Point)", |call: &HostCall| {
            let p = call.record("Size", [("w", Value::Int(1)), ("h", Value::Int(2))])?;
            Ok(Value::list(vec![p]))
        });
    for (call, expected) in [
        (
            "g.size()",
            "g.size: its signature returns Size, but it returned Record Point {x: 1, y: 2}",
        ),
        (
            "g.point()",
            "g.point: its signature returns Point, but it returned Record",
        ),
        (
            "g.circle()",
            "g.circle: its signature returns Shape, but it returned",
        ),
        (
            "g.boxed()",
            "g.boxed: its signature returns Box(Int), but it returned",
        ),
        (
            "g.tree()",
            "g.tree: its signature returns Tree, but it returned",
        ),
        (
            "g.points()",
            "g.points: its signature returns List(Point), but it returned",
        ),
    ] {
        let source = format!("import g\nimport host\nfn main() {{ host.give({call}) }}");
        let err = run(&source, host.clone()).expect_err(call);
        assert!(err.contains(expected), "{call}: {err}");
    }
}

/// What a host function builds is refused when the type has no such
/// field, variant or name, with a message that says what it has.
#[test]
fn building_a_value_of_a_type_that_does_not_fit_is_a_runtime_error() {
    let host = HostModule::new("g")
        .declare_type("type Point { x: Int, y: Int }")
        .declare_type("type Shape { Circle(Int), Dot }")
        .function("fn missing() -> Point", |call: &HostCall| {
            call.record("Point", [("x", Value::Int(1))])
        })
        .function("fn extra() -> Point", |call: &HostCall| {
            call.record(
                "Point",
                [
                    ("x", Value::Int(1)),
                    ("y", Value::Int(1)),
                    ("z", Value::Int(1)),
                ],
            )
        })
        .function("fn unknown() -> Point", |call: &HostCall| {
            call.record("Pointt", [("x", Value::Int(1))])
        })
        .function("fn not_an_enum() -> Point", |call: &HostCall| {
            call.variant("Point", "Circle", vec![])
        })
        .function("fn no_variant() -> Shape", |call: &HostCall| {
            call.variant("Shape", "Square", vec![Value::Int(1)])
        })
        .function("fn arity() -> Shape", |call: &HostCall| {
            call.variant("Shape", "Circle", vec![])
        })
        .function("fn not_a_record() -> Shape", |call: &HostCall| {
            call.record("Shape", [])
        });
    for (call, expected) in [
        (
            "g.missing()",
            "g.missing: a `Point` has the fields x, y, each once; given: x",
        ),
        (
            "g.extra()",
            "g.extra: a `Point` has the fields x, y, each once; given: x, y, z",
        ),
        (
            "g.unknown()",
            "g.unknown: there is no type `Pointt`: name a type the host module declares",
        ),
        (
            "g.not_an_enum()",
            "g.not_an_enum: `Point` is not an enum type",
        ),
        (
            "g.no_variant()",
            "g.no_variant: `Shape` has no variant `Square`: its variants are Circle, Dot",
        ),
        ("g.arity()", "g.arity: `Shape.Circle` has 1 field, given 0"),
        (
            "g.not_a_record()",
            "g.not_a_record: `Shape` is not a record type",
        ),
    ] {
        let source = format!("import g\nimport host\nfn main() {{ host.give({call}) }}");
        let err = run(&source, host.clone()).expect_err(call);
        assert!(err.contains(expected), "{call}: {err}");
    }
}

/// An opaque host type is the host's to build and read: a program
/// passes it on, and cannot build it or read its fields.
#[test]
fn an_opaque_host_type_is_built_and_read_by_the_host_only() {
    let host = HostModule::new("db")
        .declare_type("opaque type Conn { id: Int }")
        .function("fn open(id: Int) -> Conn", |call: &HostCall| {
            call.record("Conn", [("id", call.args[0].clone())])
        })
        .function("fn id(h: Conn) -> Int", |call: &HostCall| {
            Ok(Value::Int(int_field(&call.args[0], "id")?))
        });
    let source = "import db\nimport host\nfn main() { host.give(db.id(db.open(4))) }";
    assert_eq!(run(source, host.clone()), Ok(Value::Int(4)));
    for source in [
        "import db\nfn main() { let _ = db.open(4).id }",
        "import db\nfn main() { let _ = db.Conn { id: 4 } }",
    ] {
        let errors = errors(source, host.clone());
        assert_eq!(errors.len(), 1, "{source}: {errors:?}");
    }
}

/// What is wrong with a host type is reported at `<host:m>` whether or
/// not the program imports the module.
#[test]
fn a_malformed_host_type_is_reported() {
    let cases: Vec<(HostModule, &str)> = vec![
        (
            HostModule::new("h").declare_type("type Id = Int"),
            "the host type `Id` is an alias: a host module declares record and enum types",
        ),
        (
            HostModule::new("h").declare_type("fn f(x: Int) -> Int"),
            "a host type is declared by a `type` declaration",
        ),
        (
            HostModule::new("h").declare_type("type A { x: Int }\ntype B { y: Int }"),
            "a host type is declared by one `type` declaration, but the declaration of type 1 \
             of the host module `h` has 2",
        ),
        (
            HostModule::new("h").declare_type(""),
            "the declaration of type 1 of the host module `h` has 0",
        ),
        (
            HostModule::new("h").function("type P { x: Int }", |_: &HostCall| Ok(Value::Unit)),
            "a host function is declared by a `fn` header with no body",
        ),
        (
            HostModule::new("h").declare_type("type Option(a) { x: a }"),
            "the host type `Option` has the name of a type of the prelude: a host type has a \
             name of its own",
        ),
        (
            HostModule::new("h").declare_type("type Result(a, e) { Fine(a), Bad(e) }"),
            "the host type `Result` has the name of a type of the prelude",
        ),
        (
            HostModule::new("h").declare_type("type Never { x: Int }"),
            "the host type `Never` has the name of a type of the prelude",
        ),
        (
            HostModule::new("h").declare_type("type Display { x: Int }"),
            "the host type `Display` has the name of a type of the prelude",
        ),
        (
            HostModule::new("h").declare_type("pub type P { x: Int }"),
            "a host module's declarations are public: write it without `pub`",
        ),
        (
            HostModule::new("h").declare_type("pub opaque type P { x: Int }"),
            "a host module's declarations are public: write it without `pub`",
        ),
        (
            HostModule::new("h")
                .function("pub fn f(x: Int) -> Int", |_: &HostCall| Ok(Value::Int(1))),
            "a host module's declarations are public: write it without `pub`",
        ),
        (
            HostModule::new("h").declare_type("type P { f: Fn(Int) -> Int }"),
            "the host type `P` cannot hold a function: a host function has no way to call one",
        ),
        (
            HostModule::new("h").declare_type("type S { Run(List(Fn() -> ())) }"),
            "the host type `S` cannot hold a function",
        ),
        (
            HostModule::new("h").declare_type("type P { x: Strin }"),
            "unknown type 'Strin'",
        ),
        (
            HostModule::new("h")
                .declare_type("type P { x: Int }")
                .declare_type("type P { y: Int }"),
            "'P' is bound twice at the top level: by the type and by the type here",
        ),
        (
            HostModule::new("h")
                .declare_type("type Point { x: Int }")
                .function("fn Point() -> Int", |_: &HostCall| Ok(Value::Int(1))),
            "'Point' is bound twice at the top level: by the type and by the function here",
        ),
        (
            HostModule::new("h").function("fn f(p: Pt) -> Int", |_: &HostCall| Ok(Value::Int(1))),
            "unknown type 'Pt'",
        ),
        (
            HostModule::new("h").declare_type("type P { x: Int, x: Int }"),
            "duplicate field 'x' in record type 'P'",
        ),
        (
            HostModule::new("h")
                .declare_type("type B(a) { v: a }")
                .function("fn f(x: B) -> Int", |_: &HostCall| Ok(Value::Int(1))),
            "`B` takes 1 type argument here",
        ),
        (
            HostModule::new("h").declare_type("type P { x: mylib.Q }"),
            "undefined type 'mylib.Q': a host module names its own types and those of builtin \
             modules",
        ),
        (
            HostModule::new("h").function("fn f(x: time.Nope) -> Int", |_: &HostCall| {
                Ok(Value::Int(1))
            }),
            "module 'time' has no type 'Nope'",
        ),
    ];
    for (host, expected) in cases {
        let shown = format!("{host:?}");
        let errors = errors("fn main() { let _ = 1 }", host);
        assert!(!errors.is_empty(), "{shown}: no error");
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{shown}: {errors:?}"
        );
    }
}

/// A function named like a builtin module does not hide the module from
/// the signatures, and a type of a module that needs a feature is named
/// so too.
#[test]
fn a_function_named_like_a_builtin_module_does_not_hide_it() {
    let host = HostModule::new("h")
        .function("fn json(v: json.Value) -> String", |_: &HostCall| {
            Ok(Value::string("v"))
        })
        .function("fn io(e: io.IoError) -> Int", |_: &HostCall| {
            Ok(Value::Int(1))
        })
        // (An opaque builtin type, given and given back.)
        .function(
            "fn regex(r: regex.Regex) -> regex.Regex",
            |call: &HostCall| Ok(call.args[0].clone()),
        );
    let found = errors("fn main() { let _ = 1 }", host.clone());
    assert!(found.is_empty(), "{found:?}");
    let source = "import h\nimport host\nimport regex\nfn main() {\n  match regex.compile(\"a+\") {\n    Ok(r) -> host.give(regex.is_match(\"caat\", h.regex(r)))\n    Err(_) -> host.give(false)\n  }\n}";
    assert_eq!(run(source, host), Ok(Value::Bool(true)));
    #[cfg(feature = "http")]
    {
        let host = HostModule::new("web").function(
            "fn path(r: http.Request) -> String",
            |call: &HostCall| match &call.args[0] {
                Value::Record(r) => Ok(r.get("path").cloned().unwrap_or(Value::Unit)),
                _ => Err(VmError::new("expected a request".into())),
            },
        );
        let found = errors("fn main() { let _ = 1 }", host);
        assert!(found.is_empty(), "{found:?}");
    }
}

/// A host signature names the types of builtin modules without an
/// import, and a result of one is checked by its type.
#[test]
fn a_host_signature_names_builtin_module_types() {
    let host = HostModule::new("clock")
        .declare_type("type Stamp { at: time.Date, note: String }")
        .function("fn epoch() -> time.Date", |call: &HostCall| {
            call.record(
                "time.Date",
                [
                    ("year", Value::Int(1970)),
                    ("month", Value::Int(1)),
                    ("day", Value::Int(1)),
                ],
            )
        })
        .function("fn year(d: time.Date) -> Int", |call: &HostCall| {
            Ok(Value::Int(int_field(&call.args[0], "year")?))
        })
        .function("fn stamp(d: time.Date) -> Stamp", |call: &HostCall| {
            call.record(
                "Stamp",
                [("at", call.args[0].clone()), ("note", Value::string("n"))],
            )
        })
        .function("fn wrong() -> time.Date", |call: &HostCall| {
            call.record(
                "Stamp",
                [("at", Value::Int(0)), ("note", Value::string("n"))],
            )
        });
    let source = r#"
import clock
import host
import time
fn main() {
  let d = clock.epoch()
  let s = clock.stamp(d)
  host.give("{clock.year(d)} {time.weekday(s.at)}")
}
"#;
    assert_eq!(
        run(source, host.clone()),
        Ok(Value::string("1970 Thursday"))
    );
    let err = run(
        "import clock\nimport host\nfn main() { host.give(clock.wrong()) }",
        host,
    )
    .expect_err("a result of another type");
    assert!(
        err.contains("clock.wrong: its signature returns time.Date, but it returned Record"),
        "{err}"
    );
}

/// The "Host types" example of docs/ffi.md, as written there, with a
/// `main` that calls its `show`.
#[test]
fn docs_ffi_host_types() {
    use silt::session::{Builtins, Config, Entry, LockPolicy, ProjectSetup, Session};
    use silt::{Buffer, HostIo, Vm};
    use std::path::Path;

    // The field `name` of the record `value`, an Int.
    fn int(value: &Value, name: &str) -> Result<i64, VmError> {
        match value {
            Value::Record(record) => match record.get(name) {
                Some(Value::Int(n)) => Ok(*n),
                _ => Err(VmError::new(format!("no Int field `{name}`"))),
            },
            _ => Err(VmError::new("expected a record".into())),
        }
    }

    let geo = HostModule::new("geo")
        .declare_type("type Point { x: Int, y: Int }")
        .declare_type("type Shape { Circle(Int), Rect(Point, Point) }")
        .function("fn origin() -> Point", |call: &HostCall| {
            call.record("Point", [("x", Value::Int(0)), ("y", Value::Int(0))])
        })
        .function("fn norm1(p: Point) -> Int", |call: &HostCall| {
            let p = &call.args[0];
            Ok(Value::Int(int(p, "x")?.abs() + int(p, "y")?.abs()))
        })
        .function("fn circle(r: Int) -> Shape", |call: &HostCall| {
            call.variant("Shape", "Circle", vec![call.args[0].clone()])
        });

    let source = r#"import geo

fn area(s: geo.Shape) -> Int {
  match s {
    geo.Circle(r) -> 3 * r * r
    geo.Rect(a, b) -> (b.x - a.x) * (b.y - a.y)
  }
}

fn show() {
  let p = geo.Point { x: 3, y: -4 }
  println(geo.norm1(p)) -- 7
  println(geo.origin() == p.{ x: 0, y: 0 }) -- true
  println(area(geo.Rect(geo.origin(), p.{ y: 4 }))) -- 12
  println(area(geo.circle(2))) -- 12
}

fn main() { show() }
"#;
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![geo],
        builtins: Builtins::All,
    });
    let file = session.set_overlay(Path::new("main.silt"), source.to_string());
    let program = session.compile(file, Entry::Main).expect("compiles");
    let out = Buffer::new();
    Vm::new(HostIo::buffer(&out)).run_program(&program).unwrap();
    assert_eq!(out.contents(), "7\ntrue\n12\n12\n");
}

/// A field of a generic host type shows the type its parameter has at
/// a call: a result of that variable is checked against it.
#[test]
fn a_field_of_a_generic_host_type_shows_a_variable() {
    let host = HostModule::new("g")
        .declare_type("type Box(a) { tag: String, value: a }")
        .declare_type("type Either(a, b) { Left(a), Right(b) }")
        .function("fn unbox(b: Box(a)) -> a", |_: &HostCall| {
            Ok(Value::string("s"))
        })
        .function("fn right(e: Either(a, b)) -> Option(b)", |_: &HostCall| {
            Ok(Value::variant(silt::typeinfo::bv::SOME, [Value::Int(1)]))
        })
        .function("fn same(b: Box(a)) -> a", |call: &HostCall| {
            match &call.args[0] {
                Value::Record(r) => Ok(r.get("value").cloned().unwrap_or(Value::Unit)),
                _ => Err(VmError::new("expected a record".into())),
            }
        });
    for (call, expected) in [
        (
            "g.unbox(g.Box { tag: \"t\", value: 1 })",
            "g.unbox: its signature returns a, here Int (from its argument `b`), but it \
             returned String",
        ),
        (
            "g.right(g.Right(\"s\"))",
            "g.right: its signature returns Option(b), where b is String (from its argument \
             `e`), but it returned",
        ),
    ] {
        let source = format!("import g\nimport host\nfn main() {{ host.give({call}) }}");
        let err = run(&source, host.clone()).expect_err(call);
        assert!(err.contains(expected), "{call}: {err}");
    }
    let source =
        "import g\nimport host\nfn main() { host.give(g.same(g.Box { tag: \"t\", value: [2] })) }";
    assert_eq!(run(source, host), Ok(Value::list(vec![Value::Int(2)])));
}

/// A host record is a record like any other to the builtins: json
/// writes and reads it by its type.
#[test]
fn json_writes_and_reads_a_host_record() {
    let source = r#"
import geo
import host
import json
fn main() {
  let text = json.stringify(geo.Point { x: 1, y: 2 })
  host.give((text, "{json.parse(text, geo.Point)}"))
}
"#;
    assert_eq!(
        run(source, geo()),
        Ok(Value::tuple(vec![
            Value::string(r#"{"x":1,"y":2}"#),
            Value::string("Ok(Point {x: 1, y: 2})"),
        ]))
    );
}

/// A host type has Display by its structure, as every record and enum
/// has: a `main` that returns one as its error is shown so. (A program
/// cannot give a host type an impl of its own: an impl lives in the
/// module of its type or of its trait.)
#[test]
fn a_host_type_is_shown_by_its_structure() {
    let source = "import geo\nfn main() -> Result((), geo.Shape) {\n  println(\"{geo.origin()}\")\n  Err(geo.circle(2))\n}\n";
    let err = run(source, geo()).expect_err("main returns Err");
    assert!(err.contains("Circle(2)"), "{err}");
    let source = "import geo\ntrait Display for geo.Point {\n  fn display(self) -> String { \"p\" }\n}\nfn main() { }\n";
    let found = errors(source, geo());
    assert_eq!(found.len(), 1, "{found:?}");
}

/// A host type named like a type of the prelude is refused, so a
/// signature's `Option(Int)` is the prelude's, and its result is checked
/// as one (review of #336: a host `Option` was typed as the host's and
/// checked as the prelude's, and a prelude `Some` reached the program
/// as a host record).
#[test]
fn a_prelude_name_is_no_host_type_and_the_check_reads_the_prelude_type() {
    let host = HostModule::new("h")
        .declare_type("type Option(a) { x: a }")
        .fn0("fn f() -> Option(Int)", || Some(5_i64));
    let found = errors("import h\nfn main() { let o = h.f()\n println(o) }", host);
    assert!(
        found
            .iter()
            .any(|e| e.message.contains("has the name of a type of the prelude")),
        "{found:?}"
    );
    let host = HostModule::new("h").fn0("fn f() -> Option(Int)", || Some(5_i64));
    let source = "import h\nimport host\nfn main() { host.give(h.f()) }";
    assert_eq!(
        run(source, host),
        Ok(Value::variant(silt::typeinfo::bv::SOME, [Value::Int(5)]))
    );
}

/// The fields of a builtin record or enum a host function builds are
/// checked against the type's declaration, as a host type's are, and so
/// are those of a host type met inside itself at other type arguments.
#[test]
fn the_fields_of_every_record_and_enum_of_a_result_are_checked() {
    let host = HostModule::new("h")
        .declare_type("type Stamp { at: time.Date }")
        .declare_type("type Poly(a) { v: a, next: Option(Poly(List(a))) }")
        .function("fn date() -> time.Date", |call: &HostCall| {
            call.record(
                "time.Date",
                [
                    ("year", Value::string("y")),
                    ("month", Value::Int(1)),
                    ("day", Value::Int(1)),
                ],
            )
        })
        .function("fn stamp() -> Stamp", |call: &HostCall| {
            let at = call.record(
                "time.Date",
                [
                    ("year", Value::Int(1)),
                    ("month", Value::string("m")),
                    ("day", Value::Int(1)),
                ],
            )?;
            call.record("Stamp", [("at", at)])
        })
        .function("fn json() -> json.Value", |call: &HostCall| {
            call.variant("json.Value", "Int", vec![Value::string("7")])
        })
        .function("fn bad_poly() -> Poly(Int)", |call: &HostCall| {
            let none = Value::variant(silt::typeinfo::bv::NONE, []);
            let inner = call.record(
                "Poly",
                [("v", Value::list(vec![Value::string("s")])), ("next", none)],
            )?;
            let next = Value::variant(silt::typeinfo::bv::SOME, [inner]);
            call.record("Poly", [("v", Value::Int(1)), ("next", next)])
        })
        .function("fn good_poly() -> Poly(Int)", |call: &HostCall| {
            let none = Value::variant(silt::typeinfo::bv::NONE, []);
            let inner = call.record(
                "Poly",
                [("v", Value::list(vec![Value::Int(2)])), ("next", none)],
            )?;
            let next = Value::variant(silt::typeinfo::bv::SOME, [inner]);
            call.record("Poly", [("v", Value::Int(1)), ("next", next)])
        });
    for (call, expected) in [
        (
            "h.date()",
            "h.date: its signature returns time.Date, but it returned Record",
        ),
        (
            "h.stamp()",
            "h.stamp: its signature returns Stamp, but it returned Record",
        ),
        (
            "h.json()",
            "h.json: its signature returns json.Value, but it returned",
        ),
        (
            "h.bad_poly()",
            "h.bad_poly: its signature returns Poly(Int), but it returned Record",
        ),
    ] {
        let source = format!("import h\nimport host\nfn main() {{ host.give({call}) }}");
        let err = run(&source, host.clone()).expect_err(call);
        assert!(err.contains(expected), "{call}: {err}");
    }
    let source = "import h\nimport host\nfn main() { host.give(h.good_poly().v) }";
    assert_eq!(run(source, host), Ok(Value::Int(1)));
}

/// A deeply nested generic host type in a signature costs no more than
/// its size: each type's declared fields are kept once. (They were
/// copied into every use of a parameter: 3^depth shapes, which at this
/// depth would not end.)
#[test]
fn a_deeply_nested_generic_host_type_is_registered_at_once() {
    let mut ty = "Int".to_string();
    for _ in 0..24 {
        ty = format!("T3({ty})");
    }
    let host = HostModule::new("h")
        .declare_type("type T3(a) { x: a, y: a, z: a }")
        .function(format!("fn f(n: Int) -> Option({ty})"), |_: &HostCall| {
            Ok(Value::variant(silt::typeinfo::bv::NONE, []))
        });
    let source = "import h\nimport host\nfn main() { host.give(h.f(1)) }";
    assert_eq!(
        run(source, host),
        Ok(Value::variant(silt::typeinfo::bv::NONE, []))
    );
}

/// A host type that shares its name with another type of the program
/// prints qualified by its module, as the program names it.
#[test]
fn a_host_type_prints_by_its_module() {
    let source = "import geo\nimport host\ntype Point { x: Int, y: Int }\nfn main() {\n  host.give(\"{geo.Point { x: 1, y: 2 }} {Point { x: 3, y: 4 }}\")\n}";
    assert_eq!(
        run(source, geo()),
        Ok(Value::string(
            "geo.Point {x: 1, y: 2} main.Point {x: 3, y: 4}"
        ))
    );
}
