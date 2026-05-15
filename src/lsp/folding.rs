//! `textDocument/foldingRange` — emit fold regions for blocky
//! constructs so editors can collapse them.
//!
//! Covered:
//!   * Function bodies (`fn foo() { ... }`) — fold the `{ ... }`.
//!   * Trait decls & trait impls — fold the body braces.
//!   * Type decls with a body — fold the enum/record body.
//!   * Match arms' RHS blocks and block expressions.
//!
//! Each fold uses `Region` kind. silt has nested `{- -}` block comments
//! (see `lexer::Lexer::skip_block_comment`), but the folding range
//! handler does not currently emit folds for them; import groups and
//! docstring spans are also not yet distinguished.

use lsp_types::{FoldingRange, FoldingRangeKind};

use crate::ast::*;
use crate::lexer::Span;

use super::Server;
use super::text_utils::{expr_extent, find_matching_close_brace};

impl Server {
    pub(super) fn folding_range(
        &self,
        params: lsp_types::FoldingRangeParams,
    ) -> Option<Vec<FoldingRange>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;
        let source = &doc.source;

        let mut folds: Vec<FoldingRange> = Vec::new();
        for decl in &program.decls {
            collect_decl_folds(decl, source, &mut folds);
        }
        if folds.is_empty() { None } else { Some(folds) }
    }
}

fn collect_decl_folds(decl: &Decl, source: &str, out: &mut Vec<FoldingRange>) {
    match decl {
        Decl::Fn(f) => {
            // The fn body is itself an `ExprKind::Block`, and the walker's
            // Block arm pushes the block fold for us. Calling
            // `push_block_fold` here in addition would duplicate the fold
            // (round-76 D1).
            walk_expr_folds(&f.body, source, out);
        }
        Decl::Type(td) => {
            // The type decl's span covers the full `type Foo { ... }`.
            // Fold from the decl's starting line to the last line of
            // its span by extent-scanning the source.
            push_span_fold(&td.span, source, out);
        }
        Decl::Trait(t) => {
            push_span_fold(&t.span, source, out);
            for method in &t.methods {
                // Same reason as `Decl::Fn`: the walker handles the body
                // block. Avoid double-pushing (round-76 D1).
                walk_expr_folds(&method.body, source, out);
            }
        }
        Decl::TraitImpl(ti) => {
            if ti.is_auto_derived {
                return;
            }
            push_span_fold(&ti.span, source, out);
            for method in &ti.methods {
                // Same reason as `Decl::Fn`: the walker handles the body
                // block. Avoid double-pushing (round-76 D1).
                walk_expr_folds(&method.body, source, out);
            }
        }
        _ => {}
    }
}

fn walk_expr_folds(expr: &Expr, source: &str, out: &mut Vec<FoldingRange>) {
    match &expr.kind {
        ExprKind::Block(_) => {
            push_block_fold(&expr.span, expr, source, out);
            super::ast_walk::visit_expr_children(expr, |c| walk_expr_folds(c, source, out));
        }
        ExprKind::Match { arms, .. } => {
            // Fold arm bodies that are themselves blocks.
            for arm in arms {
                if let ExprKind::Block(_) = arm.body.kind {
                    push_block_fold(&arm.body.span, &arm.body, source, out);
                }
                walk_expr_folds(&arm.body, source, out);
            }
        }
        _ => {
            super::ast_walk::visit_expr_children(expr, |c| walk_expr_folds(c, source, out));
        }
    }
}

fn push_span_fold(span: &Span, source: &str, out: &mut Vec<FoldingRange>) {
    let start_line = span.line.saturating_sub(1) as u32; // LSP is 0-based
    let end_line = compute_span_end_line(span, source);
    if end_line > start_line {
        out.push(FoldingRange {
            start_line,
            start_character: None,
            end_line,
            end_character: None,
            kind: Some(FoldingRangeKind::Region),
            collapsed_text: None,
        });
    }
}

fn push_block_fold(span: &Span, expr: &Expr, source: &str, out: &mut Vec<FoldingRange>) {
    let end_offset = expr_extent(expr, source);
    let start_line = span.line.saturating_sub(1) as u32;
    let end_line = offset_to_line(source, end_offset);
    if end_line > start_line {
        out.push(FoldingRange {
            start_line,
            start_character: None,
            end_line,
            end_character: None,
            kind: Some(FoldingRangeKind::Region),
            collapsed_text: None,
        });
    }
}

fn compute_span_end_line(span: &Span, source: &str) -> u32 {
    // The Span struct only records a start line/col; walk forward from
    // `span.offset` until we hit a matching `}` at depth 0. For `trait`,
    // `type`, `fn` the header ends at `{` and the body runs to the
    // matching `}`. We delegate the scan to the shared
    // `find_matching_close_brace` helper in `text_utils` so that we
    // correctly skip `--` line comments, `{- ... -}` block comments
    // (nestable), and `"..."` / `"""..."""` strings — anything else
    // would let a `}` inside a comment or string close the body
    // prematurely (round-87 LATENT fix).
    //
    // Fallback: if no balanced `{...}` is found from `span.offset` (e.g.
    // a `type` with unit variants), fall back to the start line.
    let start_line = span.line.saturating_sub(1) as u32;
    match find_matching_close_brace(source, span.offset) {
        Some((_offset, newlines)) => start_line + newlines,
        None => start_line,
    }
}

fn offset_to_line(source: &str, offset: usize) -> u32 {
    let capped = offset.min(source.len());
    source[..capped].bytes().filter(|&b| b == b'\n').count() as u32
}
