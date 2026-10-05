//! The printer: a syntax tree and its tokens to a [`Doc`].
//!
//! The tree says what is being printed, the token cursor confirms every
//! token and brings the comments, and the source spells the literals.
//! The printer never writes a token the source does not hold, except the
//! few it decides for itself (a trailing comma, the parentheses of a
//! call whose closure it moves); a tree and a token list that do not
//! agree are an error (`Cursor::error`), not a guess.
//!
//! The style: 100 columns, two spaces; a list is on one line if it fits,
//! else one item per line with a trailing comma; blocks, `match` arms and
//! declaration bodies always break; one empty line is kept where the
//! source has one between items; a closure that is the last argument of
//! a call is written behind the call's parentheses wherever the grammar
//! allows it.

use crate::ast::*;
use crate::intern;
use crate::lexer::{Lexed, Token};

use super::cursor::{Cursor, Mismatch};
use super::doc::{Doc, render};

/// The document for `program`, whose tokens are `lexed` and whose text
/// is `source`.
pub fn program(source: &str, lexed: &Lexed, program: &Program) -> Result<Doc, Mismatch> {
    let mut printer = Printer {
        cur: Cursor::new(source, lexed),
    };
    let doc = printer.program(program);
    match printer.cur.error {
        Some(error) => Err(error),
        None => Ok(doc),
    }
}

// ── Binding powers ───────────────────────────────────────────────────
//
// The parser's (`parse_expr_bp_inner`); stage 8 step C2 moves them to
// `ast::prec`, for both to read.

const PIPE_L: u8 = 55;
const PIPE_R: u8 = 56;
const RANGE_L: u8 = 60;
const RANGE_R: u8 = 61;
const UNARY: u8 = 90;
const ASCRIPTION: u8 = 95;
const CLOSURE: u8 = 115;
const CALL: u8 = 120;
const FIELD: u8 = 130;
/// Binds tighter than any operator: an operand that is closed.
const CLOSED: u8 = u8::MAX;

fn binop_bp(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 20,
        BinOp::And => 30,
        BinOp::Eq | BinOp::Neq => 40,
        BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => 50,
        BinOp::Add | BinOp::Sub => 70,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 80,
    }
}

fn binop_token(op: BinOp) -> Token {
    match op {
        BinOp::Add => Token::Plus,
        BinOp::Sub => Token::Minus,
        BinOp::Mul => Token::Star,
        BinOp::Div => Token::Slash,
        BinOp::Mod => Token::Percent,
        BinOp::Eq => Token::EqEq,
        BinOp::Neq => Token::NotEq,
        BinOp::Lt => Token::Lt,
        BinOp::Gt => Token::Gt,
        BinOp::Leq => Token::LtEq,
        BinOp::Geq => Token::GtEq,
        BinOp::And => Token::AndAnd,
        BinOp::Or => Token::OrOr,
    }
}

/// An expression that is followed by a block (see the parser's
/// `BlockHeader`): a `{` at its own depth may be that block.
#[derive(Clone, Copy, PartialEq)]
enum Header {
    Match,
    Loop,
}

/// Where an expression stands, as far as parentheses go.
#[derive(Clone, Copy)]
struct Ctx {
    /// The expression is parsed with this minimum binding power: it
    /// needs parentheses if its own operator binds less.
    min_bp: u8,
    /// An operator of this binding power follows the expression, which
    /// is its left operand: the expression needs parentheses if its own
    /// operator binds less, or if its right end would take the operator.
    left_of: Option<u8>,
    /// The expression is the right operand of `|>`, where a `?` at its
    /// own level ends the pipeline.
    stage: bool,
    /// The expression is in a `match` or `loop` header, outside any
    /// brackets.
    header: Option<Header>,
}

impl Ctx {
    /// A whole expression: a statement, an argument, what brackets hold.
    fn top() -> Ctx {
        Ctx {
            min_bp: 0,
            left_of: None,
            stage: false,
            header: None,
        }
    }

    fn in_header(header: Header) -> Ctx {
        Ctx {
            header: Some(header),
            ..Ctx::top()
        }
    }

    /// The left operand of an operator of binding power `bp` in an
    /// expression that stands at `self`.
    fn left(self, bp: u8) -> Ctx {
        Ctx {
            min_bp: 0,
            left_of: Some(bp),
            stage: self.stage,
            header: self.header,
        }
    }

    /// A right operand, parsed with the minimum binding power `bp`.
    fn right(self, bp: u8) -> Ctx {
        Ctx {
            min_bp: bp,
            left_of: None,
            stage: false,
            header: self.header,
        }
    }

    fn stage(self) -> Ctx {
        Ctx {
            stage: true,
            ..self.right(PIPE_R)
        }
    }
}

/// The binding power of the operator at the top of `expr` as it is
/// printed.
fn top_bp(expr: &Expr) -> u8 {
    match &expr.kind {
        ExprKind::Binary(_, op, _) => binop_bp(*op),
        ExprKind::Pipe(..) => PIPE_L,
        ExprKind::Range(..) => RANGE_L,
        ExprKind::Ascription(..) => ASCRIPTION,
        ExprKind::QuestionMark(inner) if question_ends_pipeline(inner) => PIPE_L,
        _ => CLOSED,
    }
}

/// Whether `inner?` is printed as `a |> f?`: a `?` behind the last stage
/// of a pipeline applies to the pipeline, unless the stage's right end
/// takes it.
fn question_ends_pipeline(inner: &Expr) -> bool {
    matches!(&inner.kind, ExprKind::Pipe(_, stage) if takes_from(stage, Ctx::top().stage()) == CLOSED)
}

/// The lowest binding power of an operator that, written behind `expr`,
/// would be taken by the right end of `expr` instead of applying to all
/// of it; `CLOSED` if there is none. `ctx` is where `expr` stands.
fn takes_from(expr: &Expr, ctx: Ctx) -> u8 {
    if needs_parens(expr, ctx) {
        CLOSED
    } else {
        takes_from_unwrapped(expr, ctx)
    }
}

/// Whether a `?` stands at the level of the pipeline stage `expr`:
/// behind its leftmost operand, outside parentheses.
fn question_on_left_spine(expr: &Expr, ctx: Ctx) -> bool {
    let through = |left: &Expr, bp: u8| {
        let ctx = Ctx {
            stage: false,
            ..ctx.left(bp)
        };
        !needs_parens(left, ctx) && question_on_left_spine(left, ctx)
    };
    match &expr.kind {
        ExprKind::QuestionMark(_) => true,
        ExprKind::Binary(left, op, _) => through(left, binop_bp(*op)),
        ExprKind::Range(left, _) => through(left, RANGE_L),
        ExprKind::Ascription(left, _) => through(left, ASCRIPTION),
        ExprKind::Call(left, _) => through(left, CALL),
        ExprKind::FieldAccess(left, ..) | ExprKind::RecordUpdate { expr: left, .. } => {
            through(left, FIELD)
        }
        _ => false,
    }
}

/// Whether `expr` has to be in parentheses to stand at `ctx`. The
/// printer keeps the source's parentheses where this says so and drops
/// them elsewhere.
fn needs_parens(expr: &Expr, ctx: Ctx) -> bool {
    let top = top_bp(expr);
    if top < ctx.min_bp {
        return true;
    }
    if let Some(follows) = ctx.left_of {
        let open_ctx = Ctx {
            left_of: None,
            min_bp: 0,
            stage: false,
            header: None,
        };
        if top < follows || takes_from_unwrapped(expr, open_ctx) <= follows {
            return true;
        }
    }
    if ctx.stage && question_on_left_spine(expr, ctx) {
        return true;
    }
    match (ctx.header, &expr.kind) {
        // `match { ...` is a match without a scrutinee, and a `{` in a
        // header is the header's block.
        (
            Some(Header::Match),
            ExprKind::Lambda { .. } | ExprKind::Block(_) | ExprKind::AnonRecord { .. },
        ) => true,
        // `Name {}` in a header is `Name` and the header's block.
        (Some(_), ExprKind::RecordCreate { fields, .. }) => fields.is_empty(),
        _ => false,
    }
}

