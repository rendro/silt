//! The builtin registry: every builtin module as rows.
//!
//! A builtin function is a [`Row`]: its signature as silt text (`fn
//! trim(s: String) -> String`), a one-line summary, and what a call runs.
//! A builtin module ([`Module`]) is its rows, the types it declares (silt
//! `pub type` text) and its reference page (`docs/stdlib/*.md`). The
//! modules are listed in `registry/modules.rs`, one `module!` each.
//!
//! Everything else that knows the builtins reads it here:
//!
//! - the checker's builtin environment parses each module's text
//!   ([`Module::text`]) and elaborates it as it elaborates a host
//!   module's signatures, so parameter names, `where` bounds and type
//!   declarations come from the one string;
//! - the resolver's builtin definitions, the run time's builtin types
//!   and the lists of `crate::module` are the names of the rows and of
//!   the declared types;
//! - completion, hover and signature help show the signatures and the
//!   reference pages, and the generated parts of the pages (each
//!   module's summary table, each function's signature block) are
//!   written from the rows ([`docs::render_page`]);
//! - a call of a builtin finds its row by name and runs its body.
//!
//! A module of a cargo feature that is not built keeps its rows; the
//! checker does not enter them, and `import` of the module is an error
//! that names the feature.

pub mod docs;
mod modules;

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::ast::{self, Decl, ParamKind, PatternKind, TypeBody};
use crate::intern::resolve;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::source::FileId;
use crate::value::Value;
use crate::vm::{Vm, VmError};

/// What a call of a row runs, until the row has a typed body: the
/// module's `call_*` function, given the function's name and the
/// arguments as values.
pub type UntypedCall = fn(&mut Vm, &str, &[Value]) -> Result<Value, VmError>;

/// What a row is at run time.
pub enum Body {
    /// A function: the module's untyped entry point, called with the
    /// row's name.
    Untyped(UntypedCall),
    /// A constant (`math.pi`).
    Const(Value),
    /// A row of a feature that is not built.
    Off,
}

/// One builtin function or constant.
pub struct Row {
    /// A function's header, `fn trim(s: String) -> String`, read by the
    /// checker like a module's `pub fn`; a constant's name and type,
    /// `pi: Float`.
    pub signature: &'static str,
    /// One line that says what it does: the description column of the
    /// module's summary table.
    pub summary: &'static str,
    /// The name, from the signature.
    pub name: &'static str,
    /// A function's parameter names, from the signature (`a` for a
    /// `type a` parameter). Empty for a constant.
    pub params: Vec<&'static str>,
    /// Whether the last parameter may be left out of a call. A signature
    /// cannot say so; the conventions step removes the five rows that
    /// have it.
    pub optional_last: bool,
    /// The cargo feature the row needs beyond its module's, if any.
    pub feature: Option<&'static str>,
    /// Whether the row's feature and its module's are built.
    pub enabled: bool,
    pub body: Body,
}

impl Row {
    pub fn is_constant(&self) -> bool {
        !self.signature.starts_with("fn ")
    }

    /// The signature as a call site shows it, `fn string.trim(s: String)
    /// -> String`, and where each parameter is in it (byte offsets).
    /// `None` for a constant.
    pub fn qualified_signature(&self, module: &str) -> Option<(String, Vec<[u32; 2]>)> {
        let rest = self.signature.strip_prefix("fn ")?;
        let label = format!("fn {module}.{rest}");
        let open = label.find('(')?;
        let mut ranges = Vec::new();
        let (mut depth, mut start) = (0usize, open + 1);
        for (at, c) in label.char_indices().skip(open) {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        if !label[start..at].trim().is_empty() {
                            ranges.push([start as u32, at as u32]);
                        }
                        break;
                    }
                }
                ',' if depth == 1 => {
                    ranges.push([start as u32, at as u32]);
                    start = at + 2;
                }
                _ => {}
            }
        }
        Some((label, ranges))
    }

    /// Call the row with `args`.
    pub fn call(&self, vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
        match &self.body {
            Body::Untyped(call) => call(vm, self.name, args),
            Body::Const(_) | Body::Off => {
                Err(VmError::new(format!("{} is not a function", self.name)))
            }
        }
    }

    /// The row as the checker reads it: a function's header; for a
    /// constant, a function of no parameters that returns its type.
    fn header(&self) -> String {
        match self.signature.split_once(": ") {
            Some((name, ty)) if self.is_constant() => format!("fn {name}() -> {ty}"),
            _ => self.signature.to_string(),
        }
    }
}

