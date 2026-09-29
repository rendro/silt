//! Round 81 DX-LATENT-1 parity lock for `src/lsp/completion.rs`.
//!
//! The LSP's `auto_derived_methods_for` hardcodes which of the built-in
//! trait methods (`equal`, `compare`, `hash`, `display`) each canonical
//! type head offers after `v.`; the typechecker decides separately which
//! of them a program may call (Tuple, Map and Set get no `compare`). If
//! the two lists drift, completion offers a method that fails to
//! typecheck, or hides one that works.
//!
//! The lock drives both sides: for every type head, dot-completion on a
//! value of that type must offer exactly the trait methods the
//! typechecker accepts on it.

use serde_json::Value;

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

use crate::support::LspClient;

/// Type heads the LSP hardcodes, each with an expression of that type.
const TYPE_HEADS: &[(&str, &str)] = &[
    ("Int", "1"),
    ("Float", "1.5"),
    ("Bool", "true"),
    ("String", "\"s\""),
    ("Unit", "()"),
    ("List", "[1, 2]"),
    ("Tuple", "(1, 2)"),
    ("Map", "#{\"a\": 1}"),
    ("Set", "#[1, 2]"),
];

/// The auto-derived trait methods and how to call each on `v`.
const TRAIT_METHODS: &[(&str, &str)] = &[
    ("equal", "v.equal(v)"),
    ("compare", "v.compare(v)"),
    ("hash", "v.hash()"),
    ("display", "v.display()"),
];

/// Whether the typechecker accepts `call` on `let v = <expr>`.
fn typechecks(expr: &str, call: &str) -> bool {
    let src =
        format!("fn main() {{\n  let v = {expr}\n  let r = {call}\n  println(\"{{r}}\")\n}}\n");
    let tokens = Lexer::new(&src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .iter()
        .all(|e| e.severity != Severity::Error)
}

fn completion_labels(resp: &Value) -> Vec<String> {
    let result = &resp["result"];
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array())
        .unwrap_or_else(|| panic!("completion result has no items: {resp}"));
    items
        .iter()
        .filter_map(|it| it["label"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn lsp_auto_derived_completions_match_typechecker() {
    let mut client = LspClient::spawn();
    let mut mismatches = Vec::new();
    for (i, (head, expr)) in TYPE_HEADS.iter().enumerate() {
        let uri = format!("file:///tmp/silt_lsp_r81_parity_{i}.silt");
        let source = format!("fn main() {{\n  let v = {expr}\n  v.\n}}\n");
        client.did_open_and_wait(&uri, &source);
        let labels = completion_labels(&client.completion(&uri, 2, 4));
        for (method, call) in TRAIT_METHODS {
            let offered = labels.iter().any(|l| l == method);
            let accepted = typechecks(expr, call);
            if offered != accepted {
                mismatches.push(format!(
                    "{head}.{method}: completion offers it = {offered}, typechecker accepts it = {accepted}"
                ));
            }
        }
    }
    client.shutdown();
    assert!(
        mismatches.is_empty(),
        "LSP auto-derived completions (src/lsp/completion.rs::auto_derived_methods_for) \
         disagree with the typechecker's auto-derived trait impls:\n{}",
        mismatches.join("\n")
    );
}
