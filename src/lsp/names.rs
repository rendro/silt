//! The names a module writes, and what each means.
//!
//! [`Names::of`] walks a checked module once and lists every name the
//! resolver or the checker gave a meaning: a use of a definition (the
//! resolver's `Res::Def` on the node), a local name (a binder, or a use
//! of one), and a method name at a call (the checker's `Selection`).
//! References, rename, highlight and go-to-definition read these lists
//! (`workspace.rs`).
//!
//! The walk matches every kind of declaration, expression, statement,
//! pattern and type expression by name, without a catch-all arm: a new
//! kind of node does not compile until it is given a place here.

use crate::ast::{
    Decl, Expr, ExprKind, FnDecl, ImportTarget, ListElem, Pattern, PatternKind, Program, Selection,
    Stmt, StringPart, TraitRef, TypeBody, TypeExpr, TypeExprKind, WhereClause,
};
use crate::defs::{DefId, DefKind, DefTable, Res};
use crate::intern::{Symbol, resolve as resolve_sym};
use crate::session::{ImportResolution, ModuleId, Session};
use crate::source::Span;

use super::local_bindings::{find_local_binding_at_offset, nearest_local_binding_for};
use super::state::LocalBinding;

/// A use of a definition: the span of the name as written.
pub(super) struct DefUse {
    pub(super) span: Span,
    pub(super) id: DefId,
}

/// A local name as a module writes it: where a pattern, a parameter or
/// a `loop` binds it, or a use of it.
pub(super) struct LocalName {
    pub(super) span: Span,
    pub(super) name: Symbol,
    pub(super) binder: bool,
    /// The binder is the field of a record pattern written without a
    /// pattern (`P { x }` for `P { x: x }`): another name for the binding
    /// is written behind the field, `P { x: y }`.
    pub(super) punned: bool,
}

impl LocalName {
    /// The binding it is or resolves to (see [`LocalBinding::id`]), when
    /// the document's local bindings have it.
    pub(super) fn binding(&self, locals: &[LocalBinding]) -> Option<usize> {
        let at = self.span.start as usize;
        let binding = if self.binder {
            find_local_binding_at_offset(locals, at)
        } else {
            nearest_local_binding_for(locals, self.name, at)
        };
        binding.map(LocalBinding::id)
    }
}

/// A method name at a call through its trait, `Sized.scale(x, 2)`, or
/// on a type held in a value, `a.empty()`: no name of a definition (a
/// method has none of its own), but the checker says which method it
/// is.
pub(super) struct MethodUse {
    pub(super) span: Span,
    pub(super) name: Symbol,
    pub(super) selection: Selection,
}

/// A field name of a record type, with the type: where the type
/// declares it (`x` in `type P { x: Int }`), in a record literal or
/// update (`P { x: 1 }`, `p.{ x: 2 }`), in a read (`p.x`) and in a
/// record pattern (`P { x: a }`, `P { x }`).
pub(super) struct FieldUse {
    pub(super) span: Span,
    /// The record type (through an alias: the record it names).
    pub(super) record: DefId,
    pub(super) field: Symbol,
    /// The field is written without its value or pattern (`P { x }`):
    /// the name is also a local's or a definition's.
    pub(super) punned: bool,
    /// It is the field's declaration in its type.
    pub(super) declaration: bool,
}

/// Every name a module writes that has a meaning, apart from the names
/// its declarations declare (those are the definitions' own spans).
pub(super) struct Names<'a> {
    session: &'a Session,
    module: ModuleId,
    pub(super) defs: Vec<DefUse>,
    pub(super) locals: Vec<LocalName>,
    pub(super) methods: Vec<MethodUse>,
    pub(super) fields: Vec<FieldUse>,
    /// The field reads on a value whose record type is not known (a
    /// type error, an anonymous record): the span of the name and the
    /// name. A field of that name cannot be renamed while one is there,
    /// since the read may be one of its places.
    pub(super) unknown_reads: Vec<(Span, Symbol)>,
    /// The module names written before a dot: `geo` in `geo.mk(1)`, in
    /// `geo.Pt { x: 1 }`, in a type `geo.Pt`, in a pattern
    /// `geo.Shape.Circle(r)`, in `where a: geo.Sized`. A name in an
    /// expression is one only when the resolver took it for a module.
    pub(super) qualifiers: Vec<crate::ast::Qualifier>,
    /// The names of definitions written as a field without a pattern
    /// or a value: declared by a top-level `let` (`let P { x } = p`), or
    /// used in a record literal or update (`P { x }`).
    pub(super) punned: Vec<Span>,
    /// The pattern being walked is a top-level `let`'s: its names are
    /// definitions.
    declares: bool,
    /// The text of the module's file, and its tokens once a record
    /// literal needs them: where the name of a field written with its
    /// value is (the tree keeps the value only).
    text: Option<&'a str>,
    tokens: Option<Vec<crate::lexer::Tok>>,
}

