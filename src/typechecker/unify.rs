use super::*;

/// One session's type-variable supply: the substitution and the next
/// variable. Every module's check allocates from it, so a scheme one
/// module exports is valid in the next as it is.
///
/// Each variable has a level: how deep in generalisation scopes it was
/// made. The check of a `let`'s value, or of a group of top-level
/// definitions, runs one level deeper than what surrounds it, and the
/// variables still at the deeper level when it ends are the ones no
/// outer binding mentions: those are generalised. Binding a variable to
/// a type lowers the variables of the type to its own level, so a
/// variable an outer binding has reached is never generalised.
#[derive(Clone, Default)]
pub struct TyVarSupply {
    /// The substitution: maps type variables to their resolved types.
    pub(super) subst: Vec<Option<Type>>,
    /// The level of each variable.
    levels: Vec<u32>,
    /// The level new variables are made at.
    level: u32,
    /// Counter for generating fresh type variables.
    pub(super) next: TyVar,
    /// The first variable of the module being checked.
    base: TyVar,
    /// The variables below `base` the module's check bound: an earlier
    /// module's, which a later module may resolve (`pub let xs = []`).
    pub(super) trail: Vec<TyVar>,
    /// The variables each checked module allocated, in the order of the
    /// checks, and whether the module was forgotten since.
    ranges: Vec<(crate::session::ModuleId, TyVar, bool)>,
}

impl TyVarSupply {
    /// Resolve the variable `v` to `t`.
    pub(super) fn bind(&mut self, v: TyVar, t: Type) {
        if v < self.base {
            self.trail.push(v);
        }
        let t = self.without_never(t);
        self.lower(&t, self.levels[v]);
        self.subst[v] = Some(t);
    }

    /// `t` with a new variable for each `Never` in it. `Never` is the
    /// type of an expression that diverges and meets every type without
    /// deciding it; inside a type a variable keeps (the result of a
    /// function that never returns, `Fn(String) -> Never` of `panic`
    /// taken as a value) it would meet every later type of that place
    /// without deciding it: `[panic, { s -> 42 }]` is a list of
    /// `Fn(String) -> Int`, as with the closure `{ s -> panic(s) }`.
    fn without_never(&mut self, t: Type) -> Type {
        fn holds_never(t: &Type) -> bool {
            match t {
                Type::Never => true,
                Type::Fun(params, ret) => params.iter().any(holds_never) || holds_never(ret),
                Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => holds_never(inner),
                Type::Map(k, v) | Type::App(k, v) => holds_never(k) || holds_never(v),
                Type::Tuple(parts) | Type::Generic(_, parts) | Type::Con(_, parts) => {
                    parts.iter().any(holds_never)
                }
                _ => false,
            }
        }
        if !holds_never(&t) {
            return t;
        }
        let each = |ts: &[Type], this: &mut Self| -> Vec<Type> {
            ts.iter().map(|t| this.without_never(t.clone())).collect()
        };
        match t {
            Type::Never => Type::Var(self.fresh()),
            Type::Fun(params, ret) => {
                let params = each(&params, self);
                Type::Fun(params, Box::new(self.without_never(*ret)))
            }
            Type::List(inner) => Type::List(Box::new(self.without_never(*inner))),
            Type::Set(inner) => Type::Set(Box::new(self.without_never(*inner))),
            Type::Channel(inner) => Type::Channel(Box::new(self.without_never(*inner))),
            Type::Map(k, v) => {
                let k = self.without_never(*k);
                Type::Map(Box::new(k), Box::new(self.without_never(*v)))
            }
            Type::App(head, arg) => {
                let head = self.without_never(*head);
                Type::applied(head, self.without_never(*arg))
            }
            Type::Tuple(parts) => Type::Tuple(each(&parts, self)),
            Type::Generic(name, args) => Type::Generic(name, each(&args, self)),
            Type::Con(head, rest) => Type::Con(head, each(&rest, self)),
            other => other,
        }
    }

    /// A new variable, at the current level.
    fn fresh(&mut self) -> TyVar {
        let v = self.next;
        self.next += 1;
        self.subst.push(None);
        self.levels.push(self.level);
        v
    }

