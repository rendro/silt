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
}
