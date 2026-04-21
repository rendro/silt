//! `textDocument/semanticTokens/full` handler.
//!
//! Emits per-identifier semantic token spans (delta-encoded per LSP spec)
//! so clients can color idents by their semantic role (function name vs
//! type name vs parameter vs variable etc.) — a layer of precision
//! beyond what the textmate grammar can deliver. When classification is
//! uncertain for a given ident we omit it; the grammar still handles
//! keywords and literals, so a missing token is harmless.

use lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensParams,
    SemanticTokensResult,
};

use crate::ast::*;
use crate::intern::{Symbol, resolve};

use super::Server;
use super::ast_walk::visit_expr_children;
use super::conversions::span_to_position;
use super::state::Document;
use super::text_utils::find_ident_in_range;

// ── Token legend ───────────────────────────────────────────────────

/// Ordered list of token types this server emits. Clients use the
/// index into this array as the `tokenType` field on each token.
pub(super) const TOKEN_LEGEND: &[SemanticTokenType] = &[
    SemanticTokenType::FUNCTION,    // 0
    SemanticTokenType::TYPE,        // 1
    SemanticTokenType::ENUM,        // 2
    SemanticTokenType::ENUM_MEMBER, // 3
    SemanticTokenType::INTERFACE,   // 4
    SemanticTokenType::PARAMETER,   // 5
    SemanticTokenType::VARIABLE,    // 6
    SemanticTokenType::PROPERTY,    // 7
];

const TT_FUNCTION: u32 = 0;
const TT_TYPE: u32 = 1;
const TT_ENUM: u32 = 2;
const TT_ENUM_MEMBER: u32 = 3;
const TT_INTERFACE: u32 = 4;
const TT_PARAMETER: u32 = 5;
const TT_VARIABLE: u32 = 6;
const TT_PROPERTY: u32 = 7;

pub(super) fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_LEGEND.to_vec(),
        token_modifiers: vec![],
    }
}

// ── Raw token (pre-delta-encoding) ─────────────────────────────────

/// A classified token in absolute line/column (UTF-16) coordinates.
/// We collect these from the AST, then sort and delta-encode at the end.
#[derive(Debug, Clone, Copy)]
struct RawToken {
    line: u32,
    col_utf16: u32,
    length_utf16: u32,
    token_type: u32,
}

impl Server {
    pub(super) fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Option<SemanticTokensResult> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let mut tokens: Vec<RawToken> = Vec::new();
        collect_tokens(self, doc, program, &mut tokens);

        // LSP requires tokens to be reported in order (sorted by line, then
        // by starting character). Without this, delta encoding produces
        // nonsense deltas.
        tokens.sort_by_key(|t| (t.line, t.col_utf16));
        // Deduplicate: if two walkers emit a token at the same (line, col)
        // (can happen for the same ident reached via two paths), keep one.
        tokens.dedup_by_key(|t| (t.line, t.col_utf16));

        let data = encode_deltas(&tokens);
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        }))
    }
}

// ── Encoding ───────────────────────────────────────────────────────

/// Convert absolute-positioned tokens into the LSP delta-encoded wire
/// format: each token's `deltaLine` is relative to the previous token's
/// line; if they share a line, `deltaStart` is relative to the previous
/// token's start column, otherwise it's the absolute column.
fn encode_deltas(tokens: &[RawToken]) -> Vec<SemanticToken> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut prev_line: u32 = 0;
    let mut prev_col: u32 = 0;
    for (i, t) in tokens.iter().enumerate() {
        let (delta_line, delta_start) = if i == 0 {
            (t.line, t.col_utf16)
        } else if t.line == prev_line {
            (0, t.col_utf16.saturating_sub(prev_col))
        } else {
            (t.line - prev_line, t.col_utf16)
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.length_utf16,
            token_type: t.token_type,
            token_modifiers_bitset: 0,
        });
        prev_line = t.line;
        prev_col = t.col_utf16;
    }
    out
}

// ── Collection ─────────────────────────────────────────────────────

