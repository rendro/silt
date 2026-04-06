/// Module system utilities.

/// Known builtin module names whose functions are registered as `module.func`
/// in the global environment rather than loaded from files.
const BUILTIN_MODULES: &[&str] = &[
    "io", "string", "int", "float", "list", "map", "result", "option", "test", "channel", "task",
    "regex", "json", "set", "math",
];

/// Returns true if `name` is a builtin module (io, string, int, etc.).
pub fn is_builtin_module(name: &str) -> bool {
    BUILTIN_MODULES.contains(&name)
}

/// Returns the module that must be imported for a gated constructor to be available.
/// Returns `None` for prelude constructors (Ok, Err, Some, None) that are always available.
pub fn gated_constructor_module(name: &str) -> Option<&'static str> {
    match name {
        "Stop" | "Continue" => Some("list"),
        "Message" | "Closed" | "Empty" => Some("channel"),
        _ => None,
    }
}

/// Returns the list of builtin function suffixes for a given builtin module.
/// E.g., for "string" returns ["split", "trim", "trim_start", ...].
pub fn builtin_module_functions(module: &str) -> Vec<&'static str> {
    match module {
        "string" => vec![
            "from",
            "split", "trim", "trim_start", "trim_end",
            "char_code", "from_char_code",
            "contains", "replace", "join",
            "length", "to_upper", "to_lower",
            "starts_with", "ends_with", "chars",
            "repeat", "index_of", "slice",
            "pad_left", "pad_right",
            "is_empty", "is_alpha", "is_digit",
            "is_upper", "is_lower", "is_alnum",
            "is_whitespace",
        ],
        "list" => vec![
            "map", "filter", "each", "fold",
            "find", "zip", "flatten", "sort_by",
            "flat_map", "filter_map", "any", "all",
            "fold_until", "unfold",
            "head", "tail", "last", "reverse",
            "sort", "unique", "contains", "length",
            "append", "prepend", "concat",
            "get", "set", "take", "drop",
            "enumerate", "group_by",
        ],
        "map" => vec![
            "get", "set", "delete", "contains",
            "keys", "values", "length", "merge",
            "filter", "map", "entries", "from_entries",
            "each", "update",
        ],
        "io" => vec!["read_file", "write_file", "read_line", "args"],
        "int" => vec!["parse", "abs", "min", "max", "to_float", "to_string"],
        "float" => vec![
            "parse", "round", "ceil", "floor",
            "abs", "to_string", "to_int",
            "min", "max",
        ],
        "result" => vec![
            "unwrap_or", "map_ok", "map_err",
            "flatten", "flat_map", "is_ok", "is_err",
        ],
        "option" => vec![
            "map", "unwrap_or", "to_result",
            "is_some", "is_none", "flat_map",
        ],
        "test" => vec!["assert", "assert_eq", "assert_ne"],
        "math" => vec![
            "sqrt", "pow", "log", "log10",
            "sin", "cos", "tan",
            "asin", "acos", "atan", "atan2",
        ],
        "regex" => vec![
            "is_match", "find", "find_all", "split",
            "replace", "replace_all", "replace_all_with",
            "captures", "captures_all",
        ],
        "json" => vec![
            "parse", "parse_list", "parse_map",
            "stringify", "pretty",
        ],
        "channel" => vec![
            "new", "send", "receive", "close",
            "try_send", "try_receive", "select", "each",
        ],
        "task" => vec!["spawn", "join", "cancel"],
        "set" => vec![
            "new", "from_list", "to_list", "contains",
            "insert", "remove", "length",
            "union", "intersection", "difference", "is_subset",
            "map", "filter", "each", "fold",
        ],
        _ => vec![],
    }
}
