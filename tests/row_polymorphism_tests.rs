//! Type-check tests for row polymorphism: anonymous structural records
//! `{name: String, age: Int}` and row-polymorphic types
//! `{name: String, ...r}`.
//!
//! Compiles each program through the parser + typechecker and asserts
//! pass/fail behaviour. Coexists with existing nominal records.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;

fn typecheck(source: &str) -> (bool, Vec<String>) {
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => return (false, vec![format!("lex error: {}", e.message)]),
    };
    let mut parser = Parser::new(tokens);
    let mut program = match parser.parse_program() {
        Ok(p) => p,
        Err(e) => return (false, vec![format!("parse error: {}", e.message)]),
    };
    let errors = typechecker::check(&mut program);
    let messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    let pass = errors
        .iter()
        .all(|e| matches!(e.severity, silt::types::Severity::Warning));
    (pass, messages)
}

fn assert_passes(source: &str) {
    let (pass, msgs) = typecheck(source);
    assert!(
        pass,
        "expected typecheck to pass, got errors:\n  {}",
        msgs.join("\n  ")
    );
}

fn assert_fails(source: &str) {
    let (pass, msgs) = typecheck(source);
    assert!(
        !pass,
        "expected typecheck to fail; got pass with messages:\n  {}",
        msgs.join("\n  ")
    );
}

#[test]
fn closed_anon_record_literal_and_field_access() {
    assert_passes(
        r#"
fn main() = {
    let p = {name: "A", age: 30}
    let n = p.name
    let _ = n
}
"#,
    );
}

#[test]
fn closed_x_closed_unify_ok() {
    assert_passes(
        r#"
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x
fn main() = {
    let _ = ident({a: 1, b: "hi"})
}
"#,
    );
}

#[test]
fn closed_x_closed_unify_extra_field_rejected() {
    assert_fails(
        r#"
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x
fn main() = {
    let _ = ident({a: 1, b: "hi", c: 9})
}
"#,
    );
}

#[test]
fn open_accepts_subset() {
    assert_passes(
        r#"
fn first_name(p: {name: String, ...r}) -> String = p.name
fn main() = {
    let n = first_name({name: "A", age: 30})
    let _ = n
}
"#,
    );
}

#[test]
fn nominal_flows_into_open_row() {
    assert_passes(
        r#"
type Person { name: String, age: Int }
fn name(p: {name: String, ...r}) -> String = p.name
fn main() = {
    let _ = name(Person { name: "A", age: 30 })
}
"#,
    );
}

#[test]
fn extend_operator_basic() {
    assert_passes(
        r#"
fn main() = {
    let p = {name: "A"}
    let q = {...p, age: 30}
    let _ = q.name
    let _ = q.age
}
"#,
    );
}

#[test]
fn extend_rejects_existing_field() {
    assert_fails(
        r#"
fn main() = {
    let p = {name: "A"}
    let q = {...p, name: "B"}
    let _ = q
}
"#,
    );
}

#[test]
fn pattern_destructure_with_rest_anon_source() {
    assert_passes(
        r#"
fn main() = {
    let p = {name: "A", age: 30}
    let n = match p {
        {name: nm, ...rest} -> nm
    }
    let _ = n
}
"#,
    );
}

#[test]
fn closed_record_missing_field_rejected() {
    assert_fails(
        r#"
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x
fn main() = {
    let _ = ident({a: 1})
}
"#,
    );
}

#[test]
fn anon_record_field_access_unknown_field_rejected() {
    assert_fails(
        r#"
fn main() = {
    let p = {name: "A"}
    let _ = p.age
}
"#,
    );
}

#[test]
fn field_access_generates_row_constraint() {
    // `fn show_name(p) = p.name` should infer `p` as `{name: a, ...r}`.
    // Calling it with a record carrying `name` (and other fields) must
    // typecheck.
    assert_passes(
        r#"
fn show_name(p) = p.name
fn main() = {
    let _ = show_name({name: "A", age: 30})
}
"#,
    );
}

#[test]
fn multiple_field_access_on_open() {
    // `fn full(p) = p.first ++ " " ++ p.last` — silt uses `+` for
    // string concat. Both `first` and `last` should be threaded into
    // the inferred row.
    assert_passes(
        r#"
fn full(p) = p.first + " " + p.last
fn main() = {
    let _ = full({first: "A", last: "B", age: 30})
}
"#,
    );
}

#[test]
fn open_row_threading_through_fn_call() {
    // `fn id_name(p: {name: String, ...r}) -> {name: String, ...r} = p`
    // — the row variable is threaded through the return type.
    assert_passes(
        r#"
fn id_name(p: {name: String, ...r}) -> {name: String, ...r} = p
fn main() = {
    let q = id_name({name: "A", age: 30})
    let _ = q.age
}
"#,
    );
}

#[test]
fn pattern_destructure_with_rest_nominal_source() {
    // The pattern is anon-record-shape; the scrutinee is a nominal
    // record. Nominal-to-row widening should let this work.
    assert_passes(
        r#"
type Person { name: String, age: Int }
fn main() = {
    let p = Person { name: "A", age: 30 }
    let n = match p {
        {name: nm, ...rest} -> nm
    }
    let _ = n
}
"#,
    );
}

#[test]
fn anon_record_round_trip_format() {
    // Parser → formatter → parser idempotency for anon record literal,
    // anon record type, and pattern with rest.
    use silt::lexer::Lexer;
    use silt::parser::Parser;

    let source = r#"fn id(p: { name: String, ...r }) -> String = p.name

fn main() {
    let q = { name: "A", age: 30 }
    match q {
        { name: n, ...rest } -> n
    }
}
"#;
    let mut lexer = Lexer::new(source);
    let _tokens = lexer.tokenize().expect("lex");
    let formatted = silt::formatter::format(source).expect("format");
    // Re-parse the formatted output — must succeed.
    let mut lexer2 = Lexer::new(&formatted);
    let tokens2 = lexer2.tokenize().expect("lex2");
    let mut parser2 = Parser::new(tokens2);
    let _ = parser2.parse_program().expect("parse2 of formatted output");
}