/// A row as a `module!` lists it, before its module is known.
pub struct RowSpec {
    signature: &'static str,
    summary: &'static str,
    constant: Option<Value>,
    optional_last: bool,
    feature: Option<(&'static str, bool)>,
}

/// A function row whose body is its module's untyped `call`.
fn u(signature: &'static str, summary: &'static str) -> RowSpec {
    RowSpec {
        signature,
        summary,
        constant: None,
        optional_last: false,
        feature: None,
    }
}

/// A constant row, `name: Type`, with its value.
fn k(signature: &'static str, summary: &'static str, value: f64) -> RowSpec {
    RowSpec {
        signature,
        summary,
        constant: Some(Value::Float(value)),
        optional_last: false,
        feature: None,
    }
}

impl RowSpec {
    /// The last parameter may be left out of a call (see
    /// [`Row::optional_last`]).
    fn optional_last(mut self) -> RowSpec {
        self.optional_last = true;
        self
    }

    /// The row needs the cargo feature `name`, which is built or not:
    /// `.feature("tcp-tls", cfg!(feature = "tcp-tls"))`.
    fn feature(mut self, name: &'static str, built: bool) -> RowSpec {
        self.feature = Some((name, built));
        self
    }
}

/// A record or enum type a builtin module declares.
pub struct TypeDecl {
    pub name: &'static str,
    /// The number of its type parameters.
    pub params: usize,
    pub shape: TypeShape,
}

pub enum TypeShape {
    /// The variants in declaration order, each with its number of
    /// fields.
    Enum(Vec<(&'static str, usize)>),
    /// The field names in declaration order.
    Record(Vec<&'static str>),
}

impl TypeDecl {
    /// The variants of an enum; none for a record.
    pub fn variants(&self) -> &[(&'static str, usize)] {
        match &self.shape {
            TypeShape::Enum(variants) => variants,
            TypeShape::Record(_) => &[],
        }
    }
}

/// One builtin module.
pub struct Module {
    /// The name programs import it by.
    pub name: &'static str,
    /// The cargo feature the module needs, if any.
    pub feature: Option<&'static str>,
    /// Whether that feature is built (true of a module that needs none).
    pub enabled: bool,
    /// The file of the module's reference page under `docs/stdlib/`
    /// (`io-fs.md`), and its text: the part under the `# <name>` heading
    /// is the module's.
    pub page_file: &'static str,
    pub page: &'static str,
    /// The record and enum types the module declares, as silt `pub type`
    /// declarations.
    pub types: &'static str,
    /// Those types, read from the text.
    pub type_decls: Vec<TypeDecl>,
    /// The module's opaque types, each with the number of its type
    /// arguments (`("Handle", 1)` for `task.Handle(a)`): handles a
    /// program names in a type but that have no fields or variants to
    /// declare.
    pub opaque: &'static [(&'static str, usize)],
    /// The module's error enum, which implements `Error`.
    pub error: Option<&'static str>,
    /// Types of other modules the module offers too, with their
    /// variants, as (module, type): `float` offers `int`'s `ParseError`.
    pub shares: &'static [(&'static str, &'static str)],
    pub rows: Vec<Row>,
}

impl Module {
    /// The rows whose features are built.
    pub fn enabled_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|row| row.enabled)
    }

    /// The row named `name`, if its features are built.
    pub fn row(&self, name: &str) -> Option<&Row> {
        self.enabled_rows().find(|row| row.name == name)
    }