/// The definitions the item `item` of `import module.{ item }` in module
/// `importer` names: a value, a type, or both.
fn import_item_defs(
    session: &Session,
    importer: ModuleId,
    module: Symbol,
    item: Symbol,
) -> Vec<DefId> {
    use crate::typechecker::names::Binding;
    let resolution = session
        .graph()
        .module(importer)
        .imports
        .iter()
        .find(|import| import.name == module)
        .map(|import| &import.resolution);
    // A builtin module's exports are the builtin scopes'.
    let builtins = crate::typechecker::names::builtins().1;
    let exports = match resolution {
        Some(ImportResolution::Module(id)) => session
            .module_analysis(*id)
            .map(|analysis| &analysis.scope.exports),
        Some(ImportResolution::Builtin) => ModuleId::builtin(&crate::intern::resolve(module))
            .and_then(|id| builtins.modules.get(&id)),
        _ => None,
    };
    let Some(exports) = exports else {
        return Vec::new();
    };
    [exports.values.get(&item), exports.types.get(&item)]
        .into_iter()
        .filter_map(|binding| match binding {
            Some(Binding::Def(id)) => Some(*id),
            _ => None,
        })
        .collect()
}

impl<'a> Names<'a> {
    /// The names of `program`, the checked module `module` of `session`.
    pub(super) fn of(program: &Program, session: &'a Session, module: ModuleId) -> Names<'a> {
        let mut names = Names {
            session,
            module,
            defs: Vec::new(),
            locals: Vec::new(),
            methods: Vec::new(),
            punned: Vec::new(),
            fields: Vec::new(),
            qualifiers: Vec::new(),
            unknown_reads: Vec::new(),
            declares: false,
            text: session
                .graph()
                .module(module)
                .file
                .and_then(|file| session.sources().get(file))
                .map(|source| &source.text[..]),
            tokens: None,
        };
        for decl in &program.decls {
            names.decl(decl);
        }
        names
    }

    fn table(&self) -> &'a DefTable {
        self.session.defs()
    }

    /// A name at `span` that means `res`.
    fn name(&mut self, span: Span, res: Option<Res>) {
        if let Some(Res::Def(id)) = res
            && span.is_in_source()
        {
            self.defs.push(DefUse { span, id });
        }
    }

    /// The record type `id` names, through an alias: none for an enum
    /// or anything else.
    fn record(&self, id: DefId) -> Option<DefId> {
        let defs = self.table();
        let id = defs.through_alias(id);
        let def = defs.get(id);
        (matches!(def.kind, DefKind::Type(_)) && defs.variants(id).is_empty()).then_some(id)
    }

    /// The record type of a value of type `ty`, if it is one.
    fn record_of(&self, ty: Option<&crate::types::Type>) -> Option<DefId> {
        match ty {
            Some(crate::types::Type::Generic(ty, _)) => self.record(ty.id.0),
            _ => None,
        }
    }

    /// The field `field` of the record `record` at `span`.
    fn field(&mut self, span: Span, record: DefId, field: Symbol, punned: bool) {
        if span.is_in_source() {
            self.fields.push(FieldUse {
                span,
                record,
                field,
                punned,
                declaration: false,
            });
        }
    }

    /// A module name before a dot.
    fn qualifier(&mut self, qualifier: &Option<crate::ast::Qualifier>) {
        if let Some(q) = qualifier
            && q.span.is_in_source()
        {
            self.qualifiers.push(*q);
        }
    }

    /// Where the name of the field `field` is written in front of its
    /// value `value` (`x` in `x: 1`), in a record literal or update that
    /// starts at byte `from`: the last `field :` before the value, as the
    /// lexer reads the text.
    fn field_name_span(&mut self, from: u32, field: Symbol, value: &Expr) -> Option<Span> {
        use crate::lexer::Token;
        let text = self.text?;
        let tokens = self.tokens.get_or_insert_with(|| {
            crate::lexer::Lexer::new(crate::source::FileId::default(), text)
                .tokenize()
                .tokens
        });
        let before = tokens.partition_point(|tok| tok.span.end <= value.span.start);
        let tokens = &tokens[..before];
        for pair in tokens.windows(2).rev() {
            let [name, colon] = pair else { break };
            if name.span.start < from {
                break;
            }
            if let (Token::Ident(sym), Token::Colon) = (&name.kind, &colon.kind)
                && *sym == field
            {
                return Some(Span {
                    file: value.span.file,
                    ..name.span
                });
            }
        }
        None
    }

