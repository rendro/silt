//! `textDocument/completion` handler and its dot-completion helpers.

use std::collections::HashSet;
use std::sync::Arc;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, Documentation, MarkupContent,
    MarkupKind, Position, Uri,
};

use crate::ast::*;
use crate::defs::DefKind;
use crate::intern::{Symbol, intern, resolve};
use crate::lexer::{KEYWORD_LITERALS, KEYWORDS};
use crate::module;
use crate::typechecker::names::Binding;
use crate::types::Type;
use crate::types::canonical::canonical_name;

use super::Server;
use super::ast_walk::find_ident_type_by_name;
use super::conversions::position_to_offset;
use super::fields::{RecordFields, record_fields_from_type};
use super::locals::locals_at_offset;
use super::modules::{MemberKind, ModuleView};
use super::state::Document;

impl Server {
    // ── Completion ─────────────────────────────────────────────────

    pub(super) fn completion(
        &mut self,
        params: lsp_types::CompletionParams,
    ) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        // Detect dot-completion context: extract the identifier before the `.`
        let prefix = self
            .documents
            .get(uri)
            .and_then(|doc| extract_dot_prefix(&doc.source.text, &pos));
        if let Some(prefix) = prefix {
            let doc = self.documents.get(uri)?;
            let cursor = position_to_offset(&doc.source, &pos);
            // A module before the dot: its members.
            if let Some(items) = self.module_member_completions(doc, &prefix, cursor) {
                return Some(CompletionResponse::Array(items));
            }
            // An enum's name before the dot: its variants.
            if let Some(items) = self.enum_variant_completions(doc, &prefix, cursor) {
                return Some(CompletionResponse::Array(items));
            }
            let builtin = self.bound_builtin_module(doc, &prefix, cursor);
            // Round 81: the document's program was parsed from the user's
            // exact source. A partial expression at the cursor (`xs.|`)
            // is a parse error that the parser's recovery currently turns
            // into an empty function-body stub — every earlier `let`
            // inside that fn vanishes from `program.decls`, so
            // `locals_at_offset` returns nothing and the type-narrowing
            // path can't find the receiver's inferred type. Analyse a
            // fix-up source where the partial dot is completed with a
            // placeholder identifier so the surrounding statements (and
            // the receiver's `let` binding) survive. Falls back to the
            // document's analysis when the fix-up doesn't apply (no `.`
            // right before the cursor, or the text parses).
            let fixed = self.dot_completion_fixup(uri, &pos);
            let doc = self.documents.get(uri)?;
            let checked = fixed.or_else(|| self.checked_facts(doc));
            let items = self.dot_completions(checked.as_ref(), &prefix, builtin, cursor);
            return Some(CompletionResponse::Array(items));
        }
        let doc = self.documents.get(uri);

        let mut items: Vec<CompletionItem> = Vec::new();

        // Keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..CompletionItem::default()
            });
        }

        for kw in KEYWORD_LITERALS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                ..CompletionItem::default()
            });
        }

        // The names in scope at the top level: the prelude, and the
        // names the module's declarations and imports bind (a module by
        // `import m`), as the resolver bound them.
        let (_, builtin_scopes) = crate::typechecker::names::builtins();
        let scope = doc
            .and_then(|doc| self.checked_module(doc))
            .map(|m| &m.scope);
        let defs = doc
            .and_then(|doc| doc.module.as_ref())
            .and_then(|m| self.projects.get(&m.project))
            .map(|p| p.session.defs());
        let mut names: Vec<(Symbol, CompletionItemKind)> = Vec::new();
        let prelude = &builtin_scopes.prelude;
        let layers = scope
            .into_iter()
            .flat_map(|s| [&s.values, &s.types])
            .chain([&prelude.values, &prelude.types]);
        for layer in layers {
            for (name, binding) in layer {
                if let Some(kind) = Self::binding_kind(defs, binding) {
                    names.push((*name, kind));
                }
            }
        }
        names.sort_by_key(|(name, _)| resolve(*name));
        names.dedup_by_key(|(name, _)| *name);
        // The document's own definitions and the items of its
        // `import m.{ ... }` are offered below, with their types.
        let mut own: HashSet<String> = doc
            .map(|doc| doc.definitions.keys().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        if let Some(program) = doc.and_then(|doc| doc.program.as_ref()) {
            for decl in &program.decls {
                if let Decl::Import(ImportTarget::Items(module, imported), _) = decl
                    && !module::is_builtin_module(&resolve(*module))
                {
                    own.extend(imported.iter().map(|(item, _)| item.to_string()));
                }
            }
        }
        for (name, kind) in names {
            let label = resolve(name);
            if own.contains(&label) {
                continue;
            }
            let detail = self.builtin_sigs.get(&label).cloned();
            let documentation = self.builtin_docs.get(&label).map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: d.clone(),
                })
            });
            items.push(CompletionItem {
                label,
                kind: Some(kind),
                detail,
                documentation,
                ..CompletionItem::default()
            });
        }

        // User-defined names from the current document
        if let Some(doc) = doc {
            for (name, def) in &doc.definitions {
                let kind = match &def.ty {
                    Some(Type::Fun(..)) => CompletionItemKind::FUNCTION,
                    _ => CompletionItemKind::VARIABLE,
                };
                let detail = def.ty.as_ref().map(|t| format!("{t}"));
                let documentation = def.doc.as_ref().map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(kind),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }

            // The items of `import m.{ a, B }`, as their module has them.
            if let Some(program) = &doc.program {
                for decl in &program.decls {
                    let Decl::Import(ImportTarget::Items(_, imported), _) = decl else {
                        continue;
                    };
                    for (item, _) in imported {
                        let Some(view) = self.item_module(doc, *item) else {
                            continue;
                        };
                        if let Some((_, kind)) = view.members.iter().find(|(m, _)| m == item) {
                            items.push(member_item(&view, *item, *kind));
                        }
                    }
                }
            }

            // Local variables in scope at the cursor position
            if let Some(program) = &doc.program {
                let cursor = position_to_offset(&doc.source, &pos);
                for local in locals_at_offset(program, cursor) {
                    let detail = local.ty.as_ref().map(|t| format!("{t}"));
                    items.push(CompletionItem {
                        label: local.name,
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail,
                        ..CompletionItem::default()
                    });
                }
            }
        }

        Some(CompletionResponse::Array(items))
    }

    /// Produce completions after a `.` — either module functions, record
    /// fields, or methods available on the receiver's inferred type.
    ///
    /// DX-G4 (round 81): when the LSP can confidently infer the type of
    /// the receiver expression (a let-bound local with an annotation, or
    /// an identifier whose type was recovered from the typed AST), the
    /// emitted method set is narrowed to only those methods registered
    /// for that type's canonical head name. When the type cannot be
    /// inferred we fall back to the union of all method names declared
    /// in the program — preserving discoverability rather than silently
    /// hiding completions.
    fn dot_completions(
        &self,
        checked: Option<&Checked>,
        prefix: &str,
        builtin: Option<&str>,
        cursor: usize,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // 1. Builtin module → return its functions and constants with type signatures.
        //    Module constants (e.g. `math.pi`, `float.max_value`) are distinct from
        //    functions and must be surfaced here so editor autocompletion after
        //    `math.` / `float.` offers them alongside `sin`, `cos`, `parse`, etc.
        if let Some(prefix) = builtin {
            for func in module::builtin_module_functions(prefix) {
                let qualified = format!("{prefix}.{func}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                let documentation = self.builtin_docs.get(&qualified).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: func.to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }
            for constant in module::builtin_module_constants(prefix) {
                let qualified = format!("{prefix}.{constant}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                let documentation = self.builtin_docs.get(&qualified).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: constant.to_string(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }
            // The variants of the module's enums (e.g. `io.IoNotFound`,
            // `http.GET`, `channel.Recv`, `time.Monday`,
            // `postgres.PgConnect`), reached as `module.Variant`. Emitted
            // as CONSTRUCTOR entries so editors distinguish them from
            // module functions / constants.
            for (_enum_name, variants) in module::builtin_enum_variants() {
                for &variant in *variants {
                    if module::builtin_variant_module(variant) == Some(prefix) {
                        items.push(CompletionItem {
                            label: variant.to_string(),
                            kind: Some(CompletionItemKind::CONSTRUCTOR),
                            ..CompletionItem::default()
                        });
                    }
                }
            }
            // The module's types (`time.Weekday`, `task.Handle`).
            let (_, scopes) = crate::typechecker::names::builtins();
            if let Some(exports) =
                crate::session::ModuleId::builtin(prefix).and_then(|id| scopes.modules.get(&id))
            {
                for ty in exports.types.keys() {
                    items.push(CompletionItem {
                        label: resolve(*ty),
                        kind: Some(CompletionItemKind::CLASS),
                        ..CompletionItem::default()
                    });
                }
            }
            // Deterministic ordering so clients/tests see a stable list, and
            // dedupe in case a name was declared as both function and constant.
            items.sort_by(|a, b| a.label.cmp(&b.label));
            items.dedup_by(|a, b| a.label == b.label);
            return items;
        }

        let Some(checked) = checked else {
            return items;
        };
        let program = &*checked.program;

        // Resolve the receiver's inferred type once. Used both for the
        // record-field path and the method-narrowing path below. Two
        // narrow sources are tried:
        //
        //   (a) a let-bound local in scope at the cursor whose type was
        //       recovered from the value expression (annotation included);
        //   (b) any typed-AST occurrence of `prefix` as an identifier
        //       whose `expr.ty` survived inference without unresolved
        //       variables.
        //
        // Anything else (complex expressions, generics with unresolved
        // tyvars, non-identifier prefixes) leaves `receiver_ty = None`
        // and the method path falls back to every method the module
        // knows — "narrow when safe, never silently lose completions".
        let locals = locals_at_offset(program, cursor);
        let receiver_ty: Option<Type> = locals
            .iter()
            .rev()
            .find(|l| l.name == prefix)
            .and_then(|l| l.ty.clone())
            .or_else(|| find_ident_type_by_name(program, prefix));

        // 2. Record fields, if the receiver is a record-shaped type.
        let mut emitted_field_labels: HashSet<String> = HashSet::new();
        if let Some(ref ty) = receiver_ty
            && let Some(fields) = record_fields_from_type(ty, &checked.record_fields)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
                emitted_field_labels.insert(name.clone());
            }
        }

        // 3. A record type's name as the prefix (`Point.|`): offer its
        //    fields even though the prefix is not a value binding.
        if receiver_ty.is_none()
            && let Some(fields) = checked.record_fields.get(&intern(prefix))
        {
            for (name, field_ty) in fields {
                let label = resolve(*name);
                if emitted_field_labels.insert(label.clone()) {
                    items.push(CompletionItem {
                        label,
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(format!("{field_ty}")),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        // 4. Method completions: the methods the checker knows for the
        //    receiver type's canonical name, or every method it knows
        //    when the type is unknown.
        let methods = methods_for_receiver(&checked.methods, receiver_ty.as_ref());
        for label in methods {
            // Don't duplicate a name that already came through as a
            // field — fields and methods occupy the same `name.` slot.
            if emitted_field_labels.contains(&label) {
                continue;
            }
            items.push(CompletionItem {
                label,
                kind: Some(CompletionItemKind::METHOD),
                ..CompletionItem::default()
            });
        }

        items.sort_by(|a, b| a.label.cmp(&b.label));
        items.dedup_by(|a, b| a.label == b.label);
        items
    }

    /// The variants of the enum `prefix` names in the open document `doc`
    /// (declared there or imported by name), unless a local binding of
    /// that name shadows it.
    fn enum_variant_completions(
        &self,
        doc: &Document,
        prefix: &str,
        cursor: usize,
    ) -> Option<Vec<CompletionItem>> {
        if doc.program.as_ref().is_some_and(|program| {
            locals_at_offset(program, cursor)
                .iter()
                .any(|local| local.name == prefix)
        }) {
            return None;
        }
        let scope = &self.checked_module(doc)?.scope;
        let Some(Binding::Def(ty)) = scope.types.get(&intern(prefix)) else {
            return None;
        };
        let module = doc.module.as_ref()?;
        let defs = self.projects.get(&module.project)?.session.defs();
        let variants = defs.variants(*ty);
        if variants.is_empty() {
            return None;
        }
        Some(
            variants
                .iter()
                .map(|v| CompletionItem {
                    label: resolve(defs.get(*v).name),
                    kind: Some(CompletionItemKind::CONSTRUCTOR),
                    ..CompletionItem::default()
                })
                .collect(),
        )
    }

    /// The builtin module `prefix` is bound to in the open document `doc`
    /// (by `import m` or `import m as prefix`), unless a local binding
    /// shadows it. A document with no analysis yet takes a builtin
    /// module's name for the module.
    fn bound_builtin_module(
        &self,
        doc: &Document,
        prefix: &str,
        cursor: usize,
    ) -> Option<&'static str> {
        if doc.program.as_ref().is_some_and(|program| {
            locals_at_offset(program, cursor)
                .iter()
                .any(|local| local.name == prefix)
        }) {
            return None;
        }
        match self.checked_module(doc) {
            Some(checked) => match checked.scope.values.get(&intern(prefix)) {
                Some(Binding::Module(id)) => id.builtin_name(),
                _ => None,
            },
            None => module::BUILTIN_MODULES
                .iter()
                .copied()
                .find(|m| *m == prefix),
        }
    }

    /// The completion kind of a name bound at the top level: `None` for a
    /// name a bare use of which is an error (an ambiguous variant, a name
    /// of a module that failed to load).
    fn binding_kind(
        defs: Option<&crate::defs::DefTable>,
        binding: &Binding,
    ) -> Option<CompletionItemKind> {
        let id = match binding {
            Binding::Module(_) => return Some(CompletionItemKind::MODULE),
            Binding::Def(id) => *id,
            Binding::Ambiguous(_) | Binding::Poisoned => return None,
        };
        let def = match defs {
            Some(defs) => defs.get(id).clone(),
            None => crate::typechecker::names::builtin_def(id)?,
        };
        Some(match def.kind {
            DefKind::Fn | DefKind::Host => CompletionItemKind::FUNCTION,
            DefKind::Let => CompletionItemKind::VARIABLE,
            DefKind::Variant { .. } => CompletionItemKind::CONSTRUCTOR,
            DefKind::Trait(_) => CompletionItemKind::INTERFACE,
            DefKind::Type(_) | DefKind::TypeAlias => CompletionItemKind::CLASS,
        })
    }

    /// What dot completion reads about the open document `doc`: the
    /// session's analysis of its module.
    fn checked_facts(&self, doc: &Document) -> Option<Checked> {
        let checked = self.checked_module(doc)?;
        Some(Checked {
            program: checked.ast.clone(),
            methods: checked.methods.clone(),
            record_fields: checked.record_fields.clone(),
        })
    }

    /// The members of the imported module `prefix` names (`geo.`, or
    /// `g.` for `import geo as g`), unless a local binding of that name
    /// shadows it.
    fn module_member_completions(
        &self,
        doc: &Document,
        prefix: &str,
        cursor: usize,
    ) -> Option<Vec<CompletionItem>> {
        let name = intern(prefix);
        if doc.program.as_ref().is_some_and(|program| {
            locals_at_offset(program, cursor)
                .iter()
                .any(|local| local.name == prefix)
        }) {
            return None;
        }
        let view = self.imported_module(doc, name)?;
        let mut items: Vec<CompletionItem> = view
            .members
            .iter()
            .map(|(member, kind)| member_item(&view, *member, *kind))
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items.dedup_by(|a, b| a.label == b.label);
        Some(items)
    }

    /// Analyse the document `uri` with the partial dot expression at
    /// `pos` completed by a placeholder identifier, so the surrounding
    /// statements survive parser recovery.
    ///
    /// Background (round 81 DX-G4): when the cursor sits at a partial
    /// `xs.|`, the parser's `expect_ident()` after the `.` errors out,
    /// and `parse_let_stmt`'s `?` propagates the failure all the way to
    /// `parse_fn_decl_recovering`, which salvages a *recovery stub* for
    /// the enclosing function — an `FnDecl` with an empty body. Every
    /// prior `let` in that function disappears from the AST, which means
    /// `locals_at_offset` returns nothing for the receiver `xs` and the
    /// type-narrowing path can't pin down its type.
    ///
    /// The fix-up gives the project's session, for a moment, a copy of
    /// the text where the partial dot is followed by a placeholder
    /// identifier (`silt_lsp_completion_placeholder`), so
    /// `let _ = xs.silt_lsp_completion_placeholder` parses as a normal
    /// field access and the receiver keeps its type; the other modules
    /// are the session's, unsaved texts included. The document's text is
    /// given back and the open documents are analysed again (only the
    /// document's module and its importers are checked again).
    ///
    /// Returns `None` if there's no `.` immediately before the cursor or
    /// the document parses (its own analysis is used then).
    fn dot_completion_fixup(&mut self, uri: &Uri, pos: &Position) -> Option<Checked> {
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, pos);
        // Sanity: the byte just before the cursor must be `.`. If not,
        // the dot-completion context was extracted from a different
        // configuration (e.g. `xs.first().` chained-call walk reached
        // back through a `)`) and the fix-up isn't needed.
        if cursor == 0 || !doc.source.text.is_char_boundary(cursor) {
            return None;
        }
        let bytes = doc.source.text.as_bytes();
        if bytes.get(cursor.checked_sub(1)?) != Some(&b'.') {
            return None;
        }
        let module = doc.module.clone()?;
        let project = self.projects.get_mut(&module.project)?;
        if project
            .session
            .graph()
            .module(module.id)
            .problems
            .is_empty()
        {
            return None;
        }
        // The placeholder is intentionally long-and-prefixed so it can't
        // accidentally collide with a real user method name.
        const PLACEHOLDER: &str = "silt_lsp_completion_placeholder";
        let mut fixed = String::with_capacity(doc.source.text.len() + PLACEHOLDER.len());
        fixed.push_str(&doc.source.text[..cursor]);
        fixed.push_str(PLACEHOLDER);
        fixed.push_str(&doc.source.text[cursor..]);
        let original = doc.source.text.clone();
        let path = doc.path.clone();
        let open: Vec<_> = self
            .documents
            .values()
            .filter_map(|d| d.module.as_ref().filter(|m| m.project == module.project))
            .map(|m| m.id)
            .collect();

        let id = project.set_text(&path, &Arc::from(fixed))?;
        let file = project.file(id);
        project.session.analyze(file);
        let checked = project.session.module_analysis(id).map(|checked| Checked {
            program: checked.ast.clone(),
            methods: checked.methods.clone(),
            record_fields: checked.record_fields.clone(),
        });
        project.set_text(&path, &original);
        for id in open {
            let file = project.file(id);
            project.session.analyze(file);
        }
        checked
    }
}

/// The completion item of the member `member` of the imported module
/// `view`.
fn member_item(view: &ModuleView, member: Symbol, kind: MemberKind) -> CompletionItem {
    let def = view.definitions.get(&member);
    CompletionItem {
        label: resolve(member),
        kind: Some(match kind {
            MemberKind::Function => CompletionItemKind::FUNCTION,
            MemberKind::Value => CompletionItemKind::VARIABLE,
            MemberKind::Type => CompletionItemKind::CLASS,
            MemberKind::Variant => CompletionItemKind::CONSTRUCTOR,
        }),
        detail: def.and_then(|d| d.ty.as_ref()).map(|t| format!("{t}")),
        documentation: def.and_then(|d| d.doc.clone()).map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d,
            })
        }),
        ..CompletionItem::default()
    }
}

