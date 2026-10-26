//! The session's view of an open document's module and of the modules it
//! imports, for the features that cross files: definition, hover and
//! signature help on `m.f` and on an item of `import m.{ f }`.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::Uri;

use crate::ast::{Decl, ImportTarget, Program};
use crate::intern::Symbol;
use crate::lexer::{Tok, Token};
use crate::session::{ImportResolution, ModuleAnalysis};

use super::Server;
use super::definitions::build_definitions;
use super::fields::RecordFields;
use super::local_bindings::nearest_local_binding_for;
use super::project::path_key;
use super::state::{DefInfo, Document};

/// An imported module, as the session has it.
pub(super) struct ModuleView {
    /// The module's name, as `import` names it (not an alias).
    pub(super) name: Symbol,
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

/// The fields of the record types of `tables` that the module `module`
/// sees: those of an opaque record are its own module's alone.
pub(super) fn visible_fields(
    tables: &crate::typechecker::Tables,
    defs: &crate::defs::DefTable,
    module: crate::session::ModuleId,
) -> super::fields::RecordFields {
    let mut record_fields = tables.record_fields();
    record_fields.retain(|ty, _| {
        let def = defs.get(ty.id.0);
        def.vis != crate::defs::Vis::Opaque || def.module == module
    });
    record_fields
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
                // (Another module cannot build an opaque type's values.)
                if let crate::ast::TypeBody::Enum(variants) = &t.body
                    && !t.is_opaque
                {
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
    /// The fields of the record types of `tables` that the open document
    /// `doc` sees: those of an opaque record are its module's alone.
    pub(super) fn visible_record_fields(
        &self,
        doc: &Document,
        tables: &crate::typechecker::Tables,
    ) -> super::fields::RecordFields {
        match doc
            .module
            .as_ref()
            .and_then(|m| Some((m, self.projects.get(&m.project)?)))
        {
            Some((module, project)) => visible_fields(tables, project.session.defs(), module.id),
            None => tables.record_fields(),
        }
    }

    pub(super) fn checked_tables(&self, doc: &Document) -> Option<&crate::typechecker::Tables> {
        let module = doc.module.as_ref()?;
        Some(self.projects.get(&module.project)?.session.tables())
    }

    /// The module the name `name` stands for in the open document `doc`:
    /// the one `import name` imports, or `import m as name` (of two
    /// that bind the name, the one the checker binds: the first in the
    /// one order of imports).
    pub(super) fn imported_module(&self, doc: &Document, name: Symbol) -> Option<ModuleView> {
        let program = doc.program.as_ref()?;
        let module_name = crate::ast::imports_in_order(&program.decls)
            .into_iter()
            .find_map(|decl| match decl {
                Decl::Import(ImportTarget::Alias(module, alias, _), _) if *alias == name => {
                    Some(*module)
                }
                _ => None,
            })
            .unwrap_or(name);
        self.module_view(doc, module_name)
    }

    /// The hover of a module of the program: its name and file.
    pub(super) fn module_hover(view: &ModuleView) -> String {
        let file = view
            .uri
            .path()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        format!(
            "```silt\nmodule {}\n```\n\n{file}",
            crate::intern::resolve(view.name)
        )
    }

    /// The hover of the module an import of the open document `doc`
    /// names at byte `cursor` (`list` in `import list`, `time` and `t`
    /// in `import time as t`): a module of the program as
    /// `module_hover` shows it, one of silt's own as
    /// `builtin_module_hover` does.
    pub(super) fn imported_module_hover(&self, doc: &Document, cursor: usize) -> Option<String> {
        let program = doc.program.as_ref()?;
        let text = &doc.source.text;
        let module = program.decls.iter().find_map(|decl| {
            let Decl::Import(target, span) = decl else {
                return None;
            };
            let module = target.module();
            let rest = text
                .get(span.start as usize..span.end as usize)?
                .strip_prefix("import")?;
            let start = span.start as usize + "import".len() + rest.len() - rest.trim_start().len();
            let end = start + crate::intern::resolve(module).len();
            let alias = match target {
                ImportTarget::Alias(_, _, at) => at.start as usize..at.end as usize,
                _ => 0..0,
            };
            ((start..end).contains(&cursor) || alias.contains(&cursor)).then_some(module)
        })?;
        if let Some(view) = self.module_view(doc, module) {
            return Some(Self::module_hover(&view));
        }
        builtin_module_hover(module)
    }

    /// The hover of the builtin module the name `name` stands for in the
    /// open document `doc`, as the checker binds it: the one `import
    /// name` imports, or `import m as name` (of two, the first in the
    /// one order of imports).
    pub(super) fn imported_builtin_module_hover(
        &self,
        doc: &Document,
        name: Symbol,
    ) -> Option<String> {
        let program = doc.program.as_ref()?;
        let module = crate::ast::imports_in_order(&program.decls)
            .into_iter()
            .find_map(|decl| match decl {
                Decl::Import(ImportTarget::Alias(module, alias, _), _) if *alias == name => {
                    Some(*module)
                }
                Decl::Import(ImportTarget::Module(module), _) if *module == name => Some(*module),
                _ => None,
            })?;
        builtin_module_hover(module)
    }

    /// The module that binds `name` bare in the open document `doc`
    /// through `import m.{ name }` (of two, the first in the one order
    /// of imports, as the checker binds it).
    pub(super) fn item_module(&self, doc: &Document, name: Symbol) -> Option<ModuleView> {
        let program = doc.program.as_ref()?;
        let imports = crate::ast::imports_in_order(&program.decls);
        let module_name = imports.into_iter().find_map(|decl| match decl {
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
                super::definitions::with_signatures(
                    build_definitions(
                        &checked.ast,
                        Some(&checked.top_level),
                        &session.tables().record_fields(),
                    ),
                    &checked.signatures,
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
            name: module_name,
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

/// The hover of silt's own module `module`, if its feature is built: its
/// name and the sentence that opens its reference page.
fn builtin_module_hover(module: Symbol) -> Option<String> {
    let name = crate::intern::resolve(module);
    let about = crate::builtins::registry::docs::module_docs().get(name.as_str())?;
    Some(format!("```silt\nmodule {name}\n```\n\n{about}"))
}

/// The `(module, member)` of the qualified access `module.member` whose
/// member name holds `cursor`, when its receiver is a plain identifier.
pub(super) fn qualified_access_at(program: &Program, cursor: usize) -> Option<(Symbol, Symbol)> {
    find_qualified_access(program, |_, member| member.contains(&cursor))
}

/// The name before a dot at byte `cursor` of the open document `doc`
/// that names a module, if one does (`geo` in `geo.mk`, `json` in
/// `json.Encode`): in an expression, a receiver that is no local of
/// that name; in a type or a pattern, a qualifier, which is a module
/// whatever local is spelled like it.
pub(super) fn module_qualifier_at(doc: &Document, cursor: usize) -> Option<Symbol> {
    match qualifier_at(doc.program.as_ref()?, cursor) {
        Some(name) => Some(name)
            .filter(|name| nearest_local_binding_for(&doc.locals, *name, cursor).is_none()),
        None => type_qualifier_at(doc.tokens(), cursor),
    }
}

/// The name before a dot at byte `cursor` when a capitalised name
/// follows the dot, as the lexer reads `tokens`: the module of a
/// qualified type name written in an annotation (`helpers` in `q:
/// helpers.PtA`), which is no expression. Not in a string or a comment,
/// which are no names, and not on the dot.
fn type_qualifier_at(tokens: &[Tok], cursor: usize) -> Option<Symbol> {
    let at = tokens
        .iter()
        .position(|tok| (tok.span.start as usize..tok.span.end as usize).contains(&cursor))?;
    match tokens.get(at..at + 3)? {
        [
            Tok {
                kind: Token::Ident(module),
                ..
            },
            Tok {
                kind: Token::Dot, ..
            },
            Tok {
                kind: Token::Ident(name),
                ..
            },
        ] if crate::intern::resolve(*name).starts_with(|c: char| c.is_ascii_uppercase()) => {
            Some(*module)
        }
        _ => None,
    }
}

/// The receiver `module` of a qualified access `module.member` whose
/// receiver holds `cursor`, when it is a plain identifier.
fn qualifier_at(program: &Program, cursor: usize) -> Option<Symbol> {
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
