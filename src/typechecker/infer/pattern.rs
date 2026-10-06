use super::super::exhaustiveness::{CtorId, Pat, Unverified};
use super::super::inference::*;
use super::super::*;

/// Where a pattern stands, for `TypeChecker::check_pattern`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::typechecker) enum PatternMode {
    /// At a place with no branch to take when the pattern fails to
    /// match: the pattern must be irrefutable.
    Binding(BindingSite),
    /// In a `match` arm or a `when let ... else`, which go on to the next
    /// arm or to the `else` when the pattern fails to match.
    Arm,
}

/// A place that binds a pattern and has no branch to take when the
/// pattern fails to match.
///
/// `loop` bindings are not listed: a `loop` binds plain names, never a
/// pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::typechecker) enum BindingSite {
    /// A `let`, as a statement or at the top level.
    Let,
    /// A parameter of a named function or of a trait-impl method.
    FnParam,
    /// A parameter of a closure, `{ pattern -> body }`.
    ClosureParam,
}

impl BindingSite {
    /// The site, as named in a diagnostic.
    fn place(self) -> &'static str {
        match self {
            BindingSite::Let => "`let`",
            BindingSite::FnParam => "function parameter",
            BindingSite::ClosureParam => "closure parameter",
        }
    }

    /// What to write instead of a refutable pattern at this site.
    fn advice(self) -> &'static str {
        match self {
            BindingSite::Let => "use a `match` or `when let ... else` instead",
            BindingSite::FnParam | BindingSite::ClosureParam => {
                "bind the parameter to a name and use a `match` or `when let ... else` \
                 in the body instead"
            }
        }
    }
}

/// Whether `pattern` holds a constructor or record name the resolver
/// resolved to nothing.
pub(in crate::typechecker) fn names_unresolved(pattern: &Pattern) -> bool {
    if pattern.res == Some(crate::defs::Res::Error) {
        return true;
    }
    match &pattern.kind {
        PatternKind::Tuple(ps)
        | PatternKind::Or(ps)
        | PatternKind::Constructor { args: ps, .. } => ps.iter().any(names_unresolved),
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => fields
            .iter()
            .any(|(_, _, sub)| sub.as_ref().is_some_and(names_unresolved)),
        PatternKind::List(elems, rest) => {
            elems.iter().any(names_unresolved) || rest.as_deref().is_some_and(names_unresolved)
        }
        PatternKind::Map(entries) => entries.iter().any(|(_, p)| names_unresolved(p)),
        _ => false,
    }
}

/// What the check of one pattern, with the patterns inside it, shares.
struct PatternCx {
    /// The span of the value the pattern is matched against.
    span: Span,
    /// The type of every name the pattern pins, looked up before the
    /// pattern bound a name; `None` for a name that is not in scope.
    pins: HashMap<Symbol, Option<Type>>,
    /// The patterns the usefulness searches of this pattern looked at.
    cells: usize,
    /// Whether one of those searches gave up.
    unverified: bool,
}

/// The names `pattern` pins.
fn pinned_names(pattern: &Pattern, out: &mut Vec<Symbol>) {
    if let PatternKind::Pin(name) = &pattern.kind {
        out.push(*name);
    }
    for sub in sub_patterns(pattern) {
        pinned_names(sub, out);
    }
}

/// The variant a constructor pattern names, for the checker. See
/// `TypeChecker::ctor_target`.
enum CtorTarget {
    /// A variant of this enum.
    Enum(TypeRef, EnumInfo),
    /// Nothing to report: the resolver reported the name, or it comes
    /// from a module that failed to load.
    Silent,
    /// No variant of that name: the checker says what is wrong.
    Unknown,
}

impl TypeChecker {
    // ── The one pattern checker ────────────────────────────────────

