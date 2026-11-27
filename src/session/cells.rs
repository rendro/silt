//! The entries of a REPL session ("cells"). Each is a module of its own,
//! `<repl:n>`, checked and compiled like any module, that sees what the
//! earlier cells that ran bind: the session imports into it the last
//! committed cell (`import <repl:k>.{ ... }`, a name no program can
//! write), which offers all it saw as well as all it declared (a REPL
//! cell is resolved as `names::ModuleKind::Cell`), and carries the
//! earlier cells' own imports over. A cell therefore imports one cell however long the session is.
//! A trait has one impl for a type in a session as in a file: a second
//! one is an error of the cell that writes it. (A written `Display`
//! impl of a later cell is used from then on where the value was shown
//! by its structure before.)
//!
//! A cell that is not committed (it had an error, or failed when it ran)
//! is never imported, so it leaves the session as it was. What a cell
//! that failed when it ran had installed in the VM (its own impls, the
//! modules it imported) is installed anew, in global slots of its own,
//! when a later cell declares or imports it again: the session takes
//! the failed cell's definitions out of its globals (`Cells::compiled`).
//!
//! A cell's definitions are new ones, with one exception. A name a later
//! cell binds again shadows the earlier binding for the cells after it,
//! while the code of the cells before keeps the definition it was checked
//! against: a type, a trait, a `let` (one that binds a name again reads
//! the old value in its initializer, `let x = x + 1`). In the VM each
//! definition has a global slot of its own, which the session keeps for
//! every cell (see `EarlierCells::globals`).
//!
//! The exception is a function a cell defines again (owner, 2026-11-26,
//! REPL-REDEF): it replaces the old one for every caller, the code of
//! the cells before, their closures and their tasks. With the old one's
//! type it takes the old one's global slot (`EarlierCells::redefined`),
//! and the VM puts it there before the cell runs (`Vm::run_cell`); a
//! call reads the slot as before, so it costs what it did. With another
//! type it is refused while code that the session binds now names the
//! old one (`Cells::users_of`), and is a new function in a slot of its
//! own when nothing does. A function value taken before (`let f = h`)
//! is the function it was then.

// The bookkeeping of the REPL's entries: a build without the REPL keeps
// it in the session but never adds an entry.
#![cfg_attr(not(feature = "repl"), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{self, Decl, ImportTarget};
use crate::bytecode::Globals;
use crate::compiler::EarlierCells;
use crate::intern::{Symbol, intern, resolve};
use crate::source::{FileId, Span};
#[cfg(feature = "repl")]
use crate::types::{Pred, Scheme, TyVar, Type, free_vars_in, substitute_vars};

use crate::defs::{DefId, DefKind, DefTable, ModuleId, Res};
use crate::program::cell_name;

/// What a top-level name of a cell is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fn,
    Let,
    /// A type or a trait.
    Type,
    /// A module, by `import m` or `import m as name`.
    Module,
    /// An item of `import m.{ name }`.
    Item,
}

/// What binds a top-level name of the session now.
#[derive(Clone)]
struct Binder {
    kind: Kind,
    /// For a function, its definition.
    def: Option<DefId>,
    /// Where it is bound: the name in the declaration.
    span: Span,
    /// For an import, the declaration as written and its span.
    import: Option<(ImportTarget, Span)>,
    /// For a `let`: the module that an earlier cell imported by the
    /// name, which the `let` hides.
    hides: Option<Symbol>,
}

/// One cell added to the session.
pub(super) struct CellInfo {
    /// Its number: it is `<repl:n>`.
    pub n: usize,
    pub file: FileId,
    /// How many declarations the session put before the cell's own: the
    /// imports of the earlier cells, then the earlier cells' imports.
    synthesized: usize,
    /// How many of those are imports of an earlier cell (none or one):
    /// the compiler does not see them.
    cell_imports: usize,
    /// The names the cell binds, with what they are.
    own: Vec<(Symbol, Span, Kind, Option<(ImportTarget, Span)>)>,
}

impl CellInfo {
    /// The name of the function that holds the cell's statements, when
    /// it is statements.
    pub(super) fn wrapper(&self) -> String {
        cell_name(self.n)
    }

