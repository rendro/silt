//! Definitions: every name a module declares at its top level, and every
//! builtin type, variant and function, is a [`Def`] with a [`DefId`].
//!
//! The session holds one [`DefTable`]. The builtins are defs of
//! pseudo-modules, the prelude and one per builtin module (`list`,
//! `channel`, ...), registered once per process and shared by every
//! table. A module's defs are entered when the resolver
//! (`typechecker::names`) builds its scope; when the module is checked
//! again (an editor changed it), its defs are entered again in the same
//! slots.
//!
//! The resolver writes a [`Res`] on each AST node that names something:
//! what the name means at that place.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

use crate::intern::Symbol;
use crate::session::ModuleId;
use crate::source::Span;

/// A definition: an index into the [`DefTable`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// A record or enum type, named by its definition.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TypeId(pub DefId);

/// A trait, named by its definition.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TraitId(pub DefId);

/// Whether other modules may name a definition.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Vis {
    Pub,
    Private,
}

/// What a definition is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DefKind {
    Fn,
    Let,
    TypeAlias,
    /// A record or enum type.
    Type(TypeId),
    /// A variant of the enum `ty`, the `ordinal`th declared, with `arity`
    /// fields.
    Variant {
        ty: TypeId,
        ordinal: u16,
        arity: u16,
    },
    Trait(TraitId),
    /// A function of an embedder's host module.
    Host,
}

/// One definition.
#[derive(Copy, Clone, Debug)]
pub struct Def {
    pub module: ModuleId,
    pub name: Symbol,
    /// The span of the name where it is declared; a builtin's is
    /// [`Span::BUILTIN`].
    pub span: Span,
    pub vis: Vis,
    pub kind: DefKind,
}

impl Def {
    pub fn is_type(&self) -> bool {
        matches!(self.kind, DefKind::Type(_) | DefKind::TypeAlias)
    }
}

/// What a name means where it is written.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Res {
    /// A binding of the enclosing function, lambda, pattern or block, or
    /// a type variable.
    Local,
    Def(DefId),
    /// An imported module (or the alias of one).
    Module(ModuleId),
    /// Nothing: the resolver reported why, or the name comes from a
    /// module that failed to load, which is reported at its import.
    Error,
}

/// The first id of the builtin pseudo-modules: the prelude is
/// `ModuleId(BUILTIN_MODULE_BASE)`, the `k`th builtin module of
/// [`crate::module::builtin_modules`] is `ModuleId(BUILTIN_MODULE_BASE +
/// 1 + k)`. A module graph never has that many modules.
const BUILTIN_MODULE_BASE: u32 = 0xFFFF_0000;

impl ModuleId {
    /// The prelude: the names every module sees without an import.
    pub const PRELUDE: ModuleId = ModuleId(BUILTIN_MODULE_BASE);

    /// The pseudo-module of the builtin module `name` (`list`, ...).
    pub fn builtin(name: &str) -> Option<ModuleId> {
        crate::module::builtin_modules()
            .iter()
            .position(|m| *m == name)
            .map(|k| ModuleId(BUILTIN_MODULE_BASE + 1 + k as u32))
    }

    /// Whether this is the prelude or a builtin module.
    pub fn is_builtin(self) -> bool {
        self.0 >= BUILTIN_MODULE_BASE
    }

    /// The name of a builtin pseudo-module: `None` for the prelude and
    /// for a module of the graph.
    pub fn builtin_name(self) -> Option<&'static str> {
        let k = self.0.checked_sub(BUILTIN_MODULE_BASE + 1)?;
        crate::module::builtin_modules().get(k as usize).copied()
    }
}

/// The builtin types that are reached through a builtin module but have
/// no record or enum declaration (opaque handles).
pub const OPAQUE_MODULE_TYPES: &[(&str, &str)] = &[
    ("TcpListener", "tcp"),
    ("TcpStream", "tcp"),
    ("Handle", "task"),
    ("PgPool", "postgres"),
    ("PgTx", "postgres"),
    ("PgCursor", "postgres"),
    ("QueryResult", "postgres"),
    ("ExecResult", "postgres"),
    ("Value", "postgres"),
];