    /// Check `pattern` against a value of type `expected`: type its
    /// parts, bind its names in `env`, mark it and every pattern inside
    /// it irrefutable or not (`Pattern::irrefutable`, which the compiler
    /// reads), and return what it tests, for the usefulness search.
    ///
    /// `span` is the span of the value. In `PatternMode::Binding` the
    /// pattern must be irrefutable, and is reported when it is not: the
    /// compiler emits no test for the pattern of a `let` or a parameter,
    /// so a refutable one would read the payload of `Cents` as that of
    /// `Dollars`, or index past the fields of `None`.
    pub(in crate::typechecker) fn check_pattern(
        &mut self,
        pattern: &mut Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        span: Span,
        mode: PatternMode,
    ) -> Pat {
        let errors_before = self.errors.len();
        // A pin is the value of a name from outside the pattern: the
        // pinned names are looked up before the pattern binds any.
        let mut pinned = Vec::new();
        pinned_names(pattern, &mut pinned);
        let pins = pinned
            .into_iter()
            .map(|name| {
                let ty = env.lookup(name).cloned().map(|s| self.instantiate(&s));
                (name, ty)
            })
            .collect();
        let mut cx = PatternCx {
            span,
            pins,
            cells: 0,
            unverified: false,
        };
        let pat = self.type_pattern(pattern, expected, env, &mut cx);
        // What is matched has no type (it comes from a module that failed
        // to load, or an expression already reported): neither has what
        // the pattern binds, so that nothing is asked of it.
        if matches!(self.apply(expected), Type::Error) {
            for name in collect_pattern_vars(pattern) {
                env.define(name, Scheme::mono(Type::Error));
            }
        }
        if let PatternMode::Binding(site) = mode {
            // Irrefutability is judged for a pattern that type checked:
            // one that did not has its diagnostic. So has a name in the
            // pattern the resolver reported; what it matches is not
            // known.
            let failed = self.errors[errors_before..]
                .iter()
                .any(|e| matches!(e.severity, Severity::Error));
            if !failed && !pattern.irrefutable && !names_unresolved(pattern) {
                self.report_refutable(pattern, expected, span, site, cx.unverified);
            }
        }
        pat
    }

    /// Report `pattern`, which is not irrefutable, at a site that has no
    /// branch for a failed match. The verdict is the usefulness search's
    /// (`unverified`: it gave up); this function only words the
    /// diagnostic, naming the part of the pattern that can fail.
    ///
    /// `span` is the span of the value being bound. A `let` reports a
    /// refutable constructor there and any other refutable part at the
    /// part itself; a parameter always reports at the part.
    fn report_refutable(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        span: Span,
        site: BindingSite,
        unverified: bool,
    ) {
        let (reason, reason_span) = match self.refutable_part(pattern) {
            _ if unverified => (
                "the pattern is too large to verify that it matches every value".to_string(),
                pattern.span,
            ),
            Some(part) => {
                let reason_span = match (site, &part.kind) {
                    (BindingSite::Let, PatternKind::Constructor { .. }) => span,
                    _ => part.span,
                };
                (self.refutable_part_reason(part, ty), reason_span)
            }
            None => (self.refutable_type_reason(ty), pattern.span),
        };
        self.error(
            Code::InvalidPatternUse,
            format!(
                "refutable pattern in {}: {reason}; {}",
                site.place(),
                site.advice()
            ),
            reason_span,
        );
    }

