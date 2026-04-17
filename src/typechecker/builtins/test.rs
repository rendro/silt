//! Type signatures for the `test` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // test.assert: (Bool, String) -> ()
    // The message parameter is optional at runtime; registering the full
    // arity lets the typechecker validate the message type while the
    // is_method_call arity tolerance still allows the 1-arg form.
    env.define(
        intern("test.assert"),
        Scheme::mono(Type::Fun(
            vec![Type::Bool, Type::String],
            Box::new(Type::Unit),
        )),
    );

    // test.assert_eq: (a, a, String) -> ()
    // The message parameter is optional at runtime.
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("test.assert_eq"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
            },
        );
    }

    // test.assert_ne: (a, a, String) -> ()
    // The message parameter is optional at runtime.
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("test.assert_ne"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
            },
        );
    }
}
