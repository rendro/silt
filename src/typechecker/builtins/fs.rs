//! Type signatures for the `fs` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // fs.exists / fs.is_file / fs.is_dir / fs.is_symlink: (String) -> Bool
    let string_to_bool = Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool)));
    for name in &["fs.exists", "fs.is_file", "fs.is_dir", "fs.is_symlink"] {
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

    // ── FileStat record ─────────────────────────────────────────────
    //
    // Exposed as a nominal record so silt user code can pattern-match
    // fields (e.g. `stat.size`, `stat.is_file`). Mirrors the pattern
    // used by `time.{Instant,Date,DateTime}` and `http.{Request,Response}`:
    // register the record type for field typechecking, then use the
    // resulting `Type::Record` in the function signatures below.
    let file_stat_fields = vec![
        (intern("size"), Type::Int),
        (intern("is_file"), Type::Bool),
        (intern("is_dir"), Type::Bool),
        (intern("is_symlink"), Type::Bool),
        (intern("modified"), Type::Int),
        (intern("readonly"), Type::Bool),
    ];
    let file_stat_ty = Type::Record(intern("FileStat"), file_stat_fields.clone());
    checker.records.insert(
        intern("FileStat"),
        RecordInfo {
            _name: intern("FileStat"),
            _params: vec![],
            fields: file_stat_fields,
        },
    );
    super::super::register_auto_derived_impls_for(
        checker,
        &["FileStat"],
        super::super::BUILTIN_TRAIT_NAMES,
    );

    // fs.stat: (String) -> Result(FileStat, String)
    env.define(
        intern("fs.stat"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![file_stat_ty, Type::String],
            )),
        )),
    );

    // fs.read_link: (String) -> Result(String, String)
    env.define(
        intern("fs.read_link"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::String, Type::String],
            )),
        )),
    );

    // fs.walk: (String) -> Result(List(String), String)
    // fs.glob: (String) -> Result(List(String), String)
    let string_to_result_list_string = Scheme::mono(Type::Fun(
        vec![Type::String],
        Box::new(Type::Generic(
            intern("Result"),
            vec![Type::List(Box::new(Type::String)), Type::String],
        )),
    ));
    for name in &["fs.walk", "fs.glob"] {
        env.define(intern(name), string_to_result_list_string.clone());
    }
}
