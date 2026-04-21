//! Type signatures for the `int` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // int.parse: (String) -> Result(Int, ParseError)
    //
    // Phase 1 of the stdlib error redesign: `Err` surfaces a typed
    // `ParseError` enum (variants: `ParseEmpty`, `ParseInvalidDigit(Int)`,
    // `ParseOverflow`, `ParseUnderflow`) so callers can pattern-match on
    // the specific failure mode, and can still fall back to
    // `e.message()` via `trait Error for ParseError` when they don't
    // care about the variant.
    env.define(
        intern("int.parse"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Int, Type::Generic(intern("ParseError"), vec![])],
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
