//! Round-67 LATENT/DEAD F18 parity lock: every site that mentions
//! silt's builtin global free functions (`print`/`println`/`panic`)
//! MUST source the list from
//! `module::builtin_free_function_names()` rather than hand-rolling
//! its own array. Sibling shape to round-58/64's
//! `all_builtin_constructor_names` consolidation and round-63/64's
//! `KEYWORDS` / `KEYWORD_LITERALS` consolidations.
//!
//! Before round-67 the same 3-element literal `["print", "println",
//! "panic"]` was hand-rolled in:
//!   * `src/lsp/rename.rs` — `BUILTIN_FUNCTIONS` const (rejected from
//!     rename).
//!   * `src/lsp/completion.rs::builtins` — completion list head.
//!   * `src/repl.rs::builtin_names` — REPL <Tab> completion.
//!   * `src/vm/dispatch.rs::register_builtins` — seeds `BuiltinFn`
//!     globals so dispatch resolves the call.
//!   * `src/typechecker/builtins.rs::register_builtins` — attaches
//!     `GLOBALS_MD` docs to each name.
//!
//! Each call site is locked to the registry below by a separate
//! `#[test]` so a single failure points at the drifting site. The
//! checks are bidirectional: the registry and the site must contain
//! the SAME set, not just one as a subset of the other (where a
//! runtime API is available).
//!
//! The most important lock — `registry_matches_typechecker_runtime`
//! below — derives the ground-truth set from the typechecker's actual
//! free-function bindings at runtime, so the registry itself cannot
//! drift from reality. The LSP `completion` and `rename` submodules
//! are private inside `src/lsp/mod.rs`, so those sites use
//! source-grep parity locks (matching the pattern in
//! `tests/lexer_keyword_parity_tests.rs`).

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use silt::module::{all_builtin_constructor_names, builtin_free_function_names};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn registry_set() -> HashSet<&'static str> {
    builtin_free_function_names().iter().copied().collect()
}

// ─── Registry sanity ──────────────────────────────────────────────────

#[test]
fn registry_is_non_empty_and_alphabetic() {
    let names = builtin_free_function_names();
    assert!(
        !names.is_empty(),
        "module::builtin_free_function_names() must not be empty — \
         silt always has at least `print`/`println`/`panic`."
    );
    let mut sorted = names.to_vec();
    sorted.sort();
    assert_eq!(
        names,
        sorted.as_slice(),
        "module::builtin_free_function_names() must be alphabetic."
    );
}

#[test]
fn registry_contains_print_println_panic() {
    // Belt-and-braces: the three names that have been silt's free
    // functions since the language existed must always be present.
    // If you delete one, also delete its typechecker registration in
    // `src/typechecker/builtins.rs::register_builtins` and its docs.
    let set = registry_set();
    for name in ["print", "println", "panic"] {
        assert!(
            set.contains(name),
            "module::builtin_free_function_names() must contain `{name}`."
        );
    }
}

#[test]
fn registry_disjoint_from_constructors() {
    // A name cannot simultaneously be a free function and an enum
    // constructor — if this fires, one of the two registries got the
    // name wrong.
    let funcs = registry_set();
    let ctors: HashSet<&'static str> = all_builtin_constructor_names().collect();
    let overlap: Vec<&str> = funcs.intersection(&ctors).copied().collect();
    assert!(
        overlap.is_empty(),
        "free-function registry overlaps with constructor registry: {:?}",
        overlap
    );
}

// ─── Ground-truth: typechecker runtime ────────────────────────────────

