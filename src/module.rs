/// Module system utilities.
/// Known builtin module names whose functions are registered as `module.func`
/// in the global environment rather than loaded from files.
pub const BUILTIN_MODULES: &[&str] = &[
    "io", "string", "int", "float", "list", "map", "result", "option", "test", "channel", "task",
    "regex", "json", "toml", "set", "math", "time", "http", "fs", "env", "postgres", "bytes",
    "crypto", "encoding", "tcp", "stream", "uuid",
];

/// Names of the built-in primitive type descriptors (uppercase) usable
/// as `type a` arguments and for static-style trait dispatch
/// (`Int.parse(...)`, etc.). The runtime emits each one as a
/// `Value::PrimitiveDescriptor("<Name>")` global; the typechecker
/// registers each as `TypeOf(<inner>)`.
///
/// Round-73 BLOAT-2 fix: hoisted from hand-rolled lists at
/// `src/vm/dispatch.rs::register_builtins` and
/// `src/typechecker/builtins.rs::register_builtins` so adding a new
/// primitive descriptor only requires touching this constant. Per-name
/// behavior (the `Type` mapping in the typechecker, the
/// `Value::PrimitiveDescriptor` shape in the VM) still lives at the
/// call sites — only the NAME set is hoisted to prevent drift.
///
/// Parity lock: `tests/round73_descriptor_name_parity_tests.rs`.
pub const BUILTIN_PRIMITIVE_NAMES: &[&str] = &["Int", "Float", "ExtFloat", "String", "Bool"];

/// Names of the built-in generic container type descriptors (uppercase)
/// usable as `type a` arguments and for static-style trait dispatch
/// (`List.empty()`, etc.). The runtime emits each one as a
/// `Value::TypeDescriptor("<Name>")` global; the typechecker registers
/// each as a polymorphic `TypeOf(Container(...))` scheme with arity
/// matching the container's generic parameter count.
///
/// Round-73 BLOAT-2 fix: hoisted from hand-rolled lists at
/// `src/vm/dispatch.rs::register_builtins` and
/// `src/typechecker/builtins.rs::register_builtins`. Each container
/// still has its own per-name code path (different generic arity for
/// `Map(k,v)` vs the others), but the NAME set is centralised so the
/// two sites cannot drift.
///
/// Parity lock: `tests/round73_descriptor_name_parity_tests.rs`.
pub const BUILTIN_GENERIC_CONTAINER_NAMES: &[&str] = &["List", "Map", "Set", "Channel", "Tuple"];

/// Central registry of the record / enum type NAMES the stdlib registers
/// per-module at typechecker startup (i.e. via the `register` entry
/// points in `src/typechecker/builtins/`). These are uppercase nominal
/// type names a user can write in type-annotation position but are
/// owned by the stdlib — renaming one via the LSP would silently
/// rewrite user references while leaving the builtin type intact.
///
/// Unlike [`crate::types::builtins::BUILTIN_TYPES`] (which lists
/// primitives + generic containers), this registry covers the nominal
/// types declared by per-module `register` functions:
///
/// - `fs.rs`           → `FileStat`
/// - `time.rs`         → `Instant`, `Date`, `Time`, `DateTime`,
///                       `Duration`, `Weekday`
/// - `http.rs`         → `Method`, `Response`, `Request`
/// - `errors.rs`       → `IoError`, `JsonError`, `TomlError`,
///                       `ParseError`, `HttpError`, `RegexError`,
///                       `TimeError`, `BytesError`, `ChannelError`,
///                       and the cfg-gated `PgError`, `TcpError`.
///
/// Parity lock: `tests/round82_stdlib_types_registry_tests.rs` walks
/// `checker.records.keys() ∪ checker.enums.keys()` after stdlib init
/// and asserts the set matches this registry. Adding a new stdlib
/// record/enum without updating this list will fail that test.
///
/// Consumers:
/// 1. Editor grammars (`editors/vim/syntax/silt.vim`,
///    `editors/vscode/syntaxes/silt.tmLanguage.json`) — surface these
///    names so cross-editor highlighting matches the language.
/// 2. LSP rename gate (`src/lsp/rename.rs::builtin_globals`) — reject
///    rename so stdlib types are not silently rewritten away.
pub const BUILTIN_STDLIB_TYPE_NAMES: &[&str] = &[
    // fs.rs
    "FileStat",
    // time.rs
    "Instant",
    "Date",
    "Time",
    "DateTime",
    "Duration",
    "Weekday",
    // http.rs
    "Method",
    "Response",
    "Request",
    // errors.rs (always-on)
    "IoError",
    "JsonError",
    "TomlError",
    "ParseError",
    "HttpError",
    "RegexError",
    "TimeError",
    "BytesError",
    "ChannelError",
    // errors.rs (cfg-gated) — listed but conditional. The
    // `feature_gated_stdlib_type` helper below routes each gated name
    // to its required cargo feature; the parity test in
    // `tests/round82_stdlib_types_registry_tests.rs` filters by the
    // active feature set when comparing against typechecker state.
    "PgError",
    "TcpError",
];

