//! Property-based tests for the silt language.
//!
//! Uses `proptest` to generate arbitrary inputs and verify invariants:
//! - Lexer never panics on arbitrary byte strings
//! - Parser never panics on arbitrary strings
//! - Formatter is idempotent: format(format(s)) == format(s)
//! - Parse-format-parse roundtrip produces structurally equivalent ASTs

use proptest::prelude::*;
use silt::formatter;
use silt::lexer::Lexer;
use silt::parser::Parser;

// ── Generators ────────────────────────────────────────────────────────

/// Generate an arbitrary identifier.
fn arb_ident() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9_]{0,7}")
        .unwrap()
        .prop_filter("avoid keywords", |s| {
            !matches!(
                s.as_str(),
                "fn" | "let"
                    | "type"
                    | "trait"
                    | "match"
                    | "when"
                    | "return"
                    | "pub"
                    | "import"
                    | "as"
                    | "else"
                    | "where"
                    | "loop"
                    | "true"
                    | "false"
                    | "mod"
            )
        })
}

/// Generate a simple literal expression.
fn arb_literal() -> impl Strategy<Value = String> {
    prop_oneof![
        // Integers
        (-1000i64..1000).prop_map(|n| n.to_string()),
        // Floats
        (-100.0f64..100.0)
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(|f| format!("{f:.1}")),
        // Booleans
        prop::bool::ANY.prop_map(|b| if b {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        // Simple strings (no interpolation to avoid nesting complexity)
        prop::string::string_regex("[a-zA-Z0-9 ]{0,20}")
            .unwrap()
            .prop_map(|s| format!("\"{s}\"")),
    ]
}

/// Generate a simple expression (no nesting).
fn arb_simple_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_literal(),
        arb_ident(),
        // Tuple
        prop::collection::vec(arb_literal(), 2..=4)
            .prop_map(|elems| format!("({})", elems.join(", "))),
        // List
        prop::collection::vec(arb_literal(), 0..=4)
            .prop_map(|elems| format!("[{}]", elems.join(", "))),
    ]
}

/// Generate a binary expression.
fn arb_binop_expr() -> impl Strategy<Value = String> {
    let op = prop_oneof![
        Just("+"),
        Just("-"),
        Just("*"),
        Just("/"),
        Just("%"),
        Just("=="),
        Just("!="),
        Just("<"),
        Just(">"),
        Just("<="),
        Just(">="),
        Just("&&"),
        Just("||"),
    ];
    (arb_literal(), op, arb_literal()).prop_map(|(l, op, r)| format!("{l} {op} {r}"))
}

/// Generate a let binding.
fn arb_let() -> impl Strategy<Value = String> {
    (arb_ident(), arb_simple_expr()).prop_map(|(name, expr)| format!("let {name} = {expr}"))
}

/// Generate a simple function definition.
fn arb_fn() -> impl Strategy<Value = String> {
    (
        arb_ident(),
        prop::collection::vec(arb_ident(), 0..=3),
        arb_simple_expr(),
    )
        .prop_map(|(name, params, body)| {
            let params = params.join(", ");
            format!("fn {name}({params}) {{ {body} }}")
        })
}

/// Generate a match expression.
fn arb_match() -> impl Strategy<Value = String> {
    (arb_ident(), prop::collection::vec(arb_literal(), 1..=3)).prop_map(|(scrutinee, literals)| {
        let mut arms: Vec<String> = literals
            .iter()
            .enumerate()
            .map(|(i, lit)| format!("    {lit} -> {i}"))
            .collect();
        arms.push("    _ -> -1".to_string());
        format!("match {scrutinee} {{\n{}\n  }}", arms.join("\n"))
    })
}

/// Generate a valid silt expression for formatter testing.
fn arb_formattable_program() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            // Weighted toward constructs the formatter cares about
            3 => arb_fn(),
            3 => arb_let(),
            2 => arb_match().prop_map(|m| format!("let _ = {m}")),
            1 => arb_binop_expr(),
            1 => arb_simple_expr(),
        ],
        1..=4,
    )
    .prop_map(|decls| decls.join("\n\n"))
}

// ── Property tests: no panics ─────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// The lexer must never panic on arbitrary byte strings, even invalid UTF-8
    /// re-encoded as strings.
    #[test]
    fn lexer_never_panics(input in "\\PC{0,200}") {
        // We only care that it doesn't panic — errors are fine.
        let _ = Lexer::new(&input).tokenize();
    }

    /// The parser must never panic on arbitrary strings.
    #[test]
    fn parser_never_panics(input in "\\PC{0,200}") {
        if let Ok(tokens) = Lexer::new(&input).tokenize() {
            let mut parser = Parser::new(tokens);
            let _ = parser.parse_program();
        }
    }

    /// The parser's error-recovering mode must never panic.
    #[test]
    fn parser_recovery_never_panics(input in "\\PC{0,200}") {
        if let Ok(tokens) = Lexer::new(&input).tokenize() {
            let mut parser = Parser::new(tokens);
            let _ = parser.parse_program_recovering();
        }
    }

    /// The formatter must never panic, even on garbage input.
    #[test]
    fn formatter_never_panics(input in "\\PC{0,200}") {
        let _ = formatter::format(&input);
    }
}