    /// The names the cell binds.
    pub(super) fn names(&self) -> HashSet<Symbol> {
        self.own.iter().map(|(name, ..)| *name).collect()
    }

    /// The functions the cell declares, each with its name's place.
    pub(super) fn functions(&self) -> impl Iterator<Item = (Symbol, Span)> + '_ {
        (self.own.iter())
            .filter(|(_, _, kind, _)| *kind == Kind::Fn)
            .map(|(name, span, ..)| (*name, *span))
    }

    /// The cell's declarations as the compiler sees them: without the
    /// imports of the earlier cells, whose code is installed already.
    pub(super) fn compiled(&self, checked: &ast::Program) -> ast::Program {
        ast::Program {
            decls: checked.decls[self.cell_imports..].to_vec(),
            unknown: Vec::new(),
            statements: Default::default(),
            late_evidence: checked.late_evidence.clone(),
            dict_owners: checked.dict_owners.clone(),
            pattern_lets: checked.pattern_lets.clone(),
            held_types: checked.held_types.clone(),
            run_refusals: Vec::new(),
        }
    }
}

/// The cells of a session.
#[derive(Default)]
pub(super) struct Cells {
    /// The number of cells added.
    pub count: usize,
    pub info: HashMap<ModuleId, CellInfo>,
    /// The cells added, `info`'s keys: what a cell's check is told
    /// the session's cells are.
    ids: Arc<HashSet<ModuleId>>,
    /// The last committed cell.
    last: Option<ModuleId>,
    /// The cells added since that were not committed, and whose
    /// declarations the session has not forgotten yet.
    dropped: Vec<ModuleId>,
    /// What binds each top-level name the committed cells bind.
    scope: HashMap<Symbol, Binder>,
    /// The modules (not cells) whose code the committed cells installed.
    installed: HashSet<ModuleId>,
    /// The global slots of the cells that ran to their end, and the
    /// slots that cells that failed when they ran took: a slot is never
    /// given to another definition.
    pub globals: Globals,
    /// The globals the cell compiled last was compiled into, until it
    /// is committed: they are the session's then. If it is not (it
    /// failed when it ran), its definitions are taken back.
    compiled: Option<Globals>,
    /// The global slots of the definitions that the code of each
    /// top-level function and `let` the session binds now names, by its
    /// name: what a function's body names, what the closures of a
    /// `let`'s initializer name (the rest of it ran once). See
    /// [`Cells::users_of`].
    uses: HashMap<Symbol, HashSet<u16>>,
    /// The same of each method of an impl and default method of a trait
    /// of the committed cells, by its name (`Int.n`, `N.n`).
    method_uses: Vec<(String, HashSet<u16>)>,
}

impl Cells {
    /// The cells added that were not committed, each given once: what
    /// one declared is forgotten when the next cell is added. One that
    /// was compiled and failed when it ran keeps its global slots and
    /// loses its definitions (`Globals::without_definitions_since`).
    pub(super) fn take_dropped(&mut self) -> Vec<ModuleId> {
        if let Some(failed) = self.compiled.take() {
            self.globals = failed.without_definitions_since(&self.globals);
        }
        std::mem::take(&mut self.dropped)
    }

    /// The cell being evaluated was compiled into `globals`.
    pub(super) fn compiled(&mut self, globals: Globals) {
        self.compiled = Some(globals);
    }

    /// The cells added.
    pub(super) fn ids(&self) -> Arc<HashSet<ModuleId>> {
        self.ids.clone()
    }

