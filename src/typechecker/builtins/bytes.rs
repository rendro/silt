//! Type signatures for the `bytes` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::{attach_module_docs, attach_module_overview};

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // The opaque Bytes type. Forward-compat: when promoted to a
    // language-level Type::Bytes, only this construction site changes.
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let bytes_error_ty = Type::Generic(intern("BytesError"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // bytes.empty: () -> Bytes
    env.define(
        intern("bytes.empty"),
        Scheme::pure_mono(Type::Fun(vec![], Box::new(bytes_ty.clone()))),
    );

    // bytes.from_string: String -> Bytes
    env.define(
        intern("bytes.from_string"),
        Scheme::pure_mono(Type::Fun(vec![Type::String], Box::new(bytes_ty.clone()))),
    );

    // bytes.to_string: Bytes -> Result(String, String)
    env.define(
        intern("bytes.to_string"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone()],
            Box::new(result(Type::String, bytes_error_ty.clone())),
        )),
    );

    // bytes.from_hex: String -> Result(Bytes, String)
    env.define(
        intern("bytes.from_hex"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::String],
            Box::new(result(bytes_ty.clone(), bytes_error_ty.clone())),
        )),
    );

    // bytes.to_hex: Bytes -> String
    env.define(
        intern("bytes.to_hex"),
        Scheme::pure_mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::String))),
    );

    // bytes.from_base64: String -> Result(Bytes, String)
    env.define(
        intern("bytes.from_base64"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::String],
            Box::new(result(bytes_ty.clone(), bytes_error_ty.clone())),
        )),
    );

    // bytes.to_base64: Bytes -> String
    env.define(
        intern("bytes.to_base64"),
        Scheme::pure_mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::String))),
    );

    // bytes.from_list: List(Int) -> Result(Bytes, String)
    env.define(
        intern("bytes.from_list"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::List(Box::new(Type::Int))],
            Box::new(result(bytes_ty.clone(), bytes_error_ty.clone())),
        )),
    );

    // bytes.to_list: Bytes -> List(Int)
    env.define(
        intern("bytes.to_list"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone()],
            Box::new(Type::List(Box::new(Type::Int))),
        )),
    );

    // bytes.length: Bytes -> Int
    env.define(
        intern("bytes.length"),
        Scheme::pure_mono(Type::Fun(vec![bytes_ty.clone()], Box::new(Type::Int))),
    );

    // bytes.slice: (Bytes, Int, Int) -> Result(Bytes, String)
    env.define(
        intern("bytes.slice"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), Type::Int, Type::Int],
            Box::new(result(bytes_ty.clone(), bytes_error_ty.clone())),
        )),
    );

    // bytes.concat: (Bytes, Bytes) -> Bytes
    env.define(
        intern("bytes.concat"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(bytes_ty.clone()),
        )),
    );

    // bytes.concat_all: List(Bytes) -> Bytes
    env.define(
        intern("bytes.concat_all"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::List(Box::new(bytes_ty.clone()))],
            Box::new(bytes_ty.clone()),
        )),
    );

    // bytes.get: (Bytes, Int) -> Result(Int, String)
    env.define(
        intern("bytes.get"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), Type::Int],
            Box::new(result(Type::Int, bytes_error_ty.clone())),
        )),
    );

    // bytes.eq: (Bytes, Bytes) -> Bool
    env.define(
        intern("bytes.eq"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(Type::Bool),
        )),
    );

    // bytes.index_of: (Bytes, Bytes) -> Option(Int)
    env.define(
        intern("bytes.index_of"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(Type::Generic(intern("Option"), vec![Type::Int])),
        )),
    );

    // bytes.starts_with: (Bytes, Bytes) -> Bool
    env.define(
        intern("bytes.starts_with"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(Type::Bool),
        )),
    );

    // bytes.ends_with: (Bytes, Bytes) -> Bool
    env.define(
        intern("bytes.ends_with"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(Type::Bool),
        )),
    );

    // bytes.split: (Bytes, Bytes) -> List(Bytes)
    env.define(
        intern("bytes.split"),
        Scheme::pure_mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(Type::List(Box::new(bytes_ty))),
        )),
    );

    attach_module_overview(env, super::docs::BYTES_MD, "bytes");
    attach_module_docs(env, super::docs::BYTES_MD);
}
