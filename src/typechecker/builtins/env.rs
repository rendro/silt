//! Type signatures for the `env` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs_filtered;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // Every env.* operation reads or mutates process environment, which
    // is an OS-resource interaction — `!{io}` (no fs/net refinement).
    let io = EffectSet::io();

    // env.get: (String) -> Option(String)
    env.define(
        intern("env.get"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(intern("Option"), vec![Type::String])),
            ),
            io,
        ),
    );

    // env.set: (String, String) -> Unit
    env.define(
        intern("env.set"),
        Scheme::with_effects(
            Type::Fun(vec![Type::String, Type::String], Box::new(Type::Unit)),
            io,
        ),
    );

    // env.remove: (String) -> Unit
    //
    // Idempotent unset: removing a nonexistent variable is not an
    // error. Mirrors the `env.set` contract for the "mutating" half of
    // the module.
    env.define(
        intern("env.remove"),
        Scheme::with_effects(Type::Fun(vec![Type::String], Box::new(Type::Unit)), io),
    );

    // env.vars: () -> List((String, String))
    //
    // Snapshot of every environment variable at call time. Order is
    // whatever the underlying `std::env::vars()` iterator produces
    // (unspecified — typically insertion order, but do not depend on
    // it). Returning a `List` of `(key, value)` pairs rather than a
    // `Map` preserves that iteration order for callers who care.
    env.define(
        intern("env.vars"),
        Scheme::with_effects(
            Type::Fun(
                vec![],
                Box::new(Type::List(Box::new(Type::Tuple(vec![
                    Type::String,
                    Type::String,
                ])))),
            ),
            io,
        ),
    );

    attach_module_docs_filtered(env, super::docs::IO_FS_MD, "env");
}