    /// Lower the unresolved variables of `t` to `level`, those above it.
    pub(super) fn lower(&mut self, t: &Type, level: u32) {
        for v in free_vars_in(t) {
            match self.subst[v].take() {
                Some(resolved) => {
                    self.lower(&resolved, level);
                    self.subst[v] = Some(resolved);
                }
                None => self.levels[v] = self.levels[v].min(level),
            }
        }
    }

    /// Whether the unresolved variable `v` is outside every declaration:
    /// a declaration and its body are checked at least one level deep.
    pub(super) fn is_outermost(&self, v: TyVar) -> bool {
        self.levels[v] == 0
    }

    /// Whether the unresolved variable `v` is above the current level:
    /// nothing outside the scope that just ended mentions it.
    pub(super) fn is_generalizable(&self, v: TyVar) -> bool {
        self.levels[v] > self.level
    }

    /// The current level.
    pub(super) fn level(&self) -> u32 {
        self.level
    }

    /// The level of the unresolved variable `v`: the outermost scope
    /// that mentions it.
    pub(super) fn level_of(&self, v: TyVar) -> u32 {
        self.levels[v]
    }

    /// Start the check of `module`: its variables come next.
    pub(super) fn begin(&mut self, module: crate::session::ModuleId) {
        self.level = 0;
        self.base = self.next;
        self.trail.clear();
        self.ranges.push((module, self.next, false));
    }

    /// Forget the check of `module`: undo what it bound of the earlier
    /// modules' variables (`trail`), and give back the variables of the
    /// checks at the end of the supply that are all forgotten, so a long
    /// session (an editor, a REPL) does not grow with each check again.
    pub(super) fn forget(&mut self, module: crate::session::ModuleId, trail: &[TyVar]) {
        for v in trail {
            if let Some(slot) = self.subst.get_mut(*v) {
                *slot = None;
            }
        }
        for range in self.ranges.iter_mut().filter(|r| r.0 == module) {
            range.2 = true;
        }
        while let Some(&(_, start, true)) = self.ranges.last() {
            self.ranges.pop();
            self.subst.truncate(start);
            self.levels.truncate(start);
            self.next = start;
        }
    }
}

/// Why two types do not unify: one fault for each part that does not.
/// `TypeChecker::unify` reports it; a caller with a better message for
/// its place (a list element, an operand) writes its own.
pub(super) struct Mismatch(Vec<Fault>);

/// One part of two types that does not unify.
pub(super) struct Fault {
    code: Code,
    message: String,
    help: Option<String>,
    /// The two types (got, expected) when their heads differ: the report
    /// offers to wrap a value in `Ok(...)` where a `Result` is expected.
    apart: Option<(Type, Type)>,
    /// What the report says of the parts (a type constructor variable
    /// against a type that has none, or another first argument).
    note: Option<String>,
}

impl Fault {
    fn new(code: Code, message: impl Into<String>) -> Fault {
        Fault {
            code,
            message: message.into(),
            help: None,
            apart: None,
            note: None,
        }
    }
}

impl TypeChecker {
    // ── Fresh variables ─────────────────────────────────────────────

    pub(super) fn fresh_var(&mut self) -> Type {
        Type::Var(self.tables.vars.fresh())
    }

    /// Enter a generalisation scope: the variables made in it, and not
    /// reached from outside it by the time `exit_level` is called, are
    /// what `generalize` quantifies.
    pub(super) fn enter_level(&mut self) {
        self.tables.vars.level += 1;
        self.wanted_marks.push(self.wanted.len());
        self.given_marks.push(self.given_waiting.len());
    }

    /// Leave the scope `enter_level` entered.
    pub(super) fn exit_level(&mut self) {
        self.tables.vars.level -= 1;
        self.closed_mark = self.wanted_marks.pop().expect("a level is open");
        self.given_closed = self.given_marks.pop().expect("a level is open");
    }