/// The builtin types with no declaration of their own, each with the
/// number of its type arguments: the opaque handles of the builtin
/// modules, `Bytes`, and the prelude's `TypeOf`, the type of a type used
/// as a value (`Int`, a `type a` parameter).
pub const OPAQUE_TYPE_ARITY: &[(&str, usize)] = &[
    ("Bytes", 0),
    ("TcpListener", 0),
    ("TcpStream", 0),
    ("Handle", 1),
    (TYPE_OF, 1),
    ("PgPool", 0),
    ("PgTx", 0),
    ("PgCursor", 0),
    ("QueryResult", 0),
    ("ExecResult", 0),
    ("Value", 0),
];

/// The type of a type written as a value (`json.parse(s, Pt)`, a `type a`
/// parameter): `TypeOf(Pt)`, a prelude type.
pub const TYPE_OF: &str = "TypeOf";

/// The dispatch key of an anonymous record type, which no impl can
/// target: `type '<anon>' does not implement trait 'T'`. A builtin type
/// no program can name.
pub const ANON_RECORD: &str = "<anon>";

/// Every builtin type, with the builtin module that declares it (`None`
/// for the prelude), in the order of their ids: the `k`th is
/// `TypeId(DefId(k))`. The builtin definitions begin with them, so their
/// ids are known before anything else of the builtins is built. The
/// first are [`crate::typeinfo::RUNTIME_BUILTIN_TYPES`], whose ids are
/// the constants of [`crate::typeinfo::ty`]; then the prelude's, then
/// each module's: the records and enums the builtin registry declares
/// for it and its opaque handles.
pub fn builtin_types() -> &'static [(&'static str, Option<&'static str>)] {
    static TYPES: OnceLock<Vec<(&'static str, Option<&'static str>)>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let module_of = |name: &str| {
            crate::module::builtin_type_module(name).or_else(|| {
                OPAQUE_MODULE_TYPES
                    .iter()
                    .find(|(ty, _)| *ty == name)
                    .map(|(_, module)| *module)
            })
        };
        let mut types: Vec<(&'static str, Option<&'static str>)> =
            crate::typeinfo::RUNTIME_BUILTIN_TYPES
                .iter()
                .map(|name| (*name, module_of(name)))
                .collect();
        let mut add = |name: &'static str, module: Option<&'static str>| {
            if !types.iter().any(|(known, _)| *known == name) {
                types.push((name, module));
            }
        };
        for ty in crate::types::builtins::BUILTIN_TYPES {
            if ty.name == "()" || OPAQUE_MODULE_TYPES.iter().any(|(name, _)| *name == ty.name) {
                continue;
            }
            add(ty.name, None);
        }
        for ty in &crate::builtins::registry::registry().prelude_types {
            add(ty.name, None);
        }
        add(TYPE_OF, None);
        add(ANON_RECORD, None);
        for module in crate::module::builtin_modules() {
            let declared = crate::module::builtin_module_type_names(module).chain(
                OPAQUE_MODULE_TYPES
                    .iter()
                    .filter(|(_, m)| m == module)
                    .map(|(name, _)| *name),
            );
            for name in declared {
                add(name, Some(*module));
            }
        }
        types
    })
}

/// The builtin traits, in the order of their ids: the `k`th is
/// `TraitId(DefId(builtin_types().len() + k))`. The builtin definitions
/// enter them right after the builtin types.
pub const BUILTIN_TRAITS: &[&str] = &["Compare", "Display", "Equal", "Error", "Hash"];

/// The builtin trait whose method `method` is (`display` of Display,
/// `message` of Error); `None` for any other name.
pub fn builtin_trait_of_method(method: &str) -> Option<TraitId> {
    let name = match method {
        "display" => "Display",
        "compare" => "Compare",
        "equal" => "Equal",
        "hash" => "Hash",
        "message" => "Error",
        _ => return None,
    };
    builtin_trait_id(name)
}

/// The id of the builtin trait `name`; `None` when no builtin trait has
/// that name.
pub fn builtin_trait_id(name: &str) -> Option<TraitId> {
    BUILTIN_TRAITS
        .iter()
        .position(|t| *t == name)
        .map(|k| TraitId(DefId((builtin_types().len() + k) as u32)))
}

