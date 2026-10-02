//! Round-101 GAP lock: binders introduced by every binding-capable
//! pattern form must reach REPL tab completion.
//!
//! Pre-round-101 the binder walkers in the LSP and the REPL drifted from
//! the typechecker's authoritative `collect_pattern_vars`: the AnonRecord
//! `...rest` binder, `Map` value binders, `Or` alternative binders and the
//! `List` rest sub-pattern were invisible to tooling. These tests drive
//! `repl::collect_decl_completion_names` (the function the REPL calls
//! after each declaration). The LSP-side behavioural locks live in the
//! walkers' own unit-test modules (src/lsp/locals.rs,
//! src/lsp/semantic_tokens.rs).

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::repl::collect_decl_completion_names;

fn completion_names(src: &str) -> Vec<String> {
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap_or_else(|e| panic!("fixture failed to lex: {}", e.message));
    let program = Parser::new(tokens, src)
        .parse_program()
        .unwrap_or_else(|e| panic!("fixture failed to parse: {}", e.message));
    collect_decl_completion_names(&program.decls)
}

#[test]
fn repl_completion_names_include_anon_record_rest_binder() {
    // Round-101 repro 4: after `let {x, ...rest} = r`, REPL tab
    // completion offered `x` but not `rest`. This drives the exact
    // helper `eval_declaration` uses to populate its completion list.
    let names = completion_names("let {x, ...rest} = r");
    assert_eq!(
        names,
        vec!["x".to_string(), "rest".to_string()],
        "`let {{x, ...rest}} = r` must surface BOTH the shorthand field \
         and the rest binder to REPL completion"
    );
}

#[test]
fn repl_completion_names_include_list_rest_binder() {
    assert_eq!(
        completion_names("let [h, ..t] = xs"),
        vec!["h".to_string(), "t".to_string()],
        "`let [h, ..t] = xs` must surface both the head and the rest binder"
    );
}

#[test]
fn repl_completion_names_include_map_value_binder() {
    assert_eq!(
        completion_names("let #{\"k\": v} = m"),
        vec!["v".to_string()],
        "`let #{{\"k\": v}} = m` must surface the map-value binder"
    );
}