    /// The module as the checker reads it: its type declarations, then
    /// the header of each row whose features are built.
    pub fn text(&self) -> String {
        let mut text = String::from(self.types);
        for row in self.enabled_rows() {
            text.push_str(&row.header());
            text.push('\n');
        }
        text
    }
}

/// The module `$name`: see [`Module`] for the fields. `call` is the
/// module's untyped entry point, the body of each function row; with a
/// `feature`, it is compiled only when the feature is built.
macro_rules! module {
    (
        name: $name:literal,
        $(feature: $feature:literal,)?
        page: $page:literal,
        $(types: $types:expr,)?
        $(opaque: $opaque:expr,)?
        $(error: $error:literal,)?
        $(shares: $shares:expr,)?
        call: $call:expr,
        rows: [$($row:expr),* $(,)?] $(,)?
    ) => {{
        #[allow(unused_mut, unused_assignments)]
        let mut feature: Option<&'static str> = None;
        #[allow(unused_mut, unused_assignments)]
        let mut call: Option<UntypedCall> = None;
        module!(@call call, $call $(, $feature)?);
        $(feature = Some($feature);)?
        #[allow(unused_mut, unused_assignments)]
        let mut types: &'static str = "";
        $(types = $types;)?
        #[allow(unused_mut, unused_assignments)]
        let mut opaque: &'static [(&'static str, usize)] = &[];
        $(opaque = &$opaque;)?
        #[allow(unused_mut, unused_assignments)]
        let mut error: Option<&'static str> = None;
        $(error = Some($error);)?
        #[allow(unused_mut, unused_assignments)]
        let mut shares: &'static [(&'static str, &'static str)] = &[];
        $(shares = &$shares;)?
        build_module(
            $name,
            feature,
            call,
            ($page, include_str!(concat!("../../../docs/stdlib/", $page))),
            (types, opaque),
            error,
            shares,
            vec![$($row),*],
        )
    }};
    (@call $slot:ident, $call:expr) => {
        $slot = Some($call);
    };
    (@call $slot:ident, $call:expr, $feature:literal) => {
        #[cfg(feature = $feature)]
        {
            $slot = Some($call);
        }
    };
}
use module;

/// Parse `text`, declarations of the builtins. The registry's text is
/// silt's own: it parses, or the registry is wrong.
pub(crate) fn parse(text: &str) -> ast::Program {
    let tokens = Lexer::new(FileId::BUILTIN, text)
        .tokenize()
        .unwrap_or_else(|e| panic!("the builtin registry does not lex: {}\n{text}", e.message));
    Parser::new(tokens, text)
        .parse_program()
        .unwrap_or_else(|e| panic!("the builtin registry does not parse: {}\n{text}", e.message))
}

