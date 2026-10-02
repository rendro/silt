//! `textDocument/hover` handler.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::Server;
use crate::intern::resolve;

use super::ast_walk::{find_ident_at_offset_with_source, find_type_at_offset, has_unresolved_vars};
use super::conversions::char_offset_at;
use super::fields::{RecordFields, find_field_type_at_offset};
use super::local_bindings::find_local_binding_at_offset;
use super::local_bindings::nearest_local_binding_for;
use super::modules::{qualified_access_at, qualifier_at};

impl Server {
    // ── Hover ──────────────────────────────────────────────────────

    pub(super) fn hover(&self, params: lsp_types::HoverParams) -> Option<Hover> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        // A position after the end of its line, or past the last line, is
        // on no character, so there is nothing to describe.
        let cursor = char_offset_at(&doc.source, &pos)?;

        // Check if cursor is on a field name in a field access expression.
        // e.g., for `data.response`, hovering on `response` shows the field type.
        let no_records = RecordFields::new();
        let records = self
            .checked_module(doc)
            .map_or(&no_records, |checked| &checked.record_fields);
        if let Some((field_name, field_ty)) = find_field_type_at_offset(program, records, cursor) {
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```silt\n{field_name}: {field_ty}\n```"),
                }),
                range: None,
            });
        }

        // If the cursor is sitting on the BINDING (LHS) identifier of a local
        // let / param / match binding, prefer that binding's type over the
        // enclosing expression's type. Otherwise `hover` on `x` in `let x = 42`
        // returns the enclosing block's Unit type. See B9 in codebase audit.
        if let Some(binding) = find_local_binding_at_offset(&doc.locals, cursor)
            && let Some(ref ty) = binding.ty
        {
            // When this local binding also matches a top-level decl
            // with a doc comment (e.g. `let x = 42` at file scope), we
            // surface the doc alongside the type. Per-param doc is
            // phase-2; phase-1 only the top-level decl binding
            // inherits docs.
            let doc_text = doc
                .definitions
                .get(&binding.name)
                .and_then(|d| d.doc.clone());
            let mut value = format!("```silt\n{ty}\n```");
            if let Some(d) = doc_text {
                value.push_str("\n\n---\n\n");
                value.push_str(&d);
            }
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            });
        }

        // The name of an imported module before a dot (`geo` in
        // `geo.mk`): the module, not the member's type.
        if let Some(module) = qualifier_at(program, cursor)
            && nearest_local_binding_for(&doc.locals, module, cursor).is_none()
            && let Some(view) = self.imported_module(doc, module)
        {
            let file = view
                .uri
                .path()
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```silt\nmodule {}\n```\n\n{file}", resolve(module)),
                }),
                range: None,
            });
        }

        let ty = find_type_at_offset(program, cursor);

        // If the cursor is on a binding-site (e.g. `fn foo` declaration name)
        // and we have a definition for that symbol, prefer the definition's
        // type so hover on `fn foo` shows `foo`'s signature. Otherwise use
        // the expression-walk result, falling back to the definition type
        // when the expression type still has unresolved variables.
        let ident_at_cursor =
            find_ident_at_offset_with_source(program, cursor, Some(&doc.source.text));
        let def_entry = ident_at_cursor.and_then(|name| doc.definitions.get(&name));

        let ty = {
            let def_ty = def_entry
                .and_then(|def| def.ty.clone())
                .filter(|t| !has_unresolved_vars(t));
            match ty {
                Some(ref t) if !has_unresolved_vars(t) => ty,
                _ => def_ty.or(ty), // last resort: show raw type even with vars
            }
        };

        // The doc comment of what the cursor names, rendered below the
        // signature with the LSP `\n---\n` separator: a definition of
        // this file; a member of an imported module (`m.f`, or `f` from
        // `import m.{ f }`), from the session's view of that module; else
        // the builtin docs, for stdlib names (`println`, `list.map`).
        let qualified = qualified_access_at(program, cursor);
        let doc_text = def_entry
            .and_then(|def| def.doc.clone())
            .or_else(|| {
                let (module, member) = qualified?;
                let view = self.imported_module(doc, module)?;
                view.definitions.get(&member)?.doc.clone()
            })
            .or_else(|| {
                let name = ident_at_cursor?;
                let view = self.item_module(doc, name)?;
                view.definitions.get(&name)?.doc.clone()
            })
            .or_else(|| {
                if let Some(name) = ident_at_cursor
                    && let Some(d) = self.builtin_docs.get(&resolve(name))
                {
                    return Some(d.clone());
                }
                let (module, member) = qualified?;
                self.builtin_docs
                    .get(&format!("{}.{}", resolve(module), resolve(member)))
                    .cloned()
            });

        // If neither a type nor a doc is available, no hover.
        if ty.is_none() && doc_text.is_none() {
            return None;
        }

        // Round-76 D3: suppress the top-of-hover signature block when
        // it would carry unresolved TyVars AND the markdown signature
        // below the `---` separator is authoritative (i.e. `doc_text`
        // is present). The audit example: hovering `string.length` in
        // a file that forgot `import string` produced
        // `Fn(String) -> _` because the FieldAccess arm stashed a
        // fresh `Type::Var` on the AST, and the Call arm later unified
        // the parameter side but not the return slot. The markdown
        // signature already shows the correct `Fn(String) -> Int`, so
        // dropping the broken top block is strictly an improvement.
        let mut value = String::new();
        if let Some(t) = ty {
            let render_top_signature = !(has_unresolved_vars(&t) && doc_text.is_some());
            if render_top_signature {
                value.push_str(&format!("```silt\n{t}\n```"));
            }
        }
        if let Some(d) = doc_text {
            if !value.is_empty() {
                value.push_str("\n\n---\n\n");
            }
            value.push_str(&d);
        }

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: None,
        })
    }
}
