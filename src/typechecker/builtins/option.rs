//! Type signatures for the `option` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // option.map: (Option(a), (a -> b)) -> Option(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("option.map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Option"), vec![a.clone()]),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Generic(intern("Option"), vec![b])),
                ),
                constraints: vec![],
            },
        );
    }

    // option.flat_map: (Option(a), (a -> Option(b))) -> Option(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("option.flat_map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Option"), vec![a.clone()]),
                        Type::Fun(
                            vec![a],
                            Box::new(Type::Generic(intern("Option"), vec![b.clone()])),
                        ),
                    ],
                    Box::new(Type::Generic(intern("Option"), vec![b])),
                ),
                constraints: vec![],
            },
        );
    }

    // option.unwrap_or: (Option(a), a) -> a
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("option.unwrap_or"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Option"), vec![a.clone()]), a.clone()],
                    Box::new(a),
                ),
                constraints: vec![],
            },
        );
    }

    // option.to_result: (Option(a), e) -> Result(a, e)
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("option.to_result"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Option"), vec![a.clone()]), e.clone()],
                    Box::new(Type::Generic(intern("Result"), vec![a, e])),
                ),
                constraints: vec![],
            },
        );
    }

    // option.is_some: (Option(a)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("option.is_some"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Option"), vec![a])],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }

    // option.is_none: (Option(a)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("option.is_none"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Option"), vec![a])],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }
}
