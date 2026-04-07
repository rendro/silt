use crate::ast::*;
use crate::lexer::{Span, SpannedToken, Token};
use std::fmt;

// ── Error type ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.span, self.message)
    }
}

type Result<T> = std::result::Result<T, ParseError>;

// ── Parser ───────────────────────────────────────────────────────────

const MAX_DEPTH: usize = 256;

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    in_match_scrutinee: bool,
    errors: Vec<ParseError>,
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self {
            tokens,
            pos: 0,
            in_match_scrutinee: false,
            errors: Vec::new(),
            depth: 0,
        }
    }

    // ── helpers ──────────────────────────────────────────────────────

    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|(_, s)| *s)
            .unwrap_or(Span::new(0, 0))
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].0
    }

    fn at(&self, tok: &Token) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(tok)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek(), Token::Newline)
    }

    fn advance(&mut self) -> SpannedToken {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn skip_nl(&mut self) {
        while self.at_newline() {
            self.pos += 1;
        }
    }

    /// Returns true if there is a newline token right at self.pos
    /// (i.e., between the previous real token and the next real token).
    fn has_newline_before(&self) -> bool {
        matches!(self.tokens.get(self.pos), Some((Token::Newline, _)))
    }

    fn expect(&mut self, expected: &Token) -> Result<SpannedToken> {
        self.skip_nl();
        if self.at(expected) {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: format!("expected {expected}, found {}", self.peek()),
                span: self.span(),
            })
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span)> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Ident(name) => {
                let span = self.span();
                self.advance();
                Ok((name, span))
            }
            _ => Err(ParseError {
                message: format!("expected identifier, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    fn save(&self) -> usize {
        self.pos
    }

    fn restore(&mut self, pos: usize) {
        self.pos = pos;
    }

    // ── Program ──────────────────────────────────────────────────────

    pub fn parse_program(&mut self) -> Result<Program> {
        let mut decls = Vec::new();
        self.skip_nl();
        while !self.at(&Token::Eof) {
            decls.push(self.parse_decl()?);
            self.skip_nl();
        }
        Ok(Program { decls })
    }

    /// Like `parse_program`, but recovers from errors and continues parsing.
    /// Returns the (possibly partial) program and all collected parse errors.
    pub fn parse_program_recovering(&mut self) -> (Program, Vec<ParseError>) {
        let mut decls = Vec::new();
        self.skip_nl();
        while !self.at(&Token::Eof) {
            match self.parse_decl() {
                Ok(decl) => decls.push(decl),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                }
            }
            self.skip_nl();
        }
        (Program { decls }, std::mem::take(&mut self.errors))
    }

    /// Skip tokens until we find one that could start a new declaration.
    fn synchronize(&mut self) {
        loop {
            match self.peek() {
                Token::Fn
                | Token::Type
                | Token::Trait
                | Token::Pub
                | Token::Import
                | Token::Let
                | Token::Eof => break,
                _ => {
                    self.advance();
                }
            }
        }
    }

    // ── Declarations ─────────────────────────────────────────────────

    fn parse_decl(&mut self) -> Result<Decl> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Pub => {
                let span = self.span();
                self.advance();
                self.skip_nl();
                match self.peek() {
                    Token::Fn => {
                        let mut f = self.parse_fn_decl()?;
                        f.is_pub = true;
                        f.span = span;
                        Ok(Decl::Fn(f))
                    }
                    Token::Type => {
                        let mut t = self.parse_type_decl()?;
                        t.is_pub = true;
                        t.span = span;
                        Ok(Decl::Type(t))
                    }
                    Token::Let => {
                        let decl = self.parse_let_decl()?;
                        match decl {
                            Decl::Let {
                                pattern, ty, value, ..
                            } => Ok(Decl::Let {
                                pattern,
                                ty,
                                value,
                                is_pub: true,
                                span,
                            }),
                            _ => unreachable!(),
                        }
                    }
                    _ => Err(ParseError {
                        message: "expected fn, type, or let after pub".into(),
                        span: self.span(),
                    }),
                }
            }
            Token::Fn => Ok(Decl::Fn(self.parse_fn_decl()?)),
            Token::Type => Ok(Decl::Type(self.parse_type_decl()?)),
            Token::Trait => self.parse_trait_or_impl(),
            Token::Import => self.parse_import(),
            Token::Let => self.parse_let_decl(),
            _ => Err(ParseError {
                message: format!("expected declaration, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    fn parse_fn_decl(&mut self) -> Result<FnDecl> {
        let span = self.span();
        self.expect(&Token::Fn)?;
        let (name, _) = self.expect_ident()?;
        let params = self.parse_fn_params()?;

        let return_type = if self.peek_skip_nl() == &Token::Arrow {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let where_clauses = if self.peek_skip_nl() == &Token::Where {
            self.advance(); // consume 'where'
            let mut clauses = Vec::new();
            loop {
                self.skip_nl();
                let (type_param, _) = self.expect_ident()?;
                self.expect(&Token::Colon)?;
                let (trait_name, _) = self.expect_ident()?;
                clauses.push((type_param, trait_name));
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            clauses
        } else {
            Vec::new()
        };

        self.skip_nl();
        let body = if self.at(&Token::Eq) {
            // Single-expression form: fn square(x) = x * x
            self.advance();
            self.skip_nl();
            self.parse_expr()?
        } else if self.at(&Token::LBrace) {
            self.parse_block()?
        } else {
            // Abstract method — no body (e.g. trait method declarations)
            Expr::new(ExprKind::Unit, span)
        };

        Ok(FnDecl {
            name,
            params,
            return_type,
            where_clauses,
            body,
            is_pub: false,
            span,
        })
    }

    fn parse_fn_params(&mut self) -> Result<Vec<Param>> {
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        self.skip_nl();
        while !self.at(&Token::RParen) {
            let pattern = self.parse_simple_param_pattern()?;
            let ty = if self.peek_skip_nl() == &Token::Colon {
                self.advance();
                Some(self.parse_type_expr()?)
            } else {
                None
            };
            params.push(Param { pattern, ty });
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
                self.skip_nl();
            }
        }
        self.expect(&Token::RParen)?;
        Ok(params)
    }

    fn parse_simple_param_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(Pattern::Ident(name))
            }
            _ => Err(ParseError {
                message: format!("expected parameter name, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    fn parse_type_decl(&mut self) -> Result<TypeDecl> {
        let span = self.span();
        self.expect(&Token::Type)?;
        let (name, _) = self.expect_ident()?;

        // Optional type parameters: type Result(a, e) { ... }
        let params = if self.peek_skip_nl() == &Token::LParen {
            self.advance();
            let mut ps = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                let (p, _) = self.expect_ident()?;
                ps.push(p);
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    self.skip_nl();
                }
            }
            self.expect(&Token::RParen)?;
            ps
        } else {
            Vec::new()
        };

        self.expect(&Token::LBrace)?;
        self.skip_nl();

        // Determine if this is an enum or record by peeking at the first field.
        // Record fields look like `name: Type`, enum variants look like `Name` or `Name(Type)`.
        let body = if self.is_record_body() {
            self.parse_record_body()?
        } else {
            self.parse_enum_body()?
        };

        self.skip_nl();
        self.expect(&Token::RBrace)?;

        Ok(TypeDecl {
            name,
            params,
            body,
            is_pub: false,
            span,
        })
    }

    fn is_record_body(&self) -> bool {
        // Look ahead: if we see `ident :` it's a record. If we see `Ident(` or `Ident,` or `Ident }` it's enum.
        // Record field names start lowercase, enum variant names start uppercase.
        let mut i = self.pos;
        // skip newlines
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        if let Token::Ident(ref name) = self.tokens[i].0 {
            // lowercase first char → likely record field
            name.starts_with(|c: char| c.is_lowercase())
        } else {
            false
        }
    }

    fn parse_record_body(&mut self) -> Result<TypeBody> {
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) {
            self.skip_nl();
            if self.at(&Token::RBrace) {
                break;
            }
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.parse_type_expr()?;
            fields.push(RecordField { name, ty });
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            }
            self.skip_nl();
        }
        Ok(TypeBody::Record(fields))
    }

    fn parse_enum_body(&mut self) -> Result<TypeBody> {
        let mut variants = Vec::new();
        while !self.at(&Token::RBrace) {
            self.skip_nl();
            if self.at(&Token::RBrace) {
                break;
            }
            let (name, _) = self.expect_ident()?;
            let fields = if self.peek() == &Token::LParen {
                self.advance();
                let mut fs = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RParen) {
                    fs.push(self.parse_type_expr()?);
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                    }
                }
                self.expect(&Token::RParen)?;
                fs
            } else {
                Vec::new()
            };
            variants.push(EnumVariant { name, fields });
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            }
            self.skip_nl();
        }
        Ok(TypeBody::Enum(variants))
    }

    fn parse_trait_or_impl(&mut self) -> Result<Decl> {
        let span = self.span();
        self.expect(&Token::Trait)?;
        let (name, _) = self.expect_ident()?;

        self.skip_nl();
        // `trait Display for User { ... }` is an impl
        // `trait Display { ... }` is a declaration
        if self.at(&Token::Fn) || self.at(&Token::LBrace) {
            // trait declaration
            if self.at(&Token::LBrace) {
                self.advance();
            }
            let mut methods = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                methods.push(self.parse_fn_decl()?);
                self.skip_nl();
            }
            self.expect(&Token::RBrace)?;
            Ok(Decl::Trait(TraitDecl {
                name,
                methods,
                span,
            }))
        } else {
            // Must be `for Type { ... }`
            self.expect(&Token::Ident("for".into()))?;
            let (target, _) = self.expect_ident()?;
            self.expect(&Token::LBrace)?;
            let mut methods = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                methods.push(self.parse_fn_decl()?);
                self.skip_nl();
            }
            self.expect(&Token::RBrace)?;
            Ok(Decl::TraitImpl(TraitImpl {
                trait_name: name,
                target_type: target,
                methods,
                span,
            }))
        }
    }

    fn parse_import(&mut self) -> Result<Decl> {
        self.expect(&Token::Import)?;
        let (name, _) = self.expect_ident()?;

        self.skip_nl();
        if self.at(&Token::Dot) {
            self.advance();
            self.expect(&Token::LBrace)?;
            let mut items = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                let (item, _) = self.expect_ident()?;
                items.push(item);
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    self.skip_nl();
                }
            }
            self.expect(&Token::RBrace)?;
            Ok(Decl::Import(ImportTarget::Items(name, items)))
        } else if self.at(&Token::As) {
            self.advance();
            let (alias, _) = self.expect_ident()?;
            Ok(Decl::Import(ImportTarget::Alias(name, alias)))
        } else {
            Ok(Decl::Import(ImportTarget::Module(name)))
        }
    }

    // ── Type expressions ─────────────────────────────────────────────

    fn parse_type_expr(&mut self) -> Result<TypeExpr> {
        self.skip_nl();
        // Function type: Fn(A, B) -> C
        if matches!(self.peek(), Token::Ident(s) if s == "Fn") {
            self.advance();
            self.expect(&Token::LParen)?;
            let mut params = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                params.push(self.parse_type_expr()?);
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    self.skip_nl();
                }
            }
            self.expect(&Token::RParen)?;
            self.expect(&Token::Arrow)?;
            let ret = self.parse_type_expr()?;
            return Ok(TypeExpr::Function(params, Box::new(ret)));
        }
        // Tuple type: (A, B, ...)
        if self.at(&Token::LParen) {
            self.advance();
            let mut elems = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                elems.push(self.parse_type_expr()?);
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    self.skip_nl();
                }
            }
            self.expect(&Token::RParen)?;
            return Ok(TypeExpr::Tuple(elems));
        }
        let (name, _) = self.expect_ident()?;
        if self.peek() == &Token::LParen {
            self.advance();
            let mut args = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                args.push(self.parse_type_expr()?);
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    self.skip_nl();
                }
            }
            self.expect(&Token::RParen)?;
            Ok(TypeExpr::Generic(name, args))
        } else {
            Ok(TypeExpr::Named(name))
        }
    }

    // ── Block & statements ───────────────────────────────────────────

    fn parse_block(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        let stmts = self.parse_stmt_list(&Token::RBrace)?;
        self.expect(&Token::RBrace)?;
        Ok(Expr::new(ExprKind::Block(stmts), span))
    }

    fn parse_stmt_list(&mut self, terminator: &Token) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        self.skip_nl();
        while !self.at(terminator) && !self.at(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_nl();
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Let => self.parse_let_stmt(),
            Token::When => self.parse_when_stmt(),
            _ => {
                let expr = self.parse_expr()?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_let_stmt(&mut self) -> Result<Stmt> {
        self.expect(&Token::Let)?;
        let pattern = self.parse_pattern()?;
        let ty = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        self.skip_nl();
        let value = self.parse_expr()?;
        Ok(Stmt::Let { pattern, ty, value })
    }

    fn parse_let_decl(&mut self) -> Result<Decl> {
        let span = self.span();
        self.expect(&Token::Let)?;
        let pattern = self.parse_pattern()?;
        let ty = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        self.skip_nl();
        let value = self.parse_expr()?;
        Ok(Decl::Let {
            pattern,
            ty,
            value,
            is_pub: false,
            span,
        })
    }

    fn parse_when_stmt(&mut self) -> Result<Stmt> {
        self.expect(&Token::When)?;

        // Try pattern form: when <pattern> = <expr> else { <block> }
        // If parse_pattern succeeds and is followed by `=`, it's the pattern form.
        // Otherwise, backtrack and parse as boolean form: when <expr> else { <block> }
        let saved = self.save();
        if let Ok(pattern) = self.parse_pattern()
            && self.at(&Token::Eq)
        {
            self.advance(); // consume `=`
            self.skip_nl();
            let expr = self.parse_expr()?;
            self.expect(&Token::Else)?;
            let else_body = self.parse_block()?;
            return Ok(Stmt::When {
                pattern,
                expr,
                else_body,
            });
        }

        // Boolean form: when <expr> else { <block> }
        self.restore(saved);
        let condition = self.parse_expr()?;
        self.expect(&Token::Else)?;
        let else_body = self.parse_block()?;
        Ok(Stmt::WhenBool {
            condition,
            else_body,
        })
    }

    // ── Expressions (Pratt parser) ───────────────────────────────────

    pub fn parse_expr(&mut self) -> Result<Expr> {
        self.skip_nl();
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(ParseError {
                message: "expression nesting exceeds maximum depth".into(),
                span: self.span(),
            });
        }
        let result = self.parse_expr_bp_inner(min_bp);
        self.depth -= 1;
        result
    }

    fn parse_expr_bp_inner(&mut self, min_bp: u8) -> Result<Expr> {
        let mut left = self.parse_unary()?;

        loop {
            // First, try postfix operators — newline-sensitive.
            // If a newline precedes the token, don't treat it as postfix.
            if !self.has_newline_before() {
                match self.peek() {
                    Token::Question => {
                        let bp = 110;
                        if bp < min_bp {
                            break;
                        }
                        let span = left.span;
                        self.advance();
                        left = Expr::new(ExprKind::QuestionMark(Box::new(left)), span);
                        continue;
                    }
                    Token::LParen => {
                        let bp = 120;
                        if bp < min_bp {
                            break;
                        }
                        left = self.parse_call_expr(left)?;
                        continue;
                    }
                    Token::LBracket => {
                        let bp = 120;
                        if bp < min_bp {
                            break;
                        }
                        left = self.parse_index_expr(left)?;
                        continue;
                    }
                    Token::LBrace if self.is_trailing_closure() => {
                        let bp = 115; // lower than call (120) so match scrutinee can suppress it
                        if bp < min_bp {
                            break;
                        }
                        let closure = self.parse_trailing_closure()?;
                        // Append the closure to the call or wrap ident in a call
                        left = self.attach_trailing_closure(left, closure);
                        continue;
                    }
                    _ => {}
                }
            }

            // Save position, skip newlines, try infix operators.
            let saved = self.save();
            let had_newline = self.has_newline_before();
            self.skip_nl();

            match self.peek() {
                // Field access / record update (always allowed across newlines)
                Token::Dot => {
                    let bp = 130;
                    if bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    if self.at(&Token::LBrace) {
                        // Record update: expr.{ field: value }
                        let span = left.span;
                        self.advance(); // {
                        let fields = self.parse_record_fields()?;
                        self.expect(&Token::RBrace)?;
                        left = Expr::new(
                            ExprKind::RecordUpdate {
                                expr: Box::new(left),
                                fields,
                            },
                            span,
                        );
                    } else if let Token::Int(n) = self.peek() {
                        // Tuple index access: expr.0, expr.1, etc.
                        let field = n.to_string();
                        self.advance();
                        let span = left.span;
                        left = Expr::new(ExprKind::FieldAccess(Box::new(left), field), span);
                    } else {
                        let (field, _) = self.expect_ident()?;
                        let span = left.span;
                        left = Expr::new(ExprKind::FieldAccess(Box::new(left), field), span);
                    }
                    continue;
                }

                // Pipe operator — binds tighter than comparison/boolean operators
                // so `x |> f() == y` parses as `(x |> f()) == y`,
                // but looser than range so `1..10 |> f()` parses as `(1..10) |> f()`
                Token::Pipe => {
                    let (l_bp, r_bp) = (55, 56);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    // Allow trailing closures in the pipe RHS even inside a
                    // match scrutinee.  The match-body `{` appears *after*
                    // the entire pipe expression, not inside the RHS, so it
                    // is safe to re-enable trailing closures here.  Example:
                    //   match items |> list.any { x -> x > 5 } { true -> … }
                    //                           ^^^^^^^^^^^^^^^  <- trailing closure
                    //                                           ^^^^^^^^^^^^^^^^ <- match body
                    let prev_match = self.in_match_scrutinee;
                    self.in_match_scrutinee = false;
                    let right = self.parse_expr_bp(r_bp)?;
                    self.in_match_scrutinee = prev_match;
                    let span = left.span;
                    left = Expr::new(ExprKind::Pipe(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Range — binds tighter than pipe so `1..10 |> f()` works
                Token::DotDot => {
                    let (l_bp, r_bp) = (60, 61);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Range(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Binary operators
                Token::OrOr => {
                    let (l_bp, r_bp) = (20, 21);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(
                        ExprKind::Binary(Box::new(left), BinOp::Or, Box::new(right)),
                        span,
                    );
                    continue;
                }
                Token::AndAnd => {
                    let (l_bp, r_bp) = (30, 31);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(
                        ExprKind::Binary(Box::new(left), BinOp::And, Box::new(right)),
                        span,
                    );
                    continue;
                }
                Token::EqEq | Token::NotEq => {
                    let op = if self.peek() == &Token::EqEq {
                        BinOp::Eq
                    } else {
                        BinOp::Neq
                    };
                    let (l_bp, r_bp) = (40, 41);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Lt | Token::Gt | Token::LtEq | Token::GtEq => {
                    let op = match self.peek() {
                        Token::Lt => BinOp::Lt,
                        Token::Gt => BinOp::Gt,
                        Token::LtEq => BinOp::Leq,
                        Token::GtEq => BinOp::Geq,
                        _ => unreachable!(),
                    };
                    let (l_bp, r_bp) = (50, 51);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Plus | Token::Minus if !had_newline => {
                    // + and - are newline-sensitive: they are ambiguous with
                    // unary +/- at the start of the next statement, so a
                    // newline before them terminates the current expression.
                    let op = if self.peek() == &Token::Plus {
                        BinOp::Add
                    } else {
                        BinOp::Sub
                    };
                    let (l_bp, r_bp) = (70, 71);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Star | Token::Slash | Token::Percent => {
                    let op = match self.peek() {
                        Token::Star => BinOp::Mul,
                        Token::Slash => BinOp::Div,
                        Token::Percent => BinOp::Mod,
                        _ => unreachable!(),
                    };
                    let (l_bp, r_bp) = (80, 81);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }

                _ => {
                    self.restore(saved);
                    break;
                }
            }
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_nl();
        match self.peek() {
            Token::Minus => {
                let span = self.span();
                self.advance();
                let expr = self.parse_expr_bp(90)?;
                Ok(Expr::new(
                    ExprKind::Unary(UnaryOp::Neg, Box::new(expr)),
                    span,
                ))
            }
            Token::Not => {
                let span = self.span();
                self.advance();
                let expr = self.parse_expr_bp(90)?;
                Ok(Expr::new(
                    ExprKind::Unary(UnaryOp::Not, Box::new(expr)),
                    span,
                ))
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        self.skip_nl();
        let span = self.span();

        match self.peek().clone() {
            Token::Int(n) => {
                self.advance();
                Ok(Expr::new(ExprKind::Int(n), span))
            }
            Token::Float(n) => {
                self.advance();
                Ok(Expr::new(ExprKind::Float(n), span))
            }
            Token::Bool(b) => {
                self.advance();
                Ok(Expr::new(ExprKind::Bool(b), span))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(Expr::new(ExprKind::StringLit(s), span))
            }
            Token::StringStart(s) => {
                self.advance();
                self.parse_string_interp(s, span)
            }
            Token::Ident(ref name) if is_constructor(name) => {
                let name = name.clone();
                self.advance();
                // Could be: Constructor, Constructor(args), or RecordCreate { fields }
                if !self.has_newline_before() && self.at(&Token::LParen) {
                    let args = self.parse_call_args()?;
                    Ok(Expr::new(
                        ExprKind::Call(Box::new(Expr::new(ExprKind::Ident(name), span)), args),
                        span,
                    ))
                } else if !self.has_newline_before()
                    && self.at(&Token::LBrace)
                    && !self.in_match_scrutinee
                    && !self.is_trailing_closure()
                {
                    // Record creation: User { name: "Alice", ... }
                    self.advance(); // {
                    let fields = self.parse_record_fields()?;
                    self.expect(&Token::RBrace)?;
                    Ok(Expr::new(ExprKind::RecordCreate { name, fields }, span))
                } else {
                    Ok(Expr::new(ExprKind::Ident(name), span))
                }
            }
            Token::Ident(name) => {
                self.advance();
                Ok(Expr::new(ExprKind::Ident(name), span))
            }
            Token::LParen => {
                self.advance();
                self.skip_nl();
                // Unit: ()
                if self.at(&Token::RParen) {
                    self.advance();
                    return Ok(Expr::new(ExprKind::Unit, span));
                }
                // Parse first expression
                let first = self.parse_expr()?;
                self.skip_nl();
                if self.at(&Token::Comma) {
                    // Tuple: (a, b, c)
                    self.advance();
                    let mut elems = vec![first];
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        elems.push(self.parse_expr()?);
                        self.skip_nl();
                        if self.at(&Token::Comma) {
                            self.advance();
                            self.skip_nl();
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr::new(ExprKind::Tuple(elems), span))
                } else {
                    // Parenthesized expression
                    self.expect(&Token::RParen)?;
                    Ok(first)
                }
            }
            Token::LBracket => {
                self.advance();
                let mut elems = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBracket) {
                    if self.at(&Token::DotDot) {
                        self.advance();
                        elems.push(ListElem::Spread(self.parse_expr()?));
                    } else {
                        elems.push(ListElem::Single(self.parse_expr()?));
                    }
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(Expr::new(ExprKind::List(elems), span))
            }
            Token::HashBrace => {
                self.advance();
                let mut pairs = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBrace) {
                    let key = self.parse_expr()?;
                    self.expect(&Token::Colon)?;
                    self.skip_nl();
                    let value = self.parse_expr()?;
                    pairs.push((key, value));
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(Expr::new(ExprKind::Map(pairs), span))
            }
            Token::HashBracket => {
                self.advance();
                let mut elems = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBracket) {
                    elems.push(self.parse_expr()?);
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(Expr::new(ExprKind::SetLit(elems), span))
            }
            Token::LBrace => {
                // Could be a trailing closure or a block.
                if self.is_trailing_closure() {
                    self.parse_trailing_closure_as_lambda()
                } else {
                    self.parse_block()
                }
            }
            Token::Match => self.parse_match_expr(),
            Token::Loop => self.parse_loop_expr(),
            Token::Fn => self.parse_fn_expr(),
            Token::Return => {
                self.advance();
                // Return may or may not have a value
                if self.has_newline_before() || self.at(&Token::RBrace) || self.at(&Token::Eof) {
                    Ok(Expr::new(ExprKind::Return(None), span))
                } else {
                    let val = self.parse_expr()?;
                    Ok(Expr::new(ExprKind::Return(Some(Box::new(val))), span))
                }
            }
            // select is no longer a keyword; use channel.select([...])
            _ => Err(ParseError {
                message: format!("expected expression, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    // ── String interpolation ─────────────────────────────────────────

    fn parse_string_interp(&mut self, start_text: String, span: Span) -> Result<Expr> {
        let mut parts = Vec::new();
        if !start_text.is_empty() {
            parts.push(StringPart::Literal(start_text));
        }

        // Parse expression inside interpolation
        let expr = self.parse_expr()?;
        parts.push(StringPart::Expr(expr));

        // Now we should see StringMiddle or StringEnd
        loop {
            match self.peek().clone() {
                Token::StringMiddle(text) => {
                    self.advance();
                    if !text.is_empty() {
                        parts.push(StringPart::Literal(text));
                    }
                    let expr = self.parse_expr()?;
                    parts.push(StringPart::Expr(expr));
                }
                Token::StringEnd(text) => {
                    self.advance();
                    if !text.is_empty() {
                        parts.push(StringPart::Literal(text));
                    }
                    break;
                }
                _ => {
                    return Err(ParseError {
                        message: format!("expected string continuation, found {}", self.peek()),
                        span: self.span(),
                    });
                }
            }
        }

        Ok(Expr::new(ExprKind::StringInterp(parts), span))
    }

    // ── Function calls ───────────────────────────────────────────────

    fn parse_call_expr(&mut self, callee: Expr) -> Result<Expr> {
        let span = callee.span;
        let args = self.parse_call_args()?;
        // Trailing closures are handled by the postfix loop in parse_expr_bp,
        // which respects min_bp and correctly suppresses them in match scrutinees.
        Ok(Expr::new(ExprKind::Call(Box::new(callee), args), span))
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>> {
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        self.skip_nl();
        while !self.at(&Token::RParen) {
            args.push(self.parse_expr()?);
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
                self.skip_nl();
            }
        }
        self.expect(&Token::RParen)?;
        Ok(args)
    }

    fn parse_index_expr(&mut self, left: Expr) -> Result<Expr> {
        let span = left.span;
        self.expect(&Token::LBracket)?;
        let index = self.parse_expr()?;
        self.expect(&Token::RBracket)?;
        Ok(Expr::new(
            ExprKind::Call(
                Box::new(Expr::new(ExprKind::Ident("__index".into()), span)),
                vec![left, index],
            ),
            span,
        ))
    }

    // ── Trailing closures ────────────────────────────────────────────

    fn is_trailing_closure(&self) -> bool {
        // When parsing a match scrutinee, the `{` is always the match body,
        // never a trailing closure.
        if self.in_match_scrutinee {
            return false;
        }
        // Check if the current `{` starts a trailing closure by looking for `->`.
        if self.peek() != &Token::LBrace {
            return false;
        }
        let mut i = self.pos + 1; // skip `{`
        // Skip leading newlines to find the first real token
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        // If the first real token is a literal, this is a match body
        // (patterns like `0 ->`, `true ->`), not a trailing closure.
        // Note: `_` is NOT excluded here because it is a valid closure
        // parameter name (meaning "ignore this argument"). Match bodies
        // are consumed directly by parse_match_expr via expect(LBrace),
        // so they never reach this heuristic.
        if i < self.tokens.len() {
            match &self.tokens[i].0 {
                Token::Int(_) | Token::Float(_) | Token::Bool(_) => return false,
                _ => {}
            }
        }
        let mut depth = 0;
        while i < self.tokens.len() {
            match &self.tokens[i].0 {
                Token::Arrow if depth == 0 => return true,
                Token::LParen => depth += 1,
                Token::RParen if depth > 0 => depth -= 1,
                Token::Newline | Token::Ident(_) | Token::Comma => {}
                _ => return false,
            }
            i += 1;
        }
        false
    }

    fn parse_trailing_closure(&mut self) -> Result<Expr> {
        self.parse_trailing_closure_as_lambda()
    }

    fn parse_trailing_closure_as_lambda(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        self.skip_nl();
        let params = self.parse_closure_params()?;
        self.expect(&Token::Arrow)?;
        self.skip_nl();

        // Parse body statements
        let stmts = self.parse_stmt_list(&Token::RBrace)?;
        self.expect(&Token::RBrace)?;

        let body = if stmts.len() == 1 {
            if let Stmt::Expr(e) = &stmts[0] {
                e.clone()
            } else {
                Expr::new(ExprKind::Block(stmts), span)
            }
        } else {
            Expr::new(ExprKind::Block(stmts), span)
        };

        Ok(Expr::new(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        ))
    }

    fn parse_closure_params(&mut self) -> Result<Vec<Param>> {
        let mut params = Vec::new();
        loop {
            self.skip_nl();
            match self.peek() {
                Token::Arrow => break,
                Token::LParen => {
                    // Destructuring pattern like (a, b)
                    let pattern = self.parse_pattern()?;
                    params.push(Param { pattern, ty: None });
                }
                Token::Ident(_) => {
                    let pattern = self.parse_pattern()?;
                    params.push(Param { pattern, ty: None });
                }
                _ => break,
            }
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        Ok(params)
    }

    fn attach_trailing_closure(&mut self, callee: Expr, closure: Expr) -> Expr {
        let span = callee.span;
        match callee.kind {
            ExprKind::Call(f, mut args) => {
                args.push(closure);
                Expr::new(ExprKind::Call(f, args), span)
            }
            _ => {
                // Wrap as a call: `f { x -> body }` → f(closure)
                Expr::new(ExprKind::Call(Box::new(callee), vec![closure]), span)
            }
        }
    }

    // ── Match ────────────────────────────────────────────────────────

    fn parse_match_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Match)?;
        self.skip_nl();

        // Guardless match: `match { cond -> body, ... }`
        let guardless = self.at(&Token::LBrace);
        let scrutinee = if guardless {
            None
        } else {
            // Set flag so is_trailing_closure returns false while parsing the
            // scrutinee. This allows comparison/equality/boolean operators (which
            // have lower bp than the old threshold of 116) while still preventing
            // the match body `{` from being consumed as a trailing closure.
            // Save and restore the flag to handle nested match expressions.
            let prev = self.in_match_scrutinee;
            self.in_match_scrutinee = true;
            let expr = self.parse_expr()?;
            self.in_match_scrutinee = prev;
            Some(Box::new(expr))
        };

        self.expect(&Token::LBrace)?;
        self.skip_nl();

        let mut arms = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            arms.push(self.parse_match_arm(guardless)?);
            // Allow optional comma between match arms
            if self.at(&Token::Comma) {
                self.advance();
            }
            self.skip_nl();
        }
        self.expect(&Token::RBrace)?;

        Ok(Expr::new(
            ExprKind::Match {
                expr: scrutinee,
                arms,
            },
            span,
        ))
    }

    fn parse_match_arm(&mut self, guardless: bool) -> Result<MatchArm> {
        self.skip_nl();

        if guardless {
            // Guardless match: each arm's LHS is a boolean expression or `_`
            let is_wildcard = matches!(self.peek(), Token::Ident(name) if name == "_");
            if is_wildcard {
                self.advance();
                self.expect(&Token::Arrow)?;
                self.skip_nl();
                let body = self.parse_expr()?;
                return Ok(MatchArm {
                    pattern: Pattern::Wildcard,
                    guard: None,
                    body,
                });
            }
            let condition = self.parse_expr()?;
            self.expect(&Token::Arrow)?;
            self.skip_nl();
            let body = self.parse_expr()?;
            return Ok(MatchArm {
                pattern: Pattern::Wildcard,
                guard: Some(Box::new(condition)),
                body,
            });
        }

        let pattern = self.parse_pattern()?;

        // Optional guard: `when condition`
        self.skip_nl();
        let guard = if self.at(&Token::When) {
            self.advance();
            self.skip_nl();
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };

        self.expect(&Token::Arrow)?;
        self.skip_nl();
        let body = self.parse_expr()?;

        Ok(MatchArm {
            pattern,
            guard,
            body,
        })
    }

    // ── Loop expression ──────────────────────────────────────────────

    fn parse_loop_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Loop)?;

        // Check for recur: `loop(args)` — LParen immediately (no newline)
        if !self.has_newline_before() && self.at(&Token::LParen) {
            let args = self.parse_call_args()?;
            return Ok(Expr::new(ExprKind::Recur(args), span));
        }

        self.skip_nl();

        // Zero-binding variant: `loop { body }`
        if self.at(&Token::LBrace) {
            let body = self.parse_block()?;
            return Ok(Expr::new(
                ExprKind::Loop {
                    bindings: Vec::new(),
                    body: Box::new(body),
                },
                span,
            ));
        }

        // Binding variant: `loop x = init, y = init { body }`
        let mut bindings = Vec::new();
        loop {
            self.skip_nl();
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Eq)?;
            self.skip_nl();
            let init = self.parse_expr()?;
            bindings.push((name, init));
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        self.skip_nl();
        let body = self.parse_block()?;

        Ok(Expr::new(
            ExprKind::Loop {
                bindings,
                body: Box::new(body),
            },
            span,
        ))
    }

    // ── Fn expression ────────────────────────────────────────────────

    fn parse_fn_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Fn)?;
        let params = self.parse_fn_params()?;
        self.skip_nl();
        let body = self.parse_block()?;
        Ok(Expr::new(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        ))
    }

    // ── Record fields ────────────────────────────────────────────────

    fn parse_record_fields(&mut self) -> Result<Vec<(String, Expr)>> {
        let mut fields = Vec::new();
        self.skip_nl();
        while !self.at(&Token::RBrace) {
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            self.skip_nl();
            let value = self.parse_expr()?;
            fields.push((name, value));
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
                self.skip_nl();
            }
        }
        Ok(fields)
    }

    // ── Patterns ─────────────────────────────────────────────────────

    fn parse_pattern(&mut self) -> Result<Pattern> {
        let first = self.parse_primary_pattern()?;
        // Check for or-pattern: pat1 | pat2 | ...
        if self.at(&Token::Bar) {
            let mut alts = vec![first];
            while self.at(&Token::Bar) {
                self.advance();
                alts.push(self.parse_primary_pattern()?);
            }
            Ok(Pattern::Or(alts))
        } else {
            Ok(first)
        }
    }

    fn parse_primary_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Ident(ref name) if name == "_" => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            Token::Ident(ref name) if is_constructor(name) => {
                let name = name.clone();
                self.advance();
                // Constructor pattern: Some(x), Ok(value), Rect(w, h)
                if self.at(&Token::LParen) {
                    self.advance();
                    let mut pats = Vec::new();
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        pats.push(self.parse_pattern()?);
                        self.skip_nl();
                        if self.at(&Token::Comma) {
                            self.advance();
                            self.skip_nl();
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Pattern::Constructor(name, pats))
                } else if self.at(&Token::LBrace) {
                    // Record pattern: User { name, age, .. }
                    self.advance();
                    self.skip_nl();
                    let mut fields = Vec::new();
                    let mut has_rest = false;
                    while !self.at(&Token::RBrace) {
                        self.skip_nl();
                        if self.at(&Token::DotDot) {
                            self.advance();
                            has_rest = true;
                            self.skip_nl();
                            break;
                        }
                        let (field_name, _) = self.expect_ident()?;
                        // Optional sub-pattern: `name: pat`
                        let sub = if self.peek_skip_nl() == &Token::Colon {
                            self.advance();
                            Some(self.parse_pattern()?)
                        } else {
                            None
                        };
                        fields.push((field_name, sub));
                        self.skip_nl();
                        if self.at(&Token::Comma) {
                            self.advance();
                            self.skip_nl();
                        }
                    }
                    self.expect(&Token::RBrace)?;
                    Ok(Pattern::Record {
                        name: Some(name),
                        fields,
                        has_rest,
                    })
                } else {
                    Ok(Pattern::Constructor(name, Vec::new()))
                }
            }
            Token::Ident(name) => {
                self.advance();
                Ok(Pattern::Ident(name))
            }
            Token::Int(n) => {
                self.advance();
                // Check for range pattern: n..m
                if self.at(&Token::DotDot) {
                    self.advance();
                    match self.peek().clone() {
                        Token::Int(m) => {
                            self.advance();
                            Ok(Pattern::Range(n, m))
                        }
                        Token::Minus => {
                            self.advance();
                            match self.peek().clone() {
                                Token::Int(m) => {
                                    self.advance();
                                    Ok(Pattern::Range(n, -m))
                                }
                                _ => Err(ParseError {
                                    message: "expected integer after - in range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        }
                        _ => Err(ParseError {
                            message: "expected integer end for range pattern".into(),
                            span: self.span(),
                        }),
                    }
                } else {
                    Ok(Pattern::Int(n))
                }
            }
            Token::Float(n) => {
                self.advance();
                if self.at(&Token::DotDot) {
                    self.advance();
                    let end = if self.at(&Token::Minus) {
                        self.advance();
                        match self.peek().clone() {
                            Token::Float(m) => {
                                self.advance();
                                -m
                            }
                            _ => {
                                return Err(ParseError {
                                    message: "expected float after - in range pattern".into(),
                                    span: self.span(),
                                });
                            }
                        }
                    } else {
                        match self.peek().clone() {
                            Token::Float(m) => {
                                self.advance();
                                m
                            }
                            _ => {
                                return Err(ParseError {
                                    message: "expected float end for range pattern".into(),
                                    span: self.span(),
                                });
                            }
                        }
                    };
                    Ok(Pattern::FloatRange(n, end))
                } else {
                    Ok(Pattern::Float(n))
                }
            }
            Token::Bool(b) => {
                self.advance();
                Ok(Pattern::Bool(b))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(Pattern::StringLit(s))
            }
            Token::LParen => {
                self.advance();
                self.skip_nl();
                if self.at(&Token::RParen) {
                    self.advance();
                    return Ok(Pattern::Tuple(Vec::new()));
                }
                let first = self.parse_pattern()?;
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    let mut pats = vec![first];
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        pats.push(self.parse_pattern()?);
                        self.skip_nl();
                        if self.at(&Token::Comma) {
                            self.advance();
                            self.skip_nl();
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Pattern::Tuple(pats))
                } else {
                    self.expect(&Token::RParen)?;
                    // Single-element parenthesized pattern
                    Ok(first)
                }
            }
            Token::LBracket => {
                self.advance(); // consume [
                self.skip_nl();
                if self.at(&Token::RBracket) {
                    self.advance();
                    return Ok(Pattern::List(vec![], None)); // empty list pattern
                }
                let mut patterns = Vec::new();
                let mut rest = None;
                loop {
                    self.skip_nl();
                    if self.at(&Token::DotDot) {
                        // ...rest pattern
                        self.advance(); // consume ..
                        let rest_pat = self.parse_pattern()?;
                        rest = Some(Box::new(rest_pat));
                        self.skip_nl();
                        break;
                    }
                    patterns.push(self.parse_pattern()?);
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                        // Check if next is ..rest after comma
                        if self.at(&Token::DotDot) {
                            self.advance();
                            let rest_pat = self.parse_pattern()?;
                            rest = Some(Box::new(rest_pat));
                            self.skip_nl();
                            break;
                        }
                    } else {
                        break;
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(Pattern::List(patterns, rest))
            }
            Token::HashBrace => {
                // Map pattern: #{ "key": pattern, ... }
                self.advance();
                self.skip_nl();
                let mut entries = Vec::new();
                while !self.at(&Token::RBrace) {
                    self.skip_nl();
                    let key = match self.peek().clone() {
                        Token::StringLit(s) => {
                            self.advance();
                            s
                        }
                        _ => {
                            return Err(ParseError {
                                message: "expected string key in map pattern".into(),
                                span: self.span(),
                            });
                        }
                    };
                    self.expect(&Token::Colon)?;
                    let pat = self.parse_pattern()?;
                    entries.push((key, pat));
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(Pattern::Map(entries))
            }
            Token::Minus => {
                // Negative number pattern
                self.advance();
                match self.peek().clone() {
                    Token::Int(n) => {
                        self.advance();
                        // Check for range pattern: -n..m
                        if self.at(&Token::DotDot) {
                            self.advance();
                            match self.peek().clone() {
                                Token::Int(m) => {
                                    self.advance();
                                    Ok(Pattern::Range(-n, m))
                                }
                                Token::Minus => {
                                    self.advance();
                                    match self.peek().clone() {
                                        Token::Int(m) => {
                                            self.advance();
                                            Ok(Pattern::Range(-n, -m))
                                        }
                                        _ => Err(ParseError {
                                            message: "expected integer after - in range pattern"
                                                .into(),
                                            span: self.span(),
                                        }),
                                    }
                                }
                                _ => Err(ParseError {
                                    message: "expected integer end for range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        } else {
                            Ok(Pattern::Int(-n))
                        }
                    }
                    Token::Float(n) => {
                        self.advance();
                        if self.at(&Token::DotDot) {
                            self.advance();
                            match self.peek().clone() {
                                Token::Float(m) => {
                                    self.advance();
                                    Ok(Pattern::FloatRange(-n, m))
                                }
                                Token::Minus => {
                                    self.advance();
                                    match self.peek().clone() {
                                        Token::Float(m) => {
                                            self.advance();
                                            Ok(Pattern::FloatRange(-n, -m))
                                        }
                                        _ => Err(ParseError {
                                            message: "expected float after - in range pattern"
                                                .into(),
                                            span: self.span(),
                                        }),
                                    }
                                }
                                _ => Err(ParseError {
                                    message: "expected float end for range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        } else {
                            Ok(Pattern::Float(-n))
                        }
                    }
                    _ => Err(ParseError {
                        message: "expected number after -".into(),
                        span: self.span(),
                    }),
                }
            }
            Token::Caret => {
                self.advance();
                match self.peek().clone() {
                    Token::Ident(name) => {
                        let name = name.clone();
                        self.advance();
                        Ok(Pattern::Pin(name))
                    }
                    _ => Err(ParseError {
                        message: "expected identifier after ^ in pin pattern".into(),
                        span: self.span(),
                    }),
                }
            }
            _ => Err(ParseError {
                message: format!("expected pattern, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    // ── Utility ──────────────────────────────────────────────────────

    fn peek_skip_nl(&mut self) -> &Token {
        self.skip_nl();
        self.peek()
    }
}

fn is_constructor(name: &str) -> bool {
    name.starts_with(|c: char| c.is_uppercase())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(input: &str) -> Program {
        let tokens = Lexer::new(input).tokenize().unwrap();
        Parser::new(tokens).parse_program().unwrap()
    }

    #[test]
    fn test_hello_world() {
        let prog = parse(
            r#"
            fn main() {
                println("hello, world")
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        assert!(matches!(prog.decls[0], Decl::Fn(_)));
    }

    #[test]
    fn test_fizzbuzz() {
        let prog = parse(
            r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..101
  |> map { n -> fizzbuzz(n) }
  |> each { s -> println(s) }
}
        "#,
        );
        assert_eq!(prog.decls.len(), 2);
    }

    #[test]
    fn test_type_decl_record() {
        let prog = parse(
            r#"
            type User {
                name: String,
                age: Int,
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Type(ref td) = prog.decls[0] {
            assert_eq!(td.name, "User");
            assert!(matches!(td.body, TypeBody::Record(_)));
        } else {
            panic!("expected type decl");
        }
    }

    #[test]
    fn test_type_decl_enum() {
        let prog = parse(
            r#"
            type Shape {
                Circle(Float)
                Rect(Float, Float)
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Type(ref td) = prog.decls[0] {
            assert_eq!(td.name, "Shape");
            if let TypeBody::Enum(ref variants) = td.body {
                assert_eq!(variants.len(), 2);
            } else {
                panic!("expected enum");
            }
        }
    }

    #[test]
    fn test_pipe_and_trailing_closure() {
        let prog = parse(
            r#"
            fn main() {
                [1, 2, 3] |> map { x -> x * 2 }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_record_create_and_update() {
        let prog = parse(
            r#"
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_stmt() {
        let prog = parse(
            r#"
            fn main() {
                when Some(x) = find(42) else {
                    return None
                }
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_trait_impl() {
        let prog = parse(
            r#"
            trait Display for Shape {
                fn display(self) -> String {
                    match self {
                        Circle(r) -> "circle"
                        Rect(w, h) -> "rect"
                    }
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        assert!(matches!(prog.decls[0], Decl::TraitImpl(_)));
    }

    #[test]
    fn test_import() {
        let prog = parse(
            r#"
            import io
            import math.{ add, Point }
            import math as m
        "#,
        );
        assert_eq!(prog.decls.len(), 3);
    }

    #[test]
    fn test_question_mark() {
        let prog = parse(
            r#"
            fn main() {
                let x = foo()?
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_match_with_guard() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    x when x > 0 -> "positive"
                    _ -> "negative"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_string_interp() {
        let prog = parse(
            r#"
            fn main() {
                let name = "world"
                println("hello {name}")
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_where_clause() {
        let prog = parse(
            r#"
            fn show(x) where x: Display {
                x
            }
            fn main() { 0 }
        "#,
        );
        if let Decl::Fn(f) = &prog.decls[0] {
            assert_eq!(f.where_clauses, vec![("x".into(), "Display".into())]);
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_where_clause_multiple() {
        let prog = parse(
            r#"
            fn compare_show(a, b) where a: Display, b: Compare {
                a
            }
            fn main() { 0 }
        "#,
        );
        if let Decl::Fn(f) = &prog.decls[0] {
            assert_eq!(f.where_clauses.len(), 2);
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_abstract_trait_method() {
        let prog = parse(
            r#"
            trait Display {
                fn display(self) -> String
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Trait(ref td) = prog.decls[0] {
            assert_eq!(td.name, "Display");
            assert_eq!(td.methods.len(), 1);
            assert_eq!(td.methods[0].name, "display");
        } else {
            panic!("expected trait decl");
        }
    }

    #[test]
    fn test_fn_without_where_still_works() {
        // Regression test: functions without where should still parse
        let prog = parse(
            r#"
            fn add(a, b) { a + b }
            fn main() { add(1, 2) }
        "#,
        );
        assert_eq!(prog.decls.len(), 2);
    }

    #[test]
    fn test_match_with_trailing_closure_in_pipe() {
        // Trailing closures in pipe RHS should work inside match scrutinees.
        // The `{ x -> x > 5 }` is a trailing closure for `list.any`, while
        // `{ true -> ... }` is the match body.
        let prog = parse(
            r#"
            fn main() {
                let items = [1, 2, 3, 6]
                match items |> list.any { x -> x > 5 } {
                    true -> "has big"
                    _ -> "all small"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        // Verify the match has a scrutinee with a pipe expression
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block body"),
            };
            // The match expression is the last statement
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expression statement"),
            };
            if let ExprKind::Match {
                expr: Some(scrutinee),
                arms,
            } = &match_expr.kind
            {
                // Scrutinee should be a Pipe
                assert!(
                    matches!(scrutinee.kind, ExprKind::Pipe(_, _)),
                    "expected Pipe scrutinee, got {:?}",
                    scrutinee.kind
                );
                // Should have 2 arms
                assert_eq!(arms.len(), 2);
            } else {
                panic!("expected match expression with scrutinee");
            }
        }
    }

    #[test]
    fn test_match_with_chained_pipes_and_trailing_closures() {
        // Multiple pipes with trailing closures in a match scrutinee
        let prog = parse(
            r#"
            fn main() {
                match items |> filter { x -> x > 0 } |> map { x -> x * 2 } {
                    [] -> "empty"
                    _ -> "non-empty"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_bool_stmt() {
        let prog = parse(
            r#"
            fn main() {
                when x > 0 else {
                    return "negative"
                }
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_bool_mixed_with_pattern() {
        let prog = parse(
            r#"
            fn main() {
                when Ok(value) = parse(input) else {
                    return Err("failed")
                }
                when value > 0 else {
                    return Err("must be positive")
                }
                value
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }
}