    /// Decide what the scope `exit_level` just left can decide: what it
    /// owes for a type it has decided since.
    pub(super) fn decide_closed(&mut self) {
        self.reopen_level();
        self.solve_wanted(self.closed_mark);
        self.close_level();
        self.decide_tries();
    }

    /// Make variables as the scope `exit_level` just left did, until
    /// `close_level`: what is decided for the scope after it has ended
    /// is the scope's.
    pub(super) fn reopen_level(&mut self) {
        self.tables.vars.level += 1;
    }

    pub(super) fn close_level(&mut self) {
        self.tables.vars.level -= 1;
    }

    /// Keep the unresolved variables of `ty` out of every later
    /// generalisation at this level: `ty` is the type of a binding that
    /// is not generalised.
    pub(super) fn keep_monomorphic(&mut self, ty: &Type) {
        let level = self.tables.vars.level;
        self.tables.vars.lower(ty, level);
    }

    /// Allocate a fresh `TyVar` id without wrapping it in `Type::Var`.
    pub(super) fn fresh_tyvar_id(&mut self) -> TyVar {
        self.tables.vars.fresh()
    }

    // ── Substitution / apply ────────────────────────────────────────

    /// Walk the substitution chain to find the most resolved type.
    pub(super) fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::Var(v) => {
                if let Some(Some(resolved)) = self.tables.vars.subst.get(*v) {
                    self.apply(resolved)
                } else {
                    ty.clone()
                }
            }
            Type::Fun(params, ret) => {
                let params = params.iter().map(|p| self.apply(p)).collect();
                let ret = Box::new(self.apply(ret));
                Type::Fun(params, ret)
            }
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.apply(e)).collect()),
            Type::Generic(name, args) => {
                let args = args.iter().map(|a| self.apply(a)).collect();
                Type::Generic(*name, args)
            }
            Type::Map(k, v) => Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v))),
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            Type::Channel(inner) => Type::Channel(Box::new(self.apply(inner))),
            Type::Rigid(r) if !self.rigid_alias.is_empty() => Type::Rigid(self.rigid_rep(*r)),
            // A constructor variable applied, once the variable is a
            // constructor, is the full type (`Option(_)` at `Int` is
            // `Option(Int)`).
            Type::App(head, arg) => Type::applied(self.apply(head), self.apply(arg)),
            Type::Con(head, rest) => Type::Con(*head, rest.iter().map(|t| self.apply(t)).collect()),
            _ => ty.clone(),
        }
    }

    /// The annotation variable `r` stands for, among those the group of
    /// functions being inferred has made the same (`same_in_group`).
    fn rigid_rep(&self, mut r: RigidId) -> RigidId {
        while let Some(next) = self.rigid_alias.get(&r.var) {
            r = *next;
        }
        r
    }

    /// The variable the annotation variable `var` stands for in a scheme.
    pub(super) fn rigid_rep_var(&self, mut var: TyVar) -> TyVar {
        while let Some(next) = self.rigid_alias.get(&var) {
            var = next.var;
        }
        var
    }

    /// Make two annotation variables the same, if they may be: they are
    /// variables of two different functions of the group that is being
    /// inferred together. Inside the group each function has one type, so
    /// `fn ev(x: a, n) { .. od(x, n) }` and `fn od(y: b, n) { .. ev(y, n) }`
    /// say that `a` and `b` are one variable of the group; each function
    /// stays general in it. Two variables of one function stay apart.
    fn same_in_group(&mut self, r1: RigidId, r2: RigidId) -> bool {
        let (Some(owners1), Some(owners2)) = (
            self.group_rigid.get(&r1.var).cloned(),
            self.group_rigid.get(&r2.var).cloned(),
        ) else {
            return false;
        };
        if owners1.iter().any(|owner| owners2.contains(owner)) {
            return false;
        }
        self.rigid_alias.insert(r2.var, r1);
        let owners = self.group_rigid.entry(r1.var).or_default();
        owners.extend(owners2);
        // What either's `where` clauses declare holds of the one variable.
        if let Some(bounds) = self.bounds.get(&r2.var).cloned() {
            let merged = self.bounds.entry(r1.var).or_default();
            for bound in bounds {
                if !merged.contains(&bound) {
                    merged.push(bound);
                }
            }
        }
        true
    }

    // ── Unification ─────────────────────────────────────────────────

    /// The fields of the declared record `name` at the type arguments
    /// `args`, each with its type. With a wrong number of arguments
    /// (reported where the type is written) each parameter is a fresh
    /// variable: the record's own parameter variables are shared by
    /// every use of it, and a use that decided one would decide it for
    /// all. No fields for a name that is no record.
    pub(super) fn instantiate_record_fields_with_args(
        &mut self,
        name: TypeRef,
        args: &[Type],
    ) -> Vec<(Symbol, Type)> {
        if let Some(rec_info) = self.tables.records.get(&name).cloned() {
            if let Some(param_var_ids) = self.tables.record_param_var_ids.get(&name).cloned() {
                let mapping: HashMap<TyVar, Type> = if args.len() == param_var_ids.len() {
                    param_var_ids
                        .iter()
                        .zip(args.iter())
                        .map(|(&v, t)| (v, t.clone()))
                        .collect()
                } else {
                    param_var_ids
                        .iter()
                        .map(|&v| (v, self.fresh_var()))
                        .collect()
                };
                rec_info
                    .fields
                    .iter()
                    .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                    .collect()
            } else {
                rec_info.fields
            }
        } else {
            Vec::new()
        }
    }

    /// The one wording of the occurs-check diagnostic.
    pub fn infinite_type_message(t: &Type) -> String {
        format!("infinite type: the type variable appears inside {t}")
    }

    /// Resolve the variable `v` to `t`, unless that would take an
    /// annotation variable out of its declaration: a variable of
    /// something defined outside every declaration being checked (a
    /// top-level `let` that is not generalised, an earlier module's
    /// value) cannot stand for a type only one declaration's body knows.
    fn bind(&mut self, v: TyVar, t: Type, out: &mut Vec<Fault>) {
        if self.tables.vars.is_outermost(v)
            && let Some(r) = rigid_in(&self.apply(&t))
        {
            // (Said once of a variable; it stays unbound, so what else is
            // wrong with it is still said.)
            if !self.escaped_vars.insert(v) {
                return;
            }
            out.push(Fault::new(
                Code::TypeMismatch,
                format!(
                    "the type variable `{0}` would escape its declaration: inside it `{0}` \
                     stands for any type, but here it would become the type of a value \
                     defined outside",
                    r.name
                ),
            ));
            return;
        }
        self.tables.vars.bind(v, t);
    }

    /// Unify `t1` (the type got) with `t2` (the type expected) and report
    /// at `span` why they do not unify.
    pub(super) fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
        if let Err(mismatch) = self.unify_types(t1, t2) {
            self.report_mismatch(mismatch, span);
        }
    }

    /// Unify `t1` (the type got) with `t2` (the type expected). Every
    /// part that does unify is unified, whatever part does not; the
    /// error says why each of those does not, and nothing is reported.
    pub(super) fn unify_types(&mut self, t1: &Type, t2: &Type) -> Result<(), Mismatch> {
        let mut faults = Vec::new();
        self.unify_into(t1, t2, &mut faults);
        if faults.is_empty() {
            Ok(())
        } else {
            Err(Mismatch(faults))
        }
    }

    /// Unify the type of a record field's value (`got`) with the type
    /// the record declares for the field, and report at the value
    /// (`span`) why they do not unify. Where their heads or a part of
    /// them differ, the error names the field and shows both types
    /// whole: "field `age` expects Int, got String". Its help is what
    /// the parts that differ say; the fix to wrap the value in `Ok(...)`
    /// is offered where the field is a `Result` and the value is not.
    pub(super) fn unify_field(&mut self, field: Symbol, got: &Type, declared: &Type, span: Span) {
        let Err(Mismatch(faults)) = self.unify_types(got, declared) else {
            return;
        };
        let (apart, others): (Vec<Fault>, Vec<Fault>) =
            faults.into_iter().partition(|fault| fault.apart.is_some());
        if let Some(first) = apart.first() {
            let (got, declared) = (self.apply(got), self.apply(declared));
            let (got_shown, declared_shown) = self.show_apart(&got, &declared);
            let mut d = Diagnostic::error(
                Code::TypeMismatch,
                span,
                format!("field `{field}` expects {declared_shown}, got {got_shown}"),
            );
            for help in apart.iter().filter_map(|fault| fault.help.clone()) {
                if !d.help.contains(&help) {
                    d.help.push(help);
                }
            }
            Self::add_ok_wrap_fix(&mut d, &got, &declared);
            if let Some((got_part, declared_part)) = &first.apart {
                d = self.name_annotation_variables(d, declared_part, got_part);
            }
            self.errors.push(d);
        }
        if !others.is_empty() {
            self.report_mismatch(Mismatch(others), span);
        }
    }

    /// Unify the type of a pattern (`got`) with the type of what it
    /// matches (`expected`) and report at the pattern (`span`) why they
    /// do not unify. A pattern's mismatch has no help and no fix: those
    /// say what to do with an expression (`?`, `Ok(...)`, a spread). In
    /// an arm of a `match`, a mismatch an earlier arm's pattern reported
    /// with the same text is not reported again (`arm_mismatches`).
    pub(super) fn unify_pattern(&mut self, got: &Type, expected: &Type, span: Span) {
        let Err(Mismatch(faults)) = self.unify_types(got, expected) else {
            return;
        };
        for fault in faults {
            if let Some(seen) = &mut self.arm_mismatches
                && !seen.insert(fault.message.clone())
            {
                continue;
            }
            let d = self.mismatch_diagnostic(fault, span, false);
            self.errors.push(d);
        }
    }

    /// Report a failed unification at `span`.
    pub(super) fn report_mismatch(&mut self, mismatch: Mismatch, span: Span) {
        for fault in mismatch.0 {
            let d = self.mismatch_diagnostic(fault, span, true);
            self.errors.push(d);
        }
    }

    /// The diagnostic of `fault` at `span`; with its help and its fix
    /// (`advice`), or only with its notes.
    fn mismatch_diagnostic(&self, fault: Fault, span: Span, advice: bool) -> Diagnostic {
        let mut d = Diagnostic::error(fault.code, span, fault.message);
        if advice {
            d.help.extend(fault.help);
        }
        if let Some(note) = fault.note {
            d = d.with_note(note);
        }
        if let Some((got, expected)) = &fault.apart {
            if advice {
                Self::add_ok_wrap_fix(&mut d, got, expected);
            }
            d = self.name_annotation_variables(d, expected, got);
        }
        d
    }

    /// A mismatch of which one side (or each) is an annotation variable
    /// says which variable that is and shows where it was written: `a`
    /// in "expected a, got Int" is not a type the reader can look up.
    fn name_annotation_variables(
        &self,
        mut d: Diagnostic,
        expected: &Type,
        got: &Type,
    ) -> Diagnostic {
        let mut named: Vec<RigidId> = Vec::new();
        for ty in [expected, got] {
            // (A type constructor variable applied, `m(Int)`, by its own.)
            let ty = match ty {
                Type::App(head, _) => &**head,
                _ => ty,
            };
            if let Type::Rigid(r) = ty
                && !named.contains(r)
            {
                named.push(*r);
            }
        }
        for r in &named {
            if let Some(written) = self.var_written.get(&r.var) {
                d = d.with_label(
                    *written,
                    format!("the type variable `{}` is declared here", r.name),
                );
            }
        }
        // The `self` of an impl for every function or every tuple.
        if let Some((r, what, parts)) = named.iter().find_map(|r| {
            let (what, parts) = self.shape_vars.get(&r.var)?;
            Some((r, *what, *parts))
        }) {
            return d.with_note(format!(
                "in an impl for `{}`, `self` is some {what}: {parts} are not known",
                r.name
            ));
        }
        let constructors = (named.iter()).all(|r| self.constructor_vars.contains(&r.var));
        match named.as_slice() {
            [] => d,
            // (Of type constructor variables, `m(Int)`.)
            [r] if constructors => d.with_note(format!(
                "`{}` is a type constructor variable of the annotation: it stands for any type \
                 constructor, so the code must check whichever one that is",
                r.name
            )),
            [r, s, ..] if constructors => d.with_note(format!(
                "`{}` and `{}` are type constructor variables of the annotation: each stands for \
                 any type constructor, so the code must check whichever ones they are",
                r.name, s.name
            )),
            [r] => d.with_note(format!(
                "`{}` is a type variable of the annotation: it stands for any type, so the \
                 code must check whichever type that is",
                r.name
            )),
            [r, s, ..] => d.with_note(format!(
                "`{}` and `{}` are type variables of the annotation: each stands for any \
                 type, so the code must check whichever types they are",
                r.name, s.name
            )),
        }
    }

    /// The fault of two types whose heads differ.
    fn type_mismatch(&self, got: &Type, expected: &Type) -> Fault {
        let (got_shown, expected_shown) = self.show_apart(got, expected);
        // (A type given where a value is asked, or a value where a type
        // is: said in those words, not as `type _`.)
        let held = |ty: &Type| match self.apply(ty) {
            Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                args.first().cloned()
            }
            _ => None,
        };
        let message = match (held(got), held(expected)) {
            (None, Some(_)) => {
                format!("type mismatch: expected a type, got a value of type `{got_shown}`")
            }
            (Some(given), None) => format!(
                "type mismatch: expected a value of type `{expected_shown}`, got the type `{}`",
                self.show_type(&self.apply(&given))
            ),
            _ => format!("type mismatch: expected {expected_shown}, got {got_shown}"),
        };
        Fault {
            code: Code::TypeMismatch,
            message,
            help: Self::chain_hint(got, expected),
            apart: Some((got.clone(), expected.clone())),
            note: None,
        }
    }

    /// The fault of a type constructor variable applied that meets
    /// another type: no help of the types' own (an `Option`'s `?`) fits
    /// a variable that stands for any constructor.
    fn constructor_mismatch(&self, got: &Type, expected: &Type) -> Fault {
        Fault {
            help: None,
            ..self.type_mismatch(got, expected)
        }
    }

    fn unify_into(&mut self, t1: &Type, t2: &Type, out: &mut Vec<Fault>) {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        match (&t1, &t2) {
            // A type still unknown that meets what has no type (a value
            // of a module that failed to load, an expression already
            // reported) has none either: nothing more is asked of it.
            (Type::Var(v), Type::Error) | (Type::Error, Type::Var(v)) => {
                self.bind(*v, Type::Error, out);
            }
            (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never) => {}
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => {}

            (Type::Var(v1), Type::Var(v2)) if v1 == v2 => {}

            // An annotation variable is itself only, or a variable the
            // group being inferred has made the same.
            (Type::Rigid(r1), Type::Rigid(r2)) if r1 == r2 || self.same_in_group(*r1, *r2) => {}

            // Two variables: the newer one becomes the older one. A
            // variable that many uses are unified with (a parameter
            // without an annotation, a `let` whose type is still open)
            // then stays the one the others resolve to in one step;
            // bound the other way round, each use would add a link to
            // a chain every later `apply` walks.
            (Type::Var(v1), Type::Var(v2)) => {
                let (newer, older) = (*v1.max(v2), *v1.min(v2));
                self.bind(newer, Type::Var(older), out);
            }

            (Type::Var(v), t) | (t, Type::Var(v)) => {
                if occurs_in(*v, t) {
                    // Special case: trying to unify a type var `a` with
                    // `TypeOf(a)` means the user returned a `type a`
                    // parameter's value where they promised a value of
                    // type `a`. The descriptor is the *type*, not an
                    // instance of it.
                    if let Type::Generic(name, args) = t
                        && name.is_builtin(crate::defs::TYPE_OF)
                        && args.len() == 1
                        && matches!(&args[0], Type::Var(inner) if inner == v)
                    {
                        out.push(Fault::new(
                            Code::TypeMismatch,
                            "cannot return a `type a` parameter as a value of type `a` — \
                             the parameter is a type descriptor, not an instance. \
                             Construct an `a` in the body instead."
                                .to_string(),
                        ));
                    } else {
                        // (A type constructor variable by its name.)
                        out.push(Fault::new(
                            Code::InfiniteType,
                            format!(
                                "infinite type: the type variable appears inside {}",
                                self.show_type(t)
                            ),
                        ));
                    }
                } else {
                    self.bind(*v, t.clone(), out);
                }
            }

            // Two type constructor variables applied: one constructor,
            // one argument.
            (Type::App(h1, a1), Type::App(h2, a2)) => {
                let mut heads = Vec::new();
                self.unify_into(h1, h2, &mut heads);
                if !heads.is_empty() {
                    out.push(self.constructor_mismatch(&t1, &t2));
                    return;
                }
                self.unify_into(a1, a2, out);
            }

            // A type constructor variable applied, against a type: the
            // variable is the type's constructor, its argument the type's
            // first type argument (`m(Int)` is `Result(Int, e)` with `m`
            // `Result(_, e)`).
            (Type::App(head, arg), other) | (other, Type::App(head, arg)) => {
                let applied = match &t1 {
                    Type::App(..) => self.show_type(&t1),
                    _ => self.show_type(&t2),
                };
                // (The notes say what the expected `m(Int)` matches: of a
                // value of the type given where a type is expected, the
                // mismatch says it all.)
                let expected_applied = matches!(t2, Type::App(..));
                let Some((con, rest, first)) = other.split_constructor() else {
                    let mut fault = self.constructor_mismatch(&t1, &t2);
                    if expected_applied {
                        fault.note = Some(format!(
                            "'{}' stands for a type constructor: it matches a type with a type \
                             argument, like 'List(Int)' or 'Option(Int)'",
                            self.show_type(head)
                        ));
                    }
                    out.push(fault);
                    return;
                };
                // (Of a type of one type argument, there is no other to
                // say it is matched against.)
                let others = !rest.is_empty() && expected_applied;
                let mut heads = Vec::new();
                self.unify_into(head, &Type::Con(con, rest), &mut heads);
                if !heads.is_empty() {
                    out.push(self.constructor_mismatch(&t1, &t2));
                    return;
                }
                let mut args = Vec::new();
                self.unify_into(arg, &first, &mut args);
                if !args.is_empty() {
                    let mut fault = self.constructor_mismatch(&t1, &t2);
                    if others {
                        fault.note = Some(format!(
                            "'{applied}' is matched against the first type argument, '{}' here",
                            self.show_type(&self.apply(&first))
                        ));
                    }
                    out.push(fault);
                }
            }

            // Two type constructors: one head, the same other arguments.
            (Type::Con(c1, r1), Type::Con(c2, r2)) if c1 == c2 && r1.len() == r2.len() => {
                for (x, y) in r1.iter().zip(r2) {
                    self.unify_into(x, y, out);
                }
            }

            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    // Directional convention: t1 is the "got" side, t2 is
                    // the "expected" side (see the Record arm below and
                    // every unify() call site — e.g. `unify(body_ty,
                    // ret_ty, span)` passes got then expected). Earlier
                    // rounds formatted `p1.len()` as "expected", which
                    // reversed the diagnostic.
                    let (exp, got) = (p2.len(), p1.len());
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "function expects {exp} {arg_word}, got {got}",
                            arg_word = if exp == 1 { "argument" } else { "arguments" }
                        ),
                    ));
                } else {
                    for (a, b) in p1.iter().zip(p2.iter()) {
                        self.unify_into(a, b, out);
                    }
                    self.unify_into(r1, r2, out);
                }
            }

            (Type::List(a), Type::List(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                self.unify_into(k1, k2, out);
                self.unify_into(v1, v2, out);
            }

            (Type::Set(a), Type::Set(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Channel(a), Type::Channel(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.len() != b.len() {
                    // Directional convention: t1 (=a) is the "got" side,
                    // t2 (=b) is the "expected" side. Earlier wording
                    // "expected {a.len()}, got {b.len()}" reversed this.
                    out.push(Fault::new(
                        Code::TypeMismatch,
                        format!(
                            "tuple length mismatch: expected {}, got {}",
                            b.len(),
                            a.len()
                        ),
                    ));
                } else {
                    for (x, y) in a.iter().zip(b.iter()) {
                        self.unify_into(x, y, out);
                    }
                }
            }

            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                if n1 != n2 {
                    // Formatting the bare
                    // `Symbol` heads would drop type args (`Bag(String)`
                    // → `Bag`) and show the internal `TypeOf` head
                    // (`type Person` → `TypeOf`). Format via the parent
                    // `Type` values so `Type::Display`'s args rendering
                    // and TypeOf special-casing apply (mirrors the
                    // catch-all arm at the bottom of `unify`).
                    out.push(self.type_mismatch(&t1, &t2));
                } else if a1.len() != a2.len() {
                    // (In the one form for a number of type arguments:
                    // the number the type takes.)
                    let message = self.type_arguments_message(&self.spell_type(*n1), *n1);
                    out.push(Fault::new(Code::ArityMismatch, message));
                } else {
                    for (x, y) in a1.iter().zip(a2.iter()) {
                        self.unify_into(x, y, out);
                    }
                }
            }

            _ => {
                // Suppress cascade errors where either side is already in
                // error state — a previous diagnostic explained the root
                // cause and further mismatch reports would confuse.
                if matches!(&t1, Type::Error) || matches!(&t2, Type::Error) {
                    return;
                }
                // When either side is an unresolved type variable, the
                // user doesn't have a user-facing name for it yet
                // (`?17` is internal). Report as "cannot determine" and
                // nudge toward an annotation.
                match (&t1, &t2) {
                    (Type::Var(_), other) | (other, Type::Var(_)) => {
                        out.push(Fault::new(
                            Code::AmbiguousType,
                            format!(
                                "cannot determine a consistent type here; \
                                 one side resolved to `{other}` but the other \
                                 is still unspecified — add a type annotation"
                            ),
                        ));
                    }
                    _ => {
                        out.push(self.type_mismatch(&t1, &t2));
                    }
                }
            }
        }
    }

    /// Helper: create a fresh type variable and return both the Type::Var and
    /// its TyVar id.
    pub(super) fn fresh_tv(&mut self) -> (Type, TyVar) {
        let t = self.fresh_var();
        let v = match &t {
            Type::Var(v) => *v,
            _ => unreachable!(),
        };
        (t, v)
    }
}

