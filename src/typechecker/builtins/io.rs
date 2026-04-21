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

    // Every fallible io/fs call now surfaces `Err(IoError)` so downstream
    // match arms get a typed enum they can destructure, and can fall back
    // to `e.message()` via `trait Error for IoError` when they don't care
    // about the specific variant. Phase 1 of the stdlib error redesign.
    let io_error_ty = Type::Generic(intern("IoError"), vec![]);

    // io.read_file: (String) -> Result(String, IoError)
    env.define(
        intern("io.read_file"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::String, io_error_ty.clone()],
            )),
        )),
    );

    // io.write_file: (String, String) -> Result((), IoError)
    env.define(
        intern("io.write_file"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Unit, io_error_ty.clone()],
            )),
        )),
    );

    // io.read_line: () -> Result(String, IoError)
    env.define(
        intern("io.read_line"),
        Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::String, io_error_ty],
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
