//! Type signatures for the `io` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // io.inspect: a -> String
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("io.inspect"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
                constraints: vec![],
            },
        );
    }

    // io.read_file: (String) -> Result(String, String)
    env.define(
        intern("io.read_file"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::String, Type::String],
            )),
        )),
    );

    // io.write_file: (String, String) -> Result((), String)
    env.define(
        intern("io.write_file"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Unit, Type::String],
            )),
        )),
    );

    // io.read_line: () -> Result(String, String)
    env.define(
        intern("io.read_line"),
        Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::String, Type::String],
            )),
        )),
    );

    // io.args: () -> List(String)
    env.define(
        intern("io.args"),
        Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::List(Box::new(Type::String))),
        )),
    );
}
