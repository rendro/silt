//! Type signatures for the `result` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // result.map_ok: (Result(a,e), (a -> b)) -> Result(b,e)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.map_ok"),
            Scheme {
                vars: vec![av, bv, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Result"), vec![a, e.clone()]),
                        Type::Fun(vec![Type::Var(av)], Box::new(b.clone())),
                    ],
                    Box::new(Type::Generic(intern("Result"), vec![b, e])),
                ),
                constraints: vec![],
            },
        );
    }

    // result.unwrap_or: (Result(a,e), a) -> a
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.unwrap_or"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Result"), vec![a.clone(), e]),
                        a.clone(),
                    ],
                    Box::new(a),
                ),
                constraints: vec![],
            },
        );
    }

    // result.map_err: (Result(a,e), (e -> f)) -> Result(a,f)
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        let (f, fv) = checker.fresh_tv();
        env.define(
            intern("result.map_err"),
            Scheme {
                vars: vec![av, ev, fv],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                        Type::Fun(vec![e], Box::new(f.clone())),
                    ],
                    Box::new(Type::Generic(intern("Result"), vec![a, f])),
                ),
                constraints: vec![],
            },
        );
    }

    // result.flatten: (Result(Result(a,e),e)) -> Result(a,e)
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.flatten"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic(
                        intern("Result"),
                        vec![
                            Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                            e.clone(),
                        ],
                    )],
                    Box::new(Type::Generic(intern("Result"), vec![a, e])),
                ),
                constraints: vec![],
            },
        );
    }

    // result.flat_map: (Result(a, e), (a) -> Result(b, e)) -> Result(b, e)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.flat_map"),
            Scheme {
                vars: vec![av, bv, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                        Type::Fun(
                            vec![a],
                            Box::new(Type::Generic(intern("Result"), vec![b.clone(), e.clone()])),
                        ),
                    ],
                    Box::new(Type::Generic(intern("Result"), vec![b, e])),
                ),
                constraints: vec![],
            },
        );
    }

    // result.is_ok: (Result(a,e)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.is_ok"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Result"), vec![a, e])],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }

    // result.is_err: (Result(a,e)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("result.is_err"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic(intern("Result"), vec![a, e])],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }
}
