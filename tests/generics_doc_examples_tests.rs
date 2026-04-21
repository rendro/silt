//! Lock the user-facing examples in `docs/language/generics.md` behind
//! a test so doc edits don't silently regress.
//!
//! These tests mirror the key examples verbatim. Adding a new example to
//! the doc should come with a new test here; the inverse — a test here
//! without a matching doc example — is also a drift signal.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn check(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(name: &str, src: &str) {
    let errs = check(src);
    assert!(
        errs.is_empty(),
        "doc example `{name}` should typecheck; errors:\n{}",
        errs.join("\n")
    );
}

fn assert_rejected(name: &str, src: &str, needle: &str) {
    let errs = check(src);
    assert!(
        errs.iter().any(|m| m.contains(needle)),
        "doc example `{name}` should reject with {needle:?}; errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn identity() {
    assert_ok("identity", "fn identity(x: a) -> a { x }\n");
}

#[test]
fn swap() {
    assert_ok(
        "swap",
        r#"
        fn swap(pair: (a, b)) -> (b, a) {
            let (x, y) = pair
            (y, x)
        }
        "#,
    );
}

#[test]
fn generic_type_decls() {
    assert_ok(
        "option/result/pair",
        r#"
        type Option(a) { Some(a), None }
        type Result(a, e) { Ok(a), Err(e) }
        type Pair(a, b) { first: a, second: b }
        "#,
    );
}

#[test]
fn binding_rule_return_only_rejected() {
    assert_rejected(
        "return-only",
        "fn make() -> a { unreachable() }\n",
        "type variable 'a' in return type is not introduced",
    );
}

#[test]
fn where_clause_multi_bound() {
    assert_ok(
        "dedup",
        r#"
        fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash {
            xs
        }
        "#,
    );
}

#[test]
fn default_descriptor_dispatch_example() {
    assert_ok(
        "default via type descriptor",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn default(type a) -> a where a: Default {
            a.default()
        }

        fn use_it() {
            let _ = default(Int)
        }
        "#,
    );
}

#[test]
fn parse_descriptor_dispatch_example() {
    assert_ok(
        "parse via type descriptor",
        r#"
        import int

        trait Decode {
            fn decode(body: String) -> Result(Self, String)
        }

        trait Decode for Int {
            fn decode(body: String) -> Result(Int, String) {
                int.parse(body)
            }
        }

        fn parse(body: String, type a) -> Result(a, String) where a: Decode {
            a.decode(body)
        }

        fn use_it() {
            let _ = parse("42", Int)
        }
        "#,
    );
}

#[test]
fn concrete_type_path_example() {
    assert_ok(
        "Int.default()",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn use_it() {
            let _ = Int.default()
        }
        "#,
    );
}

#[test]
fn value_receiver_on_no_self_rejected() {
    assert_rejected(
        "5.default() rejected",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn use_it() {
            let _ = 5.default()
        }
        "#,
        "takes no `self`",
    );
}

#[test]
fn pipe_composition_with_type_arg() {
    assert_ok(
        "pipe",
        r#"
        import json

        type Todo { id: Int, title: String }

        fn use_it(body: String) {
            let _ = body |> json.parse(Todo)
        }
        "#,
    );
}

#[test]
fn group_by_with_trait_bound() {
    assert_ok(
        "group_by",
        r#"
        import list
        import map

        fn group_by(xs: List(a), key: Fn(a) -> k) -> Map(k, List(a))
            where k: Hash + Equal
        {
            #{}
        }
        "#,
    );
}

#[test]
fn user_defined_generic_container() {
    assert_ok(
        "Cache",
        r#"
        import map

        type Cache(k, v) {
            store: Map(k, v),
            capacity: Int,
        }

        fn get(c: Cache(k, v), key: k) -> Option(v) where k: Hash + Equal {
            map.get(c.store, key)
        }
        "#,
    );
}
