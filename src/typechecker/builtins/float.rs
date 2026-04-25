//! Type signatures for the `float` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::{attach_module_docs_filtered, attach_module_overview};

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // float.parse: (String) -> Result(Float, ParseError)
    //
    // Phase 1 of the stdlib error redesign — shares `ParseError` with
    // `int.parse`. See the note on `int.parse` for rationale.
    env.define(
        intern("float.parse"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Float, Type::Generic(intern("ParseError"), vec![])],
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

    // Float constants. Moved from `math.rs` (round 62 phase-2) so
    // `attach_module_overview` below can see them — `register_float_builtins`
    // runs before `register_math_builtins`, and the overview walks
    // `env.bindings` at call time.
    env.define(intern("float.max_value"), Scheme::mono(Type::Float));
    env.define(intern("float.min_value"), Scheme::mono(Type::Float));
    env.define(intern("float.epsilon"), Scheme::mono(Type::Float));
    env.define(intern("float.min_positive"), Scheme::mono(Type::Float));
    env.define(intern("float.infinity"), Scheme::mono(Type::ExtFloat));
    env.define(intern("float.neg_infinity"), Scheme::mono(Type::ExtFloat));
    env.define(intern("float.nan"), Scheme::mono(Type::ExtFloat));

    // The `## Float Constants` section in int-float.md does not
    // map to per-name `## float.epsilon` headings, so first
    // overview-attach the whole markdown to every float.* binding,
    // then let attach_module_docs_filtered overwrite for the
    // function names that DO have per-section bodies.
    attach_module_overview(env, super::docs::INT_FLOAT_MD, "float");
    attach_module_docs_filtered(env, super::docs::INT_FLOAT_MD, "float");
}
