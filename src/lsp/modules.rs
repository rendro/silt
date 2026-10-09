//! The session's view of an open document's module and of the modules it
//! imports, for the features that cross files: definition, hover and
//! signature help on `m.f` and on an item of `import m.{ f }`.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::Uri;

use crate::ast::{Decl, ImportTarget, Program};
use crate::intern::Symbol;
use crate::session::{ImportResolution, ModuleAnalysis};

use super::Server;
use super::definitions::build_definitions;
use super::fields::RecordFields;
use super::project::path_key;
use super::state::{DefInfo, Document};

/// An imported module, as the session has it.
pub(super) struct ModuleView {
    pub(super) uri: Uri,
    /// Its top-level definitions, with the checker's types.
    pub(super) definitions: HashMap<Symbol, DefInfo>,
    /// What it exports, in declaration order: its `pub` functions,
    /// `let`s and types, and the variants of its `pub` enums.
    pub(super) members: Vec<(Symbol, MemberKind)>,
}

/// What an exported member of a module is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MemberKind {
    Function,
    Value,
    Type,
    Variant,
}

/// The exported members of `program`.
fn members(program: &Program) -> Vec<(Symbol, MemberKind)> {
    let mut out = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) if f.is_pub => out.push((f.name, MemberKind::Function)),
            Decl::Let {
                pattern,
                is_pub: true,
                ..
            } => {
                if let crate::ast::PatternKind::Ident(name) = &pattern.kind {
                    out.push((*name, MemberKind::Value));
                }
            }
            Decl::Type(t) if t.is_pub => {
                out.push((t.name, MemberKind::Type));
                if let crate::ast::TypeBody::Enum(variants) = &t.body {
                    out.extend(variants.iter().map(|v| (v.name, MemberKind::Variant)));
                }
            }
            _ => {}
        }
    }
    out
}

impl Server {
    /// The session's analysis of the open document `doc`'s module.
    pub(super) fn checked_module(&self, doc: &Document) -> Option<&ModuleAnalysis> {
        let module = doc.module.as_ref()?;
        self.projects
            .get(&module.project)?
            .session
            .module_analysis(module.id)
    }

    /// What the checks of the open document `doc`'s session found: its
    /// types, records and methods.
    pub(super) fn checked_tables(&self, doc: &Document) -> Option<&crate::typechecker::Tables> {
        let module = doc.module.as_ref()?;
        Some(self.projects.get(&module.project)?.session.tables())
    }

    /// The module the name `name` stands for in the open document `doc`:
    /// the one `import name` imports, or `import m as name`.
    pub(super) fn imported_module(&self, doc: &Document, name: Symbol) -> Option<ModuleView> {
        let program = doc.program.as_ref()?;
        let module_name = program
            .decls
            .iter()
            .find_map(|decl| match decl {
                Decl::Import(ImportTarget::Alias(module, alias, _), _) if *alias == name => {
                    Some(*module)
                }
                _ => None,
            })
            .unwrap_or(name);
        self.module_view(doc, module_name)
    }

    /// The module that binds `name` bare in the open document `doc`
    /// through `import m.{ name }`.
    pub(super) fn item_module(&self, doc: &Document, name: Symbol) -> Option<ModuleView> {
        let program = doc.program.as_ref()?;
        let module_name = program.decls.iter().find_map(|decl| match decl {
            Decl::Import(ImportTarget::Items(module, items), _)
                if items.iter().any(|(item, _)| *item == name) =>
            {
                Some(*module)
            }
            _ => None,
        })?;
        self.module_view(doc, module_name)
    }

    /// The module the import of `module_name` in `doc` resolves to.
    fn module_view(&self, doc: &Document, module_name: Symbol) -> Option<ModuleView> {
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        let graph = session.graph();
        let target =
            graph
                .module(module.id)
                .imports
                .iter()
                .find_map(|import| match import.resolution {
                    ImportResolution::Module(id) if import.name == module_name => Some(id),
                    _ => None,
                })?;
        let target_module = graph.module(target);
        target_module.file?;
        let (definitions, members) = match session.module_analysis(target) {
            Some(checked) => (
                build_definitions(
                    &checked.ast,
                    Some(&checked.top_level),
                    &session.tables().record_fields(),
                ),
                members(&checked.ast),
            ),
            None => {
                let ast = target_module.ast.as_ref()?;
                (
                    build_definitions(ast, None, &RecordFields::new()),
                    members(ast),
                )
            }
        };
        Some(ModuleView {
            uri: self.uri_for_path(&target_module.path)?,
            definitions,
            members,
        })
    }

    /// The URI of the file at `path`: the open document's when one names
    /// it, else an indexed one's.
    pub(super) fn uri_for_path(&self, path: &Path) -> Option<Uri> {
        let key = path_key(path);
        let named = |doc: &Document| doc.path == path || doc.key == key;
        let found = |open: bool| {
            self.documents
                .iter()
                .find(|(_, doc)| doc.open == open && named(doc))
                .map(|(uri, _)| uri.clone())
        };
        found(true)
            .or_else(|| found(false))
            .or_else(|| super::path_to_file_uri(path))
    }
}

/// The `(module, member)` of the qualified access `module.member` whose
/// member name holds `cursor`, when its receiver is a plain identifier.
pub(super) fn qualified_access_at(program: &Program, cursor: usize) -> Option<(Symbol, Symbol)> {
    find_qualified_access(program, |_, member| member.contains(&cursor))
}

/// The receiver `module` of a qualified access `module.member` whose
/// receiver holds `cursor`, when it is a plain identifier.
pub(super) fn qualifier_at(program: &Program, cursor: usize) -> Option<Symbol> {
    find_qualified_access(program, |receiver, _| receiver.contains(&cursor)).map(|(m, _)| m)
}

/// The `(receiver, member)` of the last qualified access `name.member`
/// (a plain identifier before the dot) for which `hit` accepts the byte
/// ranges of the receiver and of the member name.
fn find_qualified_access(
    program: &Program,
    hit: impl Fn(std::ops::Range<usize>, std::ops::Range<usize>) -> bool,
) -> Option<(Symbol, Symbol)> {
    use super::ast_walk::visit_expr_children;
    use crate::ast::{Expr, ExprKind};

    fn walk(
        expr: &Expr,
        hit: &dyn Fn(std::ops::Range<usize>, std::ops::Range<usize>) -> bool,
        found: &mut Option<(Symbol, Symbol)>,
    ) {
        if let ExprKind::FieldAccess(receiver, field, field_span) = &expr.kind
            && let ExprKind::Ident(module) = &receiver.kind
            && hit(
                receiver.span.start as usize..receiver.span.end as usize,
                field_span.start as usize..field_span.end as usize,
            )
        {
            *found = Some((*module, *field));
        }
        visit_expr_children(expr, |child| walk(child, hit, found));
    }
    let mut found = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => walk(&f.body, &hit, &mut found),
            Decl::Let { value, .. } => walk(value, &hit, &mut found),
            Decl::Trait(t) => t
                .methods
                .iter()
                .for_each(|m| walk(&m.body, &hit, &mut found)),
            Decl::TraitImpl(ti) => ti
                .methods
                .iter()
                .for_each(|m| walk(&m.body, &hit, &mut found)),
            _ => {}
        }
    }
    found
}
