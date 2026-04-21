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

    // encoding.form_encode: List((String, String)) -> String
    //
    // Produces an `application/x-www-form-urlencoded` body: each pair
    // becomes `key=value`, values (and keys) are percent-escaped per
    // RFC 3986, and pairs are joined by `&`. Ordering of the input list
    // is preserved in the output so callers can build deterministic
    // signatures (HMAC, S3-style canonical query strings, etc.). An
    // empty list produces the empty string.
    let pair_list = Type::List(Box::new(Type::Tuple(vec![Type::String, Type::String])));
    env.define(
        intern("encoding.form_encode"),
        Scheme::mono(Type::Fun(vec![pair_list.clone()], Box::new(Type::String))),
    );

    // encoding.form_decode: String -> Result(List((String, String)), String)
    //
    // Parses an `application/x-www-form-urlencoded` body back into a
    // list of `(key, value)` pairs, preserving input order. A value
    // without an `=` sign becomes `(key, "")`. Malformed percent escapes
    // or non-UTF-8 decoded bytes surface as `Err(msg)`.
    env.define(
        intern("encoding.form_decode"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result(pair_list, Type::String)),
        )),
    );
}