/// A name of the registry, kept for the life of the process.
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// The types `text` declares.
fn type_decls(text: &str) -> Vec<TypeDecl> {
    parse(text)
        .decls
        .iter()
        .map(|decl| {
            let Decl::Type(td) = decl else {
                panic!("the builtin registry declares something other than a type:\n{text}");
            };
            let shape = match &td.body {
                TypeBody::Enum(variants) => TypeShape::Enum(
                    variants
                        .iter()
                        .map(|v| (leak(resolve(v.name)), v.fields.len()))
                        .collect(),
                ),
                TypeBody::Record(fields) => {
                    TypeShape::Record(fields.iter().map(|f| leak(resolve(f.name))).collect())
                }
                TypeBody::Alias(_) => {
                    panic!("the builtin registry declares the alias {}", td.name)
                }
            };
            TypeDecl {
                name: leak(resolve(td.name)),
                params: td.params.len(),
                shape,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_module(
    name: &'static str,
    feature: Option<&'static str>,
    call: Option<UntypedCall>,
    (page_file, page): (&'static str, &'static str),
    (types, opaque): (&'static str, &'static [(&'static str, usize)]),
    error: Option<&'static str>,
    shares: &'static [(&'static str, &'static str)],
    specs: Vec<RowSpec>,
) -> Module {
    let enabled = call.is_some();
    let mut rows: Vec<Row> = specs
        .into_iter()
        .map(|spec| {
            let on = enabled && spec.feature.is_none_or(|(_, built)| built);
            Row {
                signature: spec.signature,
                summary: spec.summary,
                name: "",
                params: Vec::new(),
                optional_last: spec.optional_last,
                feature: spec.feature.map(|(feature, _)| feature),
                enabled: on,
                body: match (spec.constant, call) {
                    _ if !on => Body::Off,
                    (Some(value), _) => Body::Const(value),
                    (None, Some(call)) => Body::Untyped(call),
                    (None, None) => Body::Off,
                },
            }
        })
        .collect();
    // The names, from the signatures: every row's header, parsed as one
    // text.
    let headers: String = rows.iter().map(|row| row.header() + "\n").collect();
    let program = parse(&headers);
    assert_eq!(
        program.decls.len(),
        rows.len(),
        "each row of {name} is one signature"
    );
    for (row, decl) in rows.iter_mut().zip(&program.decls) {
        let Decl::Fn(f) = decl else {
            panic!("the row `{}` of {name} is not a signature", row.signature);
        };
        row.name = leak(resolve(f.name));
        if !row.is_constant() {
            row.params = f
                .params
                .iter()
                .map(|p| match (&p.kind, &p.pattern.kind) {
                    (ParamKind::Data | ParamKind::Type, PatternKind::Ident(n)) => leak(resolve(*n)),
                    _ => panic!("a parameter of {name}.{} is not a name", row.name),
                })
                .collect();
        }
    }
    Module {
        name,
        feature,
        enabled,
        page_file,
        page,
        types,
        type_decls: type_decls(types),
        opaque,
        error,
        shares,
        rows,
    }
}

/// The prelude's opaque types: `Bytes`, and `TypeOf(a)`, the type of a
/// type used as a value (`Int`, a `type a` parameter).
pub const PRELUDE_OPAQUE: &[(&str, usize)] = &[("Bytes", 0), (crate::defs::TYPE_OF, 1)];

/// The enums of the prelude, declared like a module's types.
pub const PRELUDE_TYPES: &str = "\
pub type Result(a, e) { Ok(a), Err(e) }
pub type Option(a) { Some(a), None }
";

/// Every builtin module, and what is derived from all of them at once.
pub struct Registry {
    pub modules: Vec<Module>,
    /// The types of [`PRELUDE_TYPES`].
    pub prelude_types: Vec<TypeDecl>,
    /// Each module's place in `modules`, by name.
    by_name: HashMap<&'static str, usize>,
}

impl Registry {
    /// The module `name`, built or not.
    pub fn module(&self, name: &str) -> Option<&Module> {
        self.by_name.get(name).map(|k| &self.modules[*k])
    }

    /// The row `module.function`, if its features are built.
    pub fn row(&self, module: &str, function: &str) -> Option<&Row> {
        self.module(module)?.row(function)
    }

    /// The number of type arguments of the opaque type `name` (of the
    /// prelude or of a module, built or not); `None` for any other name.
    pub fn opaque_arity(&self, name: &str) -> Option<usize> {
        PRELUDE_OPAQUE
            .iter()
            .chain(self.modules.iter().flat_map(|m| m.opaque))
            .find(|(opaque, _)| *opaque == name)
            .map(|(_, arity)| *arity)
    }

    /// The modules whose feature is built.
    pub fn enabled_modules(&self) -> impl Iterator<Item = &Module> {
        self.modules.iter().filter(|m| m.enabled)
    }

    /// Every declared type with the module that declares it (`None` for
    /// the prelude), built or not.
    pub fn types(&self) -> impl Iterator<Item = (Option<&'static str>, &TypeDecl)> {
        self.prelude_types.iter().map(|ty| (None, ty)).chain(
            self.modules
                .iter()
                .flat_map(|m| m.type_decls.iter().map(|ty| (Some(m.name), ty))),
        )
    }
}

/// The registry, built on first use.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let modules = modules::modules();
        let by_name = modules
            .iter()
            .enumerate()
            .map(|(k, m)| (m.name, k))
            .collect();
        Registry {
            modules,
            prelude_types: type_decls(PRELUDE_TYPES),
            by_name,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_named_once() {
        for module in &registry().modules {
            let mut seen = std::collections::HashSet::new();
            for row in &module.rows {
                assert!(seen.insert(row.name), "{}.{} twice", module.name, row.name);
                assert!(!row.summary.is_empty(), "{}.{}", module.name, row.name);
            }
        }
    }

    #[test]
    fn a_module_s_error_enum_is_one_of_its_types() {
        for module in &registry().modules {
            if let Some(error) = module.error {
                assert!(
                    module.type_decls.iter().any(|ty| ty.name == error),
                    "{} declares {error}",
                    module.name
                );
            }
        }
    }
}