fn collect_tokens(server: &Server, doc: &Document, program: &Program, out: &mut Vec<RawToken>) {
    let source = &doc.source;

    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                emit_fn_decl_tokens(f, source, out);
                if !f.is_signature_only && !f.is_recovery_stub {
                    emit_expr_tokens(&f.body, source, doc, server, out);
                }
            }
            Decl::Type(t) => {
                emit_type_decl_tokens(t, source, out);
            }
            Decl::Trait(t) => {
                emit_trait_decl_tokens(t, source, out);
                for method in &t.methods {
                    if !method.is_signature_only && !method.is_recovery_stub {
                        emit_expr_tokens(&method.body, source, doc, server, out);
                    }
                }
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    emit_fn_decl_tokens(method, source, out);
                    if !method.is_signature_only && !method.is_recovery_stub {
                        emit_expr_tokens(&method.body, source, doc, server, out);
                    }
                }
            }
            Decl::Let { pattern, value, .. } => {
                if let PatternKind::Ident(name) = &pattern.kind {
                    emit_binding_token(source, &pattern.span, *name, TT_VARIABLE, out);
                }
                emit_expr_tokens(value, source, doc, server, out);
            }
            _ => {}
        }
    }
}

fn emit_fn_decl_tokens(f: &FnDecl, source: &str, out: &mut Vec<RawToken>) {
    // Emit FUNCTION on the fn name ident. `f.span` sits at the `fn`
    // keyword — we need to scan past it to reach the identifier. The
    // params list opens with `(`, so any `name` match in the range
    // [f.span.offset, first-`(`] is the fn name.
    let name_str = resolve(f.name);
    if let Some(paren) = source[f.span.offset.min(source.len())..]
        .find('(')
        .map(|p| f.span.offset + p)
        && let Some(off) = find_ident_in_range(source, f.span.offset, paren, &name_str)
    {
        push_token_at_offset(source, off, &name_str, TT_FUNCTION, out);
    }

    // Fn parameters: param patterns carry their own span already at the
    // ident, so we can use that directly.
    for param in &f.params {
        if let PatternKind::Ident(name) = &param.pattern.kind {
            emit_binding_token(source, &param.pattern.span, *name, TT_PARAMETER, out);
        }
    }

    // Body traversal for ident classification is driven by the caller
    // (see `collect_tokens`) so both top-level decls and impl methods
    // reach it via the same path with full `doc`/`server` context.
}

fn emit_type_decl_tokens(t: &TypeDecl, source: &str, out: &mut Vec<RawToken>) {
    let name_str = resolve(t.name);
    // Scan from `type` keyword forward for the first occurrence of the
    // name; use the opening brace or paren (whichever comes first) as
    // an upper bound.
    let start = t.span.offset.min(source.len());
    let bound_brace = source[start..].find('{').map(|p| start + p);
    let bound_paren = source[start..].find('(').map(|p| start + p);
    let end = match (bound_brace, bound_paren) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => source.len(),
    };
    let token_type = match &t.body {
        TypeBody::Enum(_) => TT_ENUM,
        TypeBody::Record(_) => TT_TYPE,
    };
    if let Some(off) = find_ident_in_range(source, start, end, &name_str) {
        push_token_at_offset(source, off, &name_str, token_type, out);
    }

    match &t.body {
        TypeBody::Enum(variants) => {
            // Variants: search for each variant name within the body
            // region following the `{`.
            if let Some(brace) = bound_brace {
                let body_end = source[brace..]
                    .rfind('}')
                    .map(|p| brace + p + 1)
                    .unwrap_or(source.len());
                for v in variants {
                    let vname = resolve(v.name);
                    if let Some(off) = find_ident_in_range(source, brace, body_end, &vname) {
                        push_token_at_offset(source, off, &vname, TT_ENUM_MEMBER, out);
                    }
                }
            }
        }
        TypeBody::Record(fields) => {
            // Record field names: emit PROPERTY for each declared field.
            if let Some(brace) = bound_brace {
                let body_end = source[brace..]
                    .rfind('}')
                    .map(|p| brace + p + 1)
                    .unwrap_or(source.len());
                for field in fields {
                    let fname = resolve(field.name);
                    if let Some(off) = find_ident_in_range(source, brace, body_end, &fname) {
                        push_token_at_offset(source, off, &fname, TT_PROPERTY, out);
                    }
                }
            }
        }
    }
}

fn emit_trait_decl_tokens(t: &TraitDecl, source: &str, out: &mut Vec<RawToken>) {
    let name_str = resolve(t.name);
    let start = t.span.offset.min(source.len());
    let end = source[start..]
        .find('{')
        .map(|p| start + p)
        .unwrap_or(source.len());
    if let Some(off) = find_ident_in_range(source, start, end, &name_str) {
        push_token_at_offset(source, off, &name_str, TT_INTERFACE, out);
    }

    // Trait method signatures behave like fn decls.
    for method in &t.methods {
        emit_fn_decl_tokens(method, source, out);
    }
}

