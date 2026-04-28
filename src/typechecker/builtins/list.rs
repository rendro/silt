//! Type signatures for the `list` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // list.map: (List(a), (a -> b)) -> List(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.filter: (List(a), (a -> Bool)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.filter"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.fold: (List(a), b, (b, a) -> b) -> b
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.fold"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.each: (List(a), (a -> ())) -> ()
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.each"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.find: (List(a), (a -> Bool)) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.find"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.zip: (List(a), List(b)) -> List((a, b))
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.zip"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::List(Box::new(b.clone())),
                    ],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![a, b])))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.flatten: (List(List(a))) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.flatten"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::List(Box::new(a.clone()))))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.sort_by: (List(a), (a -> b)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.sort_by"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(b)),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.flat_map: (List(a), (a -> List(b))) -> List(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.flat_map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::List(Box::new(b.clone())))),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.filter_map: (List(a), (a -> Option(b))) -> List(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.filter_map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(
                            vec![a],
                            Box::new(Type::Generic(intern("Option"), vec![b.clone()])),
                        ),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.any: (List(a), (a -> Bool)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.any"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.all: (List(a), (a -> Bool)) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.all"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.fold_until: (List(a), b, (b, a) -> Step(b)) -> b
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.fold_until"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(
                            vec![b.clone(), a],
                            Box::new(Type::Generic(intern("Step"), vec![b.clone()])),
                        ),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.unfold: (a, (a) -> Option((b, a))) -> List(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.unfold"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        a.clone(),
                        Type::Fun(
                            vec![a.clone()],
                            Box::new(Type::Generic(
                                intern("Option"),
                                vec![Type::Tuple(vec![b.clone(), a])],
                            )),
                        ),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.append: (List(a), a) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.append"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.prepend: (List(a), a) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.prepend"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.concat: (List(a), List(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.concat"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::List(Box::new(a.clone())),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.get: (List(a), Int) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.get"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.set: (List(a), Int, a) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.set"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int, a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.take: (List(a), Int) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.take"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.drop: (List(a), Int) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.drop"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.enumerate: (List(a)) -> List((Int, a))
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.enumerate"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![Type::Int, a])))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.head: (List(a)) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.head"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.tail: (List(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.tail"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.last: (List(a)) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.last"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.reverse: (List(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.reverse"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.sort: (List(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.sort"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.unique: (List(a)) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.unique"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.contains: (List(a), a) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.contains"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.length: (List(a)) -> Int
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.length"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::List(Box::new(a))], Box::new(Type::Int)),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.group_by: (List(a), (a -> k)) -> Map(k, List(a))
    {
        let (a, av) = checker.fresh_tv();
        let (k, kv) = checker.fresh_tv();
        env.define(
            intern("list.group_by"),
            Scheme {
                vars: vec![av, kv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(k.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(Type::List(Box::new(a))))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.index_of: (List(a), a) -> Option(Int)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.index_of"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a],
                    Box::new(Type::Generic(intern("Option"), vec![Type::Int])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.remove_at: (List(a), Int) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.remove_at"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.min_by: (List(a), (a -> b)) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.min_by"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(b)),
                    ],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.max_by: (List(a), (a -> b)) -> Option(a)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.max_by"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(b)),
                    ],
                    Box::new(Type::Generic(intern("Option"), vec![a])),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.sum: (List(Int)) -> Int
    env.define(
        intern("list.sum"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::Int))],
            Box::new(Type::Int),
        )),
    );

    // list.sum_float: (List(Float)) -> Float
    env.define(
        intern("list.sum_float"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::Float))],
            Box::new(Type::Float),
        )),
    );

    // list.product: (List(Int)) -> Int
    env.define(
        intern("list.product"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::Int))],
            Box::new(Type::Int),
        )),
    );

    // list.product_float: (List(Float)) -> Float
    env.define(
        intern("list.product_float"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::Float))],
            Box::new(Type::Float),
        )),
    );

    // list.scan: (List(a), b, (b, a) -> b) -> List(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("list.scan"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    // list.intersperse: (List(a), a) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("list.intersperse"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::TOP,
            },
        );
    }

    attach_module_docs(env, super::docs::LIST_MD);
}
