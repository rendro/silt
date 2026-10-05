//! The oracle: the check that `format` runs on its own result.
//!
//! The printer is held to the source's tokens by the cursor, but nothing
//! in it can see whether its text still means the same. This module
//! looks at the finished text instead: it lexes and parses it and
//! compares it with the input on
//!
//!   1. the syntax tree, written out in a form (`ShapeWriter`) that
//!      leaves out source positions;
//!   2. the comments of each declaration, in order, by their text with
//!      runs of white space treated as one space.
//!
//! What the printer changes on purpose and the comparison allows:
//!
//!   * parentheses, commas and the layout of a closure argument are not
//!     in the tree, so they need no rule here;
//!   * imports are moved to the top and sorted, so imports are compared
//!     as a set, each with its comments, apart from the other
//!     declarations (a top-level name is bound only once, so their order
//!     never decides what a name means);
//!   * the comments above the last empty line in front of the first
//!     declaration are the file's header: they stay at the top, whichever
//!     declaration comes first;
//!   * a pattern alternative in parentheses inside another one,
//!     `(a | b) | c`, is printed as `a | b | c`, so alternatives are
//!     written flat.

use std::fmt::Write as _;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::lexer::{Lexed, Lexer, Token};
use crate::parser::Parser;
use crate::source::{FileId, SourceFile, SourceName, Span};

use super::Refusal;

/// Check `output`, the text produced for `source`, whose tokens are
/// `lexed` and whose tree is `program`.
pub fn verify(source: &str, lexed: &Lexed, program: &Program, output: &str) -> Result<(), Refusal> {
    let output_lexed = Lexer::new(FileId::default(), output)
        .tokenize()
        .map_err(|e| unparseable(&e.message, e.span, output))?;
    let output_program = Parser::new(output_lexed.clone(), output)
        .parse_program()
        .map_err(|e| unparseable(&e.message, e.span, output))?;

    let before = decl_shapes(source, lexed, program);
    let after = decl_shapes(output, &output_lexed, &output_program);
    compare_programs(&before.decls, &after.decls)?;
    compare_comments(&before, &after)
}

fn unparseable(message: &str, span: Span, output: &str) -> Refusal {
    let file = SourceFile::new(SourceName::Builtin, output.into());
    let line_number = file.line_col(span.start).0;
    let line = file.line_text(line_number).unwrap_or("").trim();
    Refusal {
        message: format!(
            "the result would not parse ({message}, at line {line_number} of the result: `{}`)",
            excerpt(line)
        ),
        span: None,
    }
}

/// At most the first 60 characters of `text`.
fn excerpt(text: &str) -> String {
    const LIMIT: usize = 60;
    if text.chars().count() <= LIMIT {
        text.to_string()
    } else {
        let head: String = text.chars().take(LIMIT).collect();
        format!("{head}...")
    }
}

// ── Declarations ────────────────────────────────────────────────────

/// One top-level declaration, reduced to what the comparison needs.
struct DeclShape {
    is_import: bool,
    /// How to name the declaration in a message: "function `main`".
    label: String,
    span: Span,
    shape: String,
    /// Its comments: those in front of it (but for the file's header),
    /// those inside it, and those behind it on its last line.
    comments: Vec<Note>,
}

/// A comment: its text with every run of white space as one space, and
/// where it is.
#[derive(Clone)]
struct Note {
    text: String,
    span: Span,
}

/// A program as the comparison sees it.
struct Shapes {
    /// The comments above the last empty line in front of the first
    /// declaration.
    header: Vec<Note>,
    decls: Vec<DeclShape>,
    /// The comments on lines of their own behind the last declaration.
    tail: Vec<Note>,
}

