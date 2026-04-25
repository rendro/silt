//! Type signatures for the `http` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::{attach_module_docs, attach_module_overview};

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // ── HTTP module type definitions ─────────────────────────────

    // Method enum
    let method_ty = Type::Generic(intern("Method"), vec![]);

    checker.enums.insert(
        intern("Method"),
        EnumInfo {
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
    let method_variants = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    for variant in method_variants {
        checker
            .variant_to_enum
            .insert(intern(variant), intern("Method"));
        env.define(intern(variant), Scheme::mono(method_ty.clone()));
    }
    crate::value::register_variant_decl_order(method_variants);

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

    let http_error_ty = Type::Generic(intern("HttpError"), vec![]);
    let result_response = Type::Generic(intern("Result"), vec![response_ty.clone(), http_error_ty]);

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

    // http.parse_query: (String) -> Map(String, List(String))
    // Parses a URL query string (with or without a leading `?`) into a
    // multi-value map. Repeated keys accumulate into the same list in
    // encounter order. An empty input yields the empty map.
    env.define(
        intern("http.parse_query"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Map(
                Box::new(Type::String),
                Box::new(Type::List(Box::new(Type::String))),
            )),
        )),
    );

    attach_module_overview(env, super::docs::HTTP_MD, "http");
    attach_module_docs(env, super::docs::HTTP_MD);
}