    /// The part of a refutable `pattern` to name in a diagnostic: the
    /// first pattern in it, outermost first and left to right, whose own
    /// form can fail to match whatever its parts are. `None` when the
    /// pattern is irrefutable, or when only its parts together fail.
    fn refutable_part<'p>(&self, pattern: &'p Pattern) -> Option<&'p Pattern> {
        if pattern.irrefutable {
            return None;
        }
        let form_can_fail = match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Ident(_)
            | PatternKind::Tuple(_)
            | PatternKind::Record { .. }
            | PatternKind::AnonRecord { .. }
            | PatternKind::Or(_) => false,
            PatternKind::Constructor { .. } => self
                .pattern_constructor_enum(pattern)
                .is_none_or(|(_, info)| info.variants.len() != 1),
            // `[..rest]` takes a list of any length.
            PatternKind::List(elems, rest) => !(elems.is_empty() && rest.is_some()),
            PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Map(..)
            | PatternKind::Pin(_) => true,
        };
        if form_can_fail {
            return Some(pattern);
        }
        sub_patterns(pattern)
            .into_iter()
            .find_map(|p| self.refutable_part(p))
    }

    /// Why `part`, the refutable part of a pattern bound against `ty`,
    /// can fail to match.
    fn refutable_part_reason(&self, part: &Pattern, ty: &Type) -> String {
        match &part.kind {
            PatternKind::Constructor { name, .. } => match self.pattern_constructor_enum(part) {
                Some((enum_ref, info)) => format!(
                    "constructor '{}' is only one of {} variants of enum '{}'",
                    name,
                    info.variants.len(),
                    self.show_type(&Type::Generic(enum_ref, Vec::new()))
                ),
                None => self.refutable_type_reason(ty),
            },
            PatternKind::List(..) => "list patterns can fail to match".to_string(),
            PatternKind::Int(_) => {
                "integer literal patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::Float(_) => {
                "float literal patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::Bool(_) => {
                "boolean literal patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::StringLit(..) => {
                "string literal patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::Range(..) | PatternKind::FloatRange(..) => {
                "range patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::Pin(_) => {
                "pin patterns test a runtime value and can fail to match".to_string()
            }
            PatternKind::Map(..) => {
                "map patterns test a runtime value and can fail to match".to_string()
            }
            // The own shape of these always matches; only their parts can
            // fail, and `refutable_part` names the part.
            PatternKind::Wildcard
            | PatternKind::Ident(_)
            | PatternKind::Tuple(_)
            | PatternKind::Record { .. }
            | PatternKind::AnonRecord { .. }
            | PatternKind::Or(_) => self.refutable_type_reason(ty),
        }
    }

    /// The reason given when no single part of a refutable pattern can be
    /// named.
    fn refutable_type_reason(&self, ty: &Type) -> String {
        format!(
            "the pattern does not match every value of type '{}'",
            self.apply(ty)
        )
    }

    // ── Pattern type binding ────────────────────────────────────────

    /// BROKEN (soundness): duplicate bindings within a single conjunctive
    /// pattern scope (tuple elements, constructor args, record fields,
    /// list elements, fn param list) used to silently shadow each other
    /// — `let (a, a) = (1, 2)` typechecked and bound `a = 2` at runtime;
    /// `fn f(a: Int, a: Int)` typechecked with no error; `match (1, 2) {
    /// (x, x) -> x }` typechecked. Walk the pattern once before type
    /// binding and emit a diagnostic for every duplicate.
    ///
    /// Or-patterns (`p1 | p2`) are intentionally exempted: the same name
    /// appearing in both alternatives is how `|` works. We descend into
    /// each alternative with a fresh duplicate map so a name may appear
    /// once per alternative, then merge the union of binder sets back up
    /// into the outer conjunctive scope (all alternatives must bind the
    /// same set of vars — that invariant is enforced separately in the
    /// `Or` arm of `type_pattern_form`).
    pub(in crate::typechecker) fn check_pattern_duplicate_bindings(&mut self, pattern: &Pattern) {
        let mut seen: HashMap<Symbol, Span> = HashMap::new();
        let mut dups: Vec<(Symbol, Span)> = Vec::new();
        Self::collect_pattern_binders_into(pattern, &mut seen, &mut |name, dup_span| {
            dups.push((name, dup_span));
        });
        for (name, dup_span) in dups {
            self.error(
                Code::DuplicateBinding,
                format!("duplicate binding '{}' in pattern", resolve(name)),
                dup_span,
            );
        }
    }

    /// Fn-parameter variant of `check_pattern_duplicate_bindings`. A fn's
    /// parameter list is a single conjunctive scope — all binders across
    /// every param pattern must be unique. `fn f(a: Int, a: Int)` is the
    /// canonical repro: each `a` is its own pattern so the per-pattern
    /// check can't see the collision, we must thread one `seen` across
    /// the whole param list.
    pub(in crate::typechecker) fn check_fn_params_duplicate_bindings(&mut self, params: &[Param]) {
        let mut seen: HashMap<Symbol, Span> = HashMap::new();
        let mut dups: Vec<(Symbol, Span)> = Vec::new();
        for param in params {
            Self::collect_pattern_binders_into(&param.pattern, &mut seen, &mut |name, dup_span| {
                dups.push((name, dup_span));
            });
        }
        for (name, dup_span) in dups {
            self.error(
                Code::DuplicateBinding,
                format!("duplicate binding '{}' in pattern", resolve(name)),
                dup_span,
            );
        }
    }

    /// BROKEN (round 52): duplicate explicit-sub field names in a record
    /// pattern (e.g. `Point { x: a, x: b }`) slipped past the round-51
    /// binder-dedup guard because the shorthand form `{ x, x }` binds the
    /// same name twice (caught by `check_pattern_duplicate_bindings`) but
    /// the explicit-sub form binds distinct names (`a` and `b`). The
    /// field-name duplicate itself was never checked. Record literals and
    /// record type declarations already enforce this rule; the
    /// record-pattern arm is the missing sibling. Walk `fields` once and
    /// emit one diagnostic per duplicate field name, anchored at the
    /// second occurrence.
    pub(super) fn check_record_pattern_duplicate_fields(
        &mut self,
        fields: &[(Symbol, Span, Option<Pattern>)],
        outer_span: Span,
    ) {
        let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        for (field_name, _, sub_pat) in fields.iter() {
            if !seen.insert(*field_name) {
                // Point at the sub-pattern's span when present — that's
                // adjacent to the offending field name. Fall back to the
                // outer record-pattern span otherwise.
                let dup_span = sub_pat.as_ref().map(|p| p.span).unwrap_or(outer_span);
                self.error(
                    Code::DuplicateRecordField,
                    format!(
                        "duplicate field '{}' in record pattern",
                        resolve(*field_name)
                    ),
                    dup_span,
                );
            }
        }
    }

    /// Walk `pattern` in conjunctive-scope order, accumulating binder
    /// symbols into `seen`. When a name is seen twice in the same
    /// conjunctive scope, call `on_dup` with the name and the span of the
    /// second occurrence. Or-patterns open a sub-scope per alternative:
    /// each alternative is walked with a cloned `seen` (so duplicates
    /// inside an alternative are still caught), and the union of binders
    /// from all alternatives is merged back into the caller's `seen`
    /// (since any of them would bind that name at runtime).
    fn collect_pattern_binders_into(
        pattern: &Pattern,
        seen: &mut HashMap<Symbol, Span>,
        on_dup: &mut dyn FnMut(Symbol, Span),
    ) {
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(_, _)
            | PatternKind::FloatRange(_, _)
            | PatternKind::Pin(_) => {}
            PatternKind::Ident(name) => {
                if seen.contains_key(name) {
                    on_dup(*name, pattern.span);
                } else {
                    seen.insert(*name, pattern.span);
                }
            }
            PatternKind::Tuple(pats) | PatternKind::Constructor { args: pats, .. } => {
                for p in pats {
                    Self::collect_pattern_binders_into(p, seen, on_dup);
                }
            }
            PatternKind::List(pats, rest) => {
                for p in pats {
                    Self::collect_pattern_binders_into(p, seen, on_dup);
                }
                if let Some(rest_pat) = rest {
                    Self::collect_pattern_binders_into(rest_pat, seen, on_dup);
                }
            }
            PatternKind::Record { fields, .. } => {
                for (field_name, _, sub_pat) in fields {
                    match sub_pat {
                        Some(sp) => {
                            Self::collect_pattern_binders_into(sp, seen, on_dup);
                        }
                        None => {
                            // Shorthand `{ x }` binds `x` itself.
                            if seen.contains_key(field_name) {
                                on_dup(*field_name, pattern.span);
                            } else {
                                seen.insert(*field_name, pattern.span);
                            }
                        }
                    }
                }
            }
            PatternKind::Map(entries) => {
                for (_, p) in entries {
                    Self::collect_pattern_binders_into(p, seen, on_dup);
                }
            }
            PatternKind::Or(alts) => {
                // Each alternative is its own conjunctive scope (so a
                // duplicate inside a single alternative still fires), but
                // all alternatives at runtime bind the same name into the
                // outer scope — so the union of each alternative's binders
                // merges into `seen` before we continue. The or-pattern-
                // validation logic elsewhere guarantees the alternatives
                // bind the same set of names when well-formed, but we do
                // not rely on that here — we take the union defensively.
                let mut union_binders: HashMap<Symbol, Span> = HashMap::new();
                for alt in alts {
                    let mut alt_seen = seen.clone();
                    Self::collect_pattern_binders_into(alt, &mut alt_seen, on_dup);
                    // Take only the names added by this alternative.
                    for (name, sp) in alt_seen.iter() {
                        if !seen.contains_key(name) {
                            union_binders.entry(*name).or_insert(*sp);
                        }
                    }
                }
                for (name, sp) in union_binders {
                    seen.insert(name, sp);
                }
            }
            PatternKind::AnonRecord { fields, rest } => {
                for (field_name, _, sub_pat) in fields {
                    match sub_pat {
                        Some(sp) => {
                            Self::collect_pattern_binders_into(sp, seen, on_dup);
                        }
                        None => {
                            if seen.contains_key(field_name) {
                                on_dup(*field_name, pattern.span);
                            } else {
                                seen.insert(*field_name, pattern.span);
                            }
                        }
                    }
                }
                if let Some((rest_name, _)) = rest {
                    if seen.contains_key(rest_name) {
                        on_dup(*rest_name, pattern.span);
                    } else {
                        seen.insert(*rest_name, pattern.span);
                    }
                }
            }
        }
    }

    /// Bind the names `pattern` binds to the error type: its head names
    /// something the resolver reported, so nothing is known of its parts.
    fn bind_unresolved(&mut self, pattern: &Pattern, env: &mut TypeEnv) {
        for name in collect_pattern_vars(pattern) {
            env.define(name, Scheme::mono(Type::Error));
        }
    }

    /// The variant the constructor pattern `pattern` names (see
    /// `pattern_variant_enum`).
    fn ctor_target(&self, pattern: &Pattern) -> CtorTarget {
        let PatternKind::Constructor {
            qualifier, name, ..
        } = &pattern.kind
        else {
            return CtorTarget::Unknown;
        };
        if pattern.res == Some(crate::defs::Res::Error) || self.names_rejected(pattern.res, *name) {
            return CtorTarget::Silent;
        }
        let enum_name = self.pattern_variant_enum(pattern.res, qualifier);
        match enum_name.and_then(|e| self.tables.enums.get(&e).map(|info| (e, info))) {
            Some((e, info)) if info.variants.iter().any(|v| v.name == *name) => {
                CtorTarget::Enum(e, info.clone())
            }
            _ => CtorTarget::Unknown,
        }
    }

    /// `check_pattern` for a pattern and, through itself, the patterns
    /// inside it. What it returns for a pattern that matches every value
    /// is `Pat::Wild`, whatever the pattern's form: that is its mark.
    fn type_pattern(
        &mut self,
        pattern: &mut Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        cx: &mut PatternCx,
    ) -> Pat {
        let pat = self.type_pattern_form(pattern, expected, env, cx);
        pattern.irrefutable = matches!(pat, Pat::Wild);
        pat
    }

    /// The pattern of the constructor `id` with the patterns `args` of
    /// its fields.
    fn ctor_pat(&self, id: CtorId, args: Vec<Pat>) -> Pat {
        if args.iter().all(|arg| matches!(arg, Pat::Wild)) && self.stands_alone(&id) {
            Pat::Wild
        } else {
            Pat::Ctor(id, args)
        }
    }

    /// The or-pattern of the alternatives `alts`. Whether they cover the
    /// type between them is the one question of a pattern that needs a
    /// search; the searches of one pattern share a bound (`cx.cells`).
    fn or_pat(&self, alts: Vec<Pat>, cx: &mut PatternCx) -> Pat {
        if alts.iter().any(|alt| matches!(alt, Pat::Wild)) {
            return Pat::Wild;
        }
        let rows: Vec<&Pat> = alts.iter().collect();
        match self.cover_together(&rows, &mut cx.cells) {
            Ok(true) => Pat::Wild,
            Ok(false) => Pat::Or(alts),
            // A search that gave up shows nothing: the pattern keeps its
            // test.
            Err(Unverified) => {
                cx.unverified = true;
                Pat::Or(alts)
            }
        }
    }

    /// `ty` with the variables at its head replaced by what they stand
    /// for; its parts are left as they are.
    fn head<'t>(&'t self, mut ty: &'t Type) -> &'t Type {
        while let Type::Var(v) = ty
            && let Some(Some(bound)) = self.tables.vars.subst.get(*v)
        {
            ty = bound;
        }
        ty
    }

    /// `type_pattern` without the mark.
    fn type_pattern_form(
        &mut self,
        pattern: &mut Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        cx: &mut PatternCx,
    ) -> Pat {
        let span = cx.span;
        let pattern_span = pattern.span;
        let res = pattern.res;
        match &mut pattern.kind {
            PatternKind::Wildcard => Pat::Wild,
            PatternKind::Ident(name) => {
                env.define(*name, Scheme::mono(expected.clone()));
                Pat::Wild
            }
            PatternKind::Int(n) => {
                self.unify(expected, &Type::Int, span);
                Pat::IntRange(*n, *n)
            }
            PatternKind::Float(_) => {
                self.unify(expected, &Type::Float, span);
                Pat::Lit
            }
            PatternKind::Bool(b) => {
                self.unify(expected, &Type::Bool, span);
                Pat::Ctor(CtorId::Bool(*b), Vec::new())
            }
            PatternKind::StringLit(..) => {
                self.unify(expected, &Type::String, span);
                Pat::Lit
            }
            PatternKind::Range(lo, hi) => {
                self.unify(expected, &Type::Int, span);
                if (*lo, *hi) == (i64::MIN, i64::MAX) {
                    Pat::Wild
                } else {
                    Pat::IntRange(*lo, *hi)
                }
            }
            PatternKind::FloatRange(_, _) => {
                self.unify(expected, &Type::Float, span);
                Pat::Lit
            }
            PatternKind::Tuple(pats) => {
                // `()` is the unit pattern: the type of the empty tuple
                // is `Type::Unit`.
                if pats.is_empty() {
                    self.unify(expected, &Type::Unit, span);
                    return Pat::Wild;
                }
                // A value known to be a tuple of that many elements gives
                // their types as they are (no variable for each, and no
                // walk through the whole type for each level of a deep
                // pattern).
                let elem_types: Vec<Type> = match self.head(expected) {
                    Type::Tuple(elems) if elems.len() == pats.len() => elems.clone(),
                    _ => {
                        let fresh: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                        self.unify(expected, &Type::Tuple(fresh.clone()), span);
                        fresh
                    }
                };
                let subs = pats
                    .iter_mut()
                    .zip(&elem_types)
                    .map(|(p, t)| self.type_pattern(p, t, env, cx))
                    .collect();
                self.ctor_pat(CtorId::Tuple, subs)
            }
            PatternKind::Constructor { .. } => {
                self.type_constructor_pattern(pattern, expected, env, cx)
            }
            PatternKind::List(pats, rest) => {
                let elem_ty = match self.head(expected) {
                    Type::List(elem) => (**elem).clone(),
                    _ => {
                        let elem_ty = self.fresh_var();
                        let list_ty = Type::List(Box::new(elem_ty.clone()));
                        self.unify(expected, &list_ty, span);
                        elem_ty
                    }
                };
                let elems: Vec<Pat> = pats
                    .iter_mut()
                    .map(|p| self.type_pattern(p, &elem_ty, env, cx))
                    .collect();
                // A list is `nil` or `cons`: `[a, b]` is
                // `cons(a, cons(b, nil))`, and `[a, ..rest]` is
                // `cons(a, rest)`.
                let tail = match rest {
                    Some(rest_pat) => {
                        let rest_ty = Type::List(Box::new(elem_ty));
                        self.type_pattern(rest_pat, &rest_ty, env, cx)
                    }
                    None => Pat::nil(),
                };
                Pat::list(elems, tail)
            }
            PatternKind::Record { name, fields, .. } => {
                self.check_record_pattern_duplicate_fields(fields, pattern_span);
                if res == Some(crate::defs::Res::Error) {
                    self.bind_unresolved(pattern, env);
                    return Pat::Wild;
                }
                // The record type the pattern names, with its fields in
                // the order of the declaration.
                let declared = name
                    .and_then(|rec_name| self.named_record(res, rec_name, span, true))
                    .map(|(rec_ref, rec_info, param_ids)| {
                        let (fields, args) =
                            self.instantiate_record_fields(&rec_info, param_ids.as_deref());
                        self.unify(expected, &Type::Generic(rec_ref, args), span);
                        (rec_ref, fields)
                    });
                let mut subs: Vec<(Symbol, Pat)> = Vec::with_capacity(fields.len());
                for (field_name, _, sub_pat) in fields.iter_mut() {
                    let field_ty = match &declared {
                        Some((rec_ref, declared_fields)) => {
                            match declared_fields.iter().find(|(n, _)| n == field_name) {
                                Some((_, ft)) => ft.clone(),
                                None => {
                                    let base =
                                        format!("record '{rec_ref}' has no field '{field_name}'");
                                    self.error_help(
                                        Code::NoSuchField,
                                        format_record_field_suggestion(
                                            base,
                                            *field_name,
                                            declared_fields,
                                        ),
                                        span,
                                    );
                                    self.fresh_var()
                                }
                            }
                        }
                        None => self.fresh_var(),
                    };
                    let sub = match sub_pat {
                        Some(sp) => self.type_pattern(sp, &field_ty, env, cx),
                        None => {
                            // Shorthand `{ x }` binds `x`.
                            env.define(*field_name, Scheme::mono(field_ty));
                            Pat::Wild
                        }
                    };
                    subs.push((*field_name, sub));
                }
                // The pattern names a record type, and what is matched
                // is of that type (the `unify` above says so: an
                // anonymous record is not a `P`, and an open row that is
                // matched with `P { .. }` is `P`). Where it is not, the
                // mismatch is reported and the pattern covers nothing.
                let fits = match self.head(expected) {
                    Type::Generic(n, _) => match &declared {
                        Some((rec_ref, _)) => n == rec_ref,
                        None => self.tables.records.contains_key(n),
                    },
                    Type::Error | Type::Var(_) | Type::Never => true,
                    _ => false,
                };
                if !fits {
                    return Pat::Lit;
                }
                let (names, subs) = subs.into_iter().unzip();
                self.ctor_pat(CtorId::Record(names), subs)
            }
            PatternKind::Or(alts) => {
                // All alternatives bind the same names.
                if let Some((first, others)) = alts.split_first() {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(first).into_iter().collect();
                    for (i, alt) in others.iter().enumerate() {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            self.error(
                                Code::InvalidPatternUse,
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {}, alternative {} binds {}",
                                    format_symbol_set(&first_vars),
                                    i + 2,
                                    format_symbol_set(&alt_vars)
                                ),
                                span,
                            );
                        }
                    }
                }
                // Each alternative is checked in a frame of its own, and
                // every name must have one type in all of them
                // (`Left(x) | Right(x)` with an `Int` on one side and a
                // `String` on the other is rejected). The names of the
                // first alternative are the ones the or-pattern binds.
                let mut subs = Vec::with_capacity(alts.len());
                let mut bound: Option<Vec<(Symbol, Scheme)>> = None;
                for alt in alts.iter_mut() {
                    env.push();
                    subs.push(self.type_pattern(alt, expected, env, cx));
                    let names = env.pop();
                    let Some(first) = &bound else {
                        bound = Some(names);
                        continue;
                    };
                    for (name, first_scheme) in first {
                        let Some((_, scheme)) = names.iter().find(|(n, _)| n == name) else {
                            continue;
                        };
                        let a = self.apply(&first_scheme.ty);
                        let b = self.apply(&scheme.ty);
                        if self.unify_types(&a, &b).is_err() {
                            self.error(
                                Code::TypeMismatch,
                                format!(
                                    "or-pattern alternatives bind '{name}' to conflicting \
                                     types: {a} vs {b}"
                                ),
                                span,
                            );
                        }
                    }
                }
                for (name, scheme) in bound.unwrap_or_default() {
                    env.define(name, scheme);
                }
                self.or_pat(subs, cx)
            }
            PatternKind::Map(entries) => {
                // A map pattern's keys are string literals. A scrutinee
                // with another key type gets an error that says so.
                let val_ty = self.fresh_var();
                if let Type::Map(existing_key, _) = self.apply(expected) {
                    let existing_key = self.apply(&existing_key);
                    if !matches!(existing_key, Type::String | Type::Var(_) | Type::Error) {
                        self.error(Code::InvalidPatternUse,
                            format!(
                                "map patterns currently only match string keys; your scrutinee has key type '{existing_key}'"
                            ),
                            span,
                        );
                    }
                }
                let map_ty = Type::Map(Box::new(Type::String), Box::new(val_ty.clone()));
                self.unify(expected, &map_ty, span);
                let val_ty = self.apply(&val_ty);
                for (_key, pat) in entries.iter_mut() {
                    self.type_pattern(pat, &val_ty, env, cx);
                }
                // A key may be missing whatever the value patterns are.
                Pat::Lit
            }
            PatternKind::Pin(name) => {
                // A pin binds nothing: it compares with the value a name
                // has outside the pattern. A name the pattern binds
                // itself (`(x, ^x)`) is not that name.
                if let Some(pinned_ty) = cx.pins.get(name).cloned().flatten() {
                    self.unify(expected, &pinned_ty, span);
                } else {
                    let msg = format_undefined_variable_message(*name, env, "in pin pattern");
                    self.error_help(Code::UndefinedVariable, msg, pattern_span);
                }
                Pat::Lit
            }
            PatternKind::AnonRecord { fields, rest } => {
                // The pattern's type is an open record with a fresh type
                // for every field it names. The tail is open with or
                // without a `...rest` binding: without one the other
                // fields are just not named. Unification widens a nominal
                // record, so `{name: n, ...rest}` matches a
                // `type Person { ... }` value too.
                use std::collections::BTreeMap;
                let field_tys: BTreeMap<Symbol, Type> = fields
                    .iter()
                    .map(|(fname, _, _)| (*fname, self.fresh_var()))
                    .collect();
                let row_var = self.fresh_tyvar_id();
                let anon_ty = Type::AnonRecord {
                    fields: field_tys.clone(),
                    tail: RowTail::Var(row_var),
                };
                self.unify(expected, &anon_ty, span);
                let mut subs: BTreeMap<Symbol, Pat> = BTreeMap::new();
                for (fname, _, sub) in fields.iter_mut() {
                    let ft = self.apply(&field_tys[fname]);
                    let sub = match sub {
                        Some(p) => self.type_pattern(p, &ft, env, cx),
                        None => {
                            // Shorthand `{name}` binds `name`.
                            env.define(*fname, Scheme::mono(ft));
                            Pat::Wild
                        }
                    };
                    subs.insert(*fname, sub);
                }
                if let Some((rest_name, _)) = rest {
                    // The rest is a record of the fields the row
                    // variable stands for.
                    let rest_ty = self.apply(&Type::AnonRecord {
                        fields: BTreeMap::new(),
                        tail: RowTail::Var(row_var),
                    });
                    env.define(*rest_name, Scheme::mono(rest_ty));
                }
                let (names, subs) = subs.into_iter().unzip();
                self.ctor_pat(CtorId::Record(names), subs)
            }
        }
    }

    /// `type_pattern_form` for a constructor pattern.
    fn type_constructor_pattern(
        &mut self,
        pattern: &mut Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        cx: &mut PatternCx,
    ) -> Pat {
        let span = cx.span;
        let target = self.ctor_target(pattern);
        if matches!(target, CtorTarget::Silent) {
            self.bind_unresolved(pattern, env);
            return Pat::Wild;
        }
        let pattern_span = pattern.span;
        let res = pattern.res;
        let PatternKind::Constructor { name, args, .. } = &mut pattern.kind else {
            return Pat::Wild;
        };
        let name = *name;
        let CtorTarget::Enum(enum_ref, enum_info) = target else {
            // No variant of that name. A record type's name written as a
            // constructor gets the record syntax pointed out.
            if self.names_record(res, name) {
                self.error(Code::InvalidPatternUse,
                    format!(
                        "'{name}' is a record type; use record-pattern syntax `{name} {{ ... }}` instead of constructor-pattern syntax"
                    ),
                    pattern_span,
                );
            } else {
                self.error(
                    Code::UndefinedConstructor,
                    format!("undefined constructor '{name}' in pattern"),
                    pattern_span,
                );
            }
            for arg in args.iter_mut() {
                let tv = self.fresh_var();
                self.type_pattern(arg, &tv, env, cx);
            }
            return Pat::Wild;
        };
        let Some(index) = enum_info.variants.iter().position(|v| v.name == name) else {
            return Pat::Wild;
        };
        let field_types = &enum_info.variants[index].field_types;
        if args.len() != field_types.len() {
            let expected = field_types.len();
            self.error(
                Code::ArityMismatch,
                format!(
                    "constructor '{}' expects {} {}, but pattern has {}",
                    name,
                    expected,
                    plural(expected, "field", "fields"),
                    args.len()
                ),
                pattern_span,
            );
        }
        // The value is of the enum, with the type arguments it already
        // has when it is known to be of it: `let Ok(x) = 42` is a type
        // error.
        let type_args: Vec<Type> = match self.head(expected) {
            Type::Generic(n, args) if *n == enum_ref && args.len() == enum_info.params.len() => {
                args.clone()
            }
            _ => {
                let fresh: Vec<Type> = enum_info.params.iter().map(|_| self.fresh_var()).collect();
                self.unify(expected, &Type::Generic(enum_ref, fresh.clone()), span);
                fresh
            }
        };
        let subs = args
            .iter_mut()
            .enumerate()
            .map(|(i, arg)| {
                let field_ty = match field_types.get(i) {
                    Some(ft) => substitute_enum_params(ft, &enum_info.param_var_ids, &type_args),
                    None => self.fresh_var(),
                };
                self.type_pattern(arg, &field_ty, env, cx)
            })
            .collect();
        self.ctor_pat(CtorId::Variant(enum_ref, index), subs)
    }
}