/// Emit a single token at a precomputed pattern/decl span for a bound
/// identifier — no scanning needed, the span already points at the ident.
fn emit_binding_token(
    source: &str,
    span: &crate::lexer::Span,
    name: Symbol,
    token_type: u32,
    out: &mut Vec<RawToken>,
) {
    if resolve(name) == "_" {
        return;
    }
    let name_str = resolve(name);
    push_token_at_offset(source, span.offset, &name_str, token_type, out);
}

/// Convert a (byte-offset, ident-string) pair into a RawToken using
/// UTF-16-correct line/column math.
fn push_token_at_offset(
    source: &str,
    offset: usize,
    name: &str,
    token_type: u32,
    out: &mut Vec<RawToken>,
) {
    if offset > source.len() {
        return;
    }
    // Reuse span_to_position for the UTF-16 column math. We synthesize a
    // minimal Span; span_to_position derives line from source[..offset].
    let synthetic = crate::lexer::Span {
        line: 1, // value unused: span_to_position walks the source itself
        col: 1,
        offset,
    };
    let pos = span_to_position(&synthetic, source);
    // span_to_position uses `span.line - 1` for its line output, which
    // would give (1 - 1) = 0 for our synthetic span — but that's wrong
    // for any non-first-line offset. Compute the real line by counting
    // newlines up to `offset`.
    let line = source[..offset.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32;
    let length_utf16 = name.encode_utf16().count() as u32;
    out.push(RawToken {
        line,
        col_utf16: pos.character,
        length_utf16,
        token_type,
    });
}

// ── Expression-body walker ─────────────────────────────────────────

fn emit_expr_tokens(
    expr: &Expr,
    source: &str,
    doc: &Document,
    server: &Server,
    out: &mut Vec<RawToken>,
) {
    match &expr.kind {
        ExprKind::Ident(name) => {
            if let Some(tt) = classify_ident(*name, expr.span.offset, doc, server) {
                let name_str = resolve(*name);
                push_token_at_offset(source, expr.span.offset, &name_str, tt, out);
            }
        }
        ExprKind::RecordCreate { name, fields } => {
            // Record constructor: the name is a TYPE.
            let name_str = resolve(*name);
            push_token_at_offset(source, expr.span.offset, &name_str, TT_TYPE, out);
            for (_, v) in fields {
                emit_expr_tokens(v, source, doc, server, out);
            }
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                emit_stmt_tokens(stmt, source, doc, server, out);
            }
        }
        _ => {
            visit_expr_children(expr, |child| {
                emit_expr_tokens(child, source, doc, server, out);
            });
        }
    }
}

fn emit_stmt_tokens(
    stmt: &Stmt,
    source: &str,
    doc: &Document,
    server: &Server,
    out: &mut Vec<RawToken>,
) {
    match stmt {
        Stmt::Let { pattern, value, .. } => {
            emit_pattern_binding_tokens(pattern, source, out);
            emit_expr_tokens(value, source, doc, server, out);
        }
        Stmt::When {
            pattern,
            expr,
            else_body,
        } => {
            emit_pattern_binding_tokens(pattern, source, out);
            emit_expr_tokens(expr, source, doc, server, out);
            emit_expr_tokens(else_body, source, doc, server, out);
        }
        Stmt::WhenBool {
            condition,
            else_body,
        } => {
            emit_expr_tokens(condition, source, doc, server, out);
            emit_expr_tokens(else_body, source, doc, server, out);
        }
        Stmt::Expr(e) => {
            emit_expr_tokens(e, source, doc, server, out);
        }
    }
}

/// Emit VARIABLE tokens for every ident introduced by a `let` pattern.
fn emit_pattern_binding_tokens(pattern: &Pattern, source: &str, out: &mut Vec<RawToken>) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            emit_binding_token(source, &pattern.span, *name, TT_VARIABLE, out);
        }
        PatternKind::Tuple(pats) | PatternKind::List(pats, _) | PatternKind::Or(pats) => {
            for p in pats {
                emit_pattern_binding_tokens(p, source, out);
            }
        }
        PatternKind::Constructor(_, fields) => {
            for p in fields {
                emit_pattern_binding_tokens(p, source, out);
            }
        }
        PatternKind::Record { fields, .. } => {
            for (_, sub) in fields {
                if let Some(p) = sub {
                    emit_pattern_binding_tokens(p, source, out);
                }
            }
        }
        _ => {}
    }
}