/// Returns the cargo feature required for the given stdlib type to be
/// registered by the typechecker, or `None` if the type is
/// unconditionally registered under default features.
///
/// Used by the round-82 parity test to filter out feature-gated names
/// when the active build does not include the relevant feature.
pub fn feature_gated_stdlib_type(name: &str) -> Option<&'static str> {
    match name {
        "PgError" => Some("postgres"),
        "TcpError" => Some("tcp"),
        _ => None,
    }
}

/// Returns true if `name` is a builtin module (io, string, int, etc.).
pub fn is_builtin_module(name: &str) -> bool {
    BUILTIN_MODULES.contains(&name)
}

/// Returns the module that must be imported for a gated constructor to be available.
/// Returns `None` for prelude constructors (Ok, Err, Some, None) that are always available.
pub fn gated_constructor_module(name: &str) -> Option<&'static str> {
    match name {
        "Stop" | "Continue" => Some("list"),
        "Message" | "Closed" | "Empty" | "Sent" => Some("channel"),
        "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday" => {
            Some("time")
        }
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" => Some("http"),
        // Stdlib typed-error enums. Each module's error variants
        // require that module to be imported before they can be
        // constructed. See
        // `module.rs::builtin_error_enum_variants_with_arity` for
        // Phase 0 background.
        "IoNotFound" | "IoPermissionDenied" | "IoAlreadyExists" | "IoInvalidInput"
        | "IoInterrupted" | "IoUnexpectedEof" | "IoWriteZero" | "IoUnknown" => Some("io"),
        "JsonSyntax" | "JsonTypeMismatch" | "JsonMissingField" | "JsonUnknown" => Some("json"),
        "TomlSyntax" | "TomlTypeMismatch" | "TomlMissingField" | "TomlUnknown" => Some("toml"),
        // ParseError is shared between int and float. Arbitrary
        // routing pick: int. Users importing `float` can still
        // destructure these variants in a match once constructed.
        "ParseEmpty" | "ParseInvalidDigit" | "ParseOverflow" | "ParseUnderflow" => Some("int"),
        "HttpConnect"
        | "HttpTls"
        | "HttpTimeout"
        | "HttpInvalidUrl"
        | "HttpInvalidResponse"
        | "HttpClosedEarly"
        | "HttpStatusCode"
        | "HttpUnknown" => Some("http"),
        "RegexInvalidPattern" | "RegexTooBig" => Some("regex"),
        "PgConnect" | "PgTls" | "PgAuthFailed" | "PgQuery" | "PgTypeMismatch"
        | "PgNoSuchColumn" | "PgClosed" | "PgTimeout" | "PgTxnAborted" | "PgUnknown" => {
            Some("postgres")
        }
        "TcpConnect" | "TcpTls" | "TcpClosed" | "TcpTimeout" | "TcpUnknown" => Some("tcp"),
        "TimeParseFormat" | "TimeOutOfRange" => Some("time"),
        "BytesInvalidUtf8"
        | "BytesInvalidHex"
        | "BytesInvalidBase64"
        | "BytesByteOutOfRange"
        | "BytesOutOfBounds" => Some("bytes"),
        "ChannelTimeout" | "ChannelClosed" => Some("channel"),
        _ => None,
    }
}