    /// Make the parsed cell `id`, the `n`th, see the committed cells: its
    /// own declarations become exported (a later cell imports all of
    /// them), and the imports of what it sees are put before them.
    pub(super) fn prepare(
        &mut self,
        id: ModuleId,
        n: usize,
        file: FileId,
        program: &mut ast::Program,
    ) {
        let wrapper = intern(&cell_name(n));
        let mut own = Vec::new();
        for decl in &mut program.decls {
            let kind = match decl {
                Decl::Fn(f) if f.name == wrapper => continue,
                Decl::Fn(f) => {
                    f.is_pub = true;
                    Kind::Fn
                }
                Decl::Let { is_pub, .. } => {
                    *is_pub = true;
                    Kind::Let
                }
                Decl::Type(t) => {
                    t.is_pub = true;
                    Kind::Type
                }
                Decl::Trait(t) => {
                    t.is_pub = true;
                    Kind::Type
                }
                Decl::TraitImpl(_) => continue,
                Decl::Import(ImportTarget::Items(..), _) => Kind::Item,
                Decl::Import(..) => Kind::Module,
            };
            let import = match decl {
                Decl::Import(target, span) => Some((target.clone(), *span)),
                _ => None,
            };
            for (name, span, _) in crate::parser::top_level_binders(decl) {
                own.push((name, span, kind, import.clone()));
            }
        }
        // What the cell sees: each name bound by a committed cell that the
        // cell does not bind again, but for a `let`: its initializer reads
        // the value the name had (`let x = x + 1`). A function sees
        // itself, and so does a type.
        let own_names: HashSet<Symbol> = own
            .iter()
            .filter(|(_, _, kind, _)| *kind != Kind::Let)
            .map(|(name, ..)| *name)
            .collect();
        let mut seen: Vec<(Symbol, Span)> = Vec::new();
        let mut carried: Vec<Decl> = Vec::new();
        let mut items: BTreeMap<String, (Symbol, Span, Vec<(Symbol, Span)>)> = BTreeMap::new();
        let mut visible: Vec<(&Symbol, &Binder)> = self
            .scope
            .iter()
            .filter(|(name, _)| !own_names.contains(name))
            .collect();
        visible.sort_by_cached_key(|(name, _)| resolve(**name));
        // The modules the committed cells import, in the order of the
        // entries that import them: a module imported under two names is
        // named by its first import, as in a file (the checker reads the
        // imports in order).
        let cell_of: HashMap<FileId, usize> = (self.info.values())
            .map(|info| (info.file, info.n))
            .collect();
        let mut modules: Vec<((usize, u32), Decl)> = Vec::new();
        for (name, binder) in visible {
            match (binder.kind, &binder.import) {
                (Kind::Module, Some((target, span))) => {
                    let at = (cell_of.get(&span.file).copied().unwrap_or(0), span.start);
                    modules.push((at, Decl::Import(target.clone(), *span)));
                }
                (Kind::Item, Some((ImportTarget::Items(module, _), span))) => {
                    items
                        .entry(resolve(*module))
                        .or_insert_with(|| (*module, *span, Vec::new()))
                        .2
                        .push((*name, binder.span));
                }
                _ => seen.push((*name, binder.span)),
            }
        }
        modules.sort_by_key(|(at, _)| *at);
        carried.extend(modules.into_iter().map(|(_, decl)| decl));
        carried.extend(
            items.into_values().map(|(module, span, names)| {
                Decl::Import(ImportTarget::Items(module, names), span)
            }),
        );
        // The last committed cell offers every name of an earlier cell the
        // cell sees, and every type, trait and impl.
        let mut synthesized: Vec<Decl> = self
            .last
            .map(|cell| {
                let info = &self.info[&cell];
                Decl::Import(
                    ImportTarget::Items(intern(&cell_name(info.n)), seen),
                    Span::point(info.file, 0),
                )
            })
            .into_iter()
            .collect();
        let cell_imports = synthesized.len();
        synthesized.extend(carried);
        let count = synthesized.len();
        synthesized.append(&mut program.decls);
        program.decls = synthesized;
        Arc::make_mut(&mut self.ids).insert(id);
        self.dropped.push(id);
        self.info.insert(
            id,
            CellInfo {
                n,
                file,
                synthesized: count,
                cell_imports,
                own,
            },
        );
    }