#[test]
fn registry_matches_typechecker_runtime() {
    // The hardest lock: derive the ground-truth set of unqualified
    // function-typed bindings from the typechecker's actual free-
    // function table, then subtract enum constructors. The remainder
    // is what `register_builtins` actually defines as a free function
    // — and it must equal `module::builtin_free_function_names()`.
    //
    // Without this check the registry could itself drift from reality.
    let bindings = silt::typechecker::iter_builtins_for_effects_audit();
    let ctors: HashSet<&'static str> = all_builtin_constructor_names().collect();
    let runtime_free_fns: HashSet<String> = bindings
        .iter()
        // Free functions are unqualified (no `module.` prefix).
        .filter(|(name, _)| !name.contains('.'))
        // Subtract enum constructors (Ok, Err, Some, Stop, Recv, …) —
        // they're also unqualified Type::Fun bindings but they are
        // tracked by `all_builtin_constructor_names`, not here.
        .filter(|(name, _)| !ctors.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect();

    let registry_owned: HashSet<String> = builtin_free_function_names()
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let extra_in_runtime: Vec<&String> = runtime_free_fns.difference(&registry_owned).collect();
    let extra_in_registry: Vec<&String> = registry_owned.difference(&runtime_free_fns).collect();

    assert!(
        extra_in_runtime.is_empty() && extra_in_registry.is_empty(),
        "drift between `module::builtin_free_function_names()` and \
         the typechecker's actual free-function bindings:\n  \
         in typechecker but not registry: {:?}\n  \
         in registry but not typechecker: {:?}\n\
         Fix: update the registry in `src/module.rs` to match what \
         `register_builtins` in `src/typechecker/builtins.rs` actually \
         registers (or vice versa).",
        extra_in_runtime,
        extra_in_registry
    );
}

// ─── Site: src/lsp/rename.rs ─────────────────────────────────────────

#[test]
fn rename_site_routes_through_registry() {
    // After round-67 `src/lsp/rename.rs` no longer defines a
    // `BUILTIN_FUNCTIONS` const at all — `builtin_globals()` builds
    // its slice from `module::builtin_free_function_names()` plus
    // `types::builtins::iter_all()`. Source-level lock: the file must
    // mention the registry helper, and the hand-rolled
    // `BUILTIN_FUNCTIONS` const must be gone.
    let src = read_source("src/lsp/rename.rs");

    assert!(
        src.contains("builtin_free_function_names"),
        "src/lsp/rename.rs must consult \
         `module::builtin_free_function_names()` so adding a new free \
         function (e.g. `eprintln`, `assert`) flows through automatically. \
         Before round-67 the names were hand-rolled in a `BUILTIN_FUNCTIONS` const."
    );
    assert!(
        !src.contains("const BUILTIN_FUNCTIONS"),
        "src/lsp/rename.rs should not define a hand-rolled \
         `BUILTIN_FUNCTIONS` array — the authoritative list lives in \
         `module::builtin_free_function_names()`."
    );
    // Pin the exact byte sequence of the old hand-rolled trio. If
    // someone re-pastes it as `&["println", "print", "panic"]`, this
    // trips. Any reordering still triggers the `const BUILTIN_FUNCTIONS`
    // grep above.
    assert!(
        !src.contains("\"println\", \"print\", \"panic\""),
        "src/lsp/rename.rs should not hand-roll the free-function trio."
    );
}

#[test]
fn rename_rejects_every_registry_name() {
    // Runtime check via the public `is_user_renameable` API: every
    // registry name must be rejected from rename. This is the
    // bidirectional surface check — if the registry adds a new name
    // that rename's `builtin_globals()` doesn't pick up, `is_user_renameable`
    // would return `true` and rename would corrupt user code.
    for name in builtin_free_function_names() {
        assert!(
            !silt::lsp::is_user_renameable(name),
            "rename: `{name}` (a registry-listed free function) must be \
             rejected as a rename target. lsp::rename::builtin_globals() \
             likely drifted from `module::builtin_free_function_names()`."
        );
    }
}

// ─── Site: src/lsp/completion.rs ─────────────────────────────────────

#[test]
fn completion_site_routes_through_registry() {
    // `lsp::completion::builtins()` is in a private submodule; we use
    // the source-grep pattern (matching `lexer_keyword_parity_tests.rs`)
    // to assert routing. The hand-rolled head used the literal:
    //   ("print".to_string(), CompletionItemKind::FUNCTION),
    let src = read_source("src/lsp/completion.rs");
    assert!(
        src.contains("builtin_free_function_names"),
        "src/lsp/completion.rs must consult \
         `module::builtin_free_function_names()` for free-function \
         completions. Before round-67 the names were hand-rolled."
    );
    assert!(
        !src.contains("(\"print\".to_string(), CompletionItemKind::FUNCTION)"),
        "src/lsp/completion.rs should not hand-roll free-function \
         completion items — source from `builtin_free_function_names()`."
    );
    assert!(
        !src.contains("(\"println\".to_string(), CompletionItemKind::FUNCTION)"),
        "src/lsp/completion.rs should not hand-roll free-function \
         completion items — source from `builtin_free_function_names()`."
    );
    assert!(
        !src.contains("(\"panic\".to_string(), CompletionItemKind::FUNCTION)"),
        "src/lsp/completion.rs should not hand-roll free-function \
         completion items — source from `builtin_free_function_names()`."
    );
}

// ─── Site: src/repl.rs ───────────────────────────────────────────────

#[test]
fn repl_site_contains_every_registry_name() {
    // Runtime check: `repl::builtin_names()` is a SUPERSET (REPL
    // commands, keywords, constructors, type names, modules). Every
    // registry entry must appear in it.
    let names: HashSet<String> = silt::repl::builtin_names().into_iter().collect();
    for name in builtin_free_function_names() {
        assert!(
            names.contains(*name),
            "repl::builtin_names() missing free function `{name}` — \
             `module::builtin_free_function_names()` says it should be present."
        );
    }
}

#[test]
fn repl_site_routes_through_registry() {
    let src = read_source("src/repl.rs");
    assert!(
        src.contains("builtin_free_function_names"),
        "src/repl.rs must consult \
         `module::builtin_free_function_names()` for free-function \
         completions. Before round-67 the names were hand-rolled."
    );
    // Round-67: the hand-rolled trio used to live as a comma-separated
    // string-literal triple inside the `vec![...]` initializer. Pin
    // the exact byte sequence so a regress reintroduces this test
    // failure.
    assert!(
        !src.contains("\"print\", \"println\", \"panic\""),
        "src/repl.rs should not hand-roll the free-function trio — \
         source from `builtin_free_function_names()`."
    );
}

// ─── Site: src/vm/dispatch.rs ────────────────────────────────────────

#[test]
fn dispatch_site_routes_through_registry() {
    // The dispatch site seeds `Value::BuiltinFn(name)` globals for
    // every free function. We can't easily snapshot the VM's globals
    // map at startup from a test, so we use a source-level lock: the
    // for-loop must iterate `module::builtin_free_function_names()`
    // and the old hand-rolled trio must be gone.
    let src = read_source("src/vm/dispatch.rs");
    assert!(
        src.contains("builtin_free_function_names"),
        "src/vm/dispatch.rs must consult \
         `module::builtin_free_function_names()` when seeding free-fn \
         globals. Before round-67 the names were hand-rolled."
    );
    assert!(
        !src.contains("[\"print\", \"println\", \"panic\"]"),
        "src/vm/dispatch.rs should not hand-roll \
         `[\"print\", \"println\", \"panic\"]` — source from \
         `builtin_free_function_names()`."
    );
}

// ─── Site: src/typechecker/builtins.rs ───────────────────────────────

#[test]
fn typechecker_docs_attach_site_routes_through_registry() {
    // The doc-attachment site at `src/typechecker/builtins.rs:~893`
    // mixes free-fn names with a hand-rolled subset of constructors
    // (Ok/Err/Some/None/Stop/Continue/Message/Closed/Empty/Sent/
    // Recv/Send) — this constructor list is the specific set
    // documented in `GLOBALS_MD`, NOT the full builtin-constructor
    // universe (Monday, IoNotFound, … live in their per-module docs).
    //
    // We lock the FREE-FUNCTION subset specifically: the file must
    // route through `builtin_free_function_names()` for those names,
    // and the old hand-rolled trio "print", "println", "panic" must
    // not appear in the prefix position of an array literal there.
    let src = read_source("src/typechecker/builtins.rs");
    assert!(
        src.contains("builtin_free_function_names"),
        "src/typechecker/builtins.rs must consult \
         `module::builtin_free_function_names()` for the GLOBALS_MD \
         doc-attachment loop's free-function targets."
    );
    // The old hand-rolled head was:
    //   "print", "println", "panic", "Ok", ...
    // Re-introducing that exact prefix trips here.
    assert!(
        !src.contains("\"print\", \"println\", \"panic\", \"Ok\""),
        "src/typechecker/builtins.rs should not hand-roll the \
         `print`/`println`/`panic` trio in the doc-attachment loop — \
         source from `builtin_free_function_names()`."
    );
}
