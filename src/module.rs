//! Module system utilities: what the builtin modules are, as the
//! builtin registry (`crate::builtins::registry`) declares them, and
//! the diagnostic for a module file that cannot be loaded.

use std::sync::OnceLock;

use crate::builtins::registry::{TypeShape, registry};

/// The builtin modules, built or not, in the order of their ids
/// ([`crate::session::ModuleId::builtin`]): their functions
/// (`module.func`) are builtins, not loaded from files.
pub fn builtin_modules() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| registry().modules.iter().map(|m| m.name).collect())
}

/// Returns true if `name` is a builtin module (io, string, int, etc.).
pub fn is_builtin_module(name: &str) -> bool {
    registry().module(name).is_some()
}

/// The cargo feature the builtin module `name` needs and this build
/// lacks; `None` for a module that is built and for any other name.
pub fn missing_feature(name: &str) -> Option<&'static str> {
    let module = registry().module(name)?;
    if module.enabled { None } else { module.feature }
}

/// Names of the built-in primitive type descriptors (uppercase) usable
/// as `type a` arguments and for static-style trait dispatch
/// (`Int.parse(...)`, etc.). The compiler emits each one, used as a
/// value, as a `Value::PrimitiveDescriptor("<Name>")` constant; the
/// typechecker binds each as `TypeOf(<inner>)`.
pub const BUILTIN_PRIMITIVE_NAMES: &[&str] = &["Int", "Float", "String", "Bool"];

/// Names of the built-in generic container type descriptors (uppercase)
/// usable as `type a` arguments and for static-style trait dispatch
/// (`List.empty()`, etc.). The compiler emits each one, used as a value,
/// as a `Value::TypeDescriptor` constant of the builtin type; the
/// typechecker binds each but `Tuple` as a polymorphic
/// `TypeOf(Container(...))`.
pub const BUILTIN_GENERIC_CONTAINER_NAMES: &[&str] = &["List", "Map", "Set", "Channel", "Tuple"];

/// Names of silt's builtin global free functions, callable without a
/// module prefix: `print(...)`, not `io.print(...)`. Used as a value,
/// each is a `BuiltinFn` constant. Alphabetic.
pub fn builtin_free_function_names() -> &'static [&'static str] {
    &["panic", "print", "println"]
}

/// The record and enum types the builtin modules declare, built or not:
/// uppercase names a program writes in type position (`time.Date`) but
/// the stdlib owns, which editors highlight and the LSP refuses to
/// rename.
pub fn builtin_module_types() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        registry()
            .modules
            .iter()
            .flat_map(|m| m.type_decls.iter().map(|ty| ty.name))
            .collect()
    })
}

/// The builtin module that declares the record or enum type `name`:
/// `time` for `Weekday`, `http` for `Request`. `None` for the prelude
/// types and for any other name. A module's types are reached as
/// `time.Weekday`; their variants as `time.Monday` (see
/// [`builtin_variant_module`]).
pub fn builtin_type_module(name: &str) -> Option<&'static str> {
    registry()
        .modules
        .iter()
        .find(|m| m.type_decls.iter().any(|ty| ty.name == name))
        .map(|m| m.name)
}

/// The record and enum types the builtin module `module` declares.
pub fn builtin_module_type_names(module: &str) -> impl Iterator<Item = &'static str> + '_ {
    registry()
        .module(module)
        .into_iter()
        .flat_map(|m| m.type_decls.iter().map(|ty| ty.name))
}

/// The builtin module whose enum declares the variant `name`, which is
/// how the variant is reached qualified: `channel` for `Message` and for
/// `Recv`, `time` for `Monday`. `None` for the prelude variants and for
/// any other name.
pub fn builtin_variant_module(name: &str) -> Option<&'static str> {
    registry()
        .modules
        .iter()
        .find(|m| {
            m.type_decls
                .iter()
                .any(|ty| ty.variants().iter().any(|(variant, _)| *variant == name))
        })
        .map(|m| m.name)
}