// ── Property tests: formatter idempotency ─────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// For any generated valid program, formatting is idempotent:
    /// format(format(s)) == format(s).
    #[test]
    fn formatter_idempotent_on_generated_code(source in arb_formattable_program()) {
        // Only test programs that actually parse and format successfully.
        if let Ok(first) = formatter::format(&source)
            && let Ok(second) = formatter::format(&first) {
                prop_assert_eq!(first, second);
        }
    }

    /// For any generated valid program, formatting preserves parseability:
    /// if s parses, then format(s) also parses.
    #[test]
    fn formatter_preserves_parseability(source in arb_formattable_program()) {
        let tokens = Lexer::new(&source).tokenize();
        if tokens.is_err() { return Ok(()); }
        let tokens = tokens.unwrap();
        let result = Parser::new(tokens).parse_program();
        if result.is_err() { return Ok(()); }

        // Source parses — formatted version must also parse.
        if let Ok(formatted) = formatter::format(&source) {
            let tokens2 = Lexer::new(&formatted).tokenize()
                .map_err(|e| TestCaseError::Fail(format!("Formatted code fails to lex: {e}").into()))?;
            Parser::new(tokens2).parse_program()
                .map_err(|e| TestCaseError::Fail(format!("Formatted code fails to parse: {e}").into()))?;
        }
    }
}

// ── Property tests: typechecker & compiler never panic ───────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// The typechecker must never panic on arbitrary input that lexes and
    /// parses without errors. Type errors are fine, panics are not.
    #[test]
    fn typechecker_never_panics(input in "\\PC{0,200}") {
        let tokens = match Lexer::new(&input).tokenize() {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let mut program = match Parser::new(tokens).parse_program() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        // Must not panic — type errors are acceptable.
        let _ = silt::typechecker::check(&mut program);
    }

    /// The compiler must never panic on arbitrary input that lexes, parses,
    /// and typechecks without errors. Compile errors are fine, panics are not.
    #[test]
    fn compiler_never_panics(input in "\\PC{0,200}") {
        let tokens = match Lexer::new(&input).tokenize() {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let mut program = match Parser::new(tokens).parse_program() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        let type_errors = silt::typechecker::check(&mut program);
        if !type_errors.is_empty() {
            return Ok(());
        }
        // Must not panic — compile errors are acceptable.
        let mut compiler = silt::compiler::Compiler::new();
        let _ = compiler.compile_program(&program);
    }
}

// ── Property tests: expression evaluation ─────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Arithmetic expressions with integer literals must compute the same
    /// value as Rust's reference semantics when the operation does not
    /// overflow. When Rust's checked arithmetic overflows, the Silt VM is
    /// allowed (and expected) to produce a runtime error — it must not
    /// panic and must not silently return a garbage value.
    #[test]
    fn arithmetic_matches_reference_semantics(
        a in -1000i64..1000,
        b in -1000i64..1000,
        op in prop_oneof![Just("+"), Just("-"), Just("*")],
    ) {
        let source = format!("{a} {op} {b}");
        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let mut program = match Parser::new(tokens).parse_program() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        let _ = silt::typechecker::check(&mut program);
        let mut compiler = silt::compiler::Compiler::new();
        let functions = match compiler.compile_program(&program) {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        let script = match functions.into_iter().next() {
            Some(s) => s,
            None => return Ok(()),
        };
        let mut vm = silt::vm::Vm::new();
        let vm_result = vm.run(std::sync::Arc::new(script));

        // Reference semantics via Rust's checked arithmetic.
        let expected = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            _ => unreachable!(),
        };

        match expected {
            Some(reference) => {
                // No overflow: Silt must succeed and return exactly this value.
                match vm_result {
                    Ok(silt::value::Value::Int(got)) => {
                        prop_assert_eq!(
                            got,
                            reference,
                            "silt {} {} {} returned {} but reference is {}",
                            a,
                            op,
                            b,
                            got,
                            reference
                        );
                    }
                    Ok(other) => {
                        return Err(TestCaseError::Fail(
                            format!(
                                "silt {a} {op} {b} returned non-int value {other:?}, expected Int({reference})"
                            )
                            .into(),
                        ));
                    }
                    Err(e) => {
                        return Err(TestCaseError::Fail(
                            format!(
                                "silt {a} {op} {b} errored ({e:?}) but reference value is {reference}"
                            )
                            .into(),
                        ));
                    }
                }
            }
            None => {
                // Overflow: Silt must produce an error, not a garbage value
                // and not a panic. (`vm.run` returning is sufficient for
                // "did not panic" — this branch just asserts we did not
                // silently succeed with a wrong value.)
                if let Ok(silt::value::Value::Int(got)) = vm_result {
                    return Err(TestCaseError::Fail(
                        format!(
                            "silt {a} {op} {b} silently returned Int({got}) but Rust overflowed"
                        )
                        .into(),
                    ));
                }
            }
        }
    }
}
