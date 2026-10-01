//! Type signatures for the `math` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // (Float) -> Float. The ones with a restricted domain or a result
    // that can overflow (sqrt, log, log10, asin, acos, exp, tan) raise
    // at runtime rather than produce a non-finite value.
    {
        let float_to_float = Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float)));
        for name in &[
            "math.sqrt",
            "math.log",
            "math.log10",
            "math.asin",
            "math.acos",
            "math.exp",
            "math.sin",
            "math.cos",
            "math.tan",
            "math.atan",
        ] {
            env.define(intern(name), float_to_float.clone());
        }
    }

    // math.pow: (Float, Float) -> Float (raises on overflow or a
    // non-real result)
    {
        let ff_to_f = Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        ));
        env.define(intern("math.pow"), ff_to_f);
    }

    // math.atan2: (Float, Float) -> Float (always finite)
    {
        let ff_to_f = Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        ));
        env.define(intern("math.atan2"), ff_to_f);
    }

    // math.random: () -> Float
    // Reads OS entropy via the rand crate.
    env.define(
        intern("math.random"),
        Scheme::mono(Type::Fun(vec![], Box::new(Type::Float))),
    );

    // Math constants. Float constants moved to `float.rs` so the
    // overview-attach there can see them — `register_float_builtins`
    // runs before `register_math_builtins`, and `attach_module_overview`
    // walks `env.bindings` at call time.
    env.define(intern("math.pi"), Scheme::mono(Type::Float));
    env.define(intern("math.e"), Scheme::mono(Type::Float));

    attach_module_docs(env, super::docs::MATH_MD);
}