/// `takes_from` for an expression that is known to stand without
/// parentheses of its own.
fn takes_from_unwrapped(expr: &Expr, ctx: Ctx) -> u8 {
    match &expr.kind {
        ExprKind::Binary(_, op, right) => {
            let r_bp = binop_bp(*op) + 1;
            r_bp.min(takes_from(right, ctx.right(r_bp)))
        }
        ExprKind::Pipe(_, stage) => PIPE_R.min(takes_from(stage, ctx.stage())),
        ExprKind::Range(_, right) => RANGE_R.min(takes_from(right, ctx.right(RANGE_R))),
        ExprKind::Unary(_, operand) => UNARY.min(takes_from(operand, ctx.right(UNARY))),
        ExprKind::Return(_) => 0,
        _ => CLOSED,
    }
}

// ── Lists ────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct ListStyle {
    /// `{ a, b }` rather than `[a, b]`.
    spaced: bool,
    /// One item per line, whatever the width.
    always_break: bool,
}

const TIGHT: ListStyle = ListStyle {
    spaced: false,
    always_break: false,
};
const SPACED: ListStyle = ListStyle {
    spaced: true,
    ..TIGHT
};
const LINES: ListStyle = ListStyle {
    always_break: true,
    ..TIGHT
};

/// An item of a list with its comma, and whether an empty line stands
/// in front of it in the source.
struct Item {
    blank_before: bool,
    doc: Doc,
}

fn list_layout(open: Doc, items: Vec<Item>, tail: Doc, close: Doc, style: ListStyle) -> Doc {
    if items.is_empty() && tail.is_nil() {
        return Doc::concat(vec![open, close]);
    }
    let edge = || {
        if style.always_break {
            Doc::HardLine
        } else if style.spaced {
            Doc::Line
        } else {
            Doc::SoftLine
        }
    };
    let mut inner = vec![edge()];
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            inner.push(if style.always_break {
                Doc::HardLine
            } else {
                Doc::Line
            });
            if item.blank_before {
                inner.push(Doc::if_break(Doc::BlankLine, Doc::Nil));
            }
        }
        inner.push(item.doc);
    }
    inner.push(tail);
    Doc::group(Doc::concat(vec![
        open,
        Doc::nest(Doc::concat(inner)),
        edge(),
        close,
    ]))
}

/// `(inner)`, which may break behind `(` and in front of `)`.
fn parenthesized(open: Doc, inner: Doc, close: Doc) -> Doc {
    Doc::group(Doc::concat(vec![
        open,
        Doc::nest(Doc::concat(vec![Doc::SoftLine, inner])),
        Doc::SoftLine,
        close,
    ]))
}

fn ident() -> Token {
    Token::Ident(intern::intern("_"))
}

fn space() -> Doc {
    Doc::text(" ")
}

struct Printer<'a> {
    cur: Cursor<'a>,
}