/// Returns the set of builtin enums known to the compiler as
/// `(enum_name, variant_names)` pairs. Seeds the compiler's
/// `known_enum_variants` map so `EnumName.Variant` qualifier syntax
/// compiles to a bare `GetGlobal("Variant")`, matching how the same
/// rewrite already works for user-declared enums.
///
/// Includes both prelude enums (Result, Option) and gated enums.
/// Keep in sync with `src/typechecker/builtins/errors.rs` enum registrations
/// and `src/vm/dispatch.rs` variant globals.
pub fn builtin_enum_variants() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("Result", &["Ok", "Err"]),
        ("Option", &["Some", "None"]),
        ("Step", &["Stop", "Continue"]),
        ("ChannelResult", &["Message", "Closed", "Empty", "Sent"]),
        ("ChannelOp", &["Recv", "Send"]),
        (
            "Weekday",
            &[
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ],
        ),
        (
            "Method",
            &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"],
        ),
        // Stdlib typed-error enums. See
        // `module.rs::builtin_error_enum_variants_with_arity` for
        // Phase 0 background.
        (
            "IoError",
            &[
                "IoNotFound",
                "IoPermissionDenied",
                "IoAlreadyExists",
                "IoInvalidInput",
                "IoInterrupted",
                "IoUnexpectedEof",
                "IoWriteZero",
                "IoUnknown",
            ],
        ),
        (
            "JsonError",
            &[
                "JsonSyntax",
                "JsonTypeMismatch",
                "JsonMissingField",
                "JsonUnknown",
            ],
        ),
        (
            "TomlError",
            &[
                "TomlSyntax",
                "TomlTypeMismatch",
                "TomlMissingField",
                "TomlUnknown",
            ],
        ),
        (
            "ParseError",
            &[
                "ParseEmpty",
                "ParseInvalidDigit",
                "ParseOverflow",
                "ParseUnderflow",
            ],
        ),
        (
            "HttpError",
            &[
                "HttpConnect",
                "HttpTls",
                "HttpTimeout",
                "HttpInvalidUrl",
                "HttpInvalidResponse",
                "HttpClosedEarly",
                "HttpStatusCode",
                "HttpUnknown",
            ],
        ),
        ("RegexError", &["RegexInvalidPattern", "RegexTooBig"]),
        (
            "PgError",
            &[
                "PgConnect",
                "PgTls",
                "PgAuthFailed",
                "PgQuery",
                "PgTypeMismatch",
                "PgNoSuchColumn",
                "PgClosed",
                "PgTimeout",
                "PgTxnAborted",
                "PgUnknown",
            ],
        ),
        (
            "TcpError",
            &[
                "TcpConnect",
                "TcpTls",
                "TcpClosed",
                "TcpTimeout",
                "TcpUnknown",
            ],
        ),
        ("TimeError", &["TimeParseFormat", "TimeOutOfRange"]),
        (
            "BytesError",
            &[
                "BytesInvalidUtf8",
                "BytesInvalidHex",
                "BytesInvalidBase64",
                "BytesByteOutOfRange",
                "BytesOutOfBounds",
            ],
        ),
        ("ChannelError", &["ChannelTimeout", "ChannelClosed"]),
    ]
}

/// Iterator over every builtin enum variant name across all builtin enums
/// (both prelude and gated). Used by LSP rename/completion and REPL
/// completion to keep hand-rolled parallel lists from drifting away from
/// the authoritative source at `builtin_enum_variants`.
///
/// A parity-lock test at `tests/builtin_constructor_parity_tests.rs`
/// asserts every surface that mentions builtin constructors consults
/// this helper (directly or by name-set membership).
pub fn all_builtin_constructor_names() -> impl Iterator<Item = &'static str> {
    builtin_enum_variants()
        .iter()
        .flat_map(|(_, variants)| variants.iter().copied())
}

