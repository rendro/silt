//! Type signatures for the `env` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // env.get: (String) -> Option(String)
    env.define(
        intern("env.get"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(intern("Option"), vec![Type::String])),
        )),
    );

    // env.set: (String, String) -> Unit
    env.define(
        intern("env.set"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Unit),
        )),
    );

    // env.remove: (String) -> Unit
    //
    // Idempotent unset: removing a nonexistent variable is not an
    // error. Mirrors the `env.set` contract for the "mutating" half of
    // the module.
    env.define(
        intern("env.remove"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Unit))),
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
        Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::List(Box::new(Type::Tuple(vec![
                Type::String,
                Type::String,
            ])))),
        )),
    );
}
