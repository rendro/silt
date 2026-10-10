//! The unused-value error.
//!
//! A statement that is not the last of its block has no one to give its
//! value to, so it must have none: its type is `()` or `Never`.
//! Anything else is an error, and `let _ = ...` is how a value is
//! discarded on purpose.
//!
//! A value that is not a call (an operator expression, a literal, a
//! name, a field, a constructor) is unused whatever its type.
//!
//! A call whose type nothing has decided when the scope it is in is
//! settled is one of two things (`fix_statement_calls`):
//! - Its type is the type of something the caller of an enclosing
//!   function gives that function: a parameter, or data in one
//!   (`fn count(x) { dbg(x)  1 }` with `dbg` returning what it is
//!   given, `xs: List(a)`, `ch: Channel(a)`). Then it is a value, and it
//!   is unused: the same error as with the parameter annotated. Nobody's
//!   parameter becomes `()` because of a statement.
//! - Otherwise it is `()`: the result of a function the body was handed
//!   and calls as a statement (`fn run(f) { f()  "ran" }`), or of a
//!   call that returns whatever is asked of it (`fail()` that only
//!   panics). A later error about that `()` names the statement.

use super::*;

/// A call that stands as a statement and whose type is still unknown.
#[derive(Clone)]
pub(crate) struct StatementCall {
    ty: Type,
    /// The statement.
    span: Span,
    /// The functions and closures the statement is in.
    frames: Vec<FnFrame>,
}

/// A function or a closure whose body is being checked.
#[derive(Clone)]
pub(crate) struct FnFrame {
    pub(super) owner: Option<FrameOwner>,
    /// Its parameters' types.
    pub(super) params: Vec<Type>,
}

/// What names a function or closure where it is defined.
#[derive(Clone, Copy)]
pub(crate) enum FrameOwner {
    /// A top-level `fn`, or the top-level `let` a closure is the value of.
    TopLevel(Symbol),
    /// The `let` of a block a closure is the value of.
    Local(Symbol),
}

/// What a call's callee names.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Callee {
    Def(crate::defs::DefId),
    Local(Symbol),
}

/// The label of an error about a `()` that a statement call was given.
const STATEMENT_UNIT: &str = "`()` because this call is a statement: its value would be unused";

impl TypeChecker {
    /// Note the statement `stmt` of type `ty`, which is not the last of
    /// its block: a call whose type is still unknown waits for
    /// `fix_statement_calls`.
    pub(super) fn note_statement(&mut self, stmt: &Stmt, ty: &Type) {
        if let Stmt::Expr(e) = stmt
            && let Type::Var(v) = self.apply(ty)
            && self.is_call_like(e)
        {
            let call = StatementCall {
                ty: ty.clone(),
                span: e.span,
                frames: self.fn_frames.clone(),
            };
            self.await_statement_call(v, call);
        }
    }

    /// File a statement call, whose type is the unresolved variable
    /// `v`, under the scope that decides `v`.
    fn await_statement_call(&mut self, v: TyVar, call: StatementCall) {
        let level = self.tables.vars.level_of(v) as usize;
        if self.statement_calls.len() <= level {
            self.statement_calls.resize_with(level + 1, Vec::new);
        }
        self.statement_calls[level].push(call);
    }

    /// Decide the calls that stand as statements and whose type is a
    /// variable of the scope `exit_level` just left (with `all`, at the
    /// end of the module: whatever variable it still is). A variable of
    /// an outer binding waits: a later definition may decide it. One
    /// that a caller supplies stays as it is, and the statement is
    /// reported; any other is `()`. Returns the statements whose type
    /// became `()`: what the scope owes for those is to be checked
    /// again (`recheck_fixed`).
    ///
    /// Only the calls filed under the scopes that ended are looked at:
    /// one that waits for an outer scope is filed under that scope, and
    /// is not read again until it ends.
    pub(super) fn fix_statement_calls(&mut self, all: bool) -> Vec<Span> {
        let keep = match all {
            true => 0,
            false => self.tables.vars.level() as usize + 1,
        };
        let mut fixed = Vec::new();
        while self.statement_calls.len() > keep {
            for call in self.statement_calls.pop().unwrap_or_default() {
                match self.apply(&call.ty) {
                    Type::Var(v) if all || self.tables.vars.is_generalizable(v) => {
                        if self.caller_supplies(v, &call.frames) {
                            continue;
                        }
                        let handed = self.params_mentioning(v, &call.frames);
                        if self.unify_types(&call.ty, &Type::Unit).is_ok() {
                            fixed.push(call.span);
                            for (callee, param) in handed {
                                let units = self.statement_units.entry(callee).or_default();
                                units.push((param, call.span));
                            }
                        }
                    }
                    // (Its level is below the scopes that ended.)
                    Type::Var(v) => self.await_statement_call(v, call),
                    _ => {}
                }
            }
        }
        fixed
    }

