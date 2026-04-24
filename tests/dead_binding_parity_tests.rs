//! Lock tests ensuring the round-60+ dead-binding cleanups are not
//! reintroduced. Each test is a source-grep assertion against the
//! specific dummy `let _ = …;` or unused computation that was removed.

#[test]
fn inlay_hints_no_dummy_ident_end_span() {
    let src = include_str!("../src/lsp/inlay_hints.rs");
    assert!(
        !src.contains("let _ = ident_end_span"),
        "src/lsp/inlay_hints.rs reintroduced dummy ident_end_span binding"
    );
    // Also assert the ident_end_span computation itself is gone:
    assert!(
        !src.contains("let ident_end_span"),
        "src/lsp/inlay_hints.rs reintroduced unused ident_end_span computation"
    );
}

#[test]
fn toml_builtin_no_dummy_type_name_discard() {
    let src = include_str!("../src/builtins/toml.rs");
    assert!(
        !src.contains("let _ = type_name;"),
        "src/builtins/toml.rs reintroduced dummy type_name discard"
    );
}

#[test]
fn compiler_end_scope_no_dead_pop_count() {
    let src = include_str!("../src/compiler/mod.rs");
    assert!(
        !src.contains("let mut pop_count"),
        "src/compiler/mod.rs reintroduced dead pop_count counter"
    );
    assert!(
        !src.contains("pop_count += 1"),
        "src/compiler/mod.rs reintroduced dead pop_count increment"
    );
}

#[test]
fn semantic_tokens_no_dummy_def_discard() {
    let src = include_str!("../src/lsp/semantic_tokens.rs");
    assert!(
        !src.contains("let _ = def;"),
        "src/lsp/semantic_tokens.rs reintroduced dummy def discard"
    );
}