    /// Note what the code of the declarations `own` of a cell that is
    /// committed names (`uses`, `method_uses`): it binds `names`, and
    /// its statements are the function `wrapper`, which ran.
    fn note_uses(&mut self, own: &[Decl], names: &[Symbol], wrapper: Symbol) {
        for name in names {
            self.uses.remove(name);
        }
        for decl in own {
            match decl {
                Decl::Fn(f) if f.name == wrapper => {}
                Decl::Fn(f) => {
                    let slots = self.slots_named(&f.body, false);
                    self.uses.insert(f.name, slots);
                }
                Decl::Let { pattern, value, .. } => self.note_let_uses(pattern, value),
                Decl::TraitImpl(ti) => {
                    for m in &ti.methods {
                        let slots = self.slots_named(&m.body, false);
                        let name = format!("{}.{}", ti.target_type, m.name);
                        self.method_uses.push((name, slots));
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter().filter(|m| !m.is_signature_only) {
                        let slots = self.slots_named(&m.body, false);
                        self.method_uses
                            .push((format!("{}.{}", t.name, m.name), slots));
                    }
                }
                Decl::Type(_) | Decl::Import(..) => {}
            }
        }
    }

    /// Note what the closures of the initializer `value` of a `let`
    /// that binds `pattern` name, for each name it binds: of a tuple
    /// written as one (`let (a, b) = ({ -> h() }, 1)`), what its own
    /// part names; of any other part, what all of it names.
    fn note_let_uses(&mut self, pattern: &ast::Pattern, value: &ast::Expr) {
        if let (ast::PatternKind::Tuple(parts), ast::ExprKind::Tuple(values)) =
            (&pattern.kind, &value.kind)
            && parts.len() == values.len()
        {
            for (part, value) in parts.iter().zip(values) {
                self.note_let_uses(part, value);
            }
            return;
        }
        let slots = self.slots_named(value, true);
        pattern.for_each_binder(&mut |name, _, _| {
            self.uses.insert(name, slots.clone());
        });
    }

    /// The global slots of the definitions `body` names; with
    /// `in_closures`, those its closures name only.
    fn slots_named(&self, body: &ast::Expr, in_closures: bool) -> HashSet<u16> {
        let mut slots = HashSet::new();
        let mut named = |e: &ast::Expr| {
            if let Some(Res::Def(def)) = e.res
                && let Some(slot) = self.globals.def(def)
            {
                slots.insert(slot);
            }
        };
        match in_closures {
            false => body.for_each_expr(&mut named),
            true => body.for_each_expr(&mut |e| {
                if matches!(e.kind, ast::ExprKind::Lambda { .. }) {
                    e.for_each_expr(&mut named);
                }
            }),
        }
        slots
    }

    /// The function the committed cells bind to `name`, with its slot.
    pub(super) fn function(&self, name: Symbol) -> Option<(DefId, u16)> {
        let def = self.scope.get(&name)?.def?;
        Some((def, self.globals.def(def)?))
    }

    /// What names the definition of the global slot `slot` of what the
    /// session binds now, but what a cell binds again (`replaced`): its
    /// functions and the `let`s whose closures name it, by name, then
    /// the methods of its impls and traits.
    pub(super) fn users_of(&self, slot: u16, replaced: &HashSet<Symbol>) -> Vec<(String, bool)> {
        let mut users: Vec<(String, bool)> = (self.uses.iter())
            .filter(|(name, slots)| !replaced.contains(*name) && slots.contains(&slot))
            .map(|(name, _)| (resolve(*name), true))
            .collect();
        users.sort();
        users.extend(
            (self.method_uses.iter())
                .filter(|(_, slots)| slots.contains(&slot))
                .map(|(name, _)| (name.clone(), false)),
        );
        users
    }

    /// Each name that the `let` of a committed cell binds where an
    /// earlier cell imported a module by that name, with the module.
    pub(super) fn hidden_imports(&self) -> HashMap<Symbol, Symbol> {
        (self.scope.iter())
            .filter_map(|(name, binder)| Some((*name, binder.hides?)))
            .collect()
    }

    /// What the compiler needs to know of the committed cells to compile
    /// a cell; `index` numbers the modules of its program.
    pub(super) fn earlier(&self, index: &HashMap<ModuleId, usize>) -> EarlierCells {
        EarlierCells {
            installed: self
                .installed
                .iter()
                .filter_map(|module| index.get(module).copied())
                .collect(),
            globals: self.globals.clone(),
            repl: true,
            redefined: HashMap::new(),
        }
    }

    /// The parsed cell `id` will not be checked again, and no later
    /// cell imports it: `program`, its declarations as
    /// [`Cells::prepare`] left them, gives up the names of the import
    /// of the cell before it, which are every name the cell saw. (The
    /// import itself stays: the graph counts a module's imports.)
    pub(super) fn release(&self, id: ModuleId, program: &mut ast::Program) {
        let imported = self.info.get(&id).map_or(0, |info| info.cell_imports);
        for decl in program.decls.iter_mut().take(imported) {
            if let Decl::Import(ImportTarget::Items(_, items), _) = decl {
                *items = Vec::new();
            }
        }
    }

    /// Commit cell `id`, which ran: later cells see what it binds. Its
    /// declarations are `checked`; `modules` are the modules of its
    /// program, whose code is installed now. Gives the cell that was
    /// the last committed one until now: no later cell imports it.
    pub(super) fn commit(
        &mut self,
        id: ModuleId,
        checked: &ast::Program,
        modules: &[ModuleId],
        defs: &DefTable,
    ) -> Option<ModuleId> {
        let info = &self.info[&id];
        for (name, span, kind, import) in &info.own {
            let def = (*kind == Kind::Fn)
                .then(|| {
                    (defs.named(id, *name).iter().copied())
                        .find(|def| matches!(defs.get(*def).kind, DefKind::Fn))
                })
                .flatten();
            let hides = match (kind, self.scope.get(name)) {
                (Kind::Let, Some(before)) if before.kind == Kind::Module => {
                    before.import.as_ref().map(|(target, _)| target.module())
                }
                (Kind::Let, Some(before)) => before.hides,
                _ => None,
            };
            self.scope.insert(
                *name,
                Binder {
                    kind: *kind,
                    def,
                    span: *span,
                    import: import.clone(),
                    hides,
                },
            );
        }
        let own = &checked.decls[info.synthesized..];
        // A variant the cell declares shadows a name an earlier cell
        // imported by name: later cells see the variant.
        for decl in own {
            if let Decl::Type(t) = decl
                && let ast::TypeBody::Enum(variants) = &t.body
            {
                for v in variants {
                    if self
                        .scope
                        .get(&v.name)
                        .is_some_and(|b| b.kind == Kind::Item)
                    {
                        self.scope.remove(&v.name);
                    }
                }
            }
        }
        self.dropped.retain(|cell| *cell != id);
        if let Some(globals) = self.compiled.take() {
            self.globals = globals;
        }
        let names: Vec<Symbol> = info.own.iter().map(|(name, ..)| *name).collect();
        let wrapper = intern(&info.wrapper());
        self.note_uses(own, &names, wrapper);
        self.installed.extend(
            modules
                .iter()
                .filter(|module| !self.info.contains_key(module)),
        );
        self.last.replace(id).filter(|before| *before != id)
    }
}

/// Whether two schemes are one type: the same type, and the same
/// bounds, whatever numbers their variables have. The bounds that pass
/// a dictionary are compared in order, which is the order of the
/// dictionaries: the checker orders a scheme's bounds by the place of
/// their variables in its type whatever the body or the `where` clauses
/// (`order_trait_preds`), so only two bounds of one variable can come in
/// another order. The others, which pass nothing, in any order. A function defined again
/// with the same type takes the place of the old one for its callers,
/// which were checked against that type and pass its dictionaries.
/// A variable the scheme does not quantify (a type a later entry
/// decides, of a `let` the function reads) is one variable of the
/// session's: the same in both, or they differ.
#[cfg(feature = "repl")]
pub(super) fn same_scheme(a: &Scheme, b: &Scheme, ordered: bool) -> bool {
    fn canonical(scheme: &Scheme) -> (Type, Vec<Pred>) {
        let mapping: HashMap<TyVar, Type> = (free_vars_in(&scheme.ty).into_iter())
            .filter(|var| scheme.vars.contains(var))
            .enumerate()
            .map(|(k, var)| (var, Type::Var(usize::MAX - k)))
            .collect();
        let preds = scheme
            .preds
            .iter()
            .map(|p| p.substitute(&mapping))
            .collect();
        (substitute_vars(&scheme.ty, &mapping), preds)
    }
    // (What a bound names to say who asks it is no part of the type.)
    fn same_pred(a: &Pred, b: &Pred) -> bool {
        match (a, b) {
            (
                Pred::Trait {
                    tr, args, subject, ..
                },
                Pred::Trait {
                    tr: tr2,
                    args: args2,
                    subject: subject2,
                    ..
                },
            ) => tr == tr2 && args == args2 && subject == subject2,
            (
                Pred::Builtin { check, subject, .. },
                Pred::Builtin {
                    check: check2,
                    subject: subject2,
                    ..
                },
            ) => check.unwritten() == check2.unwritten() && subject == subject2,
            // (Where in the body the method is called is not either.)
            (
                Pred::Via {
                    tr,
                    method,
                    receiver,
                    method_ty,
                    ..
                },
                Pred::Via {
                    tr: tr2,
                    method: method2,
                    receiver: receiver2,
                    method_ty: method_ty2,
                    ..
                },
            ) => tr == tr2 && method == method2 && receiver == receiver2 && method_ty == method_ty2,
            (
                Pred::Given {
                    subject,
                    given,
                    passer,
                    ..
                },
                Pred::Given {
                    subject: subject2,
                    given: given2,
                    passer: passer2,
                    ..
                },
            ) => subject == subject2 && given == given2 && passer == passer2,
            _ => a == b,
        }
    }
    let ((ty, preds), (ty2, preds2)) = (canonical(a), canonical(b));
    let traits = |preds: &[Pred]| -> Vec<Pred> {
        (preds.iter())
            .filter(|p| crate::typechecker::passes_dictionary(p))
            .cloned()
            .collect()
    };
    let (traits1, traits2) = (traits(&preds), traits(&preds2));
    let others = |preds: &[Pred]| -> Vec<Pred> {
        (preds.iter())
            .filter(|p| !crate::typechecker::passes_dictionary(p))
            .cloned()
            .collect()
    };
    let same_bag = |ones: &[Pred], mut twos: Vec<Pred>| {
        ones.len() == twos.len()
            && ones
                .iter()
                .all(|a| match twos.iter().position(|b| same_pred(a, b)) {
                    Some(at) => {
                        twos.swap_remove(at);
                        true
                    }
                    None => false,
                })
    };
    let same_traits = match ordered {
        true => {
            traits1.len() == traits2.len()
                && traits1.iter().zip(&traits2).all(|(a, b)| same_pred(a, b))
        }
        false => same_bag(&traits1, traits2),
    };
    ty == ty2 && same_bag(&others(&preds), others(&preds2)) && same_traits
}

/// The error of the function `name`, at `span`, that a cell defines
/// again with the type `is` where it was `was`, while `users` name it
/// (each with whether a cell can define it again: a function or a
/// `let`, not a method).
#[cfg(feature = "repl")]
pub(super) fn redefined_with_another_type(
    name: Symbol,
    span: Span,
    was: &str,
    is: &str,
    users: &[(String, bool)],
    reordered: bool,
) -> crate::diagnostic::Diagnostic {
    use crate::diagnostic::{Code, Diagnostic};
    let listed = |names: &[&String]| {
        let names: Vec<String> = names.iter().map(|name| format!("'{name}'")).collect();
        match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.concat(),
        }
    };
    let all: Vec<&String> = users.iter().map(|(name, _)| name).collect();
    let verb = if all.len() == 1 { "uses" } else { "use" };
    let again: Vec<&String> = (users.iter())
        .filter(|(_, again)| *again)
        .map(|(name, _)| name)
        .collect();
    let old_bounds = was.split_once(" where ").map(|(_, bounds)| bounds);
    let help = match again.is_empty() || again.len() < users.len() {
        _ if reordered && let Some(bounds) = old_bounds => {
            format!("write its bounds in the old order ({bounds})")
        }
        true => "give the new function another name".to_string(),
        false => format!(
            "give the new function another name, or first define {} again without '{name}'",
            listed(&again)
        ),
    };
    Diagnostic::error(
        Code::TypeMismatch,
        span,
        format!(
            "function '{name}' is defined again with another type, and {} {verb} it",
            listed(&all)
        ),
    )
    .with_note(match was == is && was.contains('_') {
        true => format!("it was {was}, and is {is} here, where a later entry decides each '_'"),
        false => format!("it was {was}, and is {is} here"),
    })
    .with_note(
        "a function defined again replaces the old one for every caller, so it keeps its type",
    )
    .with_help(help)
}
