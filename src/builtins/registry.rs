//! The builtin registry: every builtin module as rows.
//!
//! A builtin function is a [`Row`]: its signature as silt text (`fn
//! trim(s: String) -> String`), a one-line summary, and what a call
//! runs: a Rust function of the arguments as Rust types
//! (`builtins::typed`).
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
//! - a call of a builtin carries its row's number ([`BuiltinId`]),
//!   which the compiler took from the row it names, and runs the row's
//!   body; a builtin function as a value is that number.
//!
//! A module of a cargo feature that is not built keeps its rows; the
//! checker does not enter them, and `import` of the module is an error
//! that names the feature.

pub mod docs;
mod modules;

use std::collections::HashMap;
use std::sync::OnceLock;

use super::typed::TypedCall;
use crate::ast::{self, Decl, ParamKind, PatternKind, TypeBody};
use crate::intern::resolve;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::source::FileId;
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};

/// The text of a variant of a module's error enum, by the variant's
/// name and its fields: `None` if they are no variant of it.
pub type ErrorText = fn(&str, &[Value]) -> Option<String>;

/// What a row is at run time.
pub(crate) enum Body {
    /// A function: its typed body.
    Typed(TypedCall),
    /// A constant (`math.pi`).
    Const(Value),
    /// A row of a feature that is not built.
    Off,
}

/// A row's number: what a call of a builtin carries
/// ([`Instr::CallBuiltin`](crate::bytecode::Instr)) and what a builtin
/// function is as a value ([`Value::BuiltinFn`]). It is the row's place
/// among all rows ([`Registry::builtin`]) in this build of silt, and
/// means nothing in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuiltinId(pub(crate) u16);

impl std::fmt::Display for BuiltinId {
    /// The row's name as a program calls it: `list.map`, `println`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match registry().builtin(*self) {
            Some(row) => f.write_str(&row.qualified()),
            None => write!(f, "{}", self.0),
        }
    }
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
    /// The module the row is of.
    pub module: &'static str,
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
    pub id: BuiltinId,
    pub(crate) body: Body,
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

    /// The row's name as a program calls it: `list.map`; the bare name
    /// of a function of the prelude (`println`).
    pub fn qualified(&self) -> String {
        match self.module {
            "" => self.name.to_string(),
            module => format!("{module}.{}", self.name),
        }
    }

    /// The row as a value: a constant's value, a function.
    pub fn value(&self) -> Value {
        match &self.body {
            Body::Const(value) => value.clone(),
            _ => Value::BuiltinFn(self.id),
        }
    }

    /// Call the row with `args`.
    pub(crate) fn call(&self, vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
        match &self.body {
            Body::Typed(call) => call(vm, args).unwrap_or_else(|| Err(self.misfit(args))),
            Body::Const(_) | Body::Off => Err(VmError::new(format!(
                "{} is not a function",
                self.qualified()
            ))),
        }
    }

    /// The error of a call whose arguments are not the row's: not as
    /// many, or one of another kind than its parameter's type. It is
    /// the one such error of every builtin, and no checked program
    /// meets it: the checker read the same signature.
    fn misfit(&self, args: &[Value]) -> VmError {
        let params = self
            .signature
            .find('(')
            .zip(self.signature.rfind(") ->"))
            .map_or("()", |(open, close)| &self.signature[open..=close]);
        let kinds: Vec<&str> = args.iter().map(Value::kind).collect();
        VmError::type_confusion(format!(
            "{} takes {params}, but was called with ({})",
            self.qualified(),
            kinds.join(", ")
        ))
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
    /// The typed body of a function row that has one.
    typed: Option<TypedCall>,
    constant: Option<Value>,
    optional_last: bool,
    feature: Option<(&'static str, bool)>,
}

/// A function row with its typed body (see [`super::typed`]).
fn f(signature: &'static str, summary: &'static str, body: TypedCall) -> RowSpec {
    RowSpec {
        typed: Some(body),
        ..off(signature, summary)
    }
}

/// A function row of a feature that is not built: no body.
fn off(signature: &'static str, summary: &'static str) -> RowSpec {
    RowSpec {
        signature,
        summary,
        typed: None,
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
        typed: None,
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
    /// The builtin traits the type derives: all four ([`DERIVED`]) unless
    /// its module says otherwise (`derives:`).
    pub derives: &'static [&'static str],
}

/// The builtin traits a declared type derives when its module says
/// nothing else.
pub const DERIVED: &[&str] = &["Equal", "Compare", "Hash", "Display"];

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
    /// The declared types that do not derive all four builtin traits,
    /// each with the traits it does derive: `http.Response` holds a
    /// `Map`, which has no order, so it derives no `Compare`;
    /// `channel.ChannelOp` holds a channel and derives nothing.
    pub derives: &'static [(&'static str, &'static [&'static str])],
    /// The module's opaque types, each with the number of its type
    /// arguments (`("Handle", 1)` for `task.Handle(a)`): handles a
    /// program names in a type but that have no fields or variants to
    /// declare.
    pub opaque: &'static [(&'static str, usize)],
    /// The module's error enum, which implements `Error`.
    pub error: Option<&'static str>,
    /// The `message` of the error enum, which is its `Error` impl: a
    /// row of no module's table (`IoError.message`), found by the
    /// enum's type ([`Registry::error_module`]).
    pub message: Option<Row>,
    /// What each variant of the error enum reads as, in its `message`
    /// and where it is shown; none where the module is not built.
    pub text: Option<ErrorText>,
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

