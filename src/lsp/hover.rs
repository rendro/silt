//! `textDocument/hover` handler.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::Server;
use super::ast_walk::{
    find_ident_at_offset_with_source, find_type_at_offset, has_unresolved_vars,
};
use super::conversions::position_to_offset;
use super::fields::find_field_type_at_offset;
use super::local_bindings::find_local_binding_at_offset;

impl Server {
    // ── Hover ──────────────────────────────────────────────────────

    pub(super) fn hover(&self, params: lsp_types::HoverParams) -> Option<Hover> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);

        // Check if cursor is on a field name in a field access expression.
        // e.g., for `data.response`, hovering on `response` shows the field type.
        if let Some((field_name, field_ty)) =
            find_field_type_at_offset(program, &doc.source, cursor)
        {
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
            let doc_text = doc.definitions.get(&binding.name).and_then(|d| d.doc.clone());
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

        let ty = find_type_at_offset(program, cursor);

        // If the cursor is on a binding-site (e.g. `fn foo` declaration name)
        // and we have a definition for that symbol, prefer the definition's
        // type so hover on `fn foo` shows `foo`'s signature. Otherwise use
        // the expression-walk result, falling back to the definition type
        // when the expression type still has unresolved variables.
        let ident_at_cursor =
            find_ident_at_offset_with_source(program, cursor, Some(&doc.source));
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

        // Look up an attached doc comment — preserved from the AST
        // through `build_definitions`. Rendered below the signature
        // with the LSP `\n---\n` separator. Phase-1 cross-module doc
        // plumbing: when the identifier resolves to a definition in a
        // different open document, fall through to that document's
        // `DefInfo.doc`.
        let doc_text = def_entry.and_then(|def| def.doc.clone()).or_else(|| {
            // Cross-module fallback: iterate open documents and look
            // for a matching top-level definition with a doc string.
            ident_at_cursor.and_then(|name| {
                for (other_uri, other_doc) in &self.documents {
                    if other_uri == uri {
                        continue;
                    }
                    if let Some(d) = other_doc.definitions.get(&name)
                        && let Some(ref s) = d.doc
                    {
                        return Some(s.clone());
                    }
                }
                None
            })
        });

        // If neither a type nor a doc is available, no hover.
        if ty.is_none() && doc_text.is_none() {
            return None;
        }

        let mut value = String::new();
        if let Some(t) = ty {
            value.push_str(&format!("```silt\n{t}\n```"));
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
