//! Type signatures for the `set` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // set.new: () -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.new"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![], Box::new(Type::Set(Box::new(a)))),
                constraints: vec![],
            },
        );
    }

    // set.from_list: (List(a)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.from_list"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.to_list: (Set(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.to_list"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.contains: (Set(a), a) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.contains"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }

    // set.insert: (Set(a), a) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.insert"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a.clone()],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.remove: (Set(a), a) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.remove"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a.clone()],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.length: (Set(a)) -> Int
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.length"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::Set(Box::new(a))], Box::new(Type::Int)),
                constraints: vec![],
            },
        );
    }

    // set.union: (Set(a), Set(a)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.union"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.intersection: (Set(a), Set(a)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.intersection"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.difference: (Set(a), Set(a)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.difference"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.symmetric_difference: (Set(a), Set(a)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.symmetric_difference"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.is_subset: (Set(a), Set(a)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.is_subset"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }

    // set.map: (Set(a), (a -> b)) -> Set(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("set.map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Set(Box::new(b))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.filter: (Set(a), (a -> Bool)) -> Set(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.filter"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            },
        );
    }

    // set.each: (Set(a), (a -> ())) -> ()
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("set.each"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            },
        );
    }

    // set.fold: (Set(a), b, (b, a) -> b) -> b
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("set.fold"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
            },
        );
    }

    attach_module_docs(env, super::docs::SET_MD);
}
