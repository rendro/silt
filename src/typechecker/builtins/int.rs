//! Type signatures for the `int` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // int.parse: (String) -> Result(Int, String)
    env.define(
        intern("int.parse"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Int, Type::String],
            )),
        )),
    );

    // int.abs: (Int) -> Int
    env.define(
        intern("int.abs"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Int))),
    );

    // int.min: (Int, Int) -> Int
    env.define(
        intern("int.min"),
        Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
    );

    // int.max: (Int, Int) -> Int
    env.define(
        intern("int.max"),
        Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
    );

    // int.clamp: (Int, Int, Int) -> Int
    env.define(
        intern("int.clamp"),
        Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Int),
        )),
    );

    // int.to_float: (Int) -> Float
    env.define(
        intern("int.to_float"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Float))),
    );

    // int.to_string: (Int) -> String
    env.define(
        intern("int.to_string"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
    );
}
