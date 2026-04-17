//! Type signatures for the `fs` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(_checker: &mut TypeChecker, env: &mut TypeEnv) {
    // fs.exists / fs.is_file / fs.is_dir: (String) -> Bool
    let string_to_bool = Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool)));
    for name in &["fs.exists", "fs.is_file", "fs.is_dir"] {
        env.define(intern(name), string_to_bool.clone());
    }

    // fs.list_dir: (String) -> Result(List(String), String)
    env.define(
        intern("fs.list_dir"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::List(Box::new(Type::String)), Type::String],
            )),
        )),
    );

    // fs.mkdir / fs.remove: (String) -> Result(Unit, String)
    let string_to_result = Scheme::mono(Type::Fun(
        vec![Type::String],
        Box::new(Type::Generic(
            intern("Result"),
            vec![Type::Unit, Type::String],
        )),
    ));
    for name in &["fs.mkdir", "fs.remove"] {
        env.define(intern(name), string_to_result.clone());
    }

    // fs.rename / fs.copy: (String, String) -> Result(Unit, String)
    let ss_to_result = Scheme::mono(Type::Fun(
        vec![Type::String, Type::String],
        Box::new(Type::Generic(
            intern("Result"),
            vec![Type::Unit, Type::String],
        )),
    ));
    for name in &["fs.rename", "fs.copy"] {
        env.define(intern(name), ss_to_result.clone());
    }
}