    /// A local name at `span`.
    fn local(&mut self, span: Span, name: Symbol, binder: bool, punned: bool) {
        if self.declares && binder {
            if punned {
                self.punned.push(span);
            }
            return;
        }
        self.locals.push(LocalName {
            span,
            name,
            binder,
            punned,
        });
    }

    fn decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Fn(f) => self.fn_decl(f),
            Decl::TraitImpl(ti) => {
                self.qualifier(&ti.trait_module);
                self.qualifier(&ti.target_module);
                self.name(ti.trait_name_span, ti.trait_res);
                self.name(ti.target_type_span, ti.target_res);
                self.where_clauses(&ti.where_clauses);
                for arg in ti.trait_args.iter().chain(&ti.target_type_args) {
                    self.type_expr(arg);
                }
                for method in &ti.methods {
                    self.fn_decl(method);
                }
            }
            Decl::Trait(t) => {
                for supertrait in &t.supertraits {
                    self.trait_ref(supertrait);
                }
                self.where_clauses(&t.param_where_clauses);
                for method in &t.methods {
                    self.fn_decl(method);
                }
            }
            Decl::Let {
                value, pattern, ty, ..
            } => {
                self.declares = true;
                self.pattern(pattern);
                self.declares = false;
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
                self.expr(value);
            }
            Decl::Type(t) => match &t.body {
                TypeBody::Record(fields) => {
                    let defs = self.table();
                    let record = (defs.of_module(self.module).iter().copied())
                        .find(|id| defs.get(*id).span == t.name_span);
                    for field in fields {
                        if let Some(record) = record {
                            self.fields.push(FieldUse {
                                span: field.name_span,
                                record,
                                field: field.name,
                                punned: false,
                                declaration: true,
                            });
                        }
                        self.type_expr(&field.ty);
                    }
                }
                TypeBody::Enum(variants) => {
                    for variant in variants {
                        for field in &variant.fields {
                            self.type_expr(field);
                        }
                    }
                }
                TypeBody::Alias(ty) => self.type_expr(ty),
            },
            // An item of `import m.{ item }` names what `m` exports.
            Decl::Import(ImportTarget::Items(module, items), _) => {
                for (item, span) in items {
                    for id in import_item_defs(self.session, self.module, *module, *item) {
                        self.name(*span, Some(Res::Def(id)));
                    }
                }
            }
            Decl::Import(ImportTarget::Module(_) | ImportTarget::Alias(..), _) => {}
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        for param in &f.params {
            self.pattern(&param.pattern);
            if let Some(ty) = &param.ty {
                self.type_expr(ty);
            }
        }
        if let Some(ty) = &f.return_type {
            self.type_expr(ty);
        }
        self.where_clauses(&f.where_clauses);
        self.expr(&f.body);
    }

    fn where_clauses(&mut self, clauses: &[WhereClause]) {
        for clause in clauses {
            self.qualifier(&clause.trait_module);
            self.name(clause.trait_name_span, clause.trait_res);
            for arg in &clause.trait_args {
                self.type_expr(arg);
            }
        }
    }

    fn trait_ref(&mut self, r: &TraitRef) {
        self.qualifier(&r.module);
        self.name(r.span, r.res);
        for arg in &r.args {
            self.type_expr(arg);
        }
    }

    fn type_expr(&mut self, te: &TypeExpr) {
        match &te.kind {
            TypeExprKind::Named {
                module, name_span, ..
            } => {
                self.qualifier(module);
                self.name(*name_span, te.res);
            }
            TypeExprKind::Generic {
                module,
                name_span,
                args,
                ..
            } => {
                self.qualifier(module);
                self.name(*name_span, te.res);
                for arg in args {
                    self.type_expr(arg);
                }
            }
            TypeExprKind::Tuple(elems) => {
                for elem in elems {
                    self.type_expr(elem);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for param in params {
                    self.type_expr(param);
                }
                self.type_expr(ret);
            }
            TypeExprKind::SelfType => {}
            TypeExprKind::AnonRecord { fields, .. } => {
                for (_, ty) in fields {
                    self.type_expr(ty);
                }
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::StringLit(..)
            | ExprKind::Unit
            | ExprKind::Return(None) => {}
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    match part {
                        StringPart::Literal(_) => {}
                        StringPart::Expr(e) => self.expr(e),
                    }
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => self.expr(e),
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expr(key);
                    self.expr(value);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) | ExprKind::Recur(elems) => {
                for e in elems {
                    self.expr(e);
                }
            }
            ExprKind::Ident(name) => match expr.res {
                Some(Res::Local) => self.local(expr.span, *name, false, false),
                Some(Res::Module(_)) => self.qualifier(&Some(crate::ast::Qualifier {
                    name: *name,
                    span: expr.span,
                })),
                res => self.name(expr.span, res),
            },
            ExprKind::FieldAccess(receiver, field, field_span) => {
                // `m.f`, `m.Circle`, `Shape.Circle`: the member's own
                // name, which the resolver resolved. (The span of a
                // field access is its receiver's.) After a trait
                // (`Sized.size`) the name is the method the checker
                // selected; on a value it is a field.
                // (A record literal names its type too, `Pt { x: 1 }.x`,
                // but it is a value: `.x` behind it is a field.)
                let qualified = match receiver.res {
                    Some(Res::Module(_)) => true,
                    Some(Res::Def(_)) if matches!(receiver.kind, ExprKind::RecordCreate { .. }) => {
                        false
                    }
                    Some(Res::Def(id)) => self.table().get(id).is_type(),
                    Some(Res::Local | Res::Error) | None => false,
                };
                // `Int.empty`: the method of the trait the checker
                // selected, of the impl for the type.
                let through_type = match (receiver.res, expr.res) {
                    (Some(Res::Def(ty)), Some(Res::Def(tr))) if qualified => {
                        match (self.table().get(ty).kind, self.table().get(tr).kind) {
                            (crate::defs::DefKind::Type(ty), crate::defs::DefKind::Trait(tr)) => {
                                Some(Selection::Impl { tr, ty })
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(selection) = through_type {
                    self.methods.push(MethodUse {
                        span: *field_span,
                        name: *field,
                        selection,
                    });
                } else if qualified {
                    self.name(*field_span, expr.res);
                } else if let Some(
                    selection @ (Selection::Impl { .. }
                    | Selection::Native { .. }
                    | Selection::Dynamic { .. }),
                ) = expr.sel
                {
                    self.methods.push(MethodUse {
                        span: *field_span,
                        name: *field,
                        selection,
                    });
                } else if let Some(Selection::Field | Selection::FieldCall) = expr.sel
                    && let Some(record) = self.record_of(receiver.ty.as_ref())
                {
                    self.field(*field_span, record, *field, false);
                } else if field_span.is_in_source() {
                    self.unknown_reads.push((*field_span, *field));
                }
                self.expr(receiver);
            }
            ExprKind::Binary(lhs, _, rhs)
            | ExprKind::Pipe(lhs, rhs)
            | ExprKind::Range(lhs, rhs) => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Unary(_, e) | ExprKind::QuestionMark(e) | ExprKind::Return(Some(e)) => {
                self.expr(e);
            }
            ExprKind::Ascription(e, ty) => {
                self.expr(e);
                self.type_expr(ty);
            }
            ExprKind::Call(callee, args) => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::Lambda { params, body } => {
                for param in params {
                    self.pattern(&param.pattern);
                    if let Some(ty) = &param.ty {
                        self.type_expr(ty);
                    }
                }
                self.expr(body);
            }
            ExprKind::RecordCreate {
                module,
                name_span,
                fields,
                punned,
                ..
            } => {
                self.qualifier(module);
                self.name(*name_span, expr.res);
                let record = match expr.res {
                    Some(Res::Def(id)) => self.record(id),
                    _ => self.record_of(expr.ty.as_ref()),
                };
                self.field_values(record, name_span.end, fields, punned);
            }
            ExprKind::RecordUpdate {
                expr: base,
                fields,
                punned,
            } => {
                self.expr(base);
                let record =
                    (self.record_of(base.ty.as_ref())).or_else(|| self.record_of(expr.ty.as_ref()));
                self.field_values(record, base.span.start, fields, punned);
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(spread) = spread {
                    self.expr(spread);
                }
                for (_, value) in fields {
                    self.expr(value);
                }
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(scrutinee) = scrutinee {
                    self.expr(scrutinee);
                }
                for arm in arms {
                    self.pattern(&arm.pattern);
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    self.stmt(stmt);
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (name, span, init) in bindings {
                    self.expr(init);
                    self.local(*span, *name, true, false);
                }
                self.expr(body);
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { value, pattern, ty } => {
                self.pattern(pattern);
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
                self.expr(value);
            }
            Stmt::When {
                expr,
                else_body,
                pattern,
            } => {
                self.pattern(pattern);
                self.expr(expr);
                self.expr(else_body);
            }
            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                self.expr(condition);
                self.expr(else_body);
            }
            Stmt::Expr(e) => self.expr(e),
        }
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..) => {}
            PatternKind::Ident(name) => match pattern.res {
                Some(res @ Res::Def(_)) => self.name(pattern.span, Some(res)),
                Some(Res::Local | Res::Module(_) | Res::Error) | None => {
                    self.local(pattern.span, *name, true, false);
                }
            },
            PatternKind::Constructor {
                qualifier,
                name_span,
                args,
                ..
            } => {
                // (A module's name is lower-case, an enum's is not.)
                for segment in qualifier {
                    if resolve_sym(segment.name).starts_with(|c: char| c.is_ascii_lowercase()) {
                        self.qualifier(&Some(*segment));
                    }
                }
                self.name(*name_span, pattern.res);
                // `m.Shape.Circle(r)`, `Shape.Circle(r)`: the segment
                // that names the variant's enum.
                if let Some(Res::Def(variant)) = pattern.res
                    && let DefKind::Variant { ty, .. } = self.table().get(variant).kind
                {
                    let enum_name = self.table().get(ty.0).name;
                    for segment in qualifier.iter().filter(|q| q.name == enum_name) {
                        self.name(segment.span, Some(Res::Def(ty.0)));
                    }
                }
                for arg in args {
                    self.pattern(arg);
                }
            }
            PatternKind::Record {
                module,
                name_span,
                fields,
                ..
            } => {
                self.qualifier(module);
                self.name(*name_span, pattern.res);
                if let Some(Res::Def(record)) = pattern.res
                    && let Some(record) = self.record(record)
                {
                    for (field, span, sub) in fields {
                        self.field(*span, record, *field, sub.is_none());
                    }
                }
                self.field_patterns(fields);
            }
            PatternKind::AnonRecord { fields, rest } => {
                self.field_patterns(fields);
                if let Some((name, span)) = rest {
                    self.local(*span, *name, true, false);
                }
            }
            PatternKind::Tuple(pats) | PatternKind::Or(pats) => {
                for p in pats {
                    self.pattern(p);
                }
            }
            PatternKind::List(pats, rest) => {
                for p in pats {
                    self.pattern(p);
                }
                if let Some(rest) = rest {
                    self.pattern(rest);
                }
            }
            PatternKind::Map(entries) => {
                for (_, p) in entries {
                    self.pattern(p);
                }
            }
            // `^name` compares with the local `name`: a use of it,
            // behind the `^`.
            PatternKind::Pin(name) => {
                let len = resolve_sym(*name).len() as u32;
                if pattern.span.end >= pattern.span.start + len {
                    let span = Span {
                        start: pattern.span.end - len,
                        ..pattern.span
                    };
                    match pattern.res {
                        Some(res @ Res::Def(_)) => self.name(span, Some(res)),
                        Some(Res::Local | Res::Module(_) | Res::Error) | None => {
                            self.local(span, *name, false, false);
                        }
                    }
                }
            }
        }
    }

    /// The fields of a literal or an update of the record `record`,
    /// which starts at byte `from`: `{ x: e }` is the field `x` and `e`,
    /// the shorthand `{ x }` is the field `x` and a use of `x`; another
    /// name for that is written behind the field, `{ x: y }`.
    fn field_values(
        &mut self,
        record: Option<DefId>,
        from: u32,
        fields: &[(Symbol, Expr)],
        punned: &[Symbol],
    ) {
        for (field, value) in fields {
            match &value.kind {
                ExprKind::Ident(name) if punned.contains(field) => {
                    if let Some(record) = record {
                        self.field(value.span, record, *field, true);
                    }
                    match value.res {
                        Some(Res::Local) => self.local(value.span, *name, false, true),
                        res => {
                            if matches!(res, Some(Res::Def(_))) {
                                self.punned.push(value.span);
                            }
                            self.name(value.span, res);
                        }
                    }
                }
                _ => {
                    if let Some(record) = record
                        && let Some(span) = self.field_name_span(from, *field, value)
                    {
                        self.field(span, record, *field, false);
                    }
                    self.expr(value);
                }
            }
        }
    }

    /// The fields of a record pattern: `{ x: p }` matches `p`, the
    /// shorthand `{ x }` binds `x`.
    fn field_patterns(&mut self, fields: &[(Symbol, Span, Option<Pattern>)]) {
        for (name, span, sub) in fields {
            match sub {
                Some(sub) => self.pattern(sub),
                None => self.local(*span, *name, true, true),
            }
        }
    }
}