/// The `(enum, [(variant, arity)])` listing of the builtin enums `keep`
/// accepts, given each with whether it is its module's error enum.
fn enums_with_arity(
    keep: impl Fn(bool) -> bool,
) -> Vec<(&'static str, &'static [(&'static str, usize)])> {
    let registry = registry();
    let mut out = Vec::new();
    let errors: Vec<&str> = registry.modules.iter().filter_map(|m| m.error).collect();
    for (_, ty) in registry.types() {
        if let TypeShape::Enum(variants) = &ty.shape
            && keep(errors.contains(&ty.name))
        {
            out.push((ty.name, &*Box::leak(variants.clone().into_boxed_slice())));
        }
    }
    out
}

/// The `(variant_name, arity)` listing of every builtin module's error
/// enum (`IoError`, `JsonError`, ...), built or not: the enums that
/// implement `Error` natively.
pub fn builtin_error_enum_variants_with_arity()
-> &'static [(&'static str, &'static [(&'static str, usize)])] {
    static ENUMS: OnceLock<Vec<(&'static str, &'static [(&'static str, usize)])>> = OnceLock::new();
    ENUMS.get_or_init(|| enums_with_arity(|is_error| is_error))
}

/// The `(variant_name, arity)` listing of every other builtin enum:
/// `Result`, `Option`, `Step`, `ChannelResult`, `ChannelOp`, `Weekday`,
/// `Method`.
pub fn builtin_prelude_enum_variants_with_arity()
-> &'static [(&'static str, &'static [(&'static str, usize)])] {
    static ENUMS: OnceLock<Vec<(&'static str, &'static [(&'static str, usize)])>> = OnceLock::new();
    ENUMS.get_or_init(|| enums_with_arity(|is_error| !is_error))
}

/// The builtin enums as `(enum_name, variant_names)` pairs: the prelude
/// enums (Result, Option) and the enums of the builtin modules. The
/// compiler finds a builtin variant by its name here in the derived
/// impls of the builtin types, which name variants unresolved.
pub fn builtin_enum_variants() -> &'static [(&'static str, Vec<&'static str>)] {
    static ENUMS: OnceLock<Vec<(&'static str, Vec<&'static str>)>> = OnceLock::new();
    ENUMS.get_or_init(|| {
        builtin_prelude_enum_variants_with_arity()
            .iter()
            .chain(builtin_error_enum_variants_with_arity())
            .map(|(name, variants)| (*name, variants.iter().map(|(v, _)| *v).collect()))
            .collect()
    })
}

/// Every builtin enum variant name across all builtin enums, for LSP
/// rename and completion and for REPL completion.
pub fn all_builtin_constructor_names() -> impl Iterator<Item = &'static str> {
    builtin_enum_variants()
        .iter()
        .flat_map(|(_, variants)| variants.iter().copied())
}

/// The functions of the builtin module `module` whose features are
/// built: for "string", `["char_code", "chars", ...]`.
pub fn builtin_module_functions(module: &str) -> Vec<&'static str> {
    registry().module(module).map_or_else(Vec::new, |m| {
        m.enabled_rows()
            .filter(|row| !row.is_constant())
            .map(|row| row.name)
            .collect()
    })
}

/// The constants (non-function values) of the builtin module `module`:
/// for "math", `["e", "pi"]`.
pub fn builtin_module_constants(module: &str) -> Vec<&'static str> {
    registry().module(module).map_or_else(Vec::new, |m| {
        m.enabled_rows()
            .filter(|row| row.is_constant())
            .map(|row| row.name)
            .collect()
    })
}

/// The value of the builtin module constant `qualified` (`math.pi`);
/// `None` for any other name.
pub fn builtin_constant_value(qualified: &str) -> Option<crate::value::Value> {
    let (module, name) = qualified.split_once('.')?;
    match &registry().row(module, name)?.body {
        crate::builtins::registry::Body::Const(value) => Some(value.clone()),
        _ => None,
    }
}

/// The diagnostic for an `import` at `span` whose module file, at
/// `attempted_path`, cannot be read: the I/O error, a note naming the
/// path that was tried (`attempted_display`, as it is shown), and, when
/// the file is not there, a did-you-mean for a near-miss sibling `.silt`
/// file and a pointer at `[dependencies]` in silt.toml. A permission or
/// encoding error on an existing file does not invite a rename hunt.
pub fn module_load_error(
    module_name: &str,
    attempted_path: &std::path::Path,
    attempted_display: &str,
    err: &std::io::Error,
    span: crate::source::Span,
) -> crate::diagnostic::Diagnostic {
    let mut d = crate::diagnostic::Diagnostic::error(
        crate::diagnostic::Code::ModuleNotFound,
        span,
        format!(
            "cannot load module '{module_name}': {}",
            crate::diagnostic::io_error_text(err)
        ),
    )
    .with_note(format!("looked for `{attempted_display}`"));
    if err.kind() == std::io::ErrorKind::NotFound {
        if let Some(hint) = sibling_module_suggestion(module_name, attempted_path) {
            // The file name comes from a directory the program does not
            // control (a dependency's), so it is shown through the
            // display rule like the path above.
            let hint = crate::git::escape_for_display(&hint);
            d = d.with_help(format!(
                "did you mean `{hint}`? (`{hint}.silt` exists in the same directory)"
            ));
        }
        d = d.with_help(format!(
            "if '{module_name}' is a separate package, declare it under \
             `[dependencies]` in silt.toml (e.g. `silt add {module_name}`)"
        ));
    }
    d
}

/// Scan the directory the failed import resolved against for sibling
/// `.silt` files and return a close-enough module-name candidate, if
/// any. Candidates are file stems (`util.silt` → `util`); the attempted
/// module's own stem can't appear (its file doesn't exist — that's why
/// we're here). Distance policy delegates to the typechecker's shared
/// suggest helper so import hints and identifier hints can't drift.
fn sibling_module_suggestion(
    module_name: &str,
    attempted_path: &std::path::Path,
) -> Option<String> {
    let dir = attempted_path.parent()?;
    let mut candidates: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("silt")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            candidates.push(stem.to_string());
        }
    }
    crate::typechecker::suggest::suggest_similar(module_name, candidates.iter())
}
