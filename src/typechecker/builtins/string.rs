//! Type signatures for the `string` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // string.from: (a) -> String
    {
        let (a, av) = checker.fresh_tv();
        env.define(
            intern("string.from"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
                constraints: vec![],
            },
        );
    }

    // string.split: (String, String) -> List(String)
    env.define(
        intern("string.split"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )),
    );

    // string.join: (List(String), String) -> String
    env.define(
        intern("string.join"),
        Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::String)), Type::String],
            Box::new(Type::String),
        )),
    );

    // string.trim: (String) -> String
    env.define(
        intern("string.trim"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
    );

    // string.trim_start: (String) -> String
    env.define(
        intern("string.trim_start"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
    );

    // string.trim_end: (String) -> String
    env.define(
        intern("string.trim_end"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
    );

    // string.char_code: (String) -> Int
    env.define(
        intern("string.char_code"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
    );

    // string.from_char_code: (Int) -> String
    env.define(
        intern("string.from_char_code"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
    );

    // string.contains: (String, String) -> Bool
    env.define(
        intern("string.contains"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )),
    );

    // string.replace: (String, String, String) -> String
    env.define(
        intern("string.replace"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String, Type::String],
            Box::new(Type::String),
        )),
    );

    // string.length: (String) -> Int
    env.define(
        intern("string.length"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
    );

    // string.byte_length: (String) -> Int
    env.define(
        intern("string.byte_length"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
    );

    // string.to_upper: (String) -> String
    env.define(
        intern("string.to_upper"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
    );

    // string.to_lower: (String) -> String
    env.define(
        intern("string.to_lower"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
    );

    // string.starts_with: (String, String) -> Bool
    env.define(
        intern("string.starts_with"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )),
    );

    // string.ends_with: (String, String) -> Bool
    env.define(
        intern("string.ends_with"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )),
    );

    // string.chars: (String) -> List(String)
    env.define(
        intern("string.chars"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )),
    );

    // string.repeat: (String, Int) -> String
    env.define(
        intern("string.repeat"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int],
            Box::new(Type::String),
        )),
    );

    // string.index_of: (String, String) -> Option(Int)
    env.define(
        intern("string.index_of"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(intern("Option"), vec![Type::Int])),
        )),
    );

    // string.last_index_of: (String, String) -> Option(Int)
    env.define(
        intern("string.last_index_of"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(intern("Option"), vec![Type::Int])),
        )),
    );

    // string.split_at: (String, Int) -> (String, String)
    env.define(
        intern("string.split_at"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int],
            Box::new(Type::Tuple(vec![Type::String, Type::String])),
        )),
    );

    // string.lines: (String) -> List(String)
    env.define(
        intern("string.lines"),
        Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )),
    );

    // string.starts_with_at: (String, Int, String) -> Bool
    env.define(
        intern("string.starts_with_at"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::String],
            Box::new(Type::Bool),
        )),
    );

    // string.slice: (String, Int, Int) -> String
    env.define(
        intern("string.slice"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::Int],
            Box::new(Type::String),
        )),
    );

    // string.pad_left: (String, Int, String) -> String
    env.define(
        intern("string.pad_left"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::String],
            Box::new(Type::String),
        )),
    );

    // string.pad_right: (String, Int, String) -> String
    env.define(
        intern("string.pad_right"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::String],
            Box::new(Type::String),
        )),
    );

    // string.is_empty: (String) -> Bool
    env.define(
        intern("string.is_empty"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_alpha: (String) -> Bool
    env.define(
        intern("string.is_alpha"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_digit: (String) -> Bool
    env.define(
        intern("string.is_digit"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_upper: (String) -> Bool
    env.define(
        intern("string.is_upper"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_lower: (String) -> Bool
    env.define(
        intern("string.is_lower"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_alnum: (String) -> Bool
    env.define(
        intern("string.is_alnum"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    // string.is_whitespace: (String) -> Bool
    env.define(
        intern("string.is_whitespace"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
    );

    attach_module_docs(env, super::docs::STRING_MD);
}
