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
use std::sync::Arc;

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
#[derive(Clone, Debug)]
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
/// [`crate::module::BUILTIN_MODULES`] is `ModuleId(BUILTIN_MODULE_BASE +
/// 1 + k)`. A module graph never has that many modules.
const BUILTIN_MODULE_BASE: u32 = 0xFFFF_0000;

impl ModuleId {
    /// The prelude: the names every module sees without an import.
    pub const PRELUDE: ModuleId = ModuleId(BUILTIN_MODULE_BASE);

    /// The pseudo-module of the builtin module `name` (`list`, ...).
    pub fn builtin(name: &str) -> Option<ModuleId> {
        crate::module::BUILTIN_MODULES
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
        crate::module::BUILTIN_MODULES.get(k as usize).copied()
    }
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
