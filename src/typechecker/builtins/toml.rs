//! Type signatures for the `toml` builtin module.
//!
//! Mirrors the `json` signatures registered inline in
//! `src/typechecker/builtins.rs`. Each `parse*` takes the source string as
//! the data argument and the target type as a `type a` parameter, lowered
//! here to a `TypeOf(a)` descriptor. Type params come last so pipelines
//! compose naturally.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    let _ = checker; // no per-type record state to register for toml

    // toml.parse: (String, type a) -> Result(a, String)
    {
        let (a, av) = checker.fresh_tv();
        let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
        let result_ty = Type::Generic(intern("Result"), vec![a, Type::String]);
        env.define(
            intern("toml.parse"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                constraints: vec![],
            },
        );
    }

    // toml.parse_list: (String, type a) -> Result(List(a), String)
    {
        let (a, av) = checker.fresh_tv();
        let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
        let result_ty = Type::Generic(
            intern("Result"),
            vec![Type::List(Box::new(a)), Type::String],
        );
        env.define(
            intern("toml.parse_list"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                constraints: vec![],
            },
        );
    }

    // toml.parse_map: (String, type v) -> Result(Map(String, v), String)
    {
        let (a, av) = checker.fresh_tv();
        let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
        let result_ty = Type::Generic(
            intern("Result"),
            vec![Type::Map(Box::new(Type::String), Box::new(a)), Type::String],
        );
        env.define(
            intern("toml.parse_map"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                constraints: vec![],
            },
        );
    }

    // toml.stringify: (a) -> Result(String, String)
    //
    // Unlike json.stringify (infallible), TOML cannot represent many values
    // at the top level (it requires a table) so we surface failures as
    // Result to keep the API honest.
    {
        let (a, av) = checker.fresh_tv();
        let result_ty = Type::Generic(intern("Result"), vec![Type::String, Type::String]);
        env.define(
            intern("toml.stringify"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(result_ty)),
                constraints: vec![],
            },
        );
    }

    // toml.pretty: (a) -> Result(String, String)
    {
        let (a, av) = checker.fresh_tv();
        let result_ty = Type::Generic(intern("Result"), vec![Type::String, Type::String]);
        env.define(
            intern("toml.pretty"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(result_ty)),
                constraints: vec![],
            },
        );
    }
}