fn decl_shapes(source: &str, lexed: &Lexed, program: &Program) -> Shapes {
    let mut decls: Vec<DeclShape> = program
        .decls
        .iter()
        .map(|decl| {
            let mut writer = ShapeWriter::default();
            writer.decl(decl);
            let (label, span) = describe(decl);
            DeclShape {
                is_import: matches!(decl, Decl::Import(..)),
                label,
                span,
                shape: writer.out,
                comments: Vec::new(),
            }
        })
        .collect();

    // Each comment goes to the declaration of the token it belongs to:
    // the token in front of it if they share a line, else the token
    // behind it.
    let starts: Vec<u32> = decls.iter().map(|d| d.span.start).collect();
    let owner = |offset: u32| {
        starts
            .partition_point(|start| *start <= offset)
            .checked_sub(1)
    };
    let mut header = Vec::new();
    let mut tail = Vec::new();
    let tokens: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|tok| tok.kind != Token::Newline)
        .collect();
    for (i, tok) in tokens.iter().enumerate() {
        let comments = lexed.comments_before(tok);
        // The header ends at the last empty line in front of the first
        // token.
        // (A file without declarations has no header: its comments are
        // all behind the last declaration.)
        let header_len = if i == 0 && !decls.is_empty() {
            let mut len = 0;
            for (n, comment) in comments.iter().enumerate().skip(1) {
                if comment.newlines_before >= 2 {
                    len = n;
                }
            }
            if tok.newlines_before >= 2 {
                len = comments.len();
            }
            len
        } else {
            0
        };
        let mut same_line = i > 0;
        for (n, comment) in comments.iter().enumerate() {
            if comment.newlines_before > 0 {
                same_line = false;
            }
            let note = Note {
                text: comment
                    .text(source)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                span: comment.span,
            };
            let of = if same_line { tokens[i - 1] } else { *tok };
            if n < header_len {
                header.push(note);
            } else if of.kind == Token::Eof {
                tail.push(note);
            } else {
                match owner(of.span.start) {
                    Some(decl) => decls[decl].comments.push(note),
                    None => header.push(note),
                }
            }
        }
    }
    Shapes {
        header,
        decls,
        tail,
    }
}

fn describe(decl: &Decl) -> (String, Span) {
    match decl {
        Decl::Fn(f) => (format!("function `{}`", f.name), f.span),
        Decl::Type(t) => (format!("type `{}`", t.name), t.span),
        Decl::Trait(t) => (format!("trait `{}`", t.name), t.span),
        Decl::TraitImpl(t) => (
            format!(
                "the implementation of trait `{}` for `{}`",
                t.trait_name, t.target_type
            ),
            t.span,
        ),
        Decl::Import(target, span) => {
            let module = match target {
                ImportTarget::Module(m)
                | ImportTarget::Items(m, _)
                | ImportTarget::Alias(m, ..) => m,
            };
            (format!("the import of `{module}`"), *span)
        }
        Decl::Let { pattern, span, .. } => {
            let label = match &pattern.kind {
                PatternKind::Ident(name) => format!("the top-level binding `{name}`"),
                _ => "a top-level binding".to_string(),
            };
            (label, *span)
        }
    }
}

fn compare_programs(before: &[DeclShape], after: &[DeclShape]) -> Result<(), Refusal> {
    let mut imports_before: Vec<&DeclShape> = before.iter().filter(|d| d.is_import).collect();
    let mut imports_after: Vec<&DeclShape> = after.iter().filter(|d| d.is_import).collect();
    imports_before.sort_by(|a, b| a.shape.cmp(&b.shape));
    imports_after.sort_by(|a, b| a.shape.cmp(&b.shape));
    same_declarations(&imports_before, &imports_after)?;

    let rest_before: Vec<&DeclShape> = before.iter().filter(|d| !d.is_import).collect();
    let rest_after: Vec<&DeclShape> = after.iter().filter(|d| !d.is_import).collect();
    same_declarations(&rest_before, &rest_after)
}

fn same_declarations(before: &[&DeclShape], after: &[&DeclShape]) -> Result<(), Refusal> {
    for (index, decl) in before.iter().enumerate() {
        let same = after
            .get(index)
            .is_some_and(|other| other.shape == decl.shape);
        if !same {
            return Err(Refusal {
                message: format!(
                    "the result would change the program: {} would not stay as written",
                    decl.label
                ),
                span: Some(decl.span),
            });
        }
    }
    match after.get(before.len()) {
        None => Ok(()),
        Some(extra) => Err(Refusal {
            message: format!(
                "the result would change the program: it would hold {}, \
                 which the input does not hold in that place",
                extra.label
            ),
            span: None,
        }),
    }
}

