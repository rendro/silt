//! `textDocument/hover` handler.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::Server;
use super::ast_walk::{find_ident_at_offset_with_source, find_type_at_offset, has_unresolved_vars};
use super::conversions::position_to_offset;
use super::fields::find_field_type_at_offset;
use super::local_bindings::find_local_binding_at_offset;
use super::state::DefInfo;

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

        let ty = find_type_at_offset(program, cursor);

        // If the cursor is on a binding-site (e.g. `fn foo` declaration name)
        // and we have a definition for that symbol, prefer the definition's
        // type so hover on `fn foo` shows `foo`'s signature. Otherwise use
        // the expression-walk result, falling back to the definition type
        // when the expression type still has unresolved variables.
        let ident_at_cursor = find_ident_at_offset_with_source(program, cursor, Some(&doc.source));
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
        //
        // Phase-2 builtin-doc plumbing: if neither the local
        // `DefInfo.doc` nor any other open document carries a doc for
        // the identifier, fall through to `builtin_docs` so stdlib
        // names (`list.map`, `math.cos`, `Result`, …) surface the
        // markdown registered at their per-module typechecker
        // registration site.
        let doc_text = def_entry
            .and_then(|def| def.doc.clone())
            .or_else(|| {
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
            })
            .or_else(|| {
                // Built-in doc fallback. The cursor word might be:
                //
                //   - An unqualified identifier (`println`, `Some`) — the
                //     AST walker returns its `Symbol` directly.
                //   - A trailing identifier in a qualified module access
                //     (`list.map`, `math.cos`). The AST walker stores the
                //     module side as an `Ident` expr but the `.field` is a
                //     bare string on `FieldAccess`, so `find_ident_at_offset`
                //     returns `None` when the cursor is on the field name.
                //     We recover by scanning the source for the qualified
                //     identifier under the cursor and looking it up in
                //     `builtin_docs` directly.
                if let Some(name) = ident_at_cursor {
                    let bare = crate::intern::resolve(name);
                    if let Some(d) = self.builtin_docs.get(&bare) {
                        return Some(d.clone());
                    }
                    if let Some(qualified) = qualified_name_at(&doc.source, cursor, &bare)
                        && let Some(d) = self.builtin_docs.get(&qualified)
                    {
                        return Some(d.clone());
                    }
                }
                // Source-only fallback: extract the qualified identifier
                // sitting at the cursor and look it up. Handles the
                // FieldAccess-on-builtin-module case (`math.cos`) where
                // the AST walker bails because `cos` isn't an Ident expr.
                qualified_token_at(&doc.source, cursor)
                    .and_then(|tok| self.builtin_docs.get(&tok).cloned())
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
        // Phase B of the effect-rows proposal: render the declared
        // effect set on every fn (and the inferred set when it differs)
        // between the type signature and the doc-comment separator.
        // The block is plain markdown — no fenced code block — so it
        // sits visually between the signature code-fence and the
        // `---` doc separator.
        let effect_block = if let Some(def) = def_entry {
            render_effects(def)
        } else {
            // Phase C: the cursor is on a stdlib call (`io.read_file`,
            // `list.map`, …). The user-document `DefInfo` map has no
            // entry for those, so fall through to the cached
            // `builtin_effects` map populated from
            // `typechecker::builtin_effects()`. Lookup mirrors the
            // doc-text fallback chain above (bare → qualified-via-name
            // → qualified-via-source-scan).
            let mut effects = None;
            if let Some(name) = ident_at_cursor {
                let bare = crate::intern::resolve(name);
                if let Some(e) = self.builtin_effects.get(&bare) {
                    effects = Some(*e);
                } else if let Some(qualified) = qualified_name_at(&doc.source, cursor, &bare)
                    && let Some(e) = self.builtin_effects.get(&qualified)
                {
                    effects = Some(*e);
                }
            }
            if effects.is_none()
                && let Some(tok) = qualified_token_at(&doc.source, cursor)
                && let Some(e) = self.builtin_effects.get(&tok)
            {
                effects = Some(*e);
            }
            match effects {
                Some(e) => format!("effects: {e}"),
                None => String::new(),
            }
        };
        if !effect_block.is_empty() {
            if !value.is_empty() {
                value.push_str("\n\n");
            }
            value.push_str(&effect_block);
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

/// Walk back through `source` from `cursor` to reconstruct a
/// `<mod>.<name>` qualified identifier when the cursor is sitting
/// inside `<name>`. Returns `Some(qualified)` only when the byte
/// immediately preceding the identifier (or its enclosing run of
/// identifier chars) is a `.`, in which case we keep walking back
/// to capture the module segment. Used by hover to look up
/// `list.map` in `builtin_docs` when the AST walker only surfaced
/// `map`.
///
/// `bare` is the identifier the AST walker already extracted; we
/// use it to scan the source line for an exact match anchored on
/// `cursor`.
pub(super) fn qualified_name_at(source: &str, cursor: usize, bare: &str) -> Option<String> {
    let bytes = source.as_bytes();
    if cursor > bytes.len() {
        return None;
    }
    // Find the start of the identifier the cursor is inside / at.
    let mut start = cursor;
    while start > 0 {
        let prev = bytes[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    // Verify `bare` matches at `start`.
    let bare_bytes = bare.as_bytes();
    if start + bare_bytes.len() > bytes.len()
        || &bytes[start..start + bare_bytes.len()] != bare_bytes
    {
        // The cursor's bare ident might be shorter than the full
        // identifier (cursor at the very start). Try matching the
        // full identifier at `start` against `bare`.
        return None;
    }
    // Look for a leading `.` before `start`.
    if start == 0 || bytes[start - 1] != b'.' {
        return None;
    }
    let dot_pos = start - 1;
    let mut mod_start = dot_pos;
    while mod_start > 0 {
        let prev = bytes[mod_start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            mod_start -= 1;
        } else {
            break;
        }
    }
    if mod_start == dot_pos {
        // Just a `.` with no preceding ident — not a qualified name.
        return None;
    }
    let module = &source[mod_start..dot_pos];
    Some(format!("{module}.{bare}"))
}

/// Extract the qualified identifier (e.g. `math.cos`, `list.map`) at
/// the cursor position by walking the source alone — the AST walker
/// is unaware of `FieldAccess` field names, so this fallback handles
/// hover on the `<name>` half of `<mod>.<name>` for built-in modules
/// (where the receiver isn't a record). Returns `None` when the
/// cursor is whitespace / not on an identifier-shaped run, or when
/// the identifier is not preceded by `<mod>.`.
pub(super) fn qualified_token_at(source: &str, cursor: usize) -> Option<String> {
    let bytes = source.as_bytes();
    if cursor > bytes.len() {
        return None;
    }
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    // Find the run of identifier chars containing or adjacent to the cursor.
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let bare = std::str::from_utf8(&bytes[start..end]).ok()?;

    // Build a qualified name iff the byte before `start` is `.` AND
    // the chars before that form an identifier.
    if start == 0 || bytes[start - 1] != b'.' {
        // Bare token (no module prefix). Caller can still look this
        // up in builtin_docs as an unqualified global.
        return Some(bare.to_string());
    }
    let dot_pos = start - 1;
    let mut mod_start = dot_pos;
    while mod_start > 0 && is_ident(bytes[mod_start - 1]) {
        mod_start -= 1;
    }
    if mod_start == dot_pos {
        return Some(bare.to_string());
    }
    let module = std::str::from_utf8(&bytes[mod_start..dot_pos]).ok()?;
    Some(format!("{module}.{bare}"))
}

/// Render the effect-annotation block for hover.
///
/// Phase B of the effect-rows proposal — Part 7 of
/// `docs/proposals/effect-rows.md` specifies the placement (between
/// the signature code fence and the `\n---\n` doc separator) and the
/// markdown body shape (`effects: !{io, fs}` for the simple case).
///
/// Three cases:
///   - non-fn def (`declared_effects == None`) → empty string, no
///     block rendered.
///   - declared and inferred match (or inferred is unknown) → a single
///     line `effects: <set>`. `EffectSet::TOP` renders as `!*` so
///     hover loudly signals "no constraint declared" — the gradual-
///     rollout state is supposed to be visible.
///   - declared and inferred differ → render BOTH lines, labelled, so
///     the user can see whether the annotation is too wide (declared
///     `!{io}`, inferred `!{}`) or too narrow (which would have been
///     rejected by enforcement and only happens during the typechecker
///     error-recovery edge cases).
pub(super) fn render_effects(def: &DefInfo) -> String {
    let Some(declared) = def.declared_effects else {
        return String::new();
    };
    match def.inferred_effects {
        Some(inferred) if inferred != declared => {
            // Dual render. Useful even now (helps users tighten over-
            // wide annotations) and load-bearing in Phase D when the
            // gradual-rollout default flips.
            format!("effects: {declared}    (declared)\ninferred: {inferred}    (body)")
        }
        _ => format!("effects: {declared}"),
    }
}
