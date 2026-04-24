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

    // channel.select: (List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))
    //
    // Every list element is a `ChannelOp(a)` built with either `Recv(ch)`
    // or `Send(ch, value)`. This collapses what used to be two call
    // shapes (bare channels for receive, `(channel, value)` tuples for
    // send) into one tagged form — one way to do things.
    {
        let (a, av) = checker.fresh_tv();
        let ch_a = Type::Channel(Box::new(a.clone()));
        env.define(
            intern("channel.select"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::Generic(
                        intern("ChannelOp"),
                        vec![a.clone()],
                    )))],
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

    // channel.recv_timeout: (Channel(a), Duration) -> Result(a, ChannelError)
    //
    // Blocking receive with a timeout bound. On timely send, returns
    // `Ok(val)`; on the duration elapsing, `Err(ChannelTimeout)`; on the
    // channel closing with no more values, `Err(ChannelClosed)`. A value
    // already in the buffer / a rendezvous sender already parked wins
    // over an expired timer.
    {
        let (a, av) = checker.fresh_tv();
        let duration_ty = super::duration_ty();
        let channel_error_ty = Type::Generic(intern("ChannelError"), vec![]);
        env.define(
            intern("channel.recv_timeout"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), duration_ty],
                    Box::new(Type::Generic(intern("Result"), vec![a, channel_error_ty])),
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