/// The module `$name`: see [`Module`] for the fields. With a
/// `feature`, the rows' bodies are compiled only when the feature is
/// built.
macro_rules! module {
    (
        name: $name:literal,
        $(feature: $feature:literal,)?
        page: $page:literal,
        $(types: $types:expr,)?
        $(derives: $derives:expr,)?
        $(opaque: $opaque:expr,)?
        $(error: $error:literal => $text:expr,)?
        $(shares: $shares:expr,)?
        rows: [$(
            $row:ident ( $($arg:expr),* $(,)? ) $(. $with:ident ( $($with_arg:tt)* ))*
        ),* $(,)?] $(,)?
    ) => {{
        #[allow(unused_mut, unused_assignments)]
        let mut feature: Option<&'static str> = None;
        #[allow(unused_mut, unused_assignments)]
        let mut built = true;
        $(
            feature = Some($feature);
            built = cfg!(feature = $feature);
        )?
        #[allow(unused_mut, unused_assignments)]
        let mut types: &'static str = "";
        $(types = $types;)?
        #[allow(unused_mut, unused_assignments)]
        let mut derives: &'static [(&'static str, &'static [&'static str])] = &[];
        $(derives = &$derives;)?
        #[allow(unused_mut, unused_assignments)]
        let mut opaque: &'static [(&'static str, usize)] = &[];
        $(opaque = &$opaque;)?
        #[allow(unused_mut, unused_assignments)]
        let mut error: Option<(&'static str, &'static str, Option<$crate::builtins::registry::ErrorText>)> = None;
        module!(@error error, [$($feature)?], [$($error => $text)?]);
        #[allow(unused_mut, unused_assignments)]
        let mut shares: &'static [(&'static str, &'static str)] = &[];
        $(shares = &$shares;)?
        let rows;
        module!(
            @rows rows, [$($feature)?],
            [$({ $row ($($arg),*) [$(. $with ($($with_arg)*))*] })*]
        );
        build_module(
            $name,
            (feature, built),
            ($page, include_str!(concat!("../../../docs/stdlib/", $page))),
            (types, derives, opaque),
            error,
            shares,
            rows,
        )
    }};
    // The module's error enum: its name, the signature of its `message`
    // and what its variants read as, which is compiled only where the
    // module's feature is built.
    (@error $slot:ident, [$($of:literal)?], []) => {};
    (@error $slot:ident, [$($of:literal)?], [$error:literal => $text:expr]) => {{
        #[cfg(all($(feature = $of,)?))]
        let text: Option<$crate::builtins::registry::ErrorText> = Some($text);
        #[cfg(not(all($(feature = $of,)?)))]
        let text: Option<$crate::builtins::registry::ErrorText> = None;
        $slot = Some((
            $error,
            concat!("fn message(error: ", $error, ") -> String"),
            text,
        ));
    }};
    (@rows $slot:ident, [], [$({ $($row:tt)* })*]) => {
        $slot = vec![$(module!(@row [] $($row)*)),*];
    };
    (@rows $slot:ident, [$feature:literal], [$({ $($row:tt)* })*]) => {
        $slot = vec![$(module!(@row [$feature] $($row)*)),*];
    };
    // A function row, `f(signature, summary, body)`: alone, with
    // `.optional_last()`, or with `.feature("name")`, the cargo feature
    // the row needs beyond its module's.
    (@row [$($of:literal)?] f ($signature:expr, $summary:expr, $body:expr) []) => {
        module!(@typed [$($of)?] [] $signature, $summary, $body)
    };
    (
        @row [$($of:literal)?] f ($signature:expr, $summary:expr, $body:expr)
        [. optional_last ()]
    ) => {
        module!(@typed [$($of)?] [] $signature, $summary, $body).optional_last()
    };
    (
        @row [$($of:literal)?] f ($signature:expr, $summary:expr, $body:expr)
        [. feature ($own:literal)]
    ) => {
        module!(@typed [$($of)?] [$own] $signature, $summary, $body)
            .feature($own, cfg!(feature = $own))
    };
    // Any other row: a constant.
    (@row [$($of:literal)?] $row:ident ($($arg:expr),*) [$($with:tt)*]) => {
        $row($($arg),*) $($with)*
    };
    // A function row of a module with the feature `$of`, with the
    // feature `$own` of its own: where one of them is not built, its
    // body is not compiled, and the row is its signature and its summary.
    (
        @typed [$($of:literal)?] [$($own:literal)?]
        $signature:expr, $summary:expr, $body:expr
    ) => {{
        #[cfg(all($(feature = $of,)? $(feature = $own,)?))]
        let row = $crate::builtins::registry::f($signature, $summary, $body);
        #[cfg(not(all($(feature = $of,)? $(feature = $own,)?)))]
        let row = $crate::builtins::registry::off($signature, $summary);
        row
    }};
}
use module;