/// A rigid variable of `ty`, if it has one.
pub(super) fn rigid_in(ty: &Type) -> Option<RigidId> {
    match ty {
        Type::Rigid(r) => Some(*r),
        Type::Fun(params, ret) => params.iter().find_map(rigid_in).or_else(|| rigid_in(ret)),
        Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => rigid_in(inner),
        Type::Tuple(elems) => elems.iter().find_map(rigid_in),
        Type::Generic(_, args) => args.iter().find_map(rigid_in),
        Type::Map(k, v) => rigid_in(k).or_else(|| rigid_in(v)),
        Type::App(head, arg) => rigid_in(head).or_else(|| rigid_in(arg)),
        Type::Con(_, rest) => rest.iter().find_map(rigid_in),
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Error
        | Type::Never => None,
    }
}

/// Check if a type variable occurs in a type (occurs check for unification).
fn occurs_in(var: TyVar, ty: &Type) -> bool {
    match ty {
        Type::Var(v) => *v == var,
        Type::Fun(params, ret) => params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret),
        Type::List(inner) => occurs_in(var, inner),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in(var, e)),
        Type::Generic(_, args) => args.iter().any(|a| occurs_in(var, a)),
        Type::Map(k, v) => occurs_in(var, k) || occurs_in(var, v),
        Type::Set(inner) => occurs_in(var, inner),
        Type::Channel(inner) => occurs_in(var, inner),
        Type::App(head, arg) => occurs_in(var, head) || occurs_in(var, arg),
        Type::Con(_, rest) => rest.iter().any(|t| occurs_in(var, t)),
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => false,
    }
}
