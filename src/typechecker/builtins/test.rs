//! Type signatures for the `test` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // test.assert: (Bool, String) -> ()
    // The message parameter is optional: the signature carries the full
    // arity, so a message that is passed is type checked, and declares
    // the last parameter optional, so `test.assert(cond)` is accepted.
    env.define(
        intern("test.assert"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::Bool, Type::String],
            Box::new(Type::Unit),
        ))
        .with_optional_last_param(),
    );

    // test.assert_eq: (a, a, String) -> ()
    // The message parameter is optional.
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("test.assert_eq"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
                effects: EffectSet::pure(),
                optional_last_param: true,
            },
        );
    }

    // test.assert_ne: (a, a, String) -> ()
    // The message parameter is optional.
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("test.assert_ne"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
                effects: EffectSet::pure(),
                optional_last_param: true,
            },
        );
    }

    attach_module_docs(env, super::docs::TEST_MD);
}
