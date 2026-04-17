//! Type signatures for the `channel` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // channel.new: (Int) -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.new"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::Int], Box::new(Type::Channel(Box::new(a)))),
                constraints: vec![],
            },
        );
    }

    // channel.send: (Channel(a), a) -> Unit
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.send"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), a],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.receive: (Channel(a)) -> ChannelResult(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.receive"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone()))],
                    Box::new(Type::Generic(intern("ChannelResult"), vec![a])),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.close: (Channel(a)) -> Unit
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.close"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::Channel(Box::new(a))], Box::new(Type::Unit)),
                constraints: vec![],
            },
        );
    }

    // channel.try_send: (Channel(a), a) -> Bool
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.try_send"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), a],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.try_receive: (Channel(a)) -> ChannelResult(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.try_receive"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone()))],
                    Box::new(Type::Generic(intern("ChannelResult"), vec![a])),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.select: (List(Channel(a))) -> (Channel(a), ChannelResult(a))
    {
        let (a, av) = checker.fresh_tv();
        let ch_a = Type::Channel(Box::new(a.clone()));
        env.define(
            intern("channel.select"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(ch_a.clone()))],
                    Box::new(Type::Tuple(vec![
                        ch_a,
                        Type::Generic(intern("ChannelResult"), vec![a]),
                    ])),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.each: (Channel(a), (a) -> b) -> Unit
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("channel.each"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            },
        );
    }

    // channel.timeout: (Int) -> Channel(a)
    //
    // Creates a channel that automatically closes after the given number
    // of milliseconds. The returned channel carries no values -- the
    // runtime never sends on it, it just closes it when the deadline
    // elapses. A polymorphic element type lets the result be mixed into
    // a `channel.select` alongside channels of any element type (the
    // element will never actually be observed because the channel closes
    // before any `Message` arrives).
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("channel.timeout"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::Int], Box::new(Type::Channel(Box::new(a)))),
                constraints: vec![],
            },
        );
    }
}
