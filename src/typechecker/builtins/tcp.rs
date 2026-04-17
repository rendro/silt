//! Type signatures for the `tcp` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    let listener_ty = Type::Generic(intern("TcpListener"), vec![]);
    let stream_ty = Type::Generic(intern("TcpStream"), vec![]);
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // tcp.listen: String -> Result(TcpListener, String)
    env.define(
        intern("tcp.listen"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(listener_ty.clone(), Type::String)),
        )),
    );

    // tcp.accept: TcpListener -> Result(TcpStream, String)
    env.define(
        intern("tcp.accept"),
        Scheme::mono(Type::Fun(
            vec![listener_ty],
            Box::new(result(stream_ty.clone(), Type::String)),
        )),
    );

    // tcp.connect: String -> Result(TcpStream, String)
    env.define(
        intern("tcp.connect"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(stream_ty.clone(), Type::String)),
        )),
    );

    // tcp.read: (TcpStream, Int) -> Result(Bytes, String)
    env.define(
        intern("tcp.read"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Int],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // tcp.read_exact: (TcpStream, Int) -> Result(Bytes, String)
    env.define(
        intern("tcp.read_exact"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Int],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // tcp.write: (TcpStream, Bytes) -> Result(Unit, String)
    env.define(
        intern("tcp.write"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), bytes_ty],
            Box::new(result(Type::Unit, Type::String)),
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
            Box::new(result(Type::String, Type::String)),
        )),
    );

    // tcp.set_nodelay: (TcpStream, Bool) -> Result(Unit, String)
    env.define(
        intern("tcp.set_nodelay"),
        Scheme::mono(Type::Fun(
            vec![stream_ty.clone(), Type::Bool],
            Box::new(result(Type::Unit, Type::String)),
        )),
    );

    #[cfg(feature = "tcp-tls")]
    {
        let listener_ty_tls = Type::Generic(intern("TcpListener"), vec![]);
        let bytes_ty_tls = Type::Generic(intern("Bytes"), vec![]);
        // tcp.connect_tls: (String, String) -> Result(TcpStream, String)
        env.define(
            intern("tcp.connect_tls"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(result(stream_ty.clone(), Type::String)),
            )),
        );
        // tcp.accept_tls: (TcpListener, Bytes, Bytes) -> Result(TcpStream, String)
        env.define(
            intern("tcp.accept_tls"),
            Scheme::mono(Type::Fun(
                vec![listener_ty_tls, bytes_ty_tls.clone(), bytes_ty_tls],
                Box::new(result(stream_ty, Type::String)),
            )),
        );
    }
}
