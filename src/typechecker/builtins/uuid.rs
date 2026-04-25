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
    env.define(
        intern("uuid.v4"),
        Scheme::mono(Type::Fun(vec![], Box::new(Type::String))),
    );

    // uuid.v7: () -> String
    env.define(
        intern("uuid.v7"),
        Scheme::mono(Type::Fun(vec![], Box::new(Type::String))),
    );

    // uuid.parse: String -> Result(String, String)
    env.define(
        intern("uuid.parse"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(Type::String, Type::String)),
        )),
    );

    // uuid.nil: () -> String
    env.define(
        intern("uuid.nil"),
        Scheme::mono(Type::Fun(vec![], Box::new(Type::String))),
    );

    // uuid.is_valid: String -> Bool
    env.define(
        intern("uuid.is_valid"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    attach_module_overview(env, super::docs::UUID_MD, "uuid");
    attach_module_docs(env, super::docs::UUID_MD);
}