/// The id of the builtin type `name`; `None` when no builtin type has
/// that name. Builtin type names are unique.
pub fn builtin_type_id(name: &str) -> Option<TypeId> {
    static INDEX: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            builtin_types()
                .iter()
                .enumerate()
                .map(|(k, (name, _))| (*name, k as u32))
                .collect()
        })
        .get(name)
        .map(|k| TypeId(DefId(*k)))
}

/// The builtin definitions, shared by every [`DefTable`]: ids
/// `0..defs.len()`.
#[derive(Debug, Default)]
pub struct BuiltinDefs {
    pub defs: Vec<Def>,
    /// The variants of each builtin enum, in declaration order.
    pub variants: HashMap<DefId, Vec<DefId>>,
}

/// Every definition of a session.
#[derive(Clone, Debug)]
pub struct DefTable {
    builtins: Arc<BuiltinDefs>,
    /// The session's definitions: id `builtins.defs.len() + i` is
    /// `defs[i]`. A slot whose module was entered again and declares
    /// fewer names is `None` until it is reused.
    defs: Vec<Option<Def>>,
    /// The definitions of each module, in the order they were entered.
    by_module: HashMap<ModuleId, Vec<DefId>>,
    /// The variants of each enum of the session, in declaration order.
    variants: HashMap<DefId, Vec<DefId>>,
    /// The slots of each module that was cleared, to be reused.
    free: HashMap<ModuleId, VecDeque<DefId>>,
}

impl DefTable {
    pub fn new(builtins: Arc<BuiltinDefs>) -> DefTable {
        DefTable {
            builtins,
            defs: Vec::new(),
            by_module: HashMap::new(),
            variants: HashMap::new(),
            free: HashMap::new(),
        }
    }

    /// The definition `id`. Panics on an id of another table.
    pub fn get(&self, id: DefId) -> &Def {
        let index = id.0 as usize;
        let builtin = self.builtins.defs.len();
        if index < builtin {
            return &self.builtins.defs[index];
        }
        self.defs[index - builtin]
            .as_ref()
            .expect("a definition of a module that is entered")
    }

    /// The variants of the enum `ty`, in declaration order.
    pub fn variants(&self, ty: DefId) -> &[DefId] {
        self.builtins
            .variants
            .get(&ty)
            .or_else(|| self.variants.get(&ty))
            .map_or(&[], Vec::as_slice)
    }

    /// The type a variant belongs to; `None` for any other definition.
    pub fn variant_type(&self, variant: DefId) -> Option<&Def> {
        match self.get(variant).kind {
            DefKind::Variant { ty, .. } => Some(self.get(ty.0)),
            _ => None,
        }
    }

    /// The definitions of module `module`, in the order they were entered.
    pub fn of_module(&self, module: ModuleId) -> &[DefId] {
        self.by_module.get(&module).map_or(&[], Vec::as_slice)
    }

    /// Forget the definitions of `module` before it is entered again:
    /// its slots are reused by [`DefTable::add`].
    pub fn clear_module(&mut self, module: ModuleId) {
        let builtin = self.builtins.defs.len();
        let ids = self.by_module.remove(&module).unwrap_or_default();
        for id in &ids {
            self.defs[id.0 as usize - builtin] = None;
            self.variants.remove(id);
        }
        self.free.entry(module).or_default().extend(ids);
    }

    /// Enter `def` as a definition of its module, in a slot the module
    /// had before if one is free.
    pub fn add(&mut self, def: Def) -> DefId {
        let builtin = self.builtins.defs.len();
        let module = def.module;
        let id = match self.free.get_mut(&module).and_then(|free| free.pop_front()) {
            Some(id) => {
                self.defs[id.0 as usize - builtin] = Some(def);
                id
            }
            None => {
                self.defs.push(Some(def));
                DefId((builtin + self.defs.len() - 1) as u32)
            }
        };
        self.by_module.entry(module).or_default().push(id);
        id
    }

    /// Change the kind of the definition `id` of this session.
    pub fn set_kind(&mut self, id: DefId, kind: DefKind) {
        let builtin = self.builtins.defs.len();
        if let Some(def) = self.defs[id.0 as usize - builtin].as_mut() {
            def.kind = kind;
        }
    }

    /// Record `variant` as the next variant of the enum `ty`.
    pub fn add_variant(&mut self, ty: DefId, variant: DefId) {
        self.variants.entry(ty).or_default().push(variant);
    }
}