// ── Comments ────────────────────────────────────────────────────────

/// The result has to hold the comments of the input: the same texts in
/// the same order in the header, in each declaration, and behind the
/// last one. Imports are sorted, so they are matched by what they
/// import and what their comments say.
fn compare_comments(before: &Shapes, after: &Shapes) -> Result<(), Refusal> {
    // A comment that is nowhere in the result, or only in the result, is
    // named first: it is the plainest thing to say.
    let all = |shapes: &Shapes| -> Vec<Note> {
        shapes
            .header
            .iter()
            .chain(shapes.decls.iter().flat_map(|d| d.comments.iter()))
            .chain(shapes.tail.iter())
            .cloned()
            .collect()
    };
    let (all_before, all_after) = (all(before), all(after));
    let mut surplus: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for note in &all_before {
        *surplus.entry(note.text.as_str()).or_insert(0) += 1;
    }
    for note in &all_after {
        *surplus.entry(note.text.as_str()).or_insert(0) -= 1;
    }
    for note in &all_before {
        if surplus.get(note.text.as_str()).copied().unwrap_or(0) > 0 {
            return Err(Refusal {
                message: format!(
                    "the result would lose the comment `{}`",
                    excerpt(&note.text)
                ),
                span: Some(note.span),
            });
        }
    }
    for note in &all_after {
        if surplus.get(note.text.as_str()).copied().unwrap_or(0) < 0 {
            return Err(Refusal {
                message: format!(
                    "the result would hold the comment `{}`, which the input does not hold",
                    excerpt(&note.text)
                ),
                span: None,
            });
        }
    }

    same_comments("the top of the file", &before.header, &after.header)?;
    same_comments("the end of the file", &before.tail, &after.tail)?;
    let rest = |shapes: &'_ Shapes| -> Vec<(String, Vec<Note>)> {
        shapes
            .decls
            .iter()
            .filter(|d| !d.is_import)
            .map(|d| (d.label.clone(), d.comments.clone()))
            .collect()
    };
    for ((label, notes_before), (_, notes_after)) in rest(before).iter().zip(rest(after).iter()) {
        same_comments(label, notes_before, notes_after)?;
    }
    // Imports: the same pairs of import and comments, in any order.
    let imports = |shapes: &Shapes| -> Vec<(String, String, Vec<Note>)> {
        let mut imports: Vec<(String, String, Vec<Note>)> = shapes
            .decls
            .iter()
            .filter(|d| d.is_import)
            .map(|d| {
                let texts: Vec<&str> = d.comments.iter().map(|n| n.text.as_str()).collect();
                (
                    format!("{}\n{}", d.shape, texts.join("\n")),
                    d.label.clone(),
                    d.comments.clone(),
                )
            })
            .collect();
        imports.sort_by(|a, b| a.0.cmp(&b.0));
        imports
    };
    for ((key_before, label, notes), (key_after, ..)) in
        imports(before).iter().zip(imports(after).iter())
    {
        if key_before != key_after {
            return Err(Refusal {
                message: format!("the result would move a comment away from {label}"),
                span: notes.first().map(|n| n.span),
            });
        }
    }
    Ok(())
}

/// The comments of one place, before and after.
fn same_comments(place: &str, before: &[Note], after: &[Note]) -> Result<(), Refusal> {
    let texts = |notes: &[Note]| -> Vec<String> { notes.iter().map(|n| n.text.clone()).collect() };
    if texts(before) == texts(after) {
        return Ok(());
    }
    // The first place where the two lists differ.
    let at = before
        .iter()
        .zip(after.iter())
        .position(|(a, b)| a.text != b.text)
        .unwrap_or(before.len().min(after.len()));
    Err(match (before.get(at), after.get(at)) {
        (Some(note), _) => Refusal {
            message: format!(
                "the result would move the comment `{}` of {place}",
                excerpt(&note.text)
            ),
            span: Some(note.span),
        },
        (None, Some(note)) => Refusal {
            message: format!(
                "the result would move the comment `{}` to {place}",
                excerpt(&note.text)
            ),
            span: None,
        },
        (None, None) => Refusal {
            message: format!("the result would move a comment of {place}"),
            span: None,
        },
    })
}

