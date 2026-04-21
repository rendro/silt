//! `textDocument/inlayHint` handler.
//!
//! Silt's tagline ("types without annotations") makes inferred-type
//! hints especially valuable — users see the types the compiler gave
//! their bindings without having to write them.
//!
//! Scope for v1:
//!   * `let x = expr` where the user did not write `: Type` → emit
//!     `: <type>` after the pattern.
//!   * Function parameters without annotations → emit `: <type>` after
//!     the parameter pattern.
//!
//! Skipped (intentional):
//!   * Destructuring patterns — compound pattern widths aren't carried
//!     in the AST. Hoverable types are still available via hover.
//!   * Return-type hints on fns — silt's fn header already displays
//!     the inferred signature in hover / completion.
//!   * Generic `Type::Var(_)` placeholders — suppressed to avoid
//!     showing `?17` to users.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position};

use crate::ast::*;
use crate::lexer::Span;
use crate::types::Type;

use super::Server;
use super::ast_walk::{has_unresolved_vars, visit_expr_children};
use super::conversions::span_to_range;

impl Server {
    pub(super) fn inlay_hints(
        &self,
        params: lsp_types::InlayHintParams,
    ) -> Option<Vec<InlayHint>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;
        let source = &doc.source;

        let start_offset = position_to_offset(source, &params.range.start);
        let end_offset = position_to_offset(source, &params.range.end);

        let mut hints: Vec<HintRecord> = Vec::new();
        for decl in &program.decls {
            walk_decl(decl, &mut hints);
        }

        let lsp_hints: Vec<InlayHint> = hints
            .into_iter()
            .filter(|h| {
                let off = h.ident_span.offset;
                off >= start_offset && off <= end_offset
            })
            .filter_map(|h| render_hint(h, source))
            .collect();

        if lsp_hints.is_empty() {
            None
        } else {
            Some(lsp_hints)
        }
    }
}

// ── Hint collection ────────────────────────────────────────────────

struct HintRecord {
    /// Span of the ident the hint follows. The hint text is rendered
    /// at `span.offset + ident_len`.
    ident_span: Span,
    ident_len: usize,
    ty: Type,
}

fn walk_decl(decl: &Decl, out: &mut Vec<HintRecord>) {
    match decl {
        Decl::Fn(f) => {
            collect_fn_hints(f, out);
        }
        Decl::Let {
            pattern,
            ty,
            value,
            ..
        } => {
            if ty.is_none()
                && let Some(binding_ty) = &value.ty
            {
                emit_ident_hint(pattern, binding_ty, out);
            }
            walk_expr(value, out);
        }
        Decl::TraitImpl(ti) => {
            for method in &ti.methods {
                collect_fn_hints(method, out);
            }
        }
        _ => {}
    }
}

fn collect_fn_hints(f: &FnDecl, out: &mut Vec<HintRecord>) {
    // Param hints: only for params whose author didn't write `: T`.
    for param in &f.params {
        if param.ty.is_some() || param.kind != ParamKind::Data {
            continue;
        }
        if let PatternKind::Ident(name) = &param.pattern.kind {
            let name_str = crate::intern::resolve(*name);
            // Pull the inferred param type by finding the ident in the
            // typed body.
            let inferred =
                super::definitions::find_param_type(&f.body, *name);
            if let Some(ty) = inferred
                && !has_unresolved_vars(&ty)
            {
                out.push(HintRecord {
                    ident_span: param.pattern.span,
                    ident_len: name_str.len(),
                    ty,
                });
            }
        }
    }
    walk_expr(&f.body, out);
}

fn walk_expr(expr: &Expr, out: &mut Vec<HintRecord>) {
    if let ExprKind::Block(stmts) = &expr.kind {
        for stmt in stmts {
            match stmt {
                Stmt::Let { pattern, ty, value } => {
                    if ty.is_none()
                        && let Some(binding_ty) = &value.ty
                    {
                        emit_ident_hint(pattern, binding_ty, out);
                    }
                    walk_expr(value, out);
                }
                Stmt::Expr(e) => walk_expr(e, out),
                Stmt::When { expr, else_body, .. } => {
                    walk_expr(expr, out);
                    walk_expr(else_body, out);
                }
                Stmt::WhenBool { condition, else_body } => {
                    walk_expr(condition, out);
                    walk_expr(else_body, out);
                }
            }
        }
        return;
    }
    visit_expr_children(expr, |child| walk_expr(child, out));
}

fn emit_ident_hint(pattern: &Pattern, ty: &Type, out: &mut Vec<HintRecord>) {
    if let PatternKind::Ident(name) = &pattern.kind {
        let name_str = crate::intern::resolve(*name);
        if name_str == "_" {
            return;
        }
        if has_unresolved_vars(ty) {
            return;
        }
        out.push(HintRecord {
            ident_span: pattern.span,
            ident_len: name_str.len(),
            ty: ty.clone(),
        });
    }
    // Destructuring patterns (tuple/record/constructor) intentionally
    // skipped — widths aren't in the AST and hover already covers them.
}

fn render_hint(h: HintRecord, source: &str) -> Option<InlayHint> {
    // Compute the LSP position at the end of the ident.
    let ident_end_span = Span::new(h.ident_span.line, h.ident_span.col + h.ident_len);
    // span_to_range uses the original offset to compute both ends;
    // we only need the start position, which we place just past the
    // identifier. Re-use the UTF-16 conversion for correctness.
    let ident_range = span_to_range(&h.ident_span, source);
    // The hint sits at the position = start + ident_len in UTF-16.
    // Recompute: advance from start by counting UTF-16 units of the ident.
    let start_line = ident_range.start.line;
    let start_char = ident_range.start.character;
    let ident_text = source
        .get(h.ident_span.offset..h.ident_span.offset + h.ident_len)?;
    let width_utf16: u32 = ident_text.encode_utf16().count() as u32;
    let position = Position {
        line: start_line,
        character: start_char + width_utf16,
    };

    // Dummy reference to keep ident_end_span typed-used.
    let _ = ident_end_span;

    Some(InlayHint {
        position,
        label: InlayHintLabel::String(format!(": {}", h.ty)),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(false),
        data: None,
    })
}

// ── Helpers ────────────────────────────────────────────────────────

fn position_to_offset(source: &str, pos: &Position) -> usize {
    super::conversions::position_to_offset(source, pos)
}
