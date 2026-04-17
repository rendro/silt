//! Type signatures for the `bytes` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // The opaque Bytes type. Forward-compat: when promoted to a
    // language-level Type::Bytes, only this construction site changes.
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // bytes.empty: () -> Bytes
    env.define(
        intern("bytes.empty"),
        Scheme::mono(Type::Fun(vec![], Box::new(bytes_ty.clone()))),
    );

    // bytes.from_string: String -> Bytes
    env.define(
        intern("bytes.from_string"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(bytes_ty.clone()))),
    );

    // bytes.to_string: Bytes -> Result(String, String)
    env.define(
        intern("bytes.to_string"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone()],
            Box::new(result(Type::String, Type::String)),
        )),
    );

    // bytes.from_hex: String -> Result(Bytes, String)
    env.define(
        intern("bytes.from_hex"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // bytes.to_hex: Bytes -> String
    env.define(
        intern("bytes.to_hex"),
        Scheme::mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::String))),
    );

    // bytes.from_base64: String -> Result(Bytes, String)
    env.define(
        intern("bytes.from_base64"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // bytes.to_base64: Bytes -> String
    env.define(
        intern("bytes.to_base64"),
        Scheme::mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::String))),
    );

    // bytes.from_list: List(Int) -> Result(Bytes, String)
    env.define(
        intern("bytes.from_list"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::Int))],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // bytes.to_list: Bytes -> List(Int)
    env.define(
        intern("bytes.to_list"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone()],
            Box::new(Type::List(Box::new(Type::Int))),
        )),
    );

    // bytes.length: Bytes -> Int
    env.define(
        intern("bytes.length"),
        Scheme::mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::Int))),
    );

    // bytes.slice: (Bytes, Int, Int) -> Result(Bytes, String)
    env.define(
        intern("bytes.slice"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), Type::Int, Type::Int],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // bytes.concat: (Bytes, Bytes) -> Bytes
    env.define(
        intern("bytes.concat"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(bytes_ty.clone()),
        )),
    );

    // bytes.concat_all: List(Bytes) -> Bytes
    env.define(
        intern("bytes.concat_all"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(bytes_ty.clone()))],
            Box::new(bytes_ty.clone()),
        )),
    );

    // bytes.get: (Bytes, Int) -> Result(Int, String)
    env.define(
        intern("bytes.get"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), Type::Int],
            Box::new(result(Type::Int, Type::String)),
        )),
    );

    // bytes.eq: (Bytes, Bytes) -> Bool
    env.define(
        intern("bytes.eq"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty],
            Box::new(Type::Bool),
        )),
    );
}
