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
//! The registry is checked against the typechecker's actual free-function
//! bindings, and each consuming site that exposes a public surface (LSP
//! rename, REPL completion, builtin docs) is checked by behaviour.

use std::collections::HashSet;

use silt::module::{all_builtin_constructor_names, builtin_free_function_names};

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

// ─── Site: src/typechecker/builtins.rs ───────────────────────────────

#[test]
fn every_registry_name_has_a_builtin_doc() {
    // The GLOBALS_MD doc-attachment loop must cover every free function,
    // so hover works on each of them.
    let docs = silt::typechecker::builtin_docs();
    for name in builtin_free_function_names() {
        assert!(
            docs.contains_key(*name),
            "builtin_docs() has no entry for free function `{name}` — the \
             GLOBALS_MD attach loop in src/typechecker/builtins.rs no longer \
             covers every `module::builtin_free_function_names()` entry."
        );
    }
}