    /// Check again, with `recheck`, what a scope owes for the types of
    /// the statements `fixed`, which are `()` now; an error that this
    /// reports inside one of the statements says where the `()` is from.
    pub(super) fn recheck_fixed(&mut self, fixed: &[Span], recheck: impl FnOnce(&mut Self)) {
        if fixed.is_empty() {
            return;
        }
        let from = self.errors.len();
        recheck(self);
        for d in self.errors.iter_mut().skip(from) {
            let within =
                |s: &&Span| s.file == d.span.file && s.start <= d.span.start && d.span.end <= s.end;
            if let Some(statement) = fixed.iter().find(within) {
                d.labels.push((*statement, STATEMENT_UNIT.to_string()));
            }
        }
    }

    /// Whether a value of the type `v` is given to one of the functions
    /// `frames` by its caller: `v` is the type of a parameter, or of
    /// data in one. (The result of a function a parameter holds is not
    /// given by the caller: the body gets it by calling.)
    fn caller_supplies(&self, v: TyVar, frames: &[FnFrame]) -> bool {
        frames
            .iter()
            .flat_map(|frame| &frame.params)
            .any(|param| holds(&self.apply(param), v))
    }

    /// The parameters of the functions `frames` whose types mention
    /// `v`, each with what names its function.
    fn params_mentioning(&self, v: TyVar, frames: &[FnFrame]) -> Vec<(Callee, usize)> {
        let mut found = Vec::new();
        for frame in frames {
            let Some(callee) = frame.owner.and_then(|owner| self.callee_of(owner)) else {
                continue;
            };
            for (i, param) in frame.params.iter().enumerate() {
                if free_vars_in(&self.apply(param)).contains(&v) {
                    found.push((callee, i));
                }
            }
        }
        found
    }

    /// What a call names when it calls the function of `owner`.
    fn callee_of(&self, owner: FrameOwner) -> Option<Callee> {
        match owner {
            FrameOwner::Local(name) => Some(Callee::Local(name)),
            FrameOwner::TopLevel(name) => {
                let defs = self.defs.as_ref()?;
                defs.of_module(self.module)
                    .iter()
                    .find(|id| {
                        let def = defs.get(**id);
                        def.name == name
                            && matches!(
                                def.kind,
                                crate::defs::DefKind::Fn | crate::defs::DefKind::Let
                            )
                    })
                    .map(|id| Callee::Def(*id))
            }
        }
    }

    /// The statement because of which the parameter `param` of the
    /// function `callee` names takes a function that returns `()`.
    pub(super) fn statement_unit(&self, callee: &Expr, param: usize) -> Option<Span> {
        if self.statement_units.is_empty() {
            return None;
        }
        let callee = match (callee.res, &callee.kind) {
            (Some(crate::defs::Res::Def(id)), _) => Callee::Def(id),
            (Some(crate::defs::Res::Local), ExprKind::Ident(name)) => Callee::Local(*name),
            _ => return None,
        };
        let units = self.statement_units.get(&callee)?;
        units
            .iter()
            .find(|(i, _)| *i == param)
            .map(|(_, statement)| *statement)
    }

    /// Forget what is known of the closures a body bound with `let`:
    /// the names are the body's.
    pub(super) fn forget_local_statement_units(&mut self) {
        self.statement_units
            .retain(|callee, _| matches!(callee, Callee::Def(_)));
    }

    /// The parameters of the enclosing functions whose types share a
    /// variable with `ty`, the type of an argument: what is asked of the
    /// argument is asked of them.
    pub(super) fn frames_sharing(&self, ty: &Type) -> Vec<(Callee, usize)> {
        let vars = free_vars_in(&self.apply(ty));
        let mut found = Vec::new();
        for v in vars {
            for at in self.params_mentioning(v, &self.fn_frames) {
                if !found.contains(&at) {
                    found.push(at);
                }
            }
        }
        found
    }