impl Printer<'_> {
    fn tok(&mut self, kind: Token) -> Doc {
        self.cur.token(&kind)
    }

    fn name(&mut self) -> Doc {
        self.cur.token(&ident())
    }

    /// `name` or `module.name`.
    fn qualified(&mut self, qualified: bool) -> Doc {
        if qualified {
            Doc::concat(vec![self.name(), self.tok(Token::Dot), self.name()])
        } else {
            self.name()
        }
    }

    /// The comma behind the last item of a list, written when the list
    /// is broken (every list of the grammar takes one); the source's is
    /// skipped and its comments stay here.
    fn last_comma(&mut self, style: ListStyle, required: bool) -> Doc {
        let comma = if required || style.always_break {
            Doc::text(",")
        } else {
            Doc::if_break(Doc::text(","), Doc::Nil)
        };
        Doc::concat(vec![comma, self.cur.skip(&Token::Comma)])
    }

    /// The comments in front of a closing bracket, for inside the
    /// indentation of what the bracket closes.
    fn dangling(&mut self) -> Doc {
        if self.cur.comment_ahead() {
            self.cur.leading()
        } else {
            Doc::Nil
        }
    }

    /// `open item, item close`.
    fn delimited<T>(
        &mut self,
        open: Token,
        close: Token,
        style: ListStyle,
        items: &[T],
        mut item: impl FnMut(&mut Self, &T) -> Doc,
    ) -> Doc {
        let open = self.tok(open);
        if items.is_empty() && self.cur.line_ended() {
            // `[ -- nothing yet` and `]` on the next line: the comment
            // stays inside.
            let tail = self.dangling();
            let close = self.tok(close);
            return Doc::group(Doc::concat(vec![
                open,
                Doc::nest(tail),
                Doc::SoftLine,
                close,
            ]));
        }
        let mut docs = Vec::new();
        for (i, it) in items.iter().enumerate() {
            let blank_before = i > 0 && self.cur.blank_before();
            let doc = item(self, it);
            let comma = if i + 1 < items.len() {
                self.tok(Token::Comma)
            } else {
                self.last_comma(style, false)
            };
            docs.push(Item {
                blank_before,
                doc: Doc::concat(vec![doc, comma]),
            });
        }
        let tail = self.dangling();
        let close = self.tok(close);
        list_layout(open, docs, tail, close, style)
    }

    /// The items between two braces, one per line; `next` gives the next
    /// one, or `None` at the closing brace. Empty if there is nothing
    /// between the braces.
    fn lines(&mut self, mut next: impl FnMut(&mut Self) -> Option<Doc>) -> Doc {
        let mut inner = Vec::new();
        loop {
            let blank = !inner.is_empty() && self.cur.blank_before();
            let Some(doc) = next(self) else { break };
            inner.push(Doc::HardLine);
            if blank {
                inner.push(Doc::BlankLine);
            }
            inner.push(doc);
        }
        if self.cur.comment_ahead() {
            let blank = !inner.is_empty() && self.cur.blank_before();
            inner.push(Doc::HardLine);
            if blank {
                inner.push(Doc::BlankLine);
            }
            inner.push(self.cur.leading());
        }
        if inner.is_empty() {
            return Doc::Nil;
        }
        Doc::concat(vec![Doc::nest(Doc::concat(inner)), Doc::HardLine])
    }

    /// What stands behind `=` or `->`: on the same line, or, if a
    /// comment on a line of its own comes first, indented under it.
    fn after(&mut self, expr: &Expr) -> Doc {
        if self.cur.own_line_comment_ahead() {
            Doc::nest(self.expr(expr, Ctx::top()))
        } else {
            Doc::concat(vec![space(), self.expr(expr, Ctx::top())])
        }
    }

    // ── Program ──────────────────────────────────────────────────────

    fn program(&mut self, program: &Program) -> Doc {
        let header = self.cur.header();
        let mut imports: Vec<(String, Doc)> = Vec::new();
        let mut others: Vec<Doc> = Vec::new();
        for decl in &program.decls {
            if self.cur.offset() != decl_start(decl) {
                self.cur
                    .fail("a declaration does not start where the previous one ends");
                break;
            }
            let doc = self.decl(decl);
            match decl {
                Decl::Import(target, _) => imports.push((import_key(target), doc)),
                _ => others.push(doc),
            }
        }
        imports.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        let section = |out: &mut Vec<Doc>, doc: Doc| {
            if !out.is_empty() {
                out.push(Doc::BlankLine);
            }
            out.push(doc);
        };
        if !header.is_nil() {
            section(&mut out, header);
        }
        if !imports.is_empty() {
            let mut lines = Vec::new();
            for (i, (_, doc)) in imports.into_iter().enumerate() {
                if i > 0 {
                    lines.push(Doc::HardLine);
                }
                lines.push(doc);
            }
            section(&mut out, Doc::concat(lines));
        }
        for doc in others {
            section(&mut out, doc);
        }
        // The comments behind the last declaration.
        if !self.cur.at_end() {
            self.cur
                .fail("the last declaration ends before the end of the file");
        }
        if self.cur.comment_ahead() {
            let blank = self.cur.blank_before();
            let tail = self.cur.leading();
            if !out.is_empty() {
                out.push(if blank { Doc::BlankLine } else { Doc::HardLine });
            }
            out.push(tail);
        }
        Doc::concat(out)
    }

    fn decl(&mut self, decl: &Decl) -> Doc {
        match decl {
            Decl::Fn(f) => self.fn_decl(f),
            Decl::Type(t) => self.type_decl(t),
            Decl::Trait(t) => self.trait_decl(t),
            Decl::TraitImpl(t) => self.trait_impl(t),
            Decl::Import(target, _) => self.import(target),
            Decl::Let {
                pattern,
                ty,
                value,
                is_pub,
                ..
            } => {
                let mut docs = Vec::new();
                if *is_pub {
                    docs.push(self.tok(Token::Pub));
                    docs.push(space());
                }
                docs.push(self.let_binding(pattern, ty.as_ref(), value));
                Doc::concat(docs)
            }
        }
    }

    /// `let pattern: Type = value`.
    fn let_binding(&mut self, pattern: &Pattern, ty: Option<&TypeExpr>, value: &Expr) -> Doc {
        let mut docs = vec![self.tok(Token::Let), space(), self.pattern(pattern)];
        if let Some(ty) = ty {
            docs.push(self.tok(Token::Colon));
            docs.push(space());
            docs.push(self.type_expr(ty));
        }
        docs.push(space());
        docs.push(self.tok(Token::Eq));
        docs.push(self.after(value));
        Doc::concat(docs)
    }

    fn fn_decl(&mut self, f: &FnDecl) -> Doc {
        let mut docs = Vec::new();
        if f.is_pub {
            docs.push(self.tok(Token::Pub));
            docs.push(space());
        }
        docs.push(self.tok(Token::Fn));
        docs.push(space());
        docs.push(self.name());
        docs.push(self.delimited(
            Token::LParen,
            Token::RParen,
            TIGHT,
            &f.params,
            |p, param| p.param(param),
        ));
        if let Some(ty) = &f.return_type {
            docs.push(space());
            docs.push(self.tok(Token::Arrow));
            docs.push(space());
            docs.push(self.type_expr(ty));
        }
        docs.push(self.where_clauses(&f.where_clauses));
        if !f.is_signature_only {
            docs.push(space());
            docs.push(self.block(&f.body));
        }
        Doc::concat(docs)
    }

    fn param(&mut self, param: &Param) -> Doc {
        let mut docs = Vec::new();
        if param.kind == ParamKind::Type {
            docs.push(self.tok(Token::Type));
            docs.push(space());
        }
        docs.push(self.pattern(&param.pattern));
        if let Some(ty) = &param.ty {
            docs.push(self.tok(Token::Colon));
            docs.push(space());
            docs.push(self.type_expr(ty));
        }
        Doc::concat(docs)
    }

    /// ` where a: T + U, b: V`, on the line of the header if it fits.
    /// The tree holds one clause per bound; the tokens say which bounds
    /// are joined by `+`.
    fn where_clauses(&mut self, clauses: &[WhereClause]) -> Doc {
        if clauses.is_empty() {
            return Doc::Nil;
        }
        let mut docs = vec![Doc::Line, self.tok(Token::Where), space()];
        let mut i = 0;
        while i < clauses.len() {
            docs.push(self.name());
            docs.push(self.tok(Token::Colon));
            docs.push(space());
            loop {
                let clause = &clauses[i];
                docs.push(self.trait_ref(clause.trait_module.is_some(), &clause.trait_args));
                i += 1;
                if i < clauses.len() && self.cur.at(&Token::Plus) {
                    docs.push(space());
                    docs.push(self.tok(Token::Plus));
                    docs.push(space());
                } else {
                    break;
                }
            }
            if i < clauses.len() {
                docs.push(self.tok(Token::Comma));
                docs.push(Doc::Line);
            }
        }
        Doc::group(Doc::nest(Doc::concat(docs)))
    }

    /// `Trait`, `m.Trait`, `Trait(Int)`.
    fn trait_ref(&mut self, qualified: bool, args: &[TypeExpr]) -> Doc {
        let name = self.qualified(qualified);
        if args.is_empty() {
            self.cur.skip_empty_parens();
            return name;
        }
        let args = self.delimited(Token::LParen, Token::RParen, TIGHT, args, |p, arg| {
            p.type_expr(arg)
        });
        Doc::concat(vec![name, args])
    }

    /// `A + B` behind a `:`.
    fn bounds(&mut self, bounds: &[TraitRef]) -> Doc {
        let mut docs = Vec::new();
        for (i, bound) in bounds.iter().enumerate() {
            if i > 0 {
                docs.push(space());
                docs.push(self.tok(Token::Plus));
                docs.push(space());
            }
            docs.push(self.trait_ref(bound.module.is_some(), &bound.args));
        }
        Doc::concat(docs)
    }

    /// `(a, b)` behind a declared name, or nothing.
    fn name_params(&mut self, count: usize) -> Doc {
        if count == 0 {
            self.cur.skip_empty_parens();
            return Doc::Nil;
        }
        let names: Vec<()> = vec![(); count];
        self.delimited(Token::LParen, Token::RParen, TIGHT, &names, |p, _| p.name())
    }

    fn type_decl(&mut self, t: &TypeDecl) -> Doc {
        let mut docs = Vec::new();
        if t.is_pub {
            docs.push(self.tok(Token::Pub));
            docs.push(space());
        }
        docs.push(self.tok(Token::Type));
        docs.push(space());
        docs.push(self.name());
        docs.push(self.name_params(t.params.len()));
        docs.push(space());
        match &t.body {
            TypeBody::Alias(target) => {
                docs.push(self.tok(Token::Eq));
                docs.push(space());
                docs.push(self.type_expr(target));
            }
            TypeBody::Enum(variants) => {
                docs.push(self.delimited(
                    Token::LBrace,
                    Token::RBrace,
                    LINES,
                    variants,
                    |p, variant| {
                        let name = p.name();
                        if variant.fields.is_empty() {
                            p.cur.skip_empty_parens();
                            return name;
                        }
                        let fields = p.delimited(
                            Token::LParen,
                            Token::RParen,
                            TIGHT,
                            &variant.fields,
                            |p, field| p.type_expr(field),
                        );
                        Doc::concat(vec![name, fields])
                    },
                ));
            }
            TypeBody::Record(fields) => {
                docs.push(self.delimited(
                    Token::LBrace,
                    Token::RBrace,
                    LINES,
                    fields,
                    |p, field| {
                        Doc::concat(vec![
                            p.name(),
                            p.tok(Token::Colon),
                            space(),
                            p.type_expr(&field.ty),
                        ])
                    },
                ));
            }
        }
        Doc::concat(docs)
    }

    fn trait_decl(&mut self, t: &TraitDecl) -> Doc {
        let mut docs = Vec::new();
        if t.is_pub {
            docs.push(self.tok(Token::Pub));
            docs.push(space());
        }
        docs.push(self.tok(Token::Trait));
        docs.push(space());
        docs.push(self.name());
        docs.push(self.name_params(t.params.len()));
        if !t.supertraits.is_empty() {
            docs.push(self.tok(Token::Colon));
            docs.push(space());
            docs.push(self.bounds(&t.supertraits));
        }
        docs.push(self.where_clauses(&t.param_where_clauses));
        docs.push(space());
        // The parser takes a trait body without its opening brace.
        docs.push(if self.cur.at(&Token::LBrace) {
            self.tok(Token::LBrace)
        } else {
            Doc::text("{")
        });
        // The tree holds the associated types and the methods apart;
        // the tokens say in which order they stand.
        let mut assoc_types = t.assoc_types.iter();
        let mut methods = t.methods.iter();
        docs.push(self.lines(|p| {
            if p.cur.at(&Token::Type) {
                let assoc = assoc_types.next()?;
                let mut docs = vec![p.tok(Token::Type), space(), p.name()];
                if !assoc.bounds.is_empty() {
                    docs.push(p.tok(Token::Colon));
                    docs.push(space());
                    docs.push(p.bounds(&assoc.bounds));
                }
                Some(Doc::concat(docs))
            } else {
                Some(p.fn_decl(methods.next()?))
            }
        }));
        if assoc_types.next().is_some() || methods.next().is_some() {
            self.cur.fail("a trait member is not where the tree has it");
        }
        docs.push(self.tok(Token::RBrace));
        Doc::concat(docs)
    }

    fn trait_impl(&mut self, t: &TraitImpl) -> Doc {
        let mut docs = vec![
            self.tok(Token::Trait),
            space(),
            self.trait_ref(t.trait_module.is_some(), &t.trait_args),
            space(),
            // `for` is an identifier to the lexer.
            self.name(),
            space(),
            self.qualified(t.target_module.is_some()),
        ];
        if !t.target_type_args.is_empty() {
            docs.push(self.delimited(
                Token::LParen,
                Token::RParen,
                TIGHT,
                &t.target_type_args,
                |p, arg| p.type_expr(arg),
            ));
        }
        docs.push(self.where_clauses(&t.where_clauses));
        docs.push(space());
        docs.push(self.tok(Token::LBrace));
        let mut bindings = t.assoc_type_bindings.iter();
        let mut methods = t.methods.iter();
        docs.push(self.lines(|p| {
            if p.cur.at(&Token::Type) {
                let binding = bindings.next()?;
                Some(Doc::concat(vec![
                    p.tok(Token::Type),
                    space(),
                    p.name(),
                    space(),
                    p.tok(Token::Eq),
                    space(),
                    p.type_expr(&binding.ty),
                ]))
            } else {
                Some(p.fn_decl(methods.next()?))
            }
        }));
        if bindings.next().is_some() || methods.next().is_some() {
            self.cur.fail("an impl member is not where the tree has it");
        }
        docs.push(self.tok(Token::RBrace));
        Doc::concat(docs)
    }

    fn import(&mut self, target: &ImportTarget) -> Doc {
        // Imports are sorted and stand without empty lines; their
        // comments go with them.
        let mut docs = vec![
            self.cur.leading_without_blank_lines(),
            self.tok(Token::Import),
            space(),
            self.name(),
        ];
        match target {
            ImportTarget::Module(_) => {}
            ImportTarget::Items(_, items) => {
                docs.push(self.tok(Token::Dot));
                docs.push(
                    self.delimited(Token::LBrace, Token::RBrace, SPACED, items, |p, _| p.name()),
                );
            }
            ImportTarget::Alias(..) => {
                docs.push(space());
                docs.push(self.tok(Token::As));
                docs.push(space());
                docs.push(self.name());
            }
        }
        Doc::concat(docs)
    }

    // ── Statements ───────────────────────────────────────────────────

    /// `{ statements }`: always on lines of their own.
    fn block(&mut self, block: &Expr) -> Doc {
        let ExprKind::Block(stmts) = &block.kind else {
            self.cur.fail("a body is not a block");
            return Doc::Nil;
        };
        let open = self.tok(Token::LBrace);
        let body = self.stmts(stmts);
        let close = self.tok(Token::RBrace);
        Doc::concat(vec![open, body, close])
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> Doc {
        let mut stmts = stmts.iter();
        self.lines(|p| Some(p.stmt(stmts.next()?)))
    }

    fn stmt(&mut self, stmt: &Stmt) -> Doc {
        match stmt {
            Stmt::Let { pattern, ty, value } => self.let_binding(pattern, ty.as_ref(), value),
            Stmt::When {
                pattern,
                expr,
                else_body,
            } => Doc::concat(vec![
                self.tok(Token::When),
                space(),
                self.tok(Token::Let),
                space(),
                self.pattern(pattern),
                space(),
                self.tok(Token::Eq),
                self.after(expr),
                space(),
                self.tok(Token::Else),
                space(),
                self.block(else_body),
            ]),
            Stmt::WhenBool {
                condition,
                else_body,
            } => Doc::concat(vec![
                self.tok(Token::When),
                space(),
                self.expr(condition, Ctx::top()),
                space(),
                self.tok(Token::Else),
                space(),
                self.block(else_body),
            ]),
            Stmt::Expr(expr) => self.expr(expr, Ctx::top()),
        }
    }

    // ── Expressions ──────────────────────────────────────────────────

    fn expr(&mut self, expr: &Expr, ctx: Ctx) -> Doc {
        // The source's parentheses around this expression: one pair is
        // kept if the expression needs it here, the rest is dropped.
        let wrappers = self.cur.wrappers(expr.span.end);
        if wrappers > 0 && self.cur.comments_inside_wrappers(wrappers, expr.span.end) {
            // A comment at the inside of a parenthesis may stand at a
            // line break that only the parenthesis allows: all stay,
            // and each can break at its inside, so that the comment
            // stays there.
            let mut opens = Vec::new();
            for _ in 0..wrappers {
                opens.push(self.tok(Token::LParen));
            }
            let mut doc = self.bare_expr(expr, Ctx::top());
            for open in opens.into_iter().rev() {
                let tail = self.dangling();
                let close = self.tok(Token::RParen);
                doc = parenthesized(open, Doc::concat(vec![doc, tail]), close);
            }
            doc
        } else if wrappers > 0 && needs_parens(expr, ctx) {
            let open = self.tok(Token::LParen);
            self.cur.skip_n(&Token::LParen, wrappers - 1);
            let inner = self.bare_expr(expr, Ctx::top());
            let close = self.tok(Token::RParen);
            self.cur.skip_n(&Token::RParen, wrappers - 1);
            Doc::concat(vec![open, inner, close, self.cur.carried()])
        } else if wrappers > 0 {
            self.cur.skip_n(&Token::LParen, wrappers);
            let doc = self.bare_expr(expr, ctx);
            self.cur.skip_n(&Token::RParen, wrappers);
            Doc::concat(vec![doc, self.cur.carried()])
        } else {
            self.bare_expr(expr, ctx)
        }
    }

    fn bare_expr(&mut self, expr: &Expr, ctx: Ctx) -> Doc {
        match &expr.kind {
            ExprKind::Int(_) => self.tok(Token::Int(0)),
            ExprKind::Float(_) => self.tok(Token::Float(0.0)),
            ExprKind::Bool(_) => self.tok(Token::Bool(true)),
            ExprKind::StringLit(..) => self.tok(Token::StringLit(String::new(), false)),
            ExprKind::StringInterp(parts) => self.string_interp(parts),
            ExprKind::List(elems) => self.delimited(
                Token::LBracket,
                Token::RBracket,
                TIGHT,
                elems,
                |p, elem| match elem {
                    ListElem::Single(e) => p.expr(e, Ctx::top()),
                    ListElem::Spread(e) => {
                        Doc::concat(vec![p.tok(Token::DotDot), p.expr(e, Ctx::top())])
                    }
                },
            ),
            ExprKind::Map(pairs) => self.delimited(
                Token::HashBrace,
                Token::RBrace,
                SPACED,
                pairs,
                |p, (k, v)| {
                    Doc::concat(vec![
                        p.expr(k, Ctx::top()),
                        p.tok(Token::Colon),
                        space(),
                        p.expr(v, Ctx::top()),
                    ])
                },
            ),
            ExprKind::SetLit(elems) => {
                self.delimited(Token::HashBracket, Token::RBracket, TIGHT, elems, |p, e| {
                    p.expr(e, Ctx::top())
                })
            }
            ExprKind::Tuple(elems) => self.tuple(elems, |p, e| p.expr(e, Ctx::top())),
            ExprKind::Unit => Doc::concat(vec![self.tok(Token::LParen), self.tok(Token::RParen)]),
            ExprKind::Ident(_) => self.name(),
            ExprKind::FieldAccess(base, field, _) => {
                // `(t.0).1`: without the parentheses, `0.1` is a number.
                let numbered = |name: intern::Symbol| {
                    intern::resolve(name).starts_with(|c: char| c.is_ascii_digit())
                };
                let base_kind = &base.kind;
                let base_wrapped = self.cur.wrappers(base.span.end) > 0;
                let after_number = numbered(*field)
                    && matches!(&base.kind, ExprKind::FieldAccess(_, inner, _) if numbered(*inner));
                let base = if after_number && self.cur.wrappers(base.span.end) > 0 {
                    let open = self.tok(Token::LParen);
                    let inner = self.expr(base, Ctx::top());
                    let close = self.tok(Token::RParen);
                    Doc::concat(vec![open, inner, close])
                } else {
                    self.expr(base, ctx.left(FIELD))
                };
                // `1 .0` is a field of `1`; `1.0` is a number. And behind
                // `x as T` without parentheses, only a line break keeps
                // the `.` from being part of the type.
                let gap = match base_kind {
                    ExprKind::Int(_) => space(),
                    ExprKind::Ascription(..) if !base_wrapped => Doc::nest(Doc::HardLine),
                    _ => Doc::Nil,
                };
                let dot = self.tok(Token::Dot);
                // `t.0`: the parser takes a number for a field name.
                let field = if self.cur.at(&Token::Int(0)) {
                    self.tok(Token::Int(0))
                } else {
                    self.name()
                };
                Doc::concat(vec![base, gap, dot, field])
            }
            ExprKind::Binary(_, op, _) => self.binary(expr, binop_bp(*op), ctx),
            ExprKind::Unary(op, operand) => {
                let token = match op {
                    UnaryOp::Neg => Token::Minus,
                    UnaryOp::Not => Token::Not,
                };
                let op_doc = self.tok(token);
                // `--` starts a comment: `-(-x)` keeps its parentheses,
                // `- -x` its space.
                let doubled =
                    *op == UnaryOp::Neg && matches!(operand.kind, ExprKind::Unary(UnaryOp::Neg, _));
                if doubled && self.cur.wrappers(operand.span.end) > 0 {
                    let open = self.tok(Token::LParen);
                    let inner = self.expr(operand, Ctx::top());
                    let close = self.tok(Token::RParen);
                    return Doc::concat(vec![op_doc, open, inner, close]);
                }
                let gap = if doubled { space() } else { Doc::Nil };
                let operand = self.expr(operand, ctx.right(UNARY));
                Doc::concat(vec![op_doc, gap, operand])
            }
            ExprKind::Pipe(..) => self.pipeline(expr, ctx),
            ExprKind::Range(start, end) => Doc::concat(vec![
                self.expr(start, ctx.left(RANGE_L)),
                self.tok(Token::DotDot),
                self.expr(end, ctx.right(RANGE_R)),
            ]),
            ExprKind::QuestionMark(inner) => {
                let inner = if question_ends_pipeline(inner) {
                    self.expr(inner, ctx)
                } else {
                    self.expr(inner, ctx.left(CALL))
                };
                Doc::concat(vec![inner, self.tok(Token::Question)])
            }
            ExprKind::Ascription(inner, ty) => Doc::concat(vec![
                self.expr(inner, ctx.left(ASCRIPTION)),
                space(),
                self.tok(Token::As),
                space(),
                self.type_expr(ty),
            ]),
            ExprKind::Call(callee, args) => self.call(expr, callee, args, ctx),
            ExprKind::Lambda { params, body } => self.lambda(expr, params, body),
            ExprKind::RecordCreate { module, fields, .. } => {
                let name = self.qualified(module.is_some());
                let fields = self.fields(fields);
                Doc::concat(vec![name, space(), fields])
            }
            ExprKind::RecordUpdate { expr: base, fields } => {
                // As for a field: `x as T` and a line break.
                let gap = match &base.kind {
                    ExprKind::Ascription(..) if self.cur.wrappers(base.span.end) == 0 => {
                        Doc::nest(Doc::HardLine)
                    }
                    _ => Doc::Nil,
                };
                let base = self.expr(base, ctx.left(FIELD));
                let base = Doc::concat(vec![base, gap]);
                let dot = self.tok(Token::Dot);
                let fields = self.fields(fields);
                Doc::concat(vec![base, dot, fields])
            }
            ExprKind::AnonRecord { spread, fields } => {
                // The spread is the first item of the list.
                let spread = spread.as_deref();
                let mut items: Vec<Option<&(intern::Symbol, Expr)>> = Vec::new();
                if spread.is_some() {
                    items.push(None);
                }
                items.extend(fields.iter().map(Some));
                self.delimited(
                    Token::LBrace,
                    Token::RBrace,
                    SPACED,
                    &items,
                    |p, item| match (item, spread) {
                        (Some((_, value)), _) => p.field(value),
                        (None, Some(spread)) => {
                            Doc::concat(vec![p.tok(Token::DotDotDot), p.expr(spread, Ctx::top())])
                        }
                        (None, None) => Doc::Nil,
                    },
                )
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                let mut docs = vec![self.tok(Token::Match), space()];
                if let Some(scrutinee) = scrutinee {
                    docs.push(self.expr(scrutinee, Ctx::in_header(Header::Match)));
                    docs.push(space());
                }
                docs.push(self.tok(Token::LBrace));
                let guardless = scrutinee.is_none();
                let mut arms = arms.iter();
                docs.push(self.lines(|p| Some(p.arm(arms.next()?, guardless))));
                docs.push(self.tok(Token::RBrace));
                Doc::concat(docs)
            }
            ExprKind::Return(value) => {
                let keyword = self.tok(Token::Return);
                match value {
                    Some(value) => {
                        Doc::concat(vec![keyword, space(), self.expr(value, Ctx::top())])
                    }
                    None => keyword,
                }
            }
            ExprKind::Block(_) => self.block(expr),
            ExprKind::Loop { bindings, body } => {
                let mut docs = vec![self.tok(Token::Loop), space()];
                for (i, (_, _, init)) in bindings.iter().enumerate() {
                    docs.push(self.name());
                    docs.push(space());
                    docs.push(self.tok(Token::Eq));
                    docs.push(space());
                    docs.push(self.expr(init, Ctx::in_header(Header::Loop)));
                    if i + 1 < bindings.len() {
                        docs.push(self.tok(Token::Comma));
                    }
                    docs.push(space());
                }
                docs.push(self.block(body));
                Doc::concat(docs)
            }
            ExprKind::Recur(args) => {
                let keyword = self.tok(Token::Loop);
                let args = self.delimited(Token::LParen, Token::RParen, TIGHT, args, |p, arg| {
                    p.expr(arg, Ctx::top())
                });
                Doc::concat(vec![keyword, args])
            }
        }
    }

    /// `(a, b)`; a tuple of one keeps its comma.
    fn tuple<T>(&mut self, elems: &[T], mut item: impl FnMut(&mut Self, &T) -> Doc) -> Doc {
        if elems.len() != 1 {
            return self.delimited(Token::LParen, Token::RParen, TIGHT, elems, item);
        }
        let open = self.tok(Token::LParen);
        let doc = item(self, &elems[0]);
        let comma = self.last_comma(TIGHT, true);
        let tail = self.dangling();
        let close = self.tok(Token::RParen);
        let items = vec![Item {
            blank_before: false,
            doc: Doc::concat(vec![doc, comma]),
        }];
        list_layout(open, items, tail, close, TIGHT)
    }

    /// `{ name: value, ... }`.
    fn fields(&mut self, fields: &[(intern::Symbol, Expr)]) -> Doc {
        self.delimited(
            Token::LBrace,
            Token::RBrace,
            SPACED,
            fields,
            |p, (_, value)| p.field(value),
        )
    }

    fn field(&mut self, value: &Expr) -> Doc {
        Doc::concat(vec![
            self.name(),
            self.tok(Token::Colon),
            space(),
            self.expr(value, Ctx::top()),
        ])
    }

    /// A chain of operators of one binding power: on one line, or one
    /// operand per line. A line breaks in front of the operator, except
    /// `+` and `-`, which at the start of a line would start a new
    /// statement.
    fn binary(&mut self, expr: &Expr, bp: u8, ctx: Ctx) -> Doc {
        let mut chain: Vec<(BinOp, &Expr)> = Vec::new();
        let mut first = expr;
        while let ExprKind::Binary(left, op, right) = &first.kind
            && binop_bp(*op) == bp
            && (chain.is_empty() || !needs_parens(first, ctx.left(bp)))
        {
            chain.push((*op, right));
            first = left;
        }
        let first = self.expr(first, ctx.left(bp));
        let mut rest = Vec::new();
        for (op, right) in chain.into_iter().rev() {
            let op_doc = self.tok(binop_token(op));
            let right = self.expr(right, ctx.right(bp + 1));
            if matches!(op, BinOp::Add | BinOp::Sub) {
                rest.extend([space(), op_doc, Doc::Line, right]);
            } else {
                rest.extend([Doc::Line, op_doc, space(), right]);
            }
        }
        // The first operand is not part of the group: if it spans lines
        // (a call with broken arguments, a `match`), what follows its
        // last line can still stay on that line.
        Doc::concat(vec![first, Doc::group(Doc::nest(Doc::concat(rest)))])
    }

    /// `a |> f |> g`: on one line, or one stage per line.
    fn pipeline(&mut self, expr: &Expr, ctx: Ctx) -> Doc {
        let mut stages: Vec<&Expr> = Vec::new();
        let mut first = expr;
        while let ExprKind::Pipe(left, stage) = &first.kind
            && (stages.is_empty() || !needs_parens(first, ctx.left(PIPE_L)))
        {
            stages.push(stage);
            first = left;
        }
        let first = self.expr(first, ctx.left(PIPE_L));
        let mut rest = Vec::new();
        for stage in stages.into_iter().rev() {
            rest.push(Doc::Line);
            rest.push(self.tok(Token::Pipe));
            rest.push(space());
            rest.push(self.expr(stage, ctx.stage()));
        }
        // The first operand is not part of the group: if it spans lines
        // (a call with broken arguments, a `match`), what follows its
        // last line can still stay on that line.
        Doc::concat(vec![first, Doc::group(Doc::nest(Doc::concat(rest)))])
    }

    /// A string with interpolations: its text as written, its holes on
    /// one line. If a hole holds a comment or cannot be on one line, or
    /// the string holds a line break, all of it is written as it is in
    /// the source.
    fn string_interp(&mut self, parts: &[StringPart]) -> Doc {
        let leading = self.cur.leading();
        let start = self.cur.offset();
        let comments = self.cur.comments_written();
        let holes = parts
            .iter()
            .filter(|part| matches!(part, StringPart::Expr(_)))
            .count();
        let mut docs = vec![self.tok(Token::StringStart(String::new()))];
        let mut verbatim = false;
        let mut seen = 0;
        for part in parts {
            let StringPart::Expr(hole) = part else {
                continue;
            };
            seen += 1;
            let hole = render(&self.expr(hole, Ctx::top()), usize::MAX / 2);
            let hole = hole.trim_end_matches('\n');
            verbatim |= hole.contains('\n');
            docs.push(Doc::text(hole));
            if seen < holes {
                docs.push(self.tok(Token::StringMiddle(String::new())));
            }
        }
        docs.push(
            self.cur
                .token_without_trailing(&Token::StringEnd(String::new())),
        );
        verbatim |= self.cur.comments_written() != comments;
        let text = self.cur.text_since(start);
        verbatim |= text.contains('\n');
        let body = if verbatim {
            // A comment that waits for the end of the line stands in
            // front of the ones in the string.
            Doc::concat(vec![Doc::Settle, Doc::text(text)])
        } else {
            Doc::concat(docs)
        };
        let trailing = self.cur.trailing();
        Doc::concat(vec![leading, body, trailing])
    }

    /// A call. A closure that is the last argument stands behind the
    /// parentheses, `f(a) { x -> x }`, except in a `match` header, where
    /// a `{` is the header's block, and where the callee is itself a
    /// call, which the closure would join.
    fn call(&mut self, call: &Expr, callee: &Expr, args: &[Expr], ctx: Ctx) -> Doc {
        let closure = matches!(
            args.last(),
            Some(Expr {
                kind: ExprKind::Lambda { .. },
                ..
            })
        );
        let in_match_header = ctx.header == Some(Header::Match);
        let callee_is_call = matches!(callee.kind, ExprKind::Call(..));
        let callee_bp = if closure { CLOSURE } else { CALL };
        let callee_doc = self.expr(callee, ctx.left(callee_bp));

        // `f { x -> x }`: the closure is the only argument.
        if !self.cur.at(&Token::LParen) {
            let [arg] = args else {
                self.cur.fail("a call without parentheses or a closure");
                return callee_doc;
            };
            let closure = self.expr(arg, Ctx::top());
            return if in_match_header {
                // A closure that is the only argument hugs the
                // parentheses.
                Doc::concat(vec![callee_doc, Doc::text("("), closure, Doc::text(")")])
            } else {
                Doc::concat(vec![callee_doc, space(), closure])
            };
        }

        let plain = if closure { args.len() - 1 } else { args.len() };
        // Moving the closure out of the parentheses moves the comments
        // around it to where a line break may not be allowed: with a
        // comment in the way, the closure stays where it is.
        let open_at = self.cur.offset();
        let (in_the_way, at_open) = match args.last() {
            Some(last) if closure => {
                let before = match plain {
                    0 => open_at,
                    _ => args[plain - 1].span.end,
                };
                let moves = last.span.end != call.span.end;
                (
                    moves
                        && (self.cur.comments_between(before, last.span.start)
                            || self.cur.comments_between(last.span.end, call.span.end)),
                    self.cur.comments_between(open_at, last.span.start),
                )
            }
            _ => (false, false),
        };
        let trailing =
            closure && !in_match_header && !(args.len() == 1 && callee_is_call) && !in_the_way;
        // How many arguments stand between the parentheses of the result.
        let inside = if trailing { plain } else { args.len() };

        // `f({ x -> x })` becomes `f { x -> x }`: no parentheses.
        let bare = trailing && plain == 0 && !at_open;
        let open = if bare {
            self.cur.skip(&Token::LParen)
        } else {
            self.tok(Token::LParen)
        };
        let mut items = Vec::new();
        // Whether the source closes its parentheses in front of the
        // closure.
        let mut source_trailing = false;
        for (i, arg) in args[..plain].iter().enumerate() {
            let blank_before = i > 0 && self.cur.blank_before();
            let doc = self.expr(arg, Ctx::top());
            let comma = if i + 1 == inside {
                self.last_comma(TIGHT, false)
            } else if i + 1 == plain && !self.cur.at(&Token::Comma) {
                // The closure follows in the result, behind `)` in the
                // source.
                source_trailing = true;
                Doc::concat(vec![self.cur.skip(&Token::RParen), Doc::text(",")])
            } else {
                self.tok(Token::Comma)
            };
            items.push(Item {
                blank_before,
                doc: Doc::concat(vec![doc, comma]),
            });
        }
        if !closure {
            let tail = self.dangling();
            let close = self.tok(Token::RParen);
            let list = list_layout(open, items, tail, close, TIGHT);
            return Doc::concat(vec![callee_doc, list]);
        }
        let closure = &args[plain];
        if trailing {
            let closes_here = self.cur.at(&Token::RParen);
            let list = if bare {
                let close = if closes_here {
                    self.cur.skip(&Token::RParen)
                } else {
                    Doc::Nil
                };
                Doc::concat(vec![open, close])
            } else if items.is_empty() && closes_here && self.cur.line_ended() {
                // `f( -- note` and `) { x -> x }`: the comment stays
                // between the parentheses.
                let close = self.tok(Token::RParen);
                Doc::group(Doc::concat(vec![open, Doc::SoftLine, close]))
            } else {
                let close = if closes_here {
                    self.tok(Token::RParen)
                } else {
                    Doc::text(")")
                };
                list_layout(open, items, Doc::Nil, close, TIGHT)
            };
            let closure = self.expr(closure, Ctx::top());
            let behind = if closes_here {
                Doc::Nil
            } else {
                Doc::concat(vec![
                    self.cur.skip(&Token::Comma),
                    self.cur.skip(&Token::RParen),
                ])
            };
            return Doc::concat(vec![callee_doc, list, space(), closure, behind]);
        }
        // The closure stays between the parentheses.
        let mut before = Doc::Nil;
        if plain == 0 && self.cur.at(&Token::RParen) {
            source_trailing = true;
            before = self.cur.skip(&Token::RParen);
        }
        let blank_before = plain > 0 && self.cur.blank_before();
        let doc = self.expr(closure, Ctx::top());
        let mut self_skip_comma = Doc::Nil;
        let (comma, tail, close) = if source_trailing {
            (
                Doc::if_break(Doc::text(","), Doc::Nil),
                Doc::Nil,
                Doc::text(")"),
            )
        } else if plain == 0 && !in_the_way {
            self_skip_comma = self.cur.skip(&Token::Comma);
            let tail = self.dangling();
            (Doc::Nil, tail, self.tok(Token::RParen))
        } else {
            let comma = self.last_comma(TIGHT, false);
            let tail = self.dangling();
            (comma, tail, self.tok(Token::RParen))
        };
        if plain == 0 && !in_the_way {
            // A closure that is the only argument hugs the parentheses.
            let comma = if source_trailing {
                Doc::Nil
            } else {
                self_skip_comma
            };
            return Doc::concat(vec![callee_doc, open, before, doc, comma, tail, close]);
        }
        items.push(Item {
            blank_before,
            doc: Doc::concat(vec![before, doc, comma]),
        });
        let list = list_layout(open, items, tail, close, TIGHT);
        Doc::concat(vec![callee_doc, list])
    }

    /// `{ a, b -> body }`. Statements written directly behind the arrow
    /// stand on lines of their own.
    fn lambda(&mut self, expr: &Expr, params: &[Param], body: &Expr) -> Doc {
        let mut head = vec![self.tok(Token::LBrace), space()];
        for (i, param) in params.iter().enumerate() {
            head.push(self.param(param));
            if i + 1 < params.len() {
                head.push(self.tok(Token::Comma));
            } else {
                head.push(self.cur.skip(&Token::Comma));
            }
            head.push(space());
        }
        head.push(self.tok(Token::Arrow));
        // The parser gives the statements behind the arrow the span of
        // the closure; a block written there has its own.
        let statements = match &body.kind {
            ExprKind::Block(stmts) if body.span.start == expr.span.start => Some(stmts),
            _ => None,
        };
        match statements {
            Some(stmts) => {
                let body = self.stmts(stmts);
                if body.is_nil() {
                    head.push(space());
                }
                head.push(body);
                head.push(self.tok(Token::RBrace));
                Doc::concat(head)
            }
            None => {
                let body = self.expr(body, Ctx::top());
                let tail = self.dangling();
                let close = self.tok(Token::RBrace);
                Doc::group(Doc::concat(vec![
                    Doc::concat(head),
                    Doc::nest(Doc::concat(vec![Doc::Line, body, tail])),
                    Doc::Line,
                    close,
                ]))
            }
        }
    }

    fn arm(&mut self, arm: &MatchArm, guardless: bool) -> Doc {
        let mut docs = Vec::new();
        if guardless {
            // `match { condition -> ..., _ -> ... }`.
            docs.push(match &arm.guard {
                Some(condition) => self.expr(condition, Ctx::top()),
                None => self.name(),
            });
        } else {
            docs.push(self.pattern(&arm.pattern));
            if let Some(guard) = &arm.guard {
                docs.push(space());
                docs.push(self.tok(Token::When));
                docs.push(space());
                docs.push(self.expr(guard, Ctx::top()));
            }
        }
        docs.push(space());
        docs.push(self.tok(Token::Arrow));
        docs.push(self.after(&arm.body));
        // Arms are separated by line breaks; a comma is not written.
        docs.push(self.cur.skip(&Token::Comma));
        Doc::concat(docs)
    }

    // ── Patterns ─────────────────────────────────────────────────────

    fn pattern(&mut self, pattern: &Pattern) -> Doc {
        // Parentheses around a pattern group nothing but the
        // alternatives of an or-pattern, which are one flat list.
        if !matches!(pattern.kind, PatternKind::Tuple(_) | PatternKind::Or(_)) {
            // As for an expression: parentheses with a comment at their
            // inside stay.
            // (Parentheses that hold a `|` are around the alternatives
            // of an or-pattern this pattern is one of.)
            if self.cur.comments_inside_paren() && !self.cur.paren_holds(&Token::Bar) {
                let open = self.tok(Token::LParen);
                let inner = self.pattern(pattern);
                let tail = self.dangling();
                let close = self.tok(Token::RParen);
                return parenthesized(open, Doc::concat(vec![inner, tail]), close);
            }
            self.cur.skip_open_parens();
        }
        let doc = self.bare_pattern(pattern);
        self.cur.close_skipped_parens();
        doc
    }

    /// `-1`, `1..5`, `-1.5..-0.5`: the numbers as the source spells
    /// them.
    fn number_pattern(&mut self, number: Token, range: bool) -> Doc {
        let mut docs = Vec::new();
        let signed = |p: &mut Self, docs: &mut Vec<Doc>| {
            if p.cur.at(&Token::Minus) {
                docs.push(p.tok(Token::Minus));
            }
            docs.push(p.tok(number.clone()));
        };
        signed(self, &mut docs);
        if range {
            docs.push(self.tok(Token::DotDot));
            signed(self, &mut docs);
        }
        Doc::concat(docs)
    }

    fn field_patterns(
        &mut self,
        fields: &[(intern::Symbol, crate::source::Span, Option<Pattern>)],
        rest: Option<Token>,
        named_rest: bool,
    ) -> Doc {
        // The rest is the last item of the list.
        let mut items: Vec<Option<&Option<Pattern>>> =
            fields.iter().map(|(_, _, sub)| Some(sub)).collect();
        if rest.is_some() {
            items.push(None);
        }
        self.delimited(
            Token::LBrace,
            Token::RBrace,
            SPACED,
            &items,
            |p, item| match (item, &rest) {
                (Some(sub), _) => {
                    let name = p.name();
                    match sub {
                        Some(sub) => {
                            Doc::concat(vec![name, p.tok(Token::Colon), space(), p.pattern(sub)])
                        }
                        None => name,
                    }
                }
                (None, Some(rest)) => {
                    let dots = p.tok(rest.clone());
                    if named_rest {
                        Doc::concat(vec![dots, p.name()])
                    } else {
                        dots
                    }
                }
                (None, None) => Doc::Nil,
            },
        )
    }

    fn bare_pattern(&mut self, pattern: &Pattern) -> Doc {
        match &pattern.kind {
            PatternKind::Wildcard | PatternKind::Ident(_) => self.name(),
            PatternKind::Int(_) => self.number_pattern(Token::Int(0), false),
            PatternKind::Float(_) => self.number_pattern(Token::Float(0.0), false),
            PatternKind::Range(..) => self.number_pattern(Token::Int(0), true),
            PatternKind::FloatRange(..) => self.number_pattern(Token::Float(0.0), true),
            PatternKind::Bool(_) => self.tok(Token::Bool(true)),
            PatternKind::StringLit(..) => self.tok(Token::StringLit(String::new(), false)),
            PatternKind::Tuple(elems) => self.tuple(elems, |p, elem| p.pattern(elem)),
            PatternKind::Constructor {
                qualifier, args, ..
            } => {
                let mut docs = Vec::new();
                for _ in qualifier {
                    docs.push(self.name());
                    docs.push(self.tok(Token::Dot));
                }
                docs.push(self.name());
                if args.is_empty() {
                    self.cur.skip_empty_parens();
                } else {
                    docs.push(self.delimited(
                        Token::LParen,
                        Token::RParen,
                        TIGHT,
                        args,
                        |p, arg| p.pattern(arg),
                    ));
                }
                Doc::concat(docs)
            }
            PatternKind::Record {
                module,
                fields,
                has_rest,
                ..
            } => {
                let name = self.qualified(module.is_some());
                let rest = has_rest.then_some(Token::DotDot);
                let fields = self.field_patterns(fields, rest, false);
                Doc::concat(vec![name, space(), fields])
            }
            PatternKind::AnonRecord { fields, rest } => {
                let rest = rest.is_some().then_some(Token::DotDotDot);
                self.field_patterns(fields, rest, true)
            }
            PatternKind::List(elems, rest) => {
                let rest = rest.as_deref();
                let mut items: Vec<Option<&Pattern>> = elems.iter().map(Some).collect();
                if rest.is_some() {
                    items.push(None);
                }
                self.delimited(
                    Token::LBracket,
                    Token::RBracket,
                    TIGHT,
                    &items,
                    |p, item| match (item, rest) {
                        (Some(elem), _) => p.pattern(elem),
                        (None, Some(rest)) => {
                            Doc::concat(vec![p.tok(Token::DotDot), p.pattern(rest)])
                        }
                        (None, None) => Doc::Nil,
                    },
                )
            }
            PatternKind::Or(alts) => {
                // Alternatives in parentheses are the same flat list.
                fn flat<'p>(alts: &'p [Pattern], out: &mut Vec<&'p Pattern>) {
                    for alt in alts {
                        match &alt.kind {
                            PatternKind::Or(inner) => flat(inner, out),
                            _ => out.push(alt),
                        }
                    }
                }
                let mut all = Vec::new();
                flat(alts, &mut all);
                let mut docs = Vec::new();
                let mut rest = Vec::new();
                for (i, alt) in all.into_iter().enumerate() {
                    if i == 0 {
                        docs.push(self.pattern(alt));
                    } else {
                        // A line may break behind `|`, not in front.
                        rest.extend([space(), self.tok(Token::Bar), Doc::Line, self.pattern(alt)]);
                    }
                }
                docs.push(Doc::nest(Doc::concat(rest)));
                Doc::group(Doc::concat(docs))
            }
            PatternKind::Map(entries) => self.delimited(
                Token::HashBrace,
                Token::RBrace,
                SPACED,
                entries,
                |p, (_, sub)| {
                    Doc::concat(vec![
                        p.tok(Token::StringLit(String::new(), false)),
                        p.tok(Token::Colon),
                        space(),
                        p.pattern(sub),
                    ])
                },
            ),
            PatternKind::Pin(_) => Doc::concat(vec![self.tok(Token::Caret), self.name()]),
        }
    }

    // ── Types ────────────────────────────────────────────────────────

    fn type_expr(&mut self, ty: &TypeExpr) -> Doc {
        match &ty.kind {
            TypeExprKind::Named { module, .. } => self.qualified(module.is_some()),
            TypeExprKind::SelfType => self.name(),
            TypeExprKind::Generic { module, args, .. } => {
                let name = self.qualified(module.is_some());
                let args = self.delimited(Token::LParen, Token::RParen, TIGHT, args, |p, arg| {
                    p.type_expr(arg)
                });
                Doc::concat(vec![name, args])
            }
            // `(T)` is a tuple of one: no parentheses are redundant in
            // a type.
            TypeExprKind::Tuple(elems) => {
                self.delimited(Token::LParen, Token::RParen, TIGHT, elems, |p, elem| {
                    p.type_expr(elem)
                })
            }
            TypeExprKind::Function(params, ret) => {
                // `Fn` is an identifier to the lexer.
                let name = self.name();
                let params =
                    self.delimited(Token::LParen, Token::RParen, TIGHT, params, |p, param| {
                        p.type_expr(param)
                    });
                Doc::concat(vec![
                    name,
                    params,
                    space(),
                    self.tok(Token::Arrow),
                    space(),
                    self.type_expr(ret),
                ])
            }
            TypeExprKind::AssocProj {
                receiver,
                trait_module,
                ..
            } => {
                if self.cur.at(&Token::Lt) {
                    // `<T as Trait>::Item`.
                    Doc::concat(vec![
                        self.tok(Token::Lt),
                        self.type_expr(receiver),
                        space(),
                        self.tok(Token::As),
                        space(),
                        self.qualified(trait_module.is_some()),
                        self.tok(Token::Gt),
                        self.tok(Token::ColonColon),
                        self.name(),
                    ])
                } else {
                    // `Self::Item`.
                    Doc::concat(vec![self.name(), self.tok(Token::ColonColon), self.name()])
                }
            }
            TypeExprKind::AnonRecord { fields, tail } => {
                let mut items: Vec<Option<&TypeExpr>> =
                    fields.iter().map(|(_, ty)| Some(ty)).collect();
                if tail.is_some() {
                    items.push(None);
                }
                self.delimited(
                    Token::LBrace,
                    Token::RBrace,
                    SPACED,
                    &items,
                    |p, item| match item {
                        Some(ty) => Doc::concat(vec![
                            p.name(),
                            p.tok(Token::Colon),
                            space(),
                            p.type_expr(ty),
                        ]),
                        None => Doc::concat(vec![p.tok(Token::DotDotDot), p.name()]),
                    },
                )
            }
        }
    }
}

fn decl_start(decl: &Decl) -> u32 {
    match decl {
        Decl::Fn(f) => f.span.start,
        Decl::Type(t) => t.span.start,
        Decl::Trait(t) => t.span.start,
        Decl::TraitImpl(t) => t.span.start,
        Decl::Import(_, span) | Decl::Let { span, .. } => span.start,
    }
}

/// What imports are sorted by: the import as it is written.
fn import_key(target: &ImportTarget) -> String {
    match target {
        ImportTarget::Module(module) => intern::resolve(*module),
        ImportTarget::Items(module, items) => {
            let items: Vec<String> = items
                .iter()
                .map(|(item, _)| intern::resolve(*item))
                .collect();
            format!("{}.{{ {} }}", intern::resolve(*module), items.join(", "))
        }
        ImportTarget::Alias(module, alias, _) => {
            format!(
                "{} as {}",
                intern::resolve(*module),
                intern::resolve(*alias)
            )
        }
    }
}
