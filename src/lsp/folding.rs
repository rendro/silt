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
use crate::source::{SourceFile, Span};

use super::Server;

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

fn collect_decl_folds(decl: &Decl, source: &SourceFile, out: &mut Vec<FoldingRange>) {
    match decl {
        Decl::Fn(f) => {
            // The fn body is itself an `ExprKind::Block`, and the walker's
            // Block arm pushes the block fold for us. Pushing one here in
            // addition would duplicate the fold (round-76 D1).
            walk_expr_folds(&f.body, source, out);
        }
        Decl::Type(td) => {
            // The type decl's span covers the full `type Foo { ... }`:
            // fold from its first line to its last.
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

fn walk_expr_folds(expr: &Expr, source: &SourceFile, out: &mut Vec<FoldingRange>) {
    match &expr.kind {
        ExprKind::Block(_) => {
            push_span_fold(&expr.span, source, out);
            super::ast_walk::visit_expr_children(expr, |c| walk_expr_folds(c, source, out));
        }
        ExprKind::Match { arms, .. } => {
            // Fold arm bodies that are themselves blocks.
            for arm in arms {
                if let ExprKind::Block(_) = arm.body.kind {
                    push_span_fold(&arm.body.span, source, out);
                }
                walk_expr_folds(&arm.body, source, out);
            }
        }
        _ => {
            super::ast_walk::visit_expr_children(expr, |c| walk_expr_folds(c, source, out));
        }
    }
}

/// A fold over the lines of `span`, when it runs over more than one.
fn push_span_fold(span: &Span, source: &SourceFile, out: &mut Vec<FoldingRange>) {
    // LSP lines are 0-based.
    let start_line = source.line_col(span.start).0 - 1;
    let end_line = source.line_col(span.end).0 - 1;
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