// ── Syntax tree ─────────────────────────────────────────────────────

/// Writes a tree as text: one parenthesised group per node, the node
/// kind first, then its parts in a fixed order. Every part is either
/// a group, a single word without white space or parentheses, or text
/// in quotes with escapes, so two different trees never give the same
/// text. Source positions, doc comments and everything a later pass
/// fills in are left out.
#[derive(Default)]
struct ShapeWriter {
    out: String,
}

impl ShapeWriter {
    fn open(&mut self, kind: &str) {
        self.out.push_str(" (");
        self.out.push_str(kind);
    }

    fn close(&mut self) {
        self.out.push(')');
    }

    fn word(&mut self, word: &str) {
        self.out.push(' ');
        self.out.push_str(word);
    }

    fn none(&mut self) {
        self.word("#none");
    }

    fn flag(&mut self, on: bool) {
        self.word(if on { "#yes" } else { "#no" });
    }

    fn sym(&mut self, sym: Symbol) {
        self.word(&resolve(sym));
    }

    fn opt_sym(&mut self, sym: Option<Symbol>) {
        match sym {
            Some(sym) => self.sym(sym),
            None => self.none(),
        }
    }

    fn qualifier(&mut self, module: Option<Qualifier>) {
        self.opt_sym(module.map(|m| m.name));
    }

    fn text(&mut self, text: &str) {
        let _ = write!(self.out, " {text:?}");
    }

    fn int(&mut self, n: i64) {
        let _ = write!(self.out, " {n}");
    }

    /// A float by its bits, so that no two values share a spelling.
    fn float(&mut self, x: f64) {
        let _ = write!(self.out, " #x{:016x}", x.to_bits());
    }

