//! Round-89 GAP fix — COMPREHENSIVE, self-maintaining parity lock for
//! the round-72 BROKEN class.
//!
//! ## The invariant
//!
//! Two registries decide whether a `<module>.<func>` reference works:
//!
//! 1. The **typechecker** registers a type scheme for each `module.func`
//!    via scattered `env.define(intern("module.func"), ...)` calls in
//!    `src/typechecker/builtins.rs` and the per-module submodules under
//!    `src/typechecker/builtins/`. Anything registered here *typechecks*.
//!
//! 2. The **runtime** seeds a first-class `Value::BuiltinFn` global for
//!    each name returned by `module::builtin_module_functions(module)`
//!    (plus the constant globals for each name in
//!    `module::builtin_module_constants(module)`) — see
//!    `src/vm/dispatch.rs::register_builtins` (the loop over
//!    `module::BUILTIN_MODULES`). Anything seeded here is reachable as a
//!    *value* at runtime (`let f = module.func`, `xs |> module.func(..)`,
//!    closure capture).
//!
//! Round 72 found these can drift: a name registered in the typechecker
//! but ABSENT from `builtin_module_functions` typechecks fine yet
//! crashes at runtime with `undefined global: module.func` the moment it
//! is used as a value rather than the head of a call. The fix widened
//! `builtin_module_functions`, but the lock that guarded it was a
//! hand-curated ~17-case sample
//! (`builtin_module_function_value_access_runtime_tests.rs`). The
//! runtime side enumerates ~270+ names; a future `intern("foo.bar")`
//! added without a matching arm would stay GREEN and silently re-open
//! the bug.
//!
//! ## This lock
//!
//! `every_typechecker_module_function_is_value_accessible` closes that
//! hole by enumerating the typechecker's registrations *authoritatively*
//! instead of by hand:
//!
//! - `silt::typechecker::builtin_type_signatures()` builds a fresh
//!   `TypeChecker` + `TypeEnv`, runs the full builtin registration pass
//!   (`register_builtins`), and returns every qualified (`.`-containing)
//!   name defined in the env. This IS the typechecker's source of truth.
//! - For each such `module.func` whose `module` is a
//!   `module::BUILTIN_MODULES` entry, the test asserts the `func` is
//!   present in `module::builtin_module_functions(module)` OR
//!   `module::builtin_module_constants(module)` — the exact two
//!   registries the VM iterates to seed globals.
//!
//! Because the expected set is derived from a DIFFERENT source
//! (`module.rs`) than the observed set (the typechecker pass), the test
//! is not tautological: adding `intern("math.bogus")` to a typechecker
//! submodule without widening `builtin_module_functions("math")` makes
//! `builtin_type_signatures()` report `math.bogus` while the module
//! registry does not — and the test fails, naming the exact fix site.
//!
//! ## Why constants are in the union
//!
//! `math.pi`, `math.e`, and the `float.*` constants are registered by
//! the typechecker as qualified names too, but the VM seeds them as
//! value globals (`Value::Float(..)` / `Value::ExtFloat(..)`) from a
//! separate path, mirrored by `module::builtin_module_constants`. They
//! are value-accessible — just not via `builtin_module_functions` — so
//! the union of the two registries is the correct "is seeded as a
//! global" predicate. (`module_constants_completion_tests.rs` locks
//! `builtin_module_constants` against the VM-seeded constants.)

use std::collections::BTreeSet;