/// Names of silt's builtin global free functions — i.e. callable without
/// a module prefix: `print(...)`, not `io.print(...)`. These are the
/// non-constructor identifiers `register_builtins` (in
/// `src/typechecker/builtins.rs`) defines unqualified at the top of the
/// type environment, plus the matching `BuiltinFn` globals seeded by
/// `src/vm/dispatch.rs::register_builtins`.
///
/// Authoritative for: REPL completion, LSP completion/rename, the
/// typechecker's "did you mean" candidate set for unqualified
/// identifiers, and VM dispatch's free-fn shadow check. Sibling shape
/// to `all_builtin_constructor_names` (round-58/64) and the
/// `KEYWORD_LITERALS`/`KEYWORDS` consolidations (round-63/64).
///
/// A parity-lock test at
/// `tests/builtin_free_function_parity_tests.rs` asserts every surface
/// that mentions these free-function names consults this helper, and
/// that the helper itself matches what's actually registered in the
/// typechecker's free-function table at runtime.
///
/// Keep alphabetic.
pub fn builtin_free_function_names() -> &'static [&'static str] {
    &["panic", "print", "println"]
}

/// Authoritative `(variant_name, arity)` listings for every stdlib
/// typed-error enum. Single source of truth consulted by
/// `src/vm/dispatch.rs::register_builtins` to seed the global
/// `VariantConstructor` / `Variant` entries — collapsing the
/// previously hand-rolled per-family loops at dispatch.rs:142-275 into
/// one data-driven loop.
///
/// Phase 0 of the stdlib error redesign (implemented and proposal
/// removed in commit 7680536) — this is the canonical doc-mention.
/// Sibling registries / dispatch arms cross-reference this helper
/// instead of repeating the wording.
///
/// A parity-lock test at
/// `tests/error_enum_dispatch_parity_tests.rs` asserts the typechecker
/// registrations in `src/typechecker/builtins/errors.rs::register`
/// match these `(variant, arity)` tuples shape-for-shape.
///
/// Round-64 DUP-1 fix (audit): adding/renaming a typed-error variant
/// previously required edits in three independent registries
/// (this file, `errors.rs`, and `dispatch.rs`). The dispatch-side list
/// is now derived; `errors.rs` remains the type-side definition and is
/// cross-checked against this data by the parity test.
pub fn builtin_error_enum_variants_with_arity()
-> &'static [(&'static str, &'static [(&'static str, usize)])] {
    &[
        (
            "IoError",
            &[
                ("IoNotFound", 1),
                ("IoPermissionDenied", 1),
                ("IoAlreadyExists", 1),
                ("IoInvalidInput", 1),
                ("IoInterrupted", 0),
                ("IoUnexpectedEof", 0),
                ("IoWriteZero", 0),
                ("IoUnknown", 1),
            ],
        ),
        (
            "JsonError",
            &[
                ("JsonSyntax", 2),
                ("JsonTypeMismatch", 2),
                ("JsonMissingField", 1),
                ("JsonUnknown", 1),
            ],
        ),
        (
            "TomlError",
            &[
                ("TomlSyntax", 2),
                ("TomlTypeMismatch", 2),
                ("TomlMissingField", 1),
                ("TomlUnknown", 1),
            ],
        ),
        (
            "ParseError",
            &[
                ("ParseEmpty", 0),
                ("ParseInvalidDigit", 1),
                ("ParseOverflow", 0),
                ("ParseUnderflow", 0),
            ],
        ),
        (
            "HttpError",
            &[
                ("HttpConnect", 1),
                ("HttpTls", 1),
                ("HttpTimeout", 0),
                ("HttpInvalidUrl", 1),
                ("HttpInvalidResponse", 1),
                ("HttpClosedEarly", 0),
                ("HttpStatusCode", 2),
                ("HttpUnknown", 1),
            ],
        ),
        (
            "RegexError",
            &[("RegexInvalidPattern", 2), ("RegexTooBig", 0)],
        ),
        (
            "PgError",
            &[
                ("PgConnect", 1),
                ("PgTls", 1),
                ("PgAuthFailed", 1),
                ("PgQuery", 2),
                ("PgTypeMismatch", 3),
                ("PgNoSuchColumn", 1),
                ("PgClosed", 0),
                ("PgTimeout", 0),
                ("PgTxnAborted", 0),
                ("PgUnknown", 1),
            ],
        ),
        (
            "TcpError",
            &[
                ("TcpConnect", 1),
                ("TcpTls", 1),
                ("TcpClosed", 0),
                ("TcpTimeout", 0),
                ("TcpUnknown", 1),
            ],
        ),
        (
            "TimeError",
            &[("TimeParseFormat", 1), ("TimeOutOfRange", 1)],
        ),
        (
            "BytesError",
            &[
                ("BytesInvalidUtf8", 1),
                ("BytesInvalidHex", 1),
                ("BytesInvalidBase64", 1),
                ("BytesByteOutOfRange", 1),
                ("BytesOutOfBounds", 1),
            ],
        ),
        (
            "ChannelError",
            &[("ChannelTimeout", 0), ("ChannelClosed", 0)],
        ),
    ]
}

