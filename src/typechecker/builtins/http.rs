//! Type signatures for the `http` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // ── HTTP module type definitions ─────────────────────────────

    // Method enum
    let method_ty = Type::Generic(intern("Method"), vec![]);

    checker.enums.insert(
        intern("Method"),
        EnumInfo {
            _name: intern("Method"),
            params: vec![],
            param_var_ids: vec![],
            variants: vec![
                VariantInfo {
                    name: intern("GET"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("POST"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("PUT"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("PATCH"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("DELETE"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("HEAD"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("OPTIONS"),
                    field_types: vec![],
                },
            ],
        },
    );
    for variant in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
        checker
            .variant_to_enum
            .insert(intern(variant), intern("Method"));
        env.define(intern(variant), Scheme::mono(method_ty.clone()));
    }

    // Response record
    let map_ss = Type::Map(Box::new(Type::String), Box::new(Type::String));

    let response_ty = Type::Record(
        intern("Response"),
        vec![
            (intern("status"), Type::Int),
            (intern("body"), Type::String),
            (intern("headers"), map_ss.clone()),
        ],
    );

    checker.records.insert(
        intern("Response"),
        RecordInfo {
            _name: intern("Response"),
            _params: vec![],
            fields: vec![
                (intern("status"), Type::Int),
                (intern("body"), Type::String),
                (intern("headers"), map_ss.clone()),
            ],
        },
    );

    // Request record
    let request_ty = Type::Record(
        intern("Request"),
        vec![
            (intern("method"), method_ty.clone()),
            (intern("path"), Type::String),
            (intern("query"), Type::String),
            (intern("headers"), map_ss.clone()),
            (intern("body"), Type::String),
        ],
    );

    checker.records.insert(
        intern("Request"),
        RecordInfo {
            _name: intern("Request"),
            _params: vec![],
            fields: vec![
                (intern("method"), method_ty.clone()),
                (intern("path"), Type::String),
                (intern("query"), Type::String),
                (intern("headers"), map_ss.clone()),
                (intern("body"), Type::String),
            ],
        },
    );

    // ── Function signatures ──────────────────────────────────────

    let result_response = Type::Generic(intern("Result"), vec![response_ty.clone(), Type::String]);

    // http.get: (String) -> Result(Response, String)
    env.define(
        intern("http.get"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(result_response.clone()),
        )),
    );

    // http.request: (Method, String, String, Map(String, String)) -> Result(Response, String)
    env.define(
        intern("http.request"),
        Scheme::mono(Type::Fun(
            vec![method_ty, Type::String, Type::String, map_ss],
            Box::new(result_response),
        )),
    );

    // http.serve: (Int, Fn(Request) -> Response) -> Unit
    // Binds 127.0.0.1:<port> (loopback only). Use `http.serve_all` to
    // expose on all interfaces (0.0.0.0).
    env.define(
        intern("http.serve"),
        Scheme::mono(Type::Fun(
            vec![
                Type::Int,
                Type::Fun(vec![request_ty.clone()], Box::new(response_ty.clone())),
            ],
            Box::new(Type::Unit),
        )),
    );

    // http.serve_all: (Int, Fn(Request) -> Response) -> Unit
    // Binds 0.0.0.0:<port> (all interfaces). Explicit opt-in counterpart
    // to `http.serve`.
    env.define(
        intern("http.serve_all"),
        Scheme::mono(Type::Fun(
            vec![
                Type::Int,
                Type::Fun(vec![request_ty], Box::new(response_ty)),
            ],
            Box::new(Type::Unit),
        )),
    );

    // http.segments: (String) -> List(String)
    env.define(
        intern("http.segments"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )),
    );
}
