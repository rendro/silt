//! Hardening tests that need Rust: formatter idempotency and roundtrip
//! over every example file, and `Value` float ordering. The builtin edge
//! cases, concurrent panic recovery, IO error paths and overflow locks
//! that used to live here are golden cases named `hardening__*` under
//! `tests/golden/lang/`.

use silt::formatter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;

// ── Formatter: idempotency over example files ───────────────────────

#[test]
fn test_formatter_idempotent_on_all_examples() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&examples_dir).expect("read examples dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("silt") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path).expect("read file");

        let first = match formatter::format(&source) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: format() failed: {e}"));
                continue;
            }
        };

        let second = match formatter::format(&first) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: second format() failed: {e}"));
                continue;
            }
        };

        if first != second {
            // Find first differing line for a useful error message
            let first_lines: Vec<&str> = first.lines().collect();
            let second_lines: Vec<&str> = second.lines().collect();
            let diff_line = first_lines
                .iter()
                .zip(second_lines.iter())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {}: {a:?} vs {b:?}", i + 1))
                .unwrap_or_else(|| {
                    format!("length {} vs {}", first_lines.len(), second_lines.len())
                });
            failures.push(format!("{name}: not idempotent ({diff_line})"));
        }
    }

    assert!(
        failures.is_empty(),
        "Formatter idempotency failures:\n  {}",
        failures.join("\n  ")
    );
}

// ── Formatter: roundtrip (formatted code still parses) ──────────────

#[test]
fn test_formatter_roundtrip_parses_on_all_examples() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&examples_dir).expect("read examples dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("silt") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path).expect("read file");

        let formatted = match formatter::format(&source) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: format() failed: {e}"));
                continue;
            }
        };

        // Verify the formatted code still lexes
        let tokens = match Lexer::new(silt::source::FileId::default(), &formatted).tokenize() {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!(
                    "{name}: formatted code fails to lex: {}",
                    e.message
                ));
                continue;
            }
        };

        // Verify it still parses
        if let Err(e) = Parser::new(tokens, &formatted).parse_program() {
            failures.push(format!(
                "{name}: formatted code fails to parse: {}",
                e.message
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Formatter roundtrip failures:\n  {}",
        failures.join("\n  ")
    );
}

// ── Value ordering: Float Eq/Ord consistency ───────────────────────

#[test]
fn test_float_ord_consistency() {
    // Verify that Value::Float ordering is consistent with equality.
    // Two equal floats must compare as Equal.
    let a = Value::Float(1.5);
    let b = Value::Float(1.5);
    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);

    // Different floats should order correctly.
    let c = Value::Float(2.0);
    assert_eq!(a.cmp(&c), std::cmp::Ordering::Less);
    assert_eq!(c.cmp(&a), std::cmp::Ordering::Greater);
}