/// Reverse lookup: given a variant tag (e.g. `"IoNotFound"`), return
/// the parent stdlib error enum name (e.g. `"IoError"`), or `None` if
/// the tag isn't a stdlib-error variant. Routes through the
/// authoritative `builtin_error_enum_variants_with_arity` registry, so
/// adding a new error variant requires no edit here. Used by
/// `Value::Display` to route stdlib error variants through their
/// `Error::message()` implementation rather than the default
/// constructor-form render — collapses the prior dual shape between
/// `"{e}"` and `e.message()` per the "explicit over implicit"
/// principle.
pub fn variant_to_error_enum(tag: &str) -> Option<&'static str> {
    for (enum_name, variants) in builtin_error_enum_variants_with_arity() {
        if variants.iter().any(|(v, _)| *v == tag) {
            return Some(enum_name);
        }
    }
    None
}

/// Authoritative `(variant_name, arity)` listings for every builtin
/// non-error enum: `Result`, `Option`, `Step`, `ChannelResult`,
/// `ChannelOp`, `Weekday`, `Method`. Single source of truth consulted
/// by `src/vm/dispatch.rs::register_builtins` to seed the global
/// `VariantConstructor` / `Variant` entries — collapsing the
/// previously hand-rolled per-family blocks in `register_builtins` (an
/// `insert()` pair per variant) into one data-driven loop, mirroring
/// the round-64 collapse done for the typed-error enums via
/// `builtin_error_enum_variants_with_arity`.
///
/// Round-71 PARALLEL-ARRAY-DRIFT fix: this helper exists so adding a
/// new variant to e.g. `ChannelOp` no longer requires a hand-rolled
/// `globals.insert(...)` pair next to the existing ones — the loop
/// at dispatch.rs picks up the arity automatically.
///
/// A parity-lock test at
/// `tests/round71_dispatch_collapse_and_parity_tests.rs` asserts these
/// `(variant, arity)` tuples agree with the previous hand-rolled
/// constructor registrations on every prelude / gated non-error enum.
pub fn builtin_prelude_enum_variants_with_arity()
-> &'static [(&'static str, &'static [(&'static str, usize)])] {
    &[
        ("Result", &[("Ok", 1), ("Err", 1)]),
        ("Option", &[("Some", 1), ("None", 0)]),
        ("Step", &[("Stop", 1), ("Continue", 1)]),
        (
            "ChannelResult",
            &[("Message", 1), ("Closed", 0), ("Empty", 0), ("Sent", 0)],
        ),
        // ChannelOp constructors for `channel.select`. `Recv(ch)` and
        // `Send(ch, value)` are the one-and-only shapes accepted by the
        // select op list.
        ("ChannelOp", &[("Recv", 1), ("Send", 2)]),
        (
            "Weekday",
            &[
                ("Monday", 0),
                ("Tuesday", 0),
                ("Wednesday", 0),
                ("Thursday", 0),
                ("Friday", 0),
                ("Saturday", 0),
                ("Sunday", 0),
            ],
        ),
        (
            "Method",
            &[
                ("GET", 0),
                ("POST", 0),
                ("PUT", 0),
                ("PATCH", 0),
                ("DELETE", 0),
                ("HEAD", 0),
                ("OPTIONS", 0),
            ],
        ),
    ]
}