/// The patterns directly inside `pattern`, in source order.
fn sub_patterns(pattern: &Pattern) -> Vec<&Pattern> {
    match &pattern.kind {
        PatternKind::Tuple(ps) | PatternKind::Or(ps) => ps.iter().collect(),
        PatternKind::Constructor { args, .. } => args.iter().collect(),
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
            fields.iter().filter_map(|(_, _, p)| p.as_ref()).collect()
        }
        PatternKind::List(elems, rest) => elems.iter().chain(rest.as_deref()).collect(),
        PatternKind::Map(entries) => entries.iter().map(|(_, p)| p).collect(),
        PatternKind::Wildcard
        | PatternKind::Ident(_)
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(..)
        | PatternKind::FloatRange(..)
        | PatternKind::Pin(_) => Vec::new(),
    }
}

/// Collect the set of variable names bound by a pattern.
pub(in crate::typechecker) fn collect_pattern_vars(pat: &Pattern) -> Vec<Symbol> {
    match &pat.kind {
        PatternKind::Ident(name) => vec![*name],
        PatternKind::Tuple(pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        PatternKind::List(pats, rest) => {
            let mut vars: Vec<Symbol> = pats.iter().flat_map(collect_pattern_vars).collect();
            if let Some(rest_pat) = rest {
                vars.extend(collect_pattern_vars(rest_pat));
            }
            vars
        }
        PatternKind::Constructor { args: pats, .. } => {
            pats.iter().flat_map(collect_pattern_vars).collect()
        }
        PatternKind::Record { fields, .. } => {
            let mut vars: Vec<Symbol> = Vec::new();
            for (field_name, _, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    // Shorthand field `{ x }` binds `x`
                    vars.push(*field_name);
                }
            }
            vars
        }
        PatternKind::AnonRecord { fields, rest } => {
            let mut vars: Vec<Symbol> = Vec::new();
            for (field_name, _, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    vars.push(*field_name);
                }
            }
            if let Some((r, _)) = rest {
                vars.push(*r);
            }
            vars
        }
        PatternKind::Or(alts) => {
            // Return vars from first alt (they should all be the same after validation)
            alts.first().map(collect_pattern_vars).unwrap_or_default()
        }
        PatternKind::Map(entries) => entries
            .iter()
            .flat_map(|(_, p)| collect_pattern_vars(p))
            .collect(),
        PatternKind::Wildcard
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(_, _)
        | PatternKind::FloatRange(_, _)
        | PatternKind::Pin(_) => vec![],
    }
}
