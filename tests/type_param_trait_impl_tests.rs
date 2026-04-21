//! `type a` in trait-method and trait-impl signatures, and regression
//! tests for edge cases discovered during implementation.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(src: &str) {
    let errs = type_errors(src);
    assert!(errs.is_empty(), "expected no errors, got:\n{}", errs.join("\n"));
}

#[test]
fn trait_method_decl_with_type_param() {
    // Trait declaration with `type a` in a method signature must be
    // accepted. The trait-decl registration path handles ParamKind::Type.
    let src = r#"
        trait Decodable {
            fn decode(self, type a) -> Result(a, String)
        }
    "#;
    assert_ok(src);
}

#[test]
fn trait_impl_with_type_param_registers_and_dispatches() {
    // A trait impl with a `type a` method: the signature must store the
    // `type a` parameter as `TypeOf(a)` so call-site dispatch can accept
    // a type descriptor argument.
    let src = r#"
        import json

        type Todo { id: Int, title: String }

        trait Decodable {
            fn decode(self, type a) -> Result(a, String)
        }

        trait Decodable for String {
            fn decode(self, type a) -> Result(a, String) {
                json.parse(self, a)
            }
        }

        fn use_it(body: String) {
            let _ = body.decode(Todo)
        }
    "#;
    assert_ok(src);
}

#[test]
fn trait_impl_type_param_rejects_bad_descriptor() {
    // Passing a String where a type descriptor is expected must fail.
    let src = r#"
        trait Tag {
            fn tag(self, type a) -> String
        }

        trait Tag for Int {
            fn tag(self, type a) -> String { "ok" }
        }

        fn use_it() {
            let _ = 42.tag("not a type")
        }
    "#;
    let errs = type_errors(src);
    assert!(
        !errs.is_empty(),
        "passing a String where a type descriptor is expected should fail"
    );
}

#[test]
fn method_call_arg_offset_not_regressed() {
    // Regression guard: before the method-offset fix, method-call args
    // were unified against `params[0..]` (including the self slot). For
    // methods whose self type happened to match the first explicit
    // parameter's type this was silently accepted; with `type a` the
    // types differ and the latent bug surfaced. Lock the offset in.
    let src = r#"
        trait Cast {
            fn cast(self, type a) -> a
        }

        trait Cast for Int {
            fn cast(self, type a) -> a {
                unreachable()
            }
        }

        type Box(T) { value: T }

        fn use_it() {
            -- Passing a record type here exercises: receiver=Int flows
            -- into the self slot via dispatch_method_entry; the explicit
            -- arg Box unifies against the `type a` slot (params[1]).
            let _ = 42.cast(Box)
        }
    "#;
    // The call should typecheck (cast's return = a is unbound at this
    // site but anchored by the type param, so it's fine).
    let errs = type_errors(src);
    assert!(
        !errs.iter().any(|m| m.contains("expected Int, got")),
        "method-call arg offset regressed; errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn enum_variant_shares_enum_name_no_double_register() {
    // `type Box(T) { Box(T) }` — variant `Box` shares the enum name.
    // Registering a TypeOf scheme for `Box` would clash with the variant
    // constructor. The compiler must skip the TypeOf registration but
    // still accept `Box(42)` as a constructor call.
    let src = r#"
        type Box(T) { Box(T) }

        fn use_it() {
            let _ = Box(42)
        }
    "#;
    assert_ok(src);
}

#[test]
fn enum_variant_shares_enum_name_cannot_be_type_arg() {
    // Continuation of the above: because we skip TypeOf registration
    // when the variant shares the enum's name, passing `Box` to a
    // `type a` parameter should NOT typecheck — the only scheme bound
    // under `Box` is the variant constructor.
    let src = r#"
        type Box(T) { Box(T) }

        fn tag(type a) -> String { "ok" }

        fn use_it() {
            let _ = tag(Box)
        }
    "#;
    let errs = type_errors(src);
    assert!(
        !errs.is_empty(),
        "passing a variant-shadowed enum as a type arg should fail"
    );
}

#[test]
fn enum_without_collision_is_usable_as_type_arg() {
    // Positive companion to the collision test: a plain enum with no
    // name collision registers its TypeOf scheme and can flow through
    // a `type a` parameter.
    let src = r#"
        type Color { Red, Green, Blue }

        fn tag(type a) -> String { "ok" }

        fn use_it() {
            let _ = tag(Color)
        }
    "#;
    assert_ok(src);
}
