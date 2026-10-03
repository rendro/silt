//! Row polymorphism: the formatter round trip of anonymous record
//! literals, types and rest patterns (uses the formatter API). The
//! typecheck pass/fail tests of this file are golden cases under
//! `tests/golden/lang/records/row_polymorphism__*`.

#[test]
fn anon_record_round_trip_format() {
    // Parser → formatter → parser idempotency for anon record literal,
    // anon record type, and pattern with rest.
    use silt::lexer::Lexer;
    use silt::parser::Parser;

    let source = r#"fn id(p: { name: String, ...r }) -> String { p.name }

fn main() {
    let q = { name: "A", age: 30 }
    match q {
        { name: n, ...rest } -> n
    }
}
"#;
    let mut lexer = Lexer::new(silt::source::FileId::default(), source);
    let _tokens = lexer.tokenize().expect("lex");
    let formatted = silt::formatter::format(source).expect("format");
    // Re-parse the formatted output — must succeed.
    let mut lexer2 = Lexer::new(silt::source::FileId::default(), &formatted);
    let tokens2 = lexer2.tokenize().expect("lex2");
    let mut parser2 = Parser::new(tokens2, &formatted);
    let _ = parser2.parse_program().expect("parse2 of formatted output");
}
