//! A text a builtin makes holds no more bytes than a string may
//! (10,000,000; 1,000,000 in WebAssembly): `println`, `print`, `panic`,
//! `io.eprint(ln)` and `io.inspect` of a value whose text would be
//! longer, and the builtins whose text is longer than what they are
//! given, are a runtime error of the builtin, and the long text is
//! never made (review of #336, TL-13).

use silt::session::testing::run_str;

/// The error of running `body` in a `main` that has `s`, a string of
/// 4,000,000 bytes, at hand.
fn error_of(body: &str) -> String {
    let source = format!(
        "import bytes\nimport encoding\nimport io\nimport string\n\
         fn main() {{\n  let s = string.repeat(\"ab\", 2_000_000)\n  {body}\n}}\n"
    );
    run_str(&source).expect_err(body)
}

#[test]
fn a_text_past_the_limit_is_an_error_of_what_makes_it() {
    for (body, name) in [
        ("println([s, s, s])", "println"),
        ("print([s, s, s])", "print"),
        ("io.eprintln([s, s, s])", "io.eprintln"),
        ("panic([s, s, s])", "panic"),
        ("let _ = io.inspect([s, s, s])", "io.inspect"),
        (
            "let _ = bytes.to_hex(bytes.from_string(\"{s}{s}\"))",
            "bytes.to_hex",
        ),
        (
            "let _ = bytes.to_base64(bytes.from_string(\"{s}{s}\"))",
            "bytes.to_base64",
        ),
        (
            "let _ = encoding.url_encode(string.repeat(\"/\", 4_000_000))",
            "encoding.url_encode",
        ),
        (
            "let _ = string.to_upper(string.repeat(\"\u{390}\", 2_000_000))",
            "string.to_upper",
        ),
        (
            "match bytes.from_hex(string.repeat(\"ff\", 4_000_000)) {\n    Ok(b) -> { let _ = bytes.to_string_lossy(b) }\n    Err(_) -> ()\n  }",
            "bytes.to_string_lossy",
        ),
    ] {
        let err = error_of(body);
        assert!(
            err.contains(&format!("{name}: result would exceed maximum string size")),
            "{body}: {err}"
        );
    }
}
