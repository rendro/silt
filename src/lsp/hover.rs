//! `textDocument/hover` handler.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::Server;
use crate::intern::resolve;

use super::ast_walk::{
    find_ident_at_offset, find_type_at_offset, has_unresolved_vars, rejected_type_value_at,
};
use super::conversions::char_offset_at;
use super::fields::{RecordFields, find_field_type_at_offset};
use super::local_bindings::find_local_binding_at_offset;
use super::modules::{module_qualifier_at, qualified_access_at};
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
        // `geo.mk`, `json` in `json.Encode`): the module, not the
        // member's type.
        if let Some(module) = module_qualifier_at(doc, cursor) {
            if let Some(view) = self.imported_module(doc, module) {
                return Some(markdown(Self::module_hover(&view)));
            }
            if let Some(value) = self.imported_builtin_module_hover(doc, module) {
                return Some(markdown(value));
            }
        }

        // The module an import names (`list` in `import list`).
        if let Some(value) = self.imported_module_hover(doc, cursor) {
            return Some(markdown(value));
        }

        // A type named (`P` in `p: P`): one the program declares (or an
        // alias) with its declaration as it is written and its doc
        // comment; one of silt's own with its docs, if it has any (not
        // the type of the expression around the annotation).
        let named_type = self.type_named_at(uri, cursor);
        if let Some(ty) = named_type
            && let Some(text) = self.type_declaration_text(doc, ty)
        {
            let mut value = format!("```silt\n{text}\n```");
            if let Named::Definition(info) = self.named_at(uri, cursor)
                && let Some(doc) = info.doc
            {
                value.push_str(&format!("\n\n---\n\n{doc}"));
            }
            return Some(markdown(value));
        }
        if let Some(ty) = named_type {
            // (A type of the prelude named as a value the checker
            // rejects, `Option` in `(Option, 1)`, has none.)
            let prelude = crate::typechecker::names::builtin_def(ty.0)
                .is_some_and(|def| def.module == crate::session::ModuleId::PRELUDE);
            if prelude && rejected_type_value_at(program, cursor) {
                return None;
            }
            return self.builtin_type_hover_text(doc, ty).map(markdown);
        }

        // A method named through its trait (`size` in `Sized.size(b)`):
        // the method as its trait declares it, and its doc comment.
        if let Some((tr, method)) = super::ast_walk::find_trait_method_at(program, cursor)
            && let Some(m) =
                (self.trait_call_methods(doc, tr).into_iter()).find(|m| m.name == method)
        {
            let mut value = format!("```silt\n{}\n```", m.label);
            if let Some(text) = m.doc.filter(|text| !text.is_empty()) {
                value.push_str(&format!("\n\n---\n\n{text}"));
            }
            return Some(markdown(value));
        }

        // A trait named (`Display` in `where a: Display`, in an impl's
        // header, in its own declaration): its signature and its doc.
        if let Some(tr) = self.trait_named_at(uri, cursor)
            && let Some(value) = self.trait_hover_text(doc, tr)
        {
            return Some(markdown(value));
        }

        // A name in a pattern is no expression: a constructor (`Syntax` in
        // `Err(json.Syntax(m, at)) ->`) has the type an expression that
        // names it has, a field's name its field's type, `_` the type of
        // what it matches; not the one of the expression around the
        // pattern. Nor is an item of an import (`map` in `import list.{
        // map }`): it has its definition's type, general.
        let ty = match doc.pattern_name_at(cursor) {
            Some(name) => name.ty.clone(),
            None => find_type_at_offset(program, cursor)
                .or_else(|| self.import_item_type_at(uri, cursor)),
        };

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
            // One of silt's own definitions is documented as it is
            // reached: `println`, `list.map`, `io.NotFound`. A variant
            // that an import binds bare is its module's all the same.
            if let Some(d) = self
                .builtin_at(uri, cursor)
                .and_then(|key| self.builtin_docs.get(&key))
            {
                return Some(d.clone());
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
            // A builtin module's type (`time.Weekday`) is documented
            // under its bare name.
            if crate::module::builtin_type_module(&member) == Some(module.as_str()) {
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

/// A hover of the markdown `value`.
fn markdown(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}
