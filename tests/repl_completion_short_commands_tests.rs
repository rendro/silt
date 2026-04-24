//! Lock test: REPL tab completion offers the short forms `:q` and `:h`.
//!
//! `:help` advertises `:help, :h` and `:quit, :q` (src/repl.rs::print_help),
//! and the runtime loop accepts all four (src/repl.rs ~:181). But pre-fix,
//! `builtin_names()` only contained `:quit` and `:help`, so typing `:h<Tab>`
//! at the prompt matched only `:help` — the documented short form was
//! silently unreachable via completion. This test pins the fix so a
//! regression that drops `:q` or `:h` from the builtin list fails here.

use silt::repl::{builtin_names, completion_candidates_for_prefix};

#[test]
fn test_repl_builtin_names_includes_short_commands() {
    let names = builtin_names();
    for short in [":q", ":h"] {
        assert!(
            names.contains(&short.to_string()),
            "REPL builtin_names must include short command `{short}`, got:\n{names:?}"
        );
    }
}

#[test]
fn test_repl_completion_for_colon_q_offers_short_and_long_quit() {
    // Typing `:q` and hitting Tab must offer both `:q` and `:quit`.
    // Pre-fix only `:quit` was offered, so the short form (though the
    // runtime accepts it) was invisible to tab-completion users.
    let matches = completion_candidates_for_prefix(":q");
    assert!(
        matches.iter().any(|s| s == ":q"),
        "expected `:q` in completions for `:q`, got:\n{matches:?}"
    );
    assert!(
        matches.iter().any(|s| s == ":quit"),
        "expected `:quit` in completions for `:q`, got:\n{matches:?}"
    );
}

#[test]
fn test_repl_completion_for_colon_h_offers_short_and_long_help() {
    // Typing `:h` and hitting Tab must offer both `:h` and `:help`.
    let matches = completion_candidates_for_prefix(":h");
    assert!(
        matches.iter().any(|s| s == ":h"),
        "expected `:h` in completions for `:h`, got:\n{matches:?}"
    );
    assert!(
        matches.iter().any(|s| s == ":help"),
        "expected `:help` in completions for `:h`, got:\n{matches:?}"
    );
}