/// What dot completion reads about a module: the session's analysis of
/// it.
struct Checked {
    program: Arc<Program>,
    methods: Vec<(Symbol, Symbol)>,
    record_fields: RecordFields,
}

// ── Method enumeration for dot-completion ──────────────────────────

/// The names of the methods a value of type `receiver_ty` has, from the
/// checker's `methods` (canonical type name, method name): those of the
/// type's canonical name, or every method when the type is unknown.
fn methods_for_receiver(methods: &[(Symbol, Symbol)], receiver_ty: Option<&Type>) -> Vec<String> {
    // `_` / `<anon>` / `Never` are the canonical names of unresolved
    // variables, anonymous records and bottom-typed expressions: they
    // name no dispatch head, so they do not narrow.
    let head = receiver_ty
        .map(canonical_name)
        .filter(|canon| !matches!(canon.as_str(), "_" | "<anon>" | "Never"));
    let mut names: Vec<String> = methods
        .iter()
        .filter(|(ty, _)| head.as_ref().is_none_or(|head| resolve(*ty) == *head))
        .map(|(_, method)| resolve(*method))
        .collect();
    names.sort();
    names.dedup();
    names
}

// ── Dot-completion helpers ─────────────────────────────────────────

/// Extract the identifier (or postfix-call / index-expression receiver)
/// before the `.` at the cursor position.
///
/// Returns `None` if the cursor is not in a dot-completion context.
///
/// The walk handles chained method calls and index expressions — `xs.first().`
/// and `arr[0].` must trigger completion on the receiver, not bail because
/// `)` / `]` terminate the identifier scan. We scan char-by-char from right
/// to left, skipping over matched `()` / `[]` spans, then greedily collect
/// identifier characters (plus `.` for qualified names like `mod.inner.`).
/// Anything else terminates the walk — keeps multi-statement lines from
/// greedily swallowing the previous expression.
fn extract_dot_prefix(source: &str, pos: &Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    if col == 0 {
        return None;
    }
    // Convert UTF-16 offset to byte offset
    let mut utf16_offset = 0usize;
    let mut byte_offset = line.len();
    for (byte_idx, ch) in line.char_indices() {
        if utf16_offset >= col {
            byte_offset = byte_idx;
            break;
        }
        utf16_offset += ch.len_utf16();
    }
    let before = &line[..byte_offset];
    // The last character should be '.' (cursor is right after it)
    if !before.ends_with('.') {
        return None;
    }
    let before_dot = &before[..before.len() - 1];
    // Walk backwards, skipping balanced `()` / `[]` groups so method
    // chains like `xs.first().` and index expressions like `arr[0].`
    // resolve to a sensible receiver instead of bailing on the closer.
    let chars: Vec<char> = before_dot.chars().collect();
    let mut end = chars.len(); // exclusive upper bound of the prefix
    loop {
        if end == 0 {
            break;
        }
        let last = chars[end - 1];
        match last {
            ')' | ']' => {
                let (open, close) = if last == ')' { ('(', ')') } else { ('[', ']') };
                // Scan back to the matching opener, handling nesting.
                let mut depth = 1i32;
                let mut i = end - 1; // index of the closer
                while i > 0 {
                    i -= 1;
                    let c = chars[i];
                    if c == close {
                        depth += 1;
                    } else if c == open {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                if depth != 0 {
                    // Unbalanced — stop here.
                    break;
                }
                // `i` now points at the matching opener; consume through it.
                end = i;
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => {
                // Greedily consume the identifier (possibly qualified).
                while end > 0 {
                    let ch = chars[end - 1];
                    if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                        end -= 1;
                    } else {
                        break;
                    }
                }
                break;
            }
            _ => break,
        }
    }
    let prefix: String = chars[end..].iter().collect();
    // Strip a trailing `.` — shouldn't happen in practice but keeps the
    // contract "returned prefix never ends with `.`" trivially true.
    let prefix = prefix.trim_end_matches('.').to_string();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

// ── Completion data ────────────────────────────────────────────────

// `KEYWORDS` is sourced from `crate::lexer::KEYWORDS` — the authoritative
// list maintained alongside the lexer's keyword match arms. Re-introducing
// a hand-rolled list here is guarded by `tests/meta/lexer_keyword_parity_tests.rs`.

#[cfg(test)]
mod tests {
    use super::*;

    /// The methods of a user impl and of the builtin derives, as the
    /// session's analysis of `source` has them.
    fn methods(source: &str) -> Vec<(Symbol, Symbol)> {
        let (mut session, file) = crate::session::testing::session_with(&[("main.silt", source)]);
        session.analyze(file);
        session
            .module_analysis(session.module_of(file))
            .expect("the entry is analysed")
            .methods
            .clone()
    }

    const IMPLS: &str = "trait UpperCaser { fn to_upper(self) -> String }\n\
                         trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                         trait IntListHead { fn head_int(self) -> Int }\n\
                         trait IntListHead for List(a) { fn head_int(self) -> Int { 0 } }\n\
                         fn main() { 0 }\n";

    /// A known receiver type narrows the methods to those of its
    /// canonical name, the builtin derived ones included.
    #[test]
    fn methods_narrow_to_the_receiver_type() {
        let methods = methods(IMPLS);
        let list = methods_for_receiver(&methods, Some(&Type::List(Box::new(Type::Int))));
        assert!(list.contains(&"head_int".to_string()), "{list:?}");
        assert!(list.contains(&"display".to_string()), "{list:?}");
        assert!(!list.contains(&"to_upper".to_string()), "{list:?}");
        let string = methods_for_receiver(&methods, Some(&Type::String));
        assert!(string.contains(&"to_upper".to_string()), "{string:?}");
        assert!(!string.contains(&"head_int".to_string()), "{string:?}");
    }

    /// An unknown receiver type offers every method the module knows.
    #[test]
    fn an_unknown_receiver_gets_every_method() {
        let methods = methods(IMPLS);
        let all = methods_for_receiver(&methods, None);
        for name in [
            "to_upper", "head_int", "display", "equal", "compare", "hash",
        ] {
            assert!(all.contains(&name.to_string()), "{name} missing: {all:?}");
        }
        assert_eq!(all, methods_for_receiver(&methods, Some(&Type::Var(0))));
    }
}
