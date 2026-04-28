//! Type signatures for the `uuid` builtin module.
//!
//! Narrow surface: random / timestamp-ordered UUID generation, string
//! parsing with validation, a predicate helper, and the nil sentinel.
//! All UUIDs cross the language boundary as `String` (canonical
//! lowercase hyphenated form). See `src/builtins/uuid.rs` for the
//! runtime side; round 62 phase-2 inlined the user-facing markdown
//! into `super::docs::UUID_MD`.

use super::super::*;
use super::docs::{attach_module_docs, attach_module_overview};

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // uuid.v4: () -> String
    // Reads OS entropy via the rand crate.
    env.define(
        intern("uuid.v4"),
        Scheme::io_random_mono(Type::Fun(vec![], Box::new(Type::String))),
    );

    // uuid.v7: () -> String
    // Reads wall-clock for the timestamp prefix AND OS entropy for the
    // random suffix — `!{io, time, random}`. The only stdlib operation
    // that combines two refinements.
    env.define(
        intern("uuid.v7"),
        Scheme::with_effects(
            Type::Fun(vec![], Box::new(Type::String)),
            EffectSet::io_time_random(),
        ),
    );

    // uuid.parse: String -> Result(String, String)
    // Pure validation + canonicalisation.
    env.define(
        intern("uuid.parse"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::String],
            Box::new(result(Type::String, Type::String)),
        )),
    );

    // uuid.nil: () -> String
    // Returns the literal "00000000-0000-0000-0000-000000000000". Pure.
    env.define(
        intern("uuid.nil"),
        Scheme::pure_mono(Type::Fun(vec![], Box::new(Type::String))),
    );

    // uuid.is_valid: String -> Bool
    // Pure validation predicate.
    env.define(
        intern("uuid.is_valid"),
        Scheme::pure_mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    attach_module_overview(env, super::docs::UUID_MD, "uuid");
    attach_module_docs(env, super::docs::UUID_MD);
}
