//! Type signatures for the `stream` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    #[cfg(feature = "tcp")]
    let tcp_stream_ty = Type::Generic(intern("TcpStream"), vec![]);
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
            },
        );
    }
    // stream.from_range: (Int, Int) -> Channel(Int)
    env.define(
        intern("stream.from_range"),
        Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int],
            Box::new(Type::Channel(Box::new(Type::Int))),
        )),
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
            },
        );
    }
    // stream.file_chunks: (String, Int) -> Channel(Result(Bytes, String))
    env.define(
        intern("stream.file_chunks"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int],
            Box::new(Type::Channel(Box::new(result(
                bytes_ty.clone(),
                Type::String,
            )))),
        )),
    );
    // stream.file_lines: String -> Channel(Result(String, String))
    env.define(
        intern("stream.file_lines"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Channel(Box::new(result(Type::String, Type::String)))),
        )),
    );
    #[cfg(feature = "tcp")]
    {
        // stream.tcp_chunks: (TcpStream, Int) -> Channel(Result(Bytes, String))
        env.define(
            intern("stream.tcp_chunks"),
            Scheme::mono(Type::Fun(
                vec![tcp_stream_ty.clone(), Type::Int],
                Box::new(Type::Channel(Box::new(result(
                    bytes_ty.clone(),
                    Type::String,
                )))),
            )),
        );
        // stream.tcp_lines: TcpStream -> Channel(Result(String, String))
        env.define(
            intern("stream.tcp_lines"),
            Scheme::mono(Type::Fun(
                vec![tcp_stream_ty.clone()],
                Box::new(Type::Channel(Box::new(result(Type::String, Type::String)))),
            )),
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
            },
        );
    }
    // stream.write_to_file: (Channel(Bytes), String) -> Result((), String)
    env.define(
        intern("stream.write_to_file"),
        Scheme::mono(Type::Fun(
            vec![Type::Channel(Box::new(bytes_ty.clone())), Type::String],
            Box::new(result(Type::Unit, Type::String)),
        )),
    );
    #[cfg(feature = "tcp")]
    {
        // stream.write_to_tcp: (Channel(Bytes), TcpStream) -> Result((), String)
        env.define(
            intern("stream.write_to_tcp"),
            Scheme::mono(Type::Fun(
                vec![Type::Channel(Box::new(bytes_ty)), tcp_stream_ty],
                Box::new(result(Type::Unit, Type::String)),
            )),
        );
    }
}
