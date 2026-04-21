//! Type signatures for the `crypto` builtin module.
//!
//! Mirrors the shape of `src/typechecker/builtins/bytes.rs`: every
//! function that produces raw digest / MAC bytes returns an opaque
//! `Bytes` value; fallible operations return `Result(_, String)`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // The opaque Bytes type — same construction as the bytes module so
    // crypto outputs flow into bytes.to_hex, bytes.to_base64, etc.
    let bytes_ty = Type::Generic(intern("Bytes"), vec![]);
    let result = |ok_ty: Type, err_ty: Type| -> Type {
        Type::Generic(intern("Result"), vec![ok_ty, err_ty])
    };

    // Hash functions that take Bytes and return a fixed-width Bytes
    // digest. The typechecker has no dependent-width Bytes, so the
    // output width is only documented (not enforced by the type).
    //
    //  - sha256   -> 32 bytes
    //  - sha512   -> 64 bytes
    //  - md5      -> 16 bytes  (legacy / compat only — NOT collision-resistant)
    //  - blake2b  -> 64 bytes  (BLAKE2b-512, RFC 7693)
    for name in &["crypto.sha256", "crypto.sha512", "crypto.md5", "crypto.blake2b"] {
        env.define(
            intern(name),
            Scheme::mono(Type::Fun(
                vec![bytes_ty.clone()],
                Box::new(bytes_ty.clone()),
            )),
        );
    }

    // Hex-encoded convenience variants. These exist so common use
    // cases (log lines, cache keys) don't need to import `bytes` just
    // to call `bytes.to_hex` on the digest.
    for name in &["crypto.md5_hex", "crypto.blake2b_hex"] {
        env.define(
            intern(name),
            Scheme::mono(Type::Fun(
                vec![bytes_ty.clone()],
                Box::new(Type::String),
            )),
        );
    }

    // crypto.hmac_sha256: (Bytes, Bytes) -> Bytes
    env.define(
        intern("crypto.hmac_sha256"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(bytes_ty.clone()),
        )),
    );

    // crypto.hmac_sha512: (Bytes, Bytes) -> Bytes
    env.define(
        intern("crypto.hmac_sha512"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty.clone()],
            Box::new(bytes_ty.clone()),
        )),
    );

    // crypto.random_bytes: Int -> Result(Bytes, String)
    env.define(
        intern("crypto.random_bytes"),
        Scheme::mono(Type::Fun(
            vec![Type::Int],
            Box::new(result(bytes_ty.clone(), Type::String)),
        )),
    );

    // crypto.constant_time_eq: (Bytes, Bytes) -> Bool
    env.define(
        intern("crypto.constant_time_eq"),
        Scheme::mono(Type::Fun(
            vec![bytes_ty.clone(), bytes_ty],
            Box::new(Type::Bool),
        )),
    );
}