/// Returns the list of builtin function suffixes for a given builtin module.
/// E.g., for "string" returns ["split", "trim", "trim_start", ...].
pub fn builtin_module_functions(module: &str) -> Vec<&'static str> {
    match module {
        "string" => vec![
            "from",
            "split",
            "split_at",
            "trim",
            "trim_start",
            "trim_end",
            "char_code",
            "from_char_code",
            "contains",
            "replace",
            "join",
            "length",
            "byte_length",
            "to_upper",
            "to_lower",
            "starts_with",
            "starts_with_at",
            "ends_with",
            "chars",
            "repeat",
            "index_of",
            "last_index_of",
            "lines",
            "slice",
            "pad_left",
            "pad_right",
            "is_empty",
            "is_alpha",
            "is_digit",
            "is_upper",
            "is_lower",
            "is_alnum",
            "is_whitespace",
        ],
        "list" => vec![
            "map",
            "filter",
            "each",
            "fold",
            "find",
            "zip",
            "flatten",
            "sort_by",
            "flat_map",
            "filter_map",
            "any",
            "all",
            "fold_until",
            "unfold",
            "head",
            "tail",
            "last",
            "reverse",
            "sort",
            "unique",
            "contains",
            "length",
            "append",
            "prepend",
            "concat",
            "get",
            "set",
            "take",
            "drop",
            "enumerate",
            "group_by",
            // Round-72 widen: typechecker registrations these names had
            // schemes for, but `builtin_module_functions` did not enumerate.
            // Without these entries, `Vm::register_builtins` never seeded
            // a global for `list.sum` (etc.), so first-class value access
            // (`let f = list.sum`) blew up at runtime with `undefined
            // global: list.sum` even though the call form `list.sum(xs)`
            // worked via `Op::CallBuiltin`.
            "sum",
            "sum_float",
            "product",
            "product_float",
            "min_by",
            "max_by",
            "scan",
            "intersperse",
            "remove_at",
            "index_of",
        ],
        "map" => vec![
            "get",
            "set",
            "delete",
            "contains",
            "keys",
            "values",
            "length",
            "merge",
            "filter",
            "map",
            "entries",
            "from_entries",
            "each",
            "update",
        ],
        "io" => vec!["read_file", "write_file", "read_line", "args", "inspect"],
        "int" => vec![
            "parse",
            "abs",
            "min",
            "max",
            "clamp",
            "to_float",
            "to_string",
        ],
        "float" => vec![
            "parse",
            "round",
            "ceil",
            "floor",
            "abs",
            "to_string",
            "to_int",
            "min",
            "max",
            "clamp",
            "is_finite",
            "is_infinite",
            "is_nan",
        ],
        "result" => vec![
            "unwrap_or",
            "map_ok",
            "map_err",
            "flatten",
            "flat_map",
            "is_ok",
            "is_err",
        ],
        "option" => vec![
            "map",
            "unwrap_or",
            "to_result",
            "is_some",
            "is_none",
            "flat_map",
        ],
        "test" => vec!["assert", "assert_eq", "assert_ne"],
        "math" => vec![
            "sqrt", "pow", "log", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "atan2",
            "exp", "random",
        ],
        "regex" => vec![
            "is_match",
            "find",
            "find_all",
            "split",
            "replace",
            "replace_all",
            "replace_all_with",
            "captures",
            "captures_all",
            "captures_named",
        ],
        "json" => vec!["parse", "parse_list", "parse_map", "stringify", "pretty"],
        "toml" => vec!["parse", "parse_list", "parse_map", "stringify", "pretty"],
        "channel" => vec![
            "new",
            "send",
            "receive",
            "close",
            "try_send",
            "try_receive",
            "recv_timeout",
            "select",
            "each",
            "timeout",
        ],
        "task" => vec!["spawn", "spawn_until", "deadline", "join", "cancel"],
        "set" => vec![
            "new",
            "from_list",
            "to_list",
            "contains",
            "insert",
            "remove",
            "length",
            "union",
            "intersection",
            "difference",
            "symmetric_difference",
            "is_subset",
            "map",
            "filter",
            "each",
            "fold",
        ],
        "time" => vec![
            "now",
            "today",
            "date",
            "time",
            "datetime",
            "to_datetime",
            "to_instant",
            "to_utc",
            "from_utc",
            "format",
            "format_date",
            "parse",
            "parse_date",
            "add_days",
            "add_months",
            "add",
            "since",
            "hours",
            "minutes",
            "seconds",
            "ms",
            "micros",
            "nanos",
            "weekday",
            "days_between",
            "days_in_month",
            "is_leap_year",
            "sleep",
        ],
        "http" => vec![
            "get",
            "request",
            "serve",
            "serve_all",
            "segments",
            "parse_query",
        ],
        "postgres" => vec![
            "connect",
            "connect_with",
            "query",
            "execute",
            "transact",
            "close",
            "cursor",
            "cursor_close",
            "cursor_next",
            "stream",
            "listen",
            "notify",
            "uuidv7",
        ],
        "fs" => vec![
            "exists",
            "is_file",
            "is_dir",
            "is_symlink",
            "list_dir",
            "mkdir",
            "remove",
            "rename",
            "copy",
            "stat",
            "walk",
            "glob",
            "read_link",
        ],
        "env" => vec!["get", "set", "remove", "vars"],
        "bytes" => vec![
            "empty",
            "from_string",
            "to_string",
            "from_hex",
            "to_hex",
            "from_base64",
            "to_base64",
            "from_list",
            "to_list",
            "length",
            "slice",
            "concat",
            "concat_all",
            "get",
            "eq",
            "starts_with",
            "ends_with",
            "split",
            "index_of",
        ],
        "crypto" => vec![
            "sha256",
            "sha512",
            "md5",
            "md5_hex",
            "blake2b",
            "blake2b_hex",
            "hmac_sha256",
            "hmac_sha512",
            "random_bytes",
            "constant_time_eq",
        ],
        "encoding" => vec!["url_encode", "url_decode", "form_encode", "form_decode"],
        "uuid" => vec!["v4", "v7", "parse", "nil", "is_valid"],
        "stream" => vec![
            "from_list",
            "from_range",
            "repeat",
            "unfold",
            "file_chunks",
            "file_lines",
            "tcp_chunks",
            "tcp_lines",
            "map",
            "map_ok",
            "filter",
            "filter_ok",
            "flat_map",
            "take",
            "drop",
            "take_while",
            "drop_while",
            "chunks",
            "scan",
            "dedup",
            "buffered",
            "merge",
            "zip",
            "concat",
            "collect",
            "fold",
            "each",
            "count",
            "first",
            "last",
            "write_to_tcp",
            "write_to_file",
        ],
        "tcp" => {
            #[allow(unused_mut)]
            let mut fns = vec![
                "listen",
                "accept",
                "connect",
                "read",
                "read_exact",
                "write",
                "close",
                "peer_addr",
                "set_nodelay",
            ];
            #[cfg(feature = "tcp-tls")]
            {
                fns.push("connect_tls");
                fns.push("accept_tls");
                fns.push("accept_tls_mtls");
            }
            fns
        }
        _ => vec![],
    }
}

/// Returns the list of builtin constants (non-function values) for a module.
/// E.g., for "math" returns ["pi", "e"].
///
/// Keep this in sync with the constants registered in
/// `src/typechecker/builtins.rs` (`register_math_builtins` /
/// `register_float_builtins`). LSP dot-completion (`src/lsp.rs::dot_completions`)
/// consults this list so editor autocompletion surfaces module constants.
///
/// Parity lock: `tests/module_constants_completion_tests.rs`
/// (`math_constants_are_listed`, `float_constants_are_listed`,
/// `unknown_module_has_no_constants`,
/// `float_functions_do_not_duplicate_constants`) — round-26 audit
/// findings L8/G5.
pub fn builtin_module_constants(module: &str) -> Vec<&'static str> {
    match module {
        "math" => vec!["pi", "e"],
        "float" => vec![
            "max_value",
            "min_value",
            "epsilon",
            "min_positive",
            "infinity",
            "neg_infinity",
            "nan",
        ],
        _ => vec![],
    }
}
