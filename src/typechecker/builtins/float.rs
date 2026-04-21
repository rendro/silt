//! Type signatures for the `float` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // float.parse: (String) -> Result(Float, String)
    env.define(
        intern("float.parse"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Float, Type::String],
            )),
        )),
    );

    // float.round: (Float) -> Float
    env.define(
        intern("float.round"),
        Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
    );

    // float.ceil: (Float) -> Float
    env.define(
        intern("float.ceil"),
        Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
    );

    // float.floor: (Float) -> Float
    env.define(
        intern("float.floor"),
        Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
    );

    // float.abs: (Float) -> Float
    env.define(
        intern("float.abs"),
        Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
    );

    // float.min: (Float, Float) -> Float
    env.define(
        intern("float.min"),
        Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        )),
    );

    // float.max: (Float, Float) -> Float
    env.define(
        intern("float.max"),
        Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        )),
    );

    // float.clamp: (Float, Float, Float) -> Float
    // Panics if lo > hi. Output is unspecified for NaN inputs (Float is
    // guaranteed finite by construction, so NaN shouldn't reach this
    // function through normal typed code paths).
    env.define(
        intern("float.clamp"),
        Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float, Type::Float],
            Box::new(Type::Float),
        )),
    );

    // float.is_finite: (ExtFloat) -> Bool
    // Predicates take ExtFloat because Float is guaranteed finite — you
    // can only get a NaN/Inf value as ExtFloat. Defining a Float overload
    // would be misleading ("why am I checking?").
    env.define(
        intern("float.is_finite"),
        Scheme::mono(Type::Fun(vec![Type::ExtFloat], Box::new(Type::Bool))),
    );

    // float.is_infinite: (ExtFloat) -> Bool
    env.define(
        intern("float.is_infinite"),
        Scheme::mono(Type::Fun(vec![Type::ExtFloat], Box::new(Type::Bool))),
    );

    // float.is_nan: (ExtFloat) -> Bool
    env.define(
        intern("float.is_nan"),
        Scheme::mono(Type::Fun(vec![Type::ExtFloat], Box::new(Type::Bool))),
    );

    // float.to_string: (Float, Int) -> String
    // The second argument (decimal places) is optional at runtime: the
    // 1-arg form uses a shortest round-trippable representation, and
    // the 2-arg form formats with a fixed number of decimal places.
    // Registering the 2-arg form lets the typechecker validate both
    // arguments; the 1-arg call still passes the arity check because
    // module-qualified calls go through FieldAccess which permits one
    // fewer argument (for optional trailing params on test.assert* and
    // float.to_string), and the runtime honours that tolerance to match.
    env.define(
        intern("float.to_string"),
        Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Int],
            Box::new(Type::String),
        )),
    );

    // float.to_int: (Float) -> Int
    env.define(
        intern("float.to_int"),
        Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Int))),
    );
}
