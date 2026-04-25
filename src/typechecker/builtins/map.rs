//! Type signatures for the `map` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // map.get: (Map(k, v), k) -> Option(v)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.get"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v.clone())), k],
                    Box::new(Type::Generic(intern("Option"), vec![v])),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.set: (Map(k, v), k, v) -> Map(k, v)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.set"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                        v.clone(),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.delete: (Map(k, v), k) -> Map(k, v)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.delete"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.contains: (Map(k, v), k) -> Bool  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.contains"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v)), k],
                    Box::new(Type::Bool),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.keys: (Map(k, v)) -> List(k)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.keys"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v))],
                    Box::new(Type::List(Box::new(k))),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.values: (Map(k, v)) -> List(v)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.values"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k), Box::new(v.clone()))],
                    Box::new(Type::List(Box::new(v))),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.merge: (Map(k, v), Map(k, v)) -> Map(k, v)  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.merge"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.length: (Map(k, v)) -> Int  where k: Hash
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.length"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k), Box::new(v))],
                    Box::new(Type::Int),
                ),
                constraints: vec![(kv, intern("Hash"))],
            },
        );
    }

    // map.filter: (Map(k, v), (k, v) -> Bool) -> Map(k, v)
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.filter"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(vec![k.clone(), v.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            },
        );
    }

    // map.map: (Map(k, v), (k, v) -> (k2, v2)) -> Map(k2, v2)
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        let (k2, k2v) = checker.fresh_tv();
        let (v2, v2v) = checker.fresh_tv();
        env.define(
            intern("map.map"),
            Scheme {
                vars: vec![kv, vv, k2v, v2v],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(
                            vec![k, v],
                            Box::new(Type::Tuple(vec![k2.clone(), v2.clone()])),
                        ),
                    ],
                    Box::new(Type::Map(Box::new(k2), Box::new(v2))),
                ),
                constraints: vec![],
            },
        );
    }

    // map.entries: (Map(k, v)) -> List((k, v))
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.entries"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v.clone()))],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![k, v])))),
                ),
                constraints: vec![],
            },
        );
    }

    // map.from_entries: (List((k, v))) -> Map(k, v)
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.from_entries"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::Tuple(vec![
                        k.clone(),
                        v.clone(),
                    ])))],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            },
        );
    }

    // map.each: (Map(k, v), (k, v) -> ()) -> ()
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.each"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(vec![k, v], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            },
        );
    }

    // map.update: (Map(k, v), k, v, (v) -> v) -> Map(k, v)
    {
        let (k, kv) = checker.fresh_tv();
        let (v, vv) = checker.fresh_tv();
        env.define(
            intern("map.update"),
            Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                        v.clone(),
                        Type::Fun(vec![v.clone()], Box::new(v.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            },
        );
    }

    attach_module_docs(env, super::docs::MAP_MD);
}