    /// An argument was checked against a parameter that returns `()`
    /// because of `statement`. The errors from `from` on are about that
    /// `()`: each names the statement, and the help about chaining
    /// Results, which a mismatch with a Result gets, does not apply. An
    /// argument that was accepted hands the `()` on to the parameters
    /// `passed_on` of the enclosing functions.
    pub(super) fn name_statement_unit(
        &mut self,
        statement: Span,
        from: usize,
        passed_on: Vec<(Callee, usize)>,
    ) {
        if self.errors.len() == from {
            for (callee, param) in passed_on {
                let units = self.statement_units.entry(callee).or_default();
                if !units.contains(&(param, statement)) {
                    units.push((param, statement));
                }
            }
            return;
        }
        for d in self.errors.iter_mut().skip(from) {
            d.help.clear();
            d.labels.push((statement, STATEMENT_UNIT.to_string()));
        }
    }

    /// Whether the value of `e` comes from calls alone: a call, a pipe,
    /// a `?`, or a `match`, block or loop whose every result does. A
    /// variant applied to values is a value, not a call.
    fn is_call_like(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call(callee, _) => !self.names_variant(callee),
            ExprKind::Pipe(_, right) => match &right.kind {
                ExprKind::Call(callee, _) => !self.names_variant(callee),
                _ => !self.names_variant(right),
            },
            ExprKind::QuestionMark(inner) | ExprKind::Ascription(inner, _) => {
                self.is_call_like(inner)
            }
            ExprKind::Match { arms, .. } => arms.iter().all(|arm| self.is_call_like(&arm.body)),
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(last)) => self.is_call_like(last),
                _ => true,
            },
            ExprKind::Loop { body, .. } => self.is_call_like(body),
            ExprKind::Return(_) | ExprKind::Recur(_) => true,
            _ => false,
        }
    }

    fn names_variant(&self, callee: &Expr) -> bool {
        matches!(
            callee.res,
            Some(crate::defs::Res::Def(id))
                if self.def(id).is_some_and(|def| matches!(def.kind, crate::defs::DefKind::Variant { .. }))
        )
    }

    /// Report every statement that leaves a value unused, the types
    /// being final.
    pub(super) fn check_unused_values(&mut self, program: &mut Program) {
        let mut found: Vec<Diagnostic> = Vec::new();
        let marks = std::mem::take(&mut program.statements);
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => self.unused_in(&mut f.body, &marks, &mut found),
                Decl::TraitImpl(ti) => {
                    for m in &mut ti.methods {
                        self.unused_in(&mut m.body, &marks, &mut found);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        self.unused_in(&mut m.body, &marks, &mut found);
                    }
                }
                Decl::Let { value, .. } => self.unused_in(value, &marks, &mut found),
                _ => {}
            }
        }
        program.statements = marks;
        self.errors.extend(found);
    }

    /// Whether `stmt` gives a value a next line could subtract from: a
    /// `let`, or an expression that is not of type `()`.
    fn gives_a_value(&self, stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Let { .. } => true,
            Stmt::Expr(e) => e.ty.as_ref().is_some_and(|ty| {
                !matches!(
                    self.statement_type(ty),
                    Type::Unit | Type::Never | Type::Error
                )
            }),
            Stmt::When { .. } | Stmt::WhenBool { .. } => false,
        }
    }

    /// The type of a statement as the rule reads it: an associated type
    /// of a known type is the type its impl binds (`<Printer as
    /// Sink>::Out` is `()` where the impl says `type Out = ()`).
    fn statement_type(&self, ty: &Type) -> Type {
        match self.apply(ty) {
            ty @ Type::AssocProj { .. } => {
                crate::types::canonical::canonicalize(&self.tables.resolver, &ty)
            }
            ty => ty,
        }
    }

    /// Report the unused values of `expr`'s blocks. `marks` are the
    /// program's.
    fn unused_in(&self, expr: &mut Expr, marks: &StatementMarks, found: &mut Vec<Diagnostic>) {
        resolve::each_expr_mut(expr, &mut |expr| {
            let ExprKind::Block(stmts) = &expr.kind else {
                return;
            };
            let Some((last, rest)) = stmts.split_last() else {
                return;
            };
            for (i, stmt) in rest.iter().enumerate() {
                let Stmt::Expr(e) = stmt else { continue };
                // (A statement the checker gave no type is in a body it
                // did not check.)
                let Some(ty) = &e.ty else { continue };
                let message = match self.statement_type(ty) {
                    Type::Unit | Type::Never | Type::Error => continue,
                    Type::Var(_) => {
                        "this value is unused; write `let _ = ...` to discard it".to_string()
                    }
                    // An associated type of a type that is not known
                    // here (`s.next()` with `s: a where a: Source`): the
                    // caller's type decides it, so it is a value.
                    ty @ Type::AssocProj { .. } if !free_vars_in(&ty).is_empty() => {
                        "this value is unused; write `let _ = ...` to discard it".to_string()
                    }
                    ty if ty.is_builtin("Result") => {
                        "this `Result` is unused: an error in it would go unseen; handle it, \
                         return it with `?`, or write `let _ = ...`"
                            .to_string()
                    }
                    ty if holds_result(&ty) => format!(
                        "this `{}` value is unused: it holds a `Result` whose error would go \
                         unseen; handle it, or write `let _ = ...`",
                        self.show_type(&ty)
                    ),
                    ty => format!(
                        "this `{}` value is unused; write `let _ = ...` to discard it",
                        self.show_type(&ty)
                    ),
                };
                // The statement, from its first token: the parentheses
                // around what it starts with are part of it.
                let extent = marks
                    .parenthesised
                    .iter()
                    .find(|(inner, _)| *inner == e.span)
                    .map_or(e.span, |(_, extent)| *extent);
                let mut d = Diagnostic::error(Code::UnusedValue, extent, message);
                // `point { x: 1 }`: `let _ = ` before the `{` would not
                // parse, and is not what is missing.
                if marks
                    .brace_after_name
                    .iter()
                    .any(|at| at.file == extent.file && at.start == extent.start)
                {
                    d = d.with_help(
                        "a record literal's type name starts with an upper-case letter \
                         (`Point { ... }`); a `{` behind a value starts a statement of its own",
                    );
                } else {
                    let at = Span::point(extent.file, extent.start);
                    d = d.with_fix("Discard with `let _ =`", vec![(at, "let _ = ".to_string())]);
                }
                // A line that starts with `-` does not continue the
                // line above. Said where the line above is something to
                // subtract from, on whichever of the two is unused: this
                // one behind a value, or this one in front of a `-` line
                // that ends the block (and is its value).
                if i > 0 && starts_with_minus(e) && self.gives_a_value(&rest[i - 1]) {
                    d = d.with_help(
                        "a line that starts with `-` is a statement of its own; to subtract \
                         from the line above, end that line with the `-`",
                    );
                } else if i + 1 == rest.len()
                    && matches!(last, Stmt::Expr(next) if starts_with_minus(next))
                {
                    d = d.with_help(
                        "the next line starts with `-`, so it is a statement of its own; to \
                         subtract it from this line, end this line with the `-`",
                    );
                }
                found.push(d);
            }
        });
    }
}

