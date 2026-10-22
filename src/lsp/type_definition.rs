//! `textDocument/typeDefinition` handler.
//!
//! Given a cursor on any expression or binder, look up the inferred type
//! and jump to the declaration of the type it is, or, for a type with no
//! declaration a program writes (`List(P)`), of the first declared type
//! it is made of. A type of silt's own alone (`Int`, `List(Int)`) has none, so
//! we return `None` and LSP clients render "no type definition".

use lsp_types::request::{GotoTypeDefinitionParams, GotoTypeDefinitionResponse};

use super::Server;
use super::ast_walk::find_type_at_offset;
use super::conversions::position_to_offset;
use super::local_bindings::find_local_binding_at_offset;
use crate::types::Type;

impl Server {
    // ── Go to type definition ──────────────────────────────────────

    pub(super) fn type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Option<GotoTypeDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        // A binder (`let p`, a parameter `p`) has the type of its binding;
        // another name of a pattern the type of the value it matches (a
        // constructor's, `Circle` in `Circle(r) ->`, its enum); anything
        // else the type of the innermost expression there.
        let ty = match (
            find_local_binding_at_offset(&doc.locals, cursor),
            doc.pattern_name_at(cursor),
        ) {
            (Some(binding), _) => binding.ty.clone()?,
            (None, Some(name)) => name.matched.clone()?,
            (None, None) => find_type_at_offset(program, cursor)?,
        };
        // The declaration of the type's head, or, for a type with no
        // declaration of its own (`List(P)`, `Option(P)`), of the first
        // declared type it is made of.
        let mut locations = Vec::new();
        for head in type_heads(&ty) {
            let Some(target) = self.type_target(doc, head) else {
                continue;
            };
            for location in self.declarations_of(uri, &target) {
                if !locations.contains(&location) {
                    locations.push(location);
                }
            }
            if !locations.is_empty() {
                break;
            }
        }
        match locations.len() {
            0 => None,
            1 => Some(GotoTypeDefinitionResponse::Scalar(locations.remove(0))),
            _ => Some(GotoTypeDefinitionResponse::Array(locations)),
        }
    }
}

/// The nominal types `ty` is made of, outside in: its head first, then
/// the heads of its arguments (`P` of `List(P)`), level by level.
fn type_heads(ty: &Type) -> Vec<crate::defs::TypeId> {
    let mut heads = Vec::new();
    let mut level = vec![ty];
    while !level.is_empty() {
        let mut next = Vec::new();
        for ty in level {
            if let Some(head) = type_head(ty) {
                heads.push(head);
            }
            match ty {
                Type::Generic(_, args) | Type::Tuple(args) => next.extend(args),
                Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => next.push(&**inner),
                Type::Map(k, v) => next.extend([&**k, &**v]),
                Type::Fun(params, ret) => {
                    next.extend(params);
                    next.push(&**ret);
                }
                _ => {}
            }
        }
        level = next;
    }
    heads
}

/// The type a `type` declaration declares, of which `ty` is an
/// instance. Only a nominal type has one; a structural type (tuple,
/// list, function, ...) has none.
fn type_head(ty: &Type) -> Option<crate::defs::TypeId> {
    match ty {
        Type::Generic(name, _) => Some(name.id),
        _ => None,
    }
}