/// Authoritative, self-maintaining parity lock. Enumerates every
/// typechecker-registered `module.func` and asserts each is seeded as a
/// runtime global. No hand-maintained list — drift fails the build.
#[test]
fn every_typechecker_module_function_is_value_accessible() {
    // (1) The typechecker's authoritative set of registered names.
    // `builtin_type_signatures()` runs the real `register_builtins`
    // pass over a fresh env and returns every qualified binding.
    let sigs = silt::typechecker::builtin_type_signatures();

    let builtin_modules: BTreeSet<&'static str> =
        silt::module::BUILTIN_MODULES.iter().copied().collect();

    // (2) Collect the typechecker-registered `module.func` keys whose
    // prefix is a real builtin module. (A qualified name whose prefix is
    // NOT a builtin module would itself be a bug — asserted separately
    // below.)
    let mut checked: Vec<String> = Vec::new();
    let mut non_builtin_prefix: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for name in sigs.keys() {
        let Some((module, func)) = name.split_once('.') else {
            // Unqualified names (print/println/panic, constructors) are
            // not in `sigs` — `builtin_type_signatures` only returns
            // dotted names — but guard anyway.
            continue;
        };

        if !builtin_modules.contains(module) {
            non_builtin_prefix.push(name.clone());
            continue;
        }

        checked.push(name.clone());

        // (3) The runtime source of truth: the VM seeds a global for
        // every name in `builtin_module_functions(module)` (as a
        // `BuiltinFn`) and `builtin_module_constants(module)` (as a
        // value). Membership in their union == "value-accessible global".
        let in_functions = silt::module::builtin_module_functions(module)
            .iter()
            .any(|f| *f == func);
        let in_constants = silt::module::builtin_module_constants(module)
            .iter()
            .any(|c| *c == func);

        if !in_functions && !in_constants {
            missing.push(name.clone());
        }
    }

    // Every qualified typechecker registration must belong to a known
    // builtin module. If this trips, either a new module needs adding to
    // `module::BUILTIN_MODULES` or the registration uses a stray prefix.
    non_builtin_prefix.sort();
    assert!(
        non_builtin_prefix.is_empty(),
        "typechecker registered qualified name(s) whose prefix is NOT a \
         `module::BUILTIN_MODULES` entry: {non_builtin_prefix:?}. Either add \
         the module to BUILTIN_MODULES or fix the registration prefix."
    );

    // The core lock: no typechecker registration may lack a matching
    // runtime global. This is the round-72 bug, caught statically.
    missing.sort();
    assert!(
        missing.is_empty(),
        "ROUND-72 REGRESSION: the typechecker registers {n} `module.func` \
         name(s) that are NOT seeded as runtime globals, so first-class \
         value access (`let f = m.func`) will crash with \
         `undefined global: m.func` even though it typechecks: {missing:?}.\n\
         Fix: for each, add `\"func\"` to the matching arm of \
         `builtin_module_functions` in src/module.rs (or to \
         `builtin_module_constants` if it is a module constant).",
        n = missing.len()
    );

    // Sanity floor: the round-72 finding cited ~270 runtime names and the
    // typechecker registers a comparable surface. If the enumeration ever
    // collapses to a tiny number, the authoritative source silently broke
    // (e.g. `builtin_type_signatures` stopped returning dotted names) and
    // the lock would be vacuously green — guard against that.
    assert!(
        checked.len() > 200,
        "expected the typechecker to register 200+ `module.func` names \
         (round-72 cited ~270 on the runtime side); only saw {} — the \
         authoritative enumeration likely regressed: {checked:?}",
        checked.len()
    );
}

/// Reverse direction (defensive, non-load-bearing for round 72 but cheap
/// to assert): nothing in `builtin_module_functions` should be missing a
/// typechecker signature. A runtime global with no type scheme would let
/// `let f = m.func` resolve at runtime but fail to typecheck — the
/// opposite drift. This keeps the two registries fully in lockstep, not
/// just one-directionally.
#[test]
fn every_runtime_module_function_has_a_typechecker_signature() {
    let sigs = silt::typechecker::builtin_type_signatures();

    let mut missing_sig: Vec<String> = Vec::new();
    for &module in silt::module::BUILTIN_MODULES {
        for func in silt::module::builtin_module_functions(module) {
            let qualified = format!("{module}.{func}");
            if !sigs.contains_key(&qualified) {
                missing_sig.push(qualified);
            }
        }
    }
    missing_sig.sort();
    assert!(
        missing_sig.is_empty(),
        "runtime seeds global(s) with NO typechecker signature (would \
         typecheck-fail when used as a value): {missing_sig:?}. Add a \
         matching `env.define(intern(\"module.func\"), ...)` in \
         src/typechecker/builtins/."
    );
}
