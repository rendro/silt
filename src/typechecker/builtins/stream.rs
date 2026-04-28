//! Type signatures for the `stream` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::{attach_module_docs, attach_module_overview};

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let io_err_ty = Type::Generic(intern("IoError"), vec![]);
    #[cfg(feature = "tcp")]
    let tcp_stream_ty = Type::Generic(intern("TcpStream"), vec![]);
    #[cfg(feature = "tcp")]
    let tcp_err_ty = Type::Generic(intern("TcpError"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };
    let option = |ok_ty: Type| -> Type { Type::Generic(intern("Option"), vec![ok_ty]) };

    // ── Sources ──────────────────────────────────────────────────
    // stream.from_list: List(a) -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.from_list"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.from_range: (Int, Int) -> Channel(Int)
    env.define(
        intern("stream.from_range"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::Int, Type::Int],
                Box::new(Type::Channel(Box::new(Type::Int))),
            ),
            EffectSet::pure(),
        ),
    );
    // stream.repeat: a -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.repeat"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone()], Box::new(Type::Channel(Box::new(a)))),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.unfold: (a, a -> Option((b, a))) -> Channel(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        let pair = Type::Tuple(vec![b.clone(), a.clone()]);
        env.define(
            intern("stream.unfold"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![a.clone(), Type::Fun(vec![a], Box::new(option(pair)))],
                    Box::new(Type::Channel(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.file_chunks: (String, Int) -> Channel(Result(Bytes, IoError))
    env.define(
        intern("stream.file_chunks"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::String, Type::Int],
                Box::new(Type::Channel(Box::new(result(
                    bytes_ty.clone(),
                    io_err_ty.clone(),
                )))),
            ),
            EffectSet::io_fs(),
        ),
    );
    // stream.file_lines: String -> Channel(Result(String, IoError))
    env.define(
        intern("stream.file_lines"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::String],
                Box::new(Type::Channel(Box::new(result(
                    Type::String,
                    io_err_ty.clone(),
                )))),
            ),
            EffectSet::io_fs(),
        ),
    );
    #[cfg(feature = "tcp")]
    {
        // stream.tcp_chunks: (TcpStream, Int) -> Channel(Result(Bytes, TcpError))
        env.define(
            intern("stream.tcp_chunks"),
            Scheme::with_effects(
                Type::Fun(
                    vec![tcp_stream_ty.clone(), Type::Int],
                    Box::new(Type::Channel(Box::new(result(
                        bytes_ty.clone(),
                        tcp_err_ty.clone(),
                    )))),
                ),
                EffectSet::io_net(),
            ),
        );
        // stream.tcp_lines: TcpStream -> Channel(Result(String, TcpError))
        env.define(
            intern("stream.tcp_lines"),
            Scheme::with_effects(
                Type::Fun(
                    vec![tcp_stream_ty.clone()],
                    Box::new(Type::Channel(Box::new(result(
                        Type::String,
                        tcp_err_ty.clone(),
                    )))),
                ),
                EffectSet::io_net(),
            ),
        );
    }

    // ── Transforms ──────────────────────────────────────────────
    // stream.map: (Channel(a), a -> b) -> Channel(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("stream.map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Channel(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.map_ok: (Channel(Result(a, e)), a -> b) -> Channel(Result(b, e))
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("stream.map_ok"),
            Scheme {
                vars: vec![av, bv, ev],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(result(a.clone(), e.clone()))),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Channel(Box::new(result(b, e)))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.filter: (Channel(a), a -> Bool) -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.filter"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.filter_ok: (Channel(Result(a, e)), a -> Bool) -> Channel(Result(a, e))
    {
        let (a, av) = checker.fresh_tv();
        let (e, ev) = checker.fresh_tv();
        env.define(
            intern("stream.filter_ok"),
            Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(result(a.clone(), e.clone()))),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Channel(Box::new(result(a, e)))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.flat_map: (Channel(a), a -> List(b)) -> Channel(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("stream.flat_map"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::List(Box::new(b.clone())))),
                    ],
                    Box::new(Type::Channel(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.take / drop / take_while / drop_while: (Channel(a), Int|fn) -> Channel(a)
    for name in &["stream.take", "stream.drop"] {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern(name),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), Type::Int],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    for name in &["stream.take_while", "stream.drop_while"] {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern(name),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.chunks: (Channel(a), Int) -> Channel(List(a))
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.chunks"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), Type::Int],
                    Box::new(Type::Channel(Box::new(Type::List(Box::new(a))))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.scan: (Channel(a), b, (b, a) -> b) -> Channel(b)
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("stream.scan"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Channel(Box::new(b))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.dedup: Channel(a) -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.dedup"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone()))],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.buffered: (Channel(a), Int) -> Channel(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.buffered"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone())), Type::Int],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }

    // ── Combinators ─────────────────────────────────────────────
    // stream.merge / concat: List(Channel(a)) -> Channel(a)
    for name in &["stream.merge", "stream.concat"] {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern(name),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::Channel(Box::new(a.clone()))))],
                    Box::new(Type::Channel(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.zip: (Channel(a), Channel(b)) -> Channel((a, b))
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("stream.zip"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Channel(Box::new(b.clone())),
                    ],
                    Box::new(Type::Channel(Box::new(Type::Tuple(vec![a, b])))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }

    // ── Sinks ───────────────────────────────────────────────────
    // stream.collect: Channel(a) -> List(a)
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.collect"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.fold: (Channel(a), b, (b, a) -> b) -> b
    {
        let (a, av) = checker.fresh_tv();
        let (b, bv) = checker.fresh_tv();
        env.define(
            intern("stream.fold"),
            Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.each: (Channel(a), a -> Unit) -> Unit
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.each"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Channel(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.count: Channel(a) -> Int
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("stream.count"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::Channel(Box::new(a))], Box::new(Type::Int)),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.first / last: Channel(a) -> Option(a)
    for name in &["stream.first", "stream.last"] {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern(name),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Channel(Box::new(a.clone()))],
                    Box::new(option(a)),
                ),
                constraints: vec![],
                effects: EffectSet::pure(),
            },
        );
    }
    // stream.write_to_file: (Channel(Bytes), String) -> Result((), IoError)
    env.define(
        intern("stream.write_to_file"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::Channel(Box::new(bytes_ty.clone())), Type::String],
                Box::new(result(Type::Unit, io_err_ty)),
            ),
            EffectSet::io_fs(),
        ),
    );
    #[cfg(feature = "tcp")]
    {
        // stream.write_to_tcp: (Channel(Bytes), TcpStream) -> Result((), TcpError)
        env.define(
            intern("stream.write_to_tcp"),
            Scheme::with_effects(
                Type::Fun(
                    vec![Type::Channel(Box::new(bytes_ty)), tcp_stream_ty],
                    Box::new(result(Type::Unit, tcp_err_ty)),
                ),
                EffectSet::io_net(),
            ),
        );
    }

    attach_module_overview(env, super::docs::STREAM_MD, "stream");
    attach_module_docs(env, super::docs::STREAM_MD);
}
