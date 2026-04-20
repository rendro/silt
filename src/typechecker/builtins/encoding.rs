//! Type signatures for the `encoding` builtin module.
//!
//! Narrow surface: just URL / percent encoding. Base64 and hex live on
//! the `bytes` module because they consume / produce `Bytes`, not
//! `String`. See `src/builtins/encoding.rs` for the runtime side and
//! `docs/stdlib/encoding.md` for the user-facing rationale.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // encoding.url_encode: String -> String
    env.define(
        intern("encoding.url_encode"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )),
    );

    // encoding.url_decode: String -> Result(String, String)
    env.define(
        intern("encoding.url_decode"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(Type::String, Type::String)),
        )),
    );
}
