//! The entries of a REPL session ("cells"). Each is a module of its own,
//! `<repl:n>`, checked and compiled like any module, that sees what the
//! earlier cells that ran bind: the session imports each earlier cell
//! into it (`import <repl:k>.{ ... }`, a name no program can write) and
//! carries the earlier cells' own imports over.
//!
//! A cell that is not committed (it had an error, or failed when it ran)
//! is never imported, so it leaves the session as it was.
//!
//! Redefinition is early-bound: a cell's definitions are new ones. A name
//! a later cell binds again shadows the earlier binding for the cells
//! after it, while the code of the cells before keeps the definition it
//! was checked against. In the VM, a value whose name an earlier cell
//! already installed a global under gets a global of its own,
//! `<repl:n>.name`; the compiler installs and reads it under that global
//! (see `EarlierCells::globals`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{self, Decl, ImportTarget};
use crate::compiler::EarlierCells;
use crate::intern::{Symbol, intern, resolve};
use crate::source::{FileId, Span};

use super::graph::{ModuleId, cell_name};

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
    /// The cell whose declaration binds it.
    cell: ModuleId,
    kind: Kind,
    /// Where it is bound: the name in the declaration.
    span: Span,
    /// For an import, the declaration as written and its span.
    import: Option<(ImportTarget, Span)>,
    /// For a value (a function, a `let`, an imported item), the VM
    /// global it is installed under.
    global: Option<String>,
}

/// One cell added to the session.
pub(super) struct CellInfo {
    /// Its number: it is `<repl:n>`.
    pub n: usize,
    pub file: FileId,
    /// How many declarations the session put before the cell's own: the
    /// imports of the earlier cells, then the earlier cells' imports.
    synthesized: usize,
    /// How many of those are imports of earlier cells: the compiler does
    /// not see them.
    cell_imports: usize,
    /// The names the cell binds, with what they are.
    own: Vec<(Symbol, Span, Kind, Option<(ImportTarget, Span)>)>,
    /// The global of each value the cell binds.
    globals: HashMap<Symbol, String>,
}

impl CellInfo {
    /// The name of the function that holds the cell's statements, when
    /// it is statements.
    pub fn wrapper(&self) -> String {
        cell_name(self.n)
    }

    /// The cell's declarations as the compiler sees them: without the
    /// imports of the earlier cells, whose code is installed already.
    pub fn compiled(&self, checked: &ast::Program) -> ast::Program {
        ast::Program {
            decls: checked.decls[self.cell_imports..].to_vec(),
        }
    }
}

/// The cells of a session.
#[derive(Default)]
pub(super) struct Cells {
    /// The number of cells added.
    pub count: usize,
    pub info: HashMap<ModuleId, CellInfo>,
    /// The committed cells, oldest first, and the declarations of each
    /// that are its own, as checked.
    committed: Vec<(ModuleId, Arc<ast::Program>)>,
    /// What binds each top-level name the committed cells bind.
    scope: HashMap<Symbol, Binder>,
    /// The VM globals the committed cells' values are installed under.
    taken: HashSet<String>,
    /// The modules (not cells) whose code the committed cells installed.
    installed: HashSet<ModuleId>,
}

