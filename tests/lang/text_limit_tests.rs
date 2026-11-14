//! A text a builtin makes holds no more bytes than a string may
//! (10,000,000; 1,000,000 in WebAssembly): `io.inspect` of a value whose
//! text would be longer, and the builtins whose text is longer than what
//! they are given, are a runtime error of the builtin, and the long text
//! is never made (review of #336, TL-13). A print writes a text of any
//! length, in pieces; a panic's message and the text of `main`'s `Err`
//! are cut, with how much more there was.

use silt::session::testing::run_str;
use std::process::Command;

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

/// A panic gives its own message, however long: the first 100,000 bytes
/// of it, and how many more there were.
#[test]
fn a_long_panic_message_is_cut_and_says_how_much_more() {
    let err = error_of("panic([s, s, s])");
    // `["abab..", "abab..", "abab.."]`: three quoted strings, two `, `, `[` and `]`.
    let more = 3 * (4_000_000 + 2) + 2 * 2 + 2 - 100_000;
    assert!(
        err.contains(&format!("… ({more} more bytes)")),
        "{}",
        &err[err.len() - 200..]
    );
    assert!(err.len() < 101_000, "{} bytes", err.len());
}

/// The `Err` of `main` whose `Display` fails is shown by its structure,
/// cut as a panic's message is.
#[test]
fn the_text_of_mains_err_is_cut_when_its_display_fails() {
    let source = "import string\ntype Big { text: String }\n\
                  trait Display for Big { fn display(self) -> String { panic(\"no display\") } }\n\
                  fn main() -> Result((), Big) {\n  Err(Big { text: string.repeat(\"ab\", 2_000_000) })\n}\n";
    let err = run_str(source).expect_err("main returns Err");
    assert!(
        err.contains("more bytes)"),
        "{}",
        &err[err.len().saturating_sub(200)..]
    );
    assert!(err.len() < 101_000, "{} bytes", err.len());
}

/// The command prints a value of any length, all of it (the review's
/// cli/big.silt: 2,000,000 strings, 22 MB of text), and goes on.
#[test]
fn the_command_prints_a_value_longer_than_a_string_may_be() {
    let dir = std::env::temp_dir().join(format!("silt_big_print_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(
        &file,
        "import list\nimport string\nfn main() {\n  \
         let xs = string.split(string.repeat(\"abcdefgh,\", 1100000), \",\")\n  \
         println(list.length(xs))\n  println(xs)\n  println(\"after\")\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.split('\n').collect();
    assert_eq!(lines[0], "1100001");
    // `["abcdefgh", ...]`: each of 1,100,000 strings with its quotes and
    // `, `, the last one empty.
    let shown = lines[1];
    assert!(shown.len() > 10_000_000, "{} bytes", shown.len());
    assert_eq!(shown.len(), 2 + 1_100_000 * 12 + 2);
    assert_eq!(lines[2], "after");
}