/// Parse `text`, declarations of the builtins. The registry's text is
/// silt's own: it parses, or the registry is wrong.
pub(crate) fn parse(text: &str) -> ast::Program {
    let tokens = Lexer::new(FileId::BUILTIN, text)
        .tokenize()
        .checked()
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
fn type_decls(
    text: &str,
    derives: &'static [(&'static str, &'static [&'static str])],
) -> Vec<TypeDecl> {
    for (name, _) in derives {
        assert!(
            text.contains(&format!("pub type {name}")),
            "`derives` names {name}, which the module does not declare"
        );
    }
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
            let name = leak(resolve(td.name));
            TypeDecl {
                name,
                params: td.params.len(),
                shape,
                derives: derives
                    .iter()
                    .find(|(ty, _)| *ty == name)
                    .map_or(DERIVED, |(_, traits)| traits),
            }
        })
        .collect()
}

fn build_module(
    name: &'static str,
    (feature, enabled): (Option<&'static str>, bool),
    (page_file, page): (&'static str, &'static str),
    (types, derives, opaque): (
        &'static str,
        &'static [(&'static str, &'static [&'static str])],
        &'static [(&'static str, usize)],
    ),
    error: Option<(&'static str, &'static str, Option<ErrorText>)>,
    shares: &'static [(&'static str, &'static str)],
    specs: Vec<RowSpec>,
) -> Module {
    // The `message` of the error enum: a row of the enum, not of the
    // module's table.
    let message = error.map(|(error, signature, text)| {
        let summary = "The error as text";
        let spec = match text {
            Some(_) => f(signature, summary, super::error_message),
            None => off(signature, summary),
        };
        rows(error, enabled && text.is_some(), vec![spec]).remove(0)
    });
    Module {
        name,
        feature,
        enabled,
        page_file,
        page,
        types,
        type_decls: type_decls(types, derives),
        derives,
        opaque,
        error: error.map(|(error, _, _)| error),
        message,
        text: error.and_then(|(_, _, text)| text),
        shares,
        rows: rows(name, enabled, specs),
    }
}