impl Cells {
    /// Make the parsed cell `id`, the `n`th, see the committed cells: its
    /// own declarations become exported (a later cell imports all of
    /// them), and the imports of what it sees are put before them.
    pub fn prepare(&mut self, id: ModuleId, n: usize, file: FileId, program: &mut ast::Program) {
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
                Decl::Trait(_) => Kind::Type,
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
        let globals = own
            .iter()
            .filter(|(_, _, kind, _)| matches!(kind, Kind::Fn | Kind::Let | Kind::Item))
            .filter(|(name, ..)| self.taken.contains(&resolve(*name)))
            .map(|(name, ..)| (*name, format!("{}.{name}", cell_name(n))))
            .collect();

        // What the cell sees: each name bound by a committed cell that the
        // cell does not bind again.
        let own_names: HashSet<Symbol> = own.iter().map(|(name, ..)| *name).collect();
        let mut by_cell: HashMap<ModuleId, Vec<(Symbol, Span)>> = HashMap::new();
        let mut carried: Vec<Decl> = Vec::new();
        let mut items: BTreeMap<String, (Symbol, Span, Vec<(Symbol, Span)>)> = BTreeMap::new();
        let mut visible: Vec<(&Symbol, &Binder)> = self
            .scope
            .iter()
            .filter(|(name, _)| !own_names.contains(name))
            .collect();
        visible.sort_by_key(|(name, _)| resolve(**name));
        for (name, binder) in visible {
            match (binder.kind, &binder.import) {
                (Kind::Module, Some((target, span))) => {
                    carried.push(Decl::Import(target.clone(), *span));
                }
                (Kind::Item, Some((ImportTarget::Items(module, _), span))) => {
                    items
                        .entry(resolve(*module))
                        .or_insert_with(|| (*module, *span, Vec::new()))
                        .2
                        .push((*name, binder.span));
                }
                _ => by_cell
                    .entry(binder.cell)
                    .or_default()
                    .push((*name, binder.span)),
            }
        }
        carried.extend(
            items.into_values().map(|(module, span, names)| {
                Decl::Import(ImportTarget::Items(module, names), span)
            }),
        );
        // Every committed cell is imported, the newest first, so that of
        // two types of one name the newer is the one the cell sees; one
        // that binds nothing the cell sees still brings its trait impls.
        let mut synthesized: Vec<Decl> = self
            .committed
            .iter()
            .rev()
            .map(|(cell, _)| {
                let info = &self.info[cell];
                Decl::Import(
                    ImportTarget::Items(
                        intern(&cell_name(info.n)),
                        by_cell.remove(cell).unwrap_or_default(),
                    ),
                    Span::point(info.file, 0),
                )
            })
            .collect();
        let cell_imports = synthesized.len();
        synthesized.extend(carried);
        let count = synthesized.len();
        synthesized.append(&mut program.decls);
        program.decls = synthesized;
        self.info.insert(
            id,
            CellInfo {
                n,
                file,
                synthesized: count,
                cell_imports,
                own,
                globals,
            },
        );
    }

    /// What the compiler needs to know of the committed cells to compile
    /// cell `id`; `index` numbers the modules of its program.
    pub fn earlier(&self, id: ModuleId, index: &HashMap<ModuleId, usize>) -> EarlierCells {
        let info = &self.info[&id];
        let own: HashSet<Symbol> = info.own.iter().map(|(name, ..)| *name).collect();
        let mut earlier = EarlierCells {
            programs: self
                .committed
                .iter()
                .map(|(_, program)| program.clone())
                .collect(),
            globals: info.globals.clone(),
            installed: self
                .installed
                .iter()
                .filter_map(|module| index.get(module).copied())
                .collect(),
            ..EarlierCells::default()
        };
        for (name, binder) in self.scope.iter().filter(|(name, _)| !own.contains(name)) {
            match binder.kind {
                Kind::Fn => {
                    earlier.fns.insert(*name);
                }
                Kind::Let => {
                    earlier.lets.insert(*name);
                }
                _ => {}
            }
            if let Some(global) = &binder.global
                && *global != resolve(*name)
            {
                earlier.globals.insert(*name, global.clone());
            }
        }
        earlier
    }

    /// Commit cell `id`, which ran: later cells see what it binds. Its
    /// declarations are `checked`; `modules` are the modules of its
    /// program, whose code is installed now.
    pub fn commit(&mut self, id: ModuleId, checked: &ast::Program, modules: &[ModuleId]) {
        let info = &self.info[&id];
        for (name, span, kind, import) in &info.own {
            let global = matches!(kind, Kind::Fn | Kind::Let | Kind::Item).then(|| {
                info.globals
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| resolve(*name))
            });
            if let Some(global) = &global {
                self.taken.insert(global.clone());
            }
            self.scope.insert(
                *name,
                Binder {
                    cell: id,
                    kind: *kind,
                    span: *span,
                    import: import.clone(),
                    global,
                },
            );
        }
        let own = ast::Program {
            decls: checked.decls[info.synthesized..].to_vec(),
        };
        self.committed.push((id, Arc::new(own)));
        self.installed.extend(
            modules
                .iter()
                .filter(|module| !self.info.contains_key(module)),
        );
    }
}
