//! Type signatures for the `io` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs_filtered;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // io.inspect: a -> String
    //
    // Pure formatter — runs `Value::format_silt()` and never touches any
    // OS resource. Distinct from `println` / `print`, which DO write to
    // stdout. Effect set: `!{}`.
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("io.inspect"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
                constraints: vec![],
                effects: EffectSet::pure(),
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
        Scheme::with_effects(
            Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::String, io_error_ty.clone()],
                )),
            ),
            EffectSet::io_fs(),
        ),
    );

    // io.write_file: (String, String) -> Result((), IoError)
    env.define(
        intern("io.write_file"),
        Scheme::with_effects(
            Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::Unit, io_error_ty.clone()],
                )),
            ),
            EffectSet::io_fs(),
        ),
    );

    // io.read_line: () -> Result(String, IoError)
    // Reads from stdin — that's an OS-resource interaction (`!{io}`),
    // not specifically filesystem.
    env.define(
        intern("io.read_line"),
        Scheme::with_effects(
            Type::Fun(
                vec![],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::String, io_error_ty],
                )),
            ),
            EffectSet::io(),
        ),
    );

    // io.args: () -> List(String)
    // Reads process argv — env-like OS-resource read; `!{io}`.
    env.define(
        intern("io.args"),
        Scheme::with_effects(
            Type::Fun(vec![], Box::new(Type::List(Box::new(Type::String)))),
            EffectSet::io(),
        ),
    );

    attach_module_docs_filtered(env, super::docs::IO_FS_MD, "io");
}
