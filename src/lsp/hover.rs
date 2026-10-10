//! `textDocument/hover` handler.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::Server;
use crate::intern::resolve;

use super::ast_walk::{find_ident_at_offset, find_type_at_offset, has_unresolved_vars};
use super::conversions::char_offset_at;
use super::fields::{RecordFields, find_field_type_at_offset};
use super::local_bindings::find_local_binding_at_offset;
use super::local_bindings::nearest_local_binding_for;
use super::modules::{qualified_access_at, qualifier_at};
use super::workspace::Named;

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
        let records = self
            .checked_tables(doc)
            .map_or_else(RecordFields::new, |tables| tables.record_fields());
        if let Some((field_name, field_ty)) = find_field_type_at_offset(program, &records, cursor) {
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
            // A local has no documentation of its own.
            let value = format!("```silt\n{ty}\n```");
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
        let ident_at_cursor = find_ident_at_offset(program, cursor);
        // What the name means decides whose type and documentation it
        // has: a definition's (of this file or of the module that
        // declares it), none for a local, the builtin docs for one of
        // silt's own names. A local that is spelled like a function is
        // not that function.
        let named = self.named_at(uri, cursor);
        let def_entry = match &named {
            Named::Definition(info) => Some(info),
            Named::Local | Named::Other => None,
        };

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
        // signature with the LSP `\n---\n` separator: the definition's,
        // from the module that declares it (this file, or the one `m.f`
        // or an item of `import m.{ f }` comes from); the builtin docs,
        // for one of silt's own names (`println`, `list.map`).
        let qualified = qualified_access_at(program, cursor);
        let doc_text = def_entry.and_then(|def| def.doc.clone()).or_else(|| {
            if !matches!(named, Named::Other) {
                return None;
            }
            if let Some(name) = ident_at_cursor
                && let Some(d) = self.builtin_docs.get(&resolve(name))
            {
                return Some(d.clone());
            }
            let (module, member) = qualified?;
            let (module, member) = (resolve(module), resolve(member));
            if let Some(d) = self.builtin_docs.get(&format!("{module}.{member}")) {
                return Some(d.clone());
            }
            // A builtin module's type or variant (`io.IoNotFound`,
            // `time.Weekday`) is documented under its bare name.
            let owner = crate::module::builtin_variant_module(&member)
                .or_else(|| crate::module::builtin_type_module(&member));
            if owner == Some(module.as_str()) {
                return self.builtin_docs.get(&member).cloned();
            }
            None
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
            // (So does a name the checker has no type for, a function
            // of a feature that is not built: its docs say what it is.)
            let untyped = has_unresolved_vars(&t) || matches!(t, crate::types::Type::Error);
            let render_top_signature = !(untyped && doc_text.is_some());
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