    fn decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Fn(f) => self.fn_decl(f),
            Decl::Type(t) => {
                self.open("type");
                self.flag(t.is_pub);
                self.sym(t.name);
                self.open("params");
                for param in &t.params {
                    self.sym(*param);
                }
                self.close();
                match &t.body {
                    TypeBody::Enum(variants) => {
                        self.open("enum");
                        for variant in variants {
                            self.open("variant");
                            self.sym(variant.name);
                            for field in &variant.fields {
                                self.type_expr(field);
                            }
                            self.close();
                        }
                        self.close();
                    }
                    TypeBody::Record(fields) => {
                        self.open("record");
                        for field in fields {
                            self.open("field");
                            self.sym(field.name);
                            self.type_expr(&field.ty);
                            self.close();
                        }
                        self.close();
                    }
                    TypeBody::Alias(target) => {
                        self.open("alias");
                        self.type_expr(target);
                        self.close();
                    }
                }
                self.close();
            }
            Decl::Trait(t) => {
                self.open("trait");
                self.flag(t.is_pub);
                self.sym(t.name);
                self.open("params");
                for param in &t.params {
                    self.sym(*param);
                }
                self.close();
                self.open("supertraits");
                for r in &t.supertraits {
                    self.bound(r.module, r.name, &r.args);
                }
                self.close();
                self.where_clauses(&t.param_where_clauses);
                self.open("types");
                for assoc in &t.assoc_types {
                    self.open("type");
                    self.sym(assoc.name);
                    for r in &assoc.bounds {
                        self.bound(r.module, r.name, &r.args);
                    }
                    self.close();
                }
                self.close();
                self.open("methods");
                for method in &t.methods {
                    self.fn_decl(method);
                }
                self.close();
                self.close();
            }
            Decl::TraitImpl(t) => {
                self.open("impl");
                self.bound(t.trait_module, t.trait_name, &t.trait_args);
                self.open("for");
                self.qualifier(t.target_module);
                self.sym(t.target_type);
                for arg in &t.target_type_args {
                    self.type_expr(arg);
                }
                self.close();
                self.where_clauses(&t.where_clauses);
                self.open("types");
                for binding in &t.assoc_type_bindings {
                    self.open("type");
                    self.sym(binding.name);
                    self.type_expr(&binding.ty);
                    self.close();
                }
                self.close();
                self.open("methods");
                for method in &t.methods {
                    self.fn_decl(method);
                }
                self.close();
                self.close();
            }
            Decl::Import(target, _) => {
                match target {
                    ImportTarget::Module(module) => {
                        self.open("import");
                        self.sym(*module);
                    }
                    ImportTarget::Items(module, items) => {
                        self.open("import-items");
                        self.sym(*module);
                        for (item, _) in items {
                            self.sym(*item);
                        }
                    }
                    ImportTarget::Alias(module, alias, _) => {
                        self.open("import-as");
                        self.sym(*module);
                        self.sym(*alias);
                    }
                }
                self.close();
            }
            Decl::Let {
                pattern,
                ty,
                value,
                is_pub,
                ..
            } => {
                self.open("let");
                self.flag(*is_pub);
                self.pattern(pattern);
                self.opt_type_expr(ty.as_ref());
                self.expr(value);
                self.close();
            }
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.open("fn");
        self.flag(f.is_pub);
        self.sym(f.name);
        self.params(&f.params);
        self.opt_type_expr(f.return_type.as_ref());
        self.where_clauses(&f.where_clauses);
        if f.is_signature_only {
            self.word("#signature");
        } else {
            self.expr(&f.body);
        }
        self.close();
    }

    fn params(&mut self, params: &[Param]) {
        self.open("params");
        for param in params {
            self.open(match param.kind {
                ParamKind::Data => "param",
                ParamKind::Type => "type-param",
            });
            self.pattern(&param.pattern);
            self.opt_type_expr(param.ty.as_ref());
            self.close();
        }
        self.close();
    }

    /// A trait with its arguments: `Display`, `TryInto(Int)`.
    fn bound(&mut self, module: Option<Qualifier>, name: Symbol, args: &[TypeExpr]) {
        self.open("bound");
        self.qualifier(module);
        self.sym(name);
        for arg in args {
            self.type_expr(arg);
        }
        self.close();
    }

    /// `where` bounds, one per clause of the tree, in order.
    fn where_clauses(&mut self, clauses: &[WhereClause]) {
        self.open("where");
        for clause in clauses {
            self.open("var");
            self.sym(clause.type_param);
            self.bound(clause.trait_module, clause.trait_name, &clause.trait_args);
            self.close();
        }
        self.close();
    }

    fn opt_type_expr(&mut self, ty: Option<&TypeExpr>) {
        match ty {
            Some(ty) => self.type_expr(ty),
            None => self.none(),
        }
    }

    fn type_expr(&mut self, ty: &TypeExpr) {
        match &ty.kind {
            TypeExprKind::Named { module, name, .. } => {
                self.open("named");
                self.qualifier(*module);
                self.sym(*name);
            }
            TypeExprKind::Generic {
                module, name, args, ..
            } => {
                self.open("generic");
                self.qualifier(*module);
                self.sym(*name);
                for arg in args {
                    self.type_expr(arg);
                }
            }
            TypeExprKind::Tuple(elems) => {
                self.open("tuple");
                for elem in elems {
                    self.type_expr(elem);
                }
            }
            TypeExprKind::Function(params, ret) => {
                self.open("fn");
                self.open("params");
                for param in params {
                    self.type_expr(param);
                }
                self.close();
                self.type_expr(ret);
            }
            TypeExprKind::SelfType => self.open("self"),
            TypeExprKind::AssocProj {
                receiver,
                trait_module,
                trait_name,
                assoc_name,
            } => {
                self.open("projection");
                self.type_expr(receiver);
                self.qualifier(*trait_module);
                self.sym(*trait_name);
                self.sym(*assoc_name);
            }
            TypeExprKind::AnonRecord { fields, tail } => {
                self.open("record");
                self.opt_sym(*tail);
                for (name, ty) in fields {
                    self.open("field");
                    self.sym(*name);
                    self.type_expr(ty);
                    self.close();
                }
            }
        }
        self.close();
    }

    fn opt_pattern(&mut self, pattern: Option<&Pattern>) {
        match pattern {
            Some(pattern) => self.pattern(pattern),
            None => self.none(),
        }
    }

    fn field_patterns(&mut self, fields: &[(Symbol, Span, Option<Pattern>)]) {
        for (name, _, sub) in fields {
            self.open("field");
            self.sym(*name);
            self.opt_pattern(sub.as_ref());
            self.close();
        }
    }

    /// The alternatives of an or-pattern, flat.
    fn alternatives(&mut self, alts: &[Pattern]) {
        for alt in alts {
            match &alt.kind {
                PatternKind::Or(inner) => self.alternatives(inner),
                _ => self.pattern(alt),
            }
        }
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Wildcard => self.open("wildcard"),
            PatternKind::Ident(name) => {
                self.open("bind");
                self.sym(*name);
            }
            PatternKind::Int(n) => {
                self.open("int");
                self.int(*n);
            }
            PatternKind::Float(x) => {
                self.open("float");
                self.float(*x);
            }
            PatternKind::Bool(b) => {
                self.open("bool");
                self.flag(*b);
            }
            PatternKind::StringLit(s, _) => {
                self.open("string");
                self.text(s);
            }
            PatternKind::Tuple(elems) => {
                self.open("tuple");
                for elem in elems {
                    self.pattern(elem);
                }
            }
            PatternKind::Constructor {
                qualifier,
                name,
                args,
                ..
            } => {
                self.open("constructor");
                self.open("qualifier");
                for segment in qualifier {
                    self.sym(segment.name);
                }
                self.close();
                self.sym(*name);
                for arg in args {
                    self.pattern(arg);
                }
            }
            PatternKind::Record {
                module,
                name,
                fields,
                has_rest,
                ..
            } => {
                self.open("record");
                self.qualifier(*module);
                self.opt_sym(*name);
                self.flag(*has_rest);
                self.field_patterns(fields);
            }
            PatternKind::AnonRecord { fields, rest } => {
                self.open("anon-record");
                self.opt_sym(rest.map(|(r, _)| r));
                self.field_patterns(fields);
            }
            PatternKind::List(elems, rest) => {
                self.open("list");
                self.open("rest");
                if let Some(rest) = rest {
                    self.pattern(rest);
                }
                self.close();
                for elem in elems {
                    self.pattern(elem);
                }
            }
            PatternKind::Or(alts) => {
                self.open("or");
                self.alternatives(alts);
            }
            PatternKind::Range(start, end) => {
                self.open("range");
                self.int(*start);
                self.int(*end);
            }
            PatternKind::FloatRange(start, end) => {
                self.open("float-range");
                self.float(*start);
                self.float(*end);
            }
            PatternKind::Map(entries) => {
                self.open("map");
                for (key, value) in entries {
                    self.open("entry");
                    self.text(key);
                    self.pattern(value);
                    self.close();
                }
            }
            PatternKind::Pin(name) => {
                self.open("pin");
                self.sym(*name);
            }
        }
        self.close();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                self.open("let");
                self.pattern(pattern);
                self.opt_type_expr(ty.as_ref());
                self.expr(value);
                self.close();
            }
            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                self.open("when-let");
                self.pattern(pattern);
                self.expr(expr);
                self.expr(else_body);
                self.close();
            }
            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                self.open("when");
                self.expr(condition);
                self.expr(else_body);
                self.close();
            }
            Stmt::Expr(expr) => self.expr(expr),
        }
    }

    fn opt_expr(&mut self, expr: Option<&Expr>) {
        match expr {
            Some(expr) => self.expr(expr),
            None => self.none(),
        }
    }

    fn field_values(&mut self, fields: &[(Symbol, Expr)]) {
        for (name, value) in fields {
            self.open("field");
            self.sym(*name);
            self.expr(value);
            self.close();
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Int(n) => {
                self.open("int");
                self.int(*n);
            }
            ExprKind::Float(x) => {
                self.open("float");
                self.float(*x);
            }
            ExprKind::Bool(b) => {
                self.open("bool");
                self.flag(*b);
            }
            ExprKind::StringLit(s, _) => {
                self.open("string");
                self.text(s);
            }
            ExprKind::StringInterp(parts) => {
                self.open("interpolation");
                // Text between two holes is one piece, however the
                // parser happened to cut it.
                let mut literal = String::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => literal.push_str(s),
                        StringPart::Expr(hole) => {
                            if !literal.is_empty() {
                                self.text(&literal);
                                literal.clear();
                            }
                            self.open("hole");
                            self.expr(hole);
                            self.close();
                        }
                    }
                }
                if !literal.is_empty() {
                    self.text(&literal);
                }
            }
            ExprKind::List(elems) => {
                self.open("list");
                for elem in elems {
                    match elem {
                        ListElem::Single(e) => self.expr(e),
                        ListElem::Spread(e) => {
                            self.open("spread");
                            self.expr(e);
                            self.close();
                        }
                    }
                }
            }
            ExprKind::Map(pairs) => {
                self.open("map");
                for (key, value) in pairs {
                    self.open("entry");
                    self.expr(key);
                    self.expr(value);
                    self.close();
                }
            }
            ExprKind::SetLit(elems) => {
                self.open("set");
                for elem in elems {
                    self.expr(elem);
                }
            }
            ExprKind::Tuple(elems) => {
                self.open("tuple");
                for elem in elems {
                    self.expr(elem);
                }
            }
            ExprKind::Ident(name) => {
                self.open("name");
                self.sym(*name);
            }
            ExprKind::FieldAccess(target, field, _) => {
                self.open("field-access");
                self.expr(target);
                self.sym(*field);
            }
            ExprKind::Binary(left, op, right) => {
                self.open("binary");
                self.word(&op.to_string());
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Unary(op, operand) => {
                self.open(match op {
                    UnaryOp::Neg => "negate",
                    UnaryOp::Not => "not",
                });
                self.expr(operand);
            }
            ExprKind::Pipe(left, right) => {
                self.open("pipe");
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Range(start, end) => {
                self.open("range");
                self.expr(start);
                self.expr(end);
            }
            ExprKind::QuestionMark(operand) => {
                self.open("question");
                self.expr(operand);
            }
            ExprKind::Ascription(value, ty) => {
                self.open("as");
                self.expr(value);
                self.type_expr(ty);
            }
            ExprKind::Call(callee, args) => {
                self.open("call");
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::Lambda { params, body } => {
                self.open("lambda");
                self.params(params);
                self.expr(body);
            }
            ExprKind::RecordCreate {
                module,
                name,
                fields,
                ..
            } => {
                self.open("record");
                self.qualifier(*module);
                self.sym(*name);
                self.field_values(fields);
            }
            ExprKind::RecordUpdate { expr, fields } => {
                self.open("record-update");
                self.expr(expr);
                self.field_values(fields);
            }
            ExprKind::AnonRecord { spread, fields } => {
                self.open("anon-record");
                self.opt_expr(spread.as_deref());
                self.field_values(fields);
            }
            ExprKind::Match { expr, arms } => {
                self.open("match");
                self.opt_expr(expr.as_deref());
                for arm in arms {
                    self.open("arm");
                    self.pattern(&arm.pattern);
                    self.opt_expr(arm.guard.as_deref());
                    self.expr(&arm.body);
                    self.close();
                }
            }
            ExprKind::Return(value) => {
                self.open("return");
                self.opt_expr(value.as_deref());
            }
            ExprKind::Block(stmts) => {
                self.open("block");
                for stmt in stmts {
                    self.stmt(stmt);
                }
            }
            ExprKind::Loop { bindings, body } => {
                self.open("loop");
                for (name, _, init) in bindings {
                    self.open("binding");
                    self.sym(*name);
                    self.expr(init);
                    self.close();
                }
                self.expr(body);
            }
            ExprKind::Recur(args) => {
                self.open("recur");
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::Unit => self.open("unit"),
        }
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the oracle says to `output` as the result for `source`.
    fn judge(source: &str, output: &str) -> Result<(), String> {
        let lexed = Lexer::new(FileId::default(), source).tokenize().unwrap();
        let program = Parser::new(lexed.clone(), source).parse_program().unwrap();
        verify(source, &lexed, &program, output).map_err(|refusal| refusal.message)
    }

    fn refusal(source: &str, output: &str) -> String {
        judge(source, output).expect_err("the result must be refused")
    }

    #[test]
    fn a_result_that_only_differs_in_layout_passes() {
        let source = "fn main() { -- entry\n  f((1), [2, 3,],)\n}\n";
        let output = "fn main() { -- entry\n  f(\n    1,\n    [2, 3],\n  )\n}\n";
        assert_eq!(judge(source, output), Ok(()));
    }

    #[test]
    fn a_result_that_does_not_parse_is_refused() {
        let message = refusal("fn main() {\n  1\n}\n", "fn main() {\n  (1\n}\n");
        assert!(message.contains("would not parse"), "{message}");
        assert!(message.contains("line 3 of the result"), "{message}");
    }

    #[test]
    fn a_result_with_another_tree_is_refused() {
        let message = refusal(
            "fn main() {\n  (1 + 2) * 3\n}\n",
            "fn main() {\n  1 + 2 * 3\n}\n",
        );
        assert!(
            message.contains("function `main` would not stay as written"),
            "{message}"
        );
        // Braces are part of the tree: a block is not its expression.
        let message = refusal("fn f() {\n  { 1 }\n}\n", "fn f() {\n  1\n}\n");
        assert!(message.contains("function `f`"), "{message}");
        // So is the order of `where` bounds.
        let message = refusal(
            "fn f(x: a) where a: A, a: B {\n}\n",
            "fn f(x: a) where a: B + A {\n}\n",
        );
        assert!(message.contains("function `f`"), "{message}");
    }

    #[test]
    fn a_lost_or_new_comment_is_refused() {
        let message = refusal("fn main() {\n  1 -- one\n}\n", "fn main() {\n  1\n}\n");
        assert!(message.contains("lose the comment `-- one`"), "{message}");
        let message = refusal(
            "fn main() {\n  1 {- a -}\n}\n",
            "fn main() {\n  1 {- a -} {- a -}\n}\n",
        );
        assert!(message.contains("hold the comment `{- a -}`"), "{message}");
        // The white space in a comment and around it is not compared.
        assert_eq!(
            judge(
                "fn main() {\n  1 --   one   \n}\n",
                "fn main() {\n  1 -- one\n}\n"
            ),
            Ok(())
        );
    }

    #[test]
    fn comments_that_change_their_order_or_their_declaration_are_refused() {
        let message = refusal(
            "fn main() {\n  -- a\n  1 -- b\n}\n",
            "fn main() {\n  1 -- b\n  -- a\n}\n",
        );
        assert!(
            message.contains("move the comment `-- a` of function `main`"),
            "{message}"
        );
        let message = refusal(
            "fn f() {\n  1\n}\n-- about g\nfn g() {\n  2\n}\n",
            "fn f() {\n  1 -- about g\n}\nfn g() {\n  2\n}\n",
        );
        assert!(message.contains("`-- about g`"), "{message}");
        // The file's header is a place of its own.
        let message = refusal(
            "-- header\n\nfn f() {\n  1\n}\n",
            "-- header\nfn f() {\n  1\n}\n",
        );
        assert!(message.contains("the top of the file"), "{message}");
    }

    #[test]
    fn imports_may_be_sorted_and_take_their_comments_along() {
        let source = "-- header\n\nimport b -- second\n-- about a\nimport a\nfn main() {\n}\n";
        let sorted = "-- header\n\n-- about a\nimport a\nimport b -- second\n\nfn main() {\n}\n";
        assert_eq!(judge(source, sorted), Ok(()));
        // A comment that goes to another import is refused.
        let swapped = "-- header\n\nimport a\n-- about a\nimport b -- second\n\nfn main() {\n}\n";
        let message = refusal(source, swapped);
        assert!(message.contains("move a comment away from"), "{message}");
        // Other declarations keep their order.
        let message = refusal("fn f() {\n}\nfn g() {\n}\n", "fn g() {\n}\n\nfn f() {\n}\n");
        assert!(message.contains("function `f`"), "{message}");
    }

    #[test]
    fn alternatives_in_parentheses_are_the_same_flat_list() {
        let source = "fn f(x) {\n  match x {\n    (1 | 2) | 3 -> 1\n    _ -> 0\n  }\n}\n";
        let output = "fn f(x) {\n  match x {\n    1 | 2 | 3 -> 1\n    _ -> 0\n  }\n}\n";
        assert_eq!(judge(source, output), Ok(()));
    }
}
