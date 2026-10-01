//! Type signatures for the `tcp` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::{attach_module_docs, attach_module_overview};

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    let listener_ty = Type::Generic(intern("TcpListener"), vec![]);
    let stream_ty = Type::Generic(intern("TcpStream"), vec![]);
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let tcp_err_ty = Type::Generic(intern("TcpError"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // tcp.listen: String -> Result(TcpListener, String)
    env.define(
        intern("tcp.listen"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(listener_ty.clone(), tcp_err_ty.clone())),
        )),
    );

    // tcp.accept: TcpListener -> Result(TcpStream, String)
    env.define(
        intern("tcp.accept"),
        Scheme::mono(Type::Fun(
            vec![listener_ty],
            Box::new(result(stream_ty.clone(), tcp_err_ty.clone())),
        )),
    );

    // tcp.connect: String -> Result(TcpStream, String)
    env.define(
        intern("tcp.connect"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(stream_ty.clone(), tcp_err_ty.clone())),
        )),
    );

    // tcp.read: (TcpStream, Int) -> Result(Bytes, String)
    env.define(
        intern("tcp.read"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Int],
            Box::new(result(bytes_ty.clone(), tcp_err_ty.clone())),
        )),
    );

    // tcp.read_exact: (TcpStream, Int) -> Result(Bytes, String)
    env.define(
        intern("tcp.read_exact"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Int],
            Box::new(result(bytes_ty.clone(), tcp_err_ty.clone())),
        )),
    );

    // tcp.write: (TcpStream, Bytes) -> Result(Unit, String)
    env.define(
        intern("tcp.write"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), bytes_ty],
            Box::new(result(Type::Unit, tcp_err_ty.clone())),
        )),
    );

    // tcp.close: TcpStream -> Unit
    env.define(
        intern("tcp.close"),
        Scheme::mono(Type::Fun(vec![stream_ty.clone()], Box::new(Type::Unit))),
    );

    // tcp.peer_addr: TcpStream -> Result(String, String)
    env.define(
        intern("tcp.peer_addr"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone()],
            Box::new(result(Type::String, tcp_err_ty.clone())),
        )),
    );

    // tcp.set_nodelay: (TcpStream, Bool) -> Result(Unit, String)
    env.define(
        intern("tcp.set_nodelay"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Bool],
            Box::new(result(Type::Unit, tcp_err_ty.clone())),
        )),
    );

    #[cfg(feature = "tcp-tls")]
    {
        let listener_ty_tls = Type::Generic(intern("TcpListener"), vec![]);
        let listener_ty_mtls = Type::Generic(intern("TcpListener"), vec![]);
        let bytes_ty_tls = Type::Generic(intern("Bytes"), vec![]);
        let bytes_ty_mtls = Type::Generic(intern("Bytes"), vec![]);
        // tcp.connect_tls: (String, String) -> Result(TcpStream, String)
        env.define(
            intern("tcp.connect_tls"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(result(stream_ty.clone(), tcp_err_ty.clone())),
            )),
        );
        // tcp.accept_tls: (TcpListener, Bytes, Bytes) -> Result(TcpStream, String)
        env.define(
            intern("tcp.accept_tls"),
            Scheme::mono(Type::Fun(
                vec![listener_ty_tls, bytes_ty_tls.clone(), bytes_ty_tls],
                Box::new(result(stream_ty.clone(), tcp_err_ty.clone())),
            )),
        );
        // tcp.accept_tls_mtls: (TcpListener, Bytes, Bytes, Bytes)
        //   -> Result(TcpStream, String)
        // Same as accept_tls, plus a client-CA PEM bundle used to verify
        // the peer's client certificate (mutual TLS).
        env.define(
            intern("tcp.accept_tls_mtls"),
            Scheme::mono(Type::Fun(
                vec![
                    listener_ty_mtls,
                    bytes_ty_mtls.clone(),
                    bytes_ty_mtls.clone(),
                    bytes_ty_mtls,
                ],
                Box::new(result(stream_ty, tcp_err_ty.clone())),
            )),
        );
    }

    attach_module_overview(env, super::docs::TCP_MD, "tcp");
    attach_module_docs(env, super::docs::TCP_MD);
}