/// Whether a value of type `ty` holds one of the type `v`: `v` itself,
/// or a part of data. A function holds no value of the types it
/// mentions.
fn holds(ty: &Type, v: TyVar) -> bool {
    match ty {
        Type::Var(w) => *w == v,
        Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => holds(t, v),
        Type::Map(key, value) => holds(key, v) || holds(value, v),
        Type::Tuple(ts) | Type::Generic(_, ts) => ts.iter().any(|t| holds(t, v)),
        Type::AnonRecord { fields, .. } => fields.values().any(|t| holds(t, v)),
        // The caller's type decides the associated type.
        Type::AssocProj { receiver, .. } => holds(receiver, v),
        Type::Fun(..)
        | Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => false,
    }
}

/// Whether a value of type `ty` holds a `Result`: in a list, an option,
/// a tuple, a task's handle, a channel, a record's fields. (A function
/// that returns one holds none.)
fn holds_result(ty: &Type) -> bool {
    match ty {
        Type::Generic(_, args) => ty.is_builtin("Result") || args.iter().any(holds_result),
        Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => holds_result(t),
        Type::Map(key, value) => holds_result(key) || holds_result(value),
        Type::Tuple(ts) => ts.iter().any(holds_result),
        Type::AnonRecord { fields, .. } => fields.values().any(holds_result),
        Type::Fun(..)
        | Type::AssocProj { .. }
        | Type::Var(_)
        | Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => false,
    }
}

/// Whether the text of `e` starts with a unary minus.
fn starts_with_minus(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Unary(UnaryOp::Neg, _) => true,
        ExprKind::Binary(left, _, _)
        | ExprKind::Pipe(left, _)
        | ExprKind::Range(left, _)
        | ExprKind::Call(left, _)
        | ExprKind::FieldAccess(left, _, _)
        | ExprKind::QuestionMark(left)
        | ExprKind::Ascription(left, _) => starts_with_minus(left),
        _ => false,
    }
}