/// Classify an identifier reference at `offset` using (in priority order)
/// local bindings, current-file definitions, and the workspace index.
/// Returns None when we can't be confident — callers should omit the
/// token rather than mis-classify (the textmate grammar still covers
/// keywords / literals).
fn classify_ident(name: Symbol, offset: usize, doc: &Document, server: &Server) -> Option<u32> {
    // Local binding (let / param): VARIABLE. We distinguish PARAMETER at
    // binding sites only — references to a param are just VARIABLEs in
    // the standard LSP encoding since the semantics are the same for
    // the reader.
    if super::local_bindings::nearest_local_binding_for(&doc.locals, name, offset).is_some() {
        return Some(TT_VARIABLE);
    }

    // Current file definitions.
    if let Some(def) = doc.definitions.get(&name) {
        // We don't keep kind info on DefInfo, so consult the program's
        // decl list directly.
        if let Some(program) = doc.program.as_ref() {
            for decl in &program.decls {
                match decl {
                    Decl::Fn(f) if f.name == name => return Some(TT_FUNCTION),
                    Decl::Type(t) if t.name == name => {
                        return Some(match &t.body {
                            TypeBody::Enum(_) => TT_ENUM,
                            TypeBody::Record(_) => TT_TYPE,
                        });
                    }
                    Decl::Type(t) => {
                        if let TypeBody::Enum(variants) = &t.body
                            && variants.iter().any(|v| v.name == name)
                        {
                            return Some(TT_ENUM_MEMBER);
                        }
                    }
                    Decl::Trait(tr) if tr.name == name => return Some(TT_INTERFACE),
                    Decl::Let { pattern, .. } => {
                        if let PatternKind::Ident(n) = &pattern.kind
                            && *n == name
                        {
                            return Some(TT_VARIABLE);
                        }
                    }
                    _ => {}
                }
            }
        }
        // Fallback: we know it's defined but couldn't locate which kind.
        let _ = def;
        return Some(TT_VARIABLE);
    }

    // Workspace lookup: search other open documents.
    for other in server.documents.values() {
        if let Some(program) = other.program.as_ref() {
            for decl in &program.decls {
                match decl {
                    Decl::Fn(f) if f.name == name => return Some(TT_FUNCTION),
                    Decl::Type(t) if t.name == name => {
                        return Some(match &t.body {
                            TypeBody::Enum(_) => TT_ENUM,
                            TypeBody::Record(_) => TT_TYPE,
                        });
                    }
                    Decl::Type(t) => {
                        if let TypeBody::Enum(variants) = &t.body
                            && variants.iter().any(|v| v.name == name)
                        {
                            return Some(TT_ENUM_MEMBER);
                        }
                    }
                    Decl::Trait(tr) if tr.name == name => return Some(TT_INTERFACE),
                    _ => {}
                }
            }
        }
    }

    None
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_deltas_single_token() {
        let tokens = vec![RawToken {
            line: 0,
            col_utf16: 3,
            length_utf16: 3,
            token_type: TT_FUNCTION,
        }];
        let encoded = encode_deltas(&tokens);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0].delta_line, 0);
        assert_eq!(encoded[0].delta_start, 3);
        assert_eq!(encoded[0].length, 3);
        assert_eq!(encoded[0].token_type, TT_FUNCTION);
    }

    #[test]
    fn encode_deltas_same_line() {
        let tokens = vec![
            RawToken {
                line: 0,
                col_utf16: 3,
                length_utf16: 3,
                token_type: TT_FUNCTION,
            },
            RawToken {
                line: 0,
                col_utf16: 10,
                length_utf16: 2,
                token_type: TT_VARIABLE,
            },
        ];
        let encoded = encode_deltas(&tokens);
        // Second token on same line: delta_line=0, delta_start = 10-3 = 7
        assert_eq!(encoded[1].delta_line, 0);
        assert_eq!(encoded[1].delta_start, 7);
    }

    #[test]
    fn encode_deltas_new_line() {
        let tokens = vec![
            RawToken {
                line: 0,
                col_utf16: 3,
                length_utf16: 3,
                token_type: TT_FUNCTION,
            },
            RawToken {
                line: 2,
                col_utf16: 4,
                length_utf16: 1,
                token_type: TT_VARIABLE,
            },
        ];
        let encoded = encode_deltas(&tokens);
        // New line: delta_line = 2, delta_start = absolute col = 4
        assert_eq!(encoded[1].delta_line, 2);
        assert_eq!(encoded[1].delta_start, 4);
    }

    #[test]
    fn legend_has_expected_entries() {
        let legend = semantic_tokens_legend();
        assert_eq!(legend.token_types.len(), TOKEN_LEGEND.len());
        assert!(legend.token_types.contains(&SemanticTokenType::FUNCTION));
        assert!(legend.token_types.contains(&SemanticTokenType::INTERFACE));
        assert!(legend.token_modifiers.is_empty());
    }
}