/// The rows of `specs`, of the module `name` (of the prelude: `""`),
/// which is built or not.
fn rows(name: &'static str, enabled: bool, specs: Vec<RowSpec>) -> Vec<Row> {
    let mut rows: Vec<Row> = specs
        .into_iter()
        .map(|spec| {
            let on = enabled && spec.feature.is_none_or(|(_, built)| built);
            Row {
                signature: spec.signature,
                summary: spec.summary,
                name: "",
                module: name,
                params: Vec::new(),
                optional_last: spec.optional_last,
                feature: spec.feature.map(|(feature, _)| feature),
                enabled: on,
                // (Its place among all rows: the registry's to say.)
                id: BuiltinId(0),
                body: match (spec.constant, spec.typed) {
                    _ if !on => Body::Off,
                    (Some(value), _) => Body::Const(value),
                    (None, Some(typed)) => Body::Typed(typed),
                    (None, None) => {
                        panic!("the row `{}` of {name} has no body", spec.signature)
                    }
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
    rows
}

/// The prelude's opaque types: `Bytes`, and `TypeOf(a)`, the type of a
/// type used as a value (`Int`, a `type a` parameter).
pub const PRELUDE_OPAQUE: &[(&str, usize)] = &[("Bytes", 0), (crate::defs::TYPE_OF, 1)];

/// `Option` and `Result` derive no `Compare`.
const PRELUDE_DERIVES: &[(&str, &[&str])] = &[
    ("Result", &["Equal", "Hash", "Display"]),
    ("Option", &["Equal", "Hash", "Display"]),
];

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
    /// The functions of the prelude, called by their bare names
    /// (`println`): rows of no module.
    pub prelude: Vec<Row>,
    /// Each module's place in `modules`, by name.
    by_name: HashMap<&'static str, usize>,
    /// Where each row is, by its id.
    by_id: Vec<Place>,
}

/// Where a row is in the registry.
#[derive(Clone, Copy)]
enum Place {
    /// The row of this place in the rows of the module of this place.
    Row(usize, usize),
    /// The `message` of the error enum of the module of this place.
    Message(usize),
    /// The function of the prelude of this place.
    Prelude(usize),
}

impl Registry {
    /// The row with the id.
    pub fn builtin(&self, id: BuiltinId) -> Option<&Row> {
        match *self.by_id.get(usize::from(id.0))? {
            Place::Row(module, row) => self.modules[module].rows.get(row),
            Place::Message(module) => self.modules[module].message.as_ref(),
            Place::Prelude(row) => self.prelude.get(row),
        }
    }

    /// The module whose error enum the type `id` is (`IoError`: `io`),
    /// found by the type's id; `None` for any other type.
    pub fn error_module(&self, id: crate::defs::TypeId) -> Option<&Module> {
        // Each builtin type's module, if the type is its error enum,
        // in the order of the builtin types' ids.
        static BY_TYPE: OnceLock<Vec<Option<usize>>> = OnceLock::new();
        let by_type = BY_TYPE.get_or_init(|| {
            crate::defs::builtin_types()
                .iter()
                .map(|(ty, module)| {
                    let module = *self.by_name.get((*module)?)?;
                    (self.modules[module].error == Some(*ty)).then_some(module)
                })
                .collect()
        });
        let module = (*by_type.get(id.0.0 as usize)?)?;
        Some(&self.modules[module])
    }

    /// The row a program names `qualified`: `list.map`, or a function
    /// of the prelude by its bare name; if its features are built.
    pub fn named(&self, qualified: &str) -> Option<&Row> {
        match qualified.split_once('.') {
            Some((module, function)) => self.row(module, function),
            None => self.prelude.iter().find(|row| row.name == qualified),
        }
    }

    /// The prelude's functions as the checker reads them: each row's
    /// header.
    pub fn prelude_text(&self) -> String {
        self.prelude.iter().map(|row| row.header() + "\n").collect()
    }

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
        let mut modules = modules::modules();
        let mut prelude = rows("", true, modules::prelude());
        let by_name = modules
            .iter()
            .enumerate()
            .map(|(k, m)| (m.name, k))
            .collect();
        // Every row's id: its place among all rows, the modules' in
        // their order (each one's rows, then its error enum's
        // `message`), then the prelude's.
        let mut by_id = Vec::new();
        let mut number = |row: &mut Row, place: Place| {
            let id = u16::try_from(by_id.len()).expect("more builtins than an id counts");
            row.id = BuiltinId(id);
            by_id.push(place);
        };
        for (m, module) in modules.iter_mut().enumerate() {
            for (k, row) in module.rows.iter_mut().enumerate() {
                number(row, Place::Row(m, k));
            }
            if let Some(message) = &mut module.message {
                number(message, Place::Message(m));
            }
        }
        for (k, row) in prelude.iter_mut().enumerate() {
            number(row, Place::Prelude(k));
        }
        Registry {
            modules,
            prelude_types: type_decls(PRELUDE_TYPES, PRELUDE_DERIVES),
            prelude,
            by_name,
            by_id,
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

    /// An error enum's `message` is a row of the enum, found by the
    /// enum's type and by its own id, and its body gives what the
    /// module says its variants read as.
    #[test]
    fn an_error_enum_s_message_is_a_row_found_by_its_type() {
        let registry = registry();
        let mut vm = Vm::new(crate::vm::HostIo::process());
        let mut seen = 0;
        for module in registry.enabled_modules() {
            let Some(error) = module.error else { continue };
            seen += 1;
            let ty = crate::types::TypeRef::builtin(error).id;
            let found = registry.error_module(ty).expect(error);
            assert_eq!(found.name, module.name);
            let message = module.message.as_ref().expect(error);
            assert_eq!(message.qualified(), format!("{error}.message"));
            let by_id = registry.builtin(message.id).expect(error);
            assert!(std::ptr::eq(by_id, message), "{error}");
            // No row of the module's table: a program cannot name it.
            assert!(
                registry
                    .named(&format!("{}.message", module.name))
                    .is_none()
            );
            // What is no variant of the enum is the one error.
            let Err(misfit) = message.call(&mut vm, &[Value::Int(1), Value::Int(2)]) else {
                panic!("{error}.message of two Ints");
            };
            assert_eq!(
                misfit.message,
                format!("{error}.message takes (error: {error}), but was called with (Int, Int)")
            );
        }
        assert!(seen >= 9, "{seen} error enums");
        // A type that is no module's error enum has none.
        let option = crate::types::TypeRef::builtin("Option").id;
        assert!(registry.error_module(option).is_none());
        // The text of a variant, and of a value shown.
        let closed = Value::variant(crate::typeinfo::bv::CHANNEL_CLOSED, vec![]);
        assert_eq!(closed.to_string(), "channel closed with no more values");
        let channel = registry.module("channel").expect("channel");
        let message = channel.message.as_ref().expect("ChannelError.message");
        let Ok(Step::Done(Value::String(text))) = message.call(&mut vm, &[closed]) else {
            panic!("ChannelError.message");
        };
        assert_eq!(text, "channel closed with no more values");
    }

    /// A call whose arguments are not the row's (which no checked
    /// program makes) is the one error of its kind, with one wording:
    /// what the builtin takes, and the kinds of what it was given.
    #[test]
    fn arguments_that_are_not_the_row_s_are_one_error() {
        let mut vm = Vm::new(crate::vm::HostIo::process());
        let abs = registry().row("int", "abs").expect("int.abs");
        let map = Value::Map(Default::default());
        for (args, said) in [
            (
                vec![map.clone()],
                "int.abs takes (n: Int), but was called with (Map)",
            ),
            (vec![], "int.abs takes (n: Int), but was called with ()"),
            (
                vec![Value::Int(1), Value::String("x".into())],
                "int.abs takes (n: Int), but was called with (Int, String)",
            ),
        ] {
            let Err(error) = abs.call(&mut vm, &args) else {
                panic!("{said}");
            };
            assert_eq!(error.message, said);
            assert!(error.type_confusion);
        }
        // An error of the builtin's own is no such error.
        let Err(overflow) = abs.call(&mut vm, &[Value::Int(i64::MIN)]) else {
            panic!("abs of the least Int");
        };
        assert!(!overflow.type_confusion, "{}", overflow.message);
        let random = registry().row("math", "random").expect("math.random");
        let Err(error) = random.call(&mut vm, &[map]) else {
            panic!("math.random of a map");
        };
        assert_eq!(
            error.message,
            "math.random takes (), but was called with (Map)"
        );
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
