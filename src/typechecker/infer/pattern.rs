use super::super::exhaustiveness::Irrefutability;
use super::super::inference::*;
use super::super::*;

/// A place that binds a pattern and has no branch to take when the
/// pattern fails to match. Such a place accepts irrefutable patterns
/// only; see `TypeChecker::bind_irrefutable_pattern`.
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
    // ── Irrefutable binding sites ──────────────────────────────────
    //
    // A binding site without a failure branch — `let`, a function
    // parameter, a closure parameter — takes only a pattern that matches
    // every value of its type. The compiler emits no test for such a
    // pattern, so a refutable one would read the payload of `Cents` as
    // that of `Dollars`, or index past the fields of `None`. `match`
    // arms and `when let ... else` have a failure branch; they bind with
    // `check_pattern` / `bind_pattern` directly.

    /// Bind `pattern` against `ty` at a binding site that has no failure
    /// branch, and require the pattern to be irrefutable for `ty`.
    ///
    /// The pattern is type checked first, because irrefutability is
    /// judged against the type the pattern settles: a closure parameter
    /// starts as a fresh type variable, and it is `bind_pattern` that
    /// ties it to the enum or tuple the pattern names. A pattern that
    /// failed to type check already has its diagnostic and is not judged.
    pub(in crate::typechecker) fn bind_irrefutable_pattern(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        env: &mut TypeEnv,
        span: Span,
        site: BindingSite,
    ) {
        let errors_before = self.errors.len();
        self.bind_pattern(pattern, ty, env, span);
        let bind_failed = self.errors[errors_before..]
            .iter()
            .any(|e| matches!(e.severity, Severity::Error));
        // A name in the pattern the resolver reported: what it matches
        // is not known.
        if !bind_failed && !names_unresolved(pattern) {
            self.require_irrefutable(pattern, ty, span, site);
        }
    }

    /// Report `pattern` unless it is irrefutable for `ty`. The verdict is
    /// the exhaustiveness checker's (`irrefutability`); this function only
    /// words the diagnostic, naming the part of the pattern that can fail.
    ///
    /// `span` is the span of the value being bound. A `let` reports a
    /// refutable constructor there and any other refutable part at the
    /// part itself; a parameter always reports at the part.
    fn require_irrefutable(&mut self, pattern: &Pattern, ty: &Type, span: Span, site: BindingSite) {
        let verdict = self.irrefutability(pattern, ty);
        if verdict == Irrefutability::Irrefutable {
            return;
        }
        let part = match verdict {
            Irrefutability::Refutable => self.refutable_part(pattern),
            Irrefutability::Irrefutable | Irrefutability::Unverified => None,
        };
        let (reason, reason_span) = match part {
            Some(part) => {
                let reason_span = match (site, &part.kind) {
                    (BindingSite::Let, PatternKind::Constructor { .. }) => span,
                    _ => part.span,
                };
                (self.refutable_part_reason(part, ty), reason_span)
            }
            None if verdict == Irrefutability::Unverified => (
                "the pattern is nested too deeply to verify that it matches every value"
                    .to_string(),
                pattern.span,
            ),
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

    /// Why `part`, the refutable part of a pattern bound against `ty`,
    /// can fail to match.
    fn refutable_part_reason(&self, part: &Pattern, ty: &Type) -> String {
        match &part.kind {
            PatternKind::Constructor { name, .. } => match self.pattern_constructor_enum(part) {
                Some((enum_name, info)) => format!(
                    "constructor '{}' is only one of {} variants of enum '{}'",
                    name,
                    info.variants.len(),
                    enum_name
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
    /// `Or` arms of `bind_pattern` / `check_pattern`).
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

    /// Bind names in a pattern to their types in the environment.
    pub(in crate::typechecker) fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        env: &mut TypeEnv,
        span: Span,
    ) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Ident(name) => {
                env.define(*name, Scheme::mono(ty.clone()));
            }
            // BROKEN (round 35 F3): literal patterns in binding position
            // (e.g. `let 5 = "hello"`) used to fall through as empty arms,
            // silently ignoring the scrutinee's type. Mirror `check_pattern`
            // and unify the scrutinee against the literal's concrete type
            // so `let 5 = "hello"` becomes a compile-time error.
            PatternKind::Int(_) => {
                self.unify(ty, &Type::Int, span);
            }
            PatternKind::Float(_) => {
                self.unify(ty, &Type::Float, span);
            }
            PatternKind::Bool(_) => {
                self.unify(ty, &Type::Bool, span);
            }
            PatternKind::StringLit(..) => {
                self.unify(ty, &Type::String, span);
            }
            PatternKind::Tuple(pats) => {
                // BROKEN (round 15): bind_pattern Pattern::Tuple used to
                // silently fall through to fresh vars when the scrutinee
                // wasn't already a tuple, letting `let (a, b) = 42` slip
                // past the type checker and blow up at runtime. Build the
                // expected tuple shape up front and either unify against
                // the scrutinee (general mismatch) or emit a dedicated
                // arity error whose wording reads from the pattern's
                // perspective ("expected 3, got 2"). The two message
                // orderings differ because unify's tuple-tuple arm puts
                // the first arg as "expected", while its fallback
                // general-mismatch arm puts the second arg as "expected".
                //
                // BROKEN (round 23 #1): the empty-tuple pattern `()` is
                // the unit pattern. `resolve_type_expr` normalizes the
                // empty tuple type expr to `Type::Unit` (mod.rs around
                // the `TypeExpr::Tuple` arm). Unifying the scrutinee
                // against `Type::Tuple(vec![])` instead of `Type::Unit`
                // produced a nonsense "expected (), got ()" diagnostic
                // because the two types render identically but aren't
                // equal. Match the type-expr side of the language and
                // unify against `Type::Unit` when `pats.is_empty()`.
                if pats.is_empty() {
                    self.unify(ty, &Type::Unit, span);
                    return;
                }
                let resolved_pre = self.apply(ty);
                if let Type::Tuple(scrutinee_elems) = &resolved_pre {
                    if scrutinee_elems.len() == pats.len() {
                        let elems = scrutinee_elems.clone();
                        for (p, t) in pats.iter().zip(elems.iter()) {
                            self.bind_pattern(p, t, env, span);
                        }
                    } else {
                        // Arity mismatch — emit the pattern-centric error
                        // directly so the message reads "expected <N>, got
                        // <M>" from the pattern's point of view.
                        self.error(
                            Code::TypeMismatch,
                            format!(
                                "tuple length mismatch: expected {}, got {}",
                                pats.len(),
                                scrutinee_elems.len()
                            ),
                            span,
                        );
                        for p in pats {
                            let tv = self.fresh_var();
                            self.bind_pattern(p, &tv, env, span);
                        }
                    }
                } else {
                    // Non-tuple scrutinee (or an unresolved var). Unify
                    // against a fresh tuple shape so a) Var scrutinees get
                    // the correct tuple type, and b) concrete non-tuple
                    // scrutinees produce "expected (..), got <type>".
                    let shape_elems: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                    let shape = Type::Tuple(shape_elems.clone());
                    self.unify(ty, &shape, span);
                    // After unify, if the scrutinee unified into a tuple
                    // (via a fresh var), recurse properly; otherwise fall
                    // back to the shape vars.
                    let resolved_post = self.apply(ty);
                    match &resolved_post {
                        Type::Tuple(elems) if elems.len() == pats.len() => {
                            let elems = elems.clone();
                            for (p, t) in pats.iter().zip(elems.iter()) {
                                self.bind_pattern(p, t, env, span);
                            }
                        }
                        _ => {
                            for (p, t) in pats.iter().zip(shape_elems.iter()) {
                                self.bind_pattern(p, t, env, span);
                            }
                        }
                    }
                }
            }
            PatternKind::Constructor {
                name,
                args: sub_pats,
                ..
            } => {
                let resolved: Option<(TypeRef, EnumInfo)> = match self.ctor_target(pattern) {
                    CtorTarget::Enum(enum_name, info) => Some((enum_name, info)),
                    CtorTarget::Unknown => None,
                    CtorTarget::Silent => {
                        self.bind_unresolved(pattern, env);
                        return;
                    }
                };
                // Look up the constructor to find inner types
                if let Some((enum_name, enum_info)) = resolved
                    && let Some(var_info) = enum_info.variants.iter().find(|v| v.name == *name)
                {
                    if sub_pats.len() != var_info.field_types.len() {
                        let expected = var_info.field_types.len();
                        // Fix A: point the caret at the constructor pattern
                        // itself, not at the enclosing let/when scrutinee.
                        self.error(
                            Code::ArityMismatch,
                            format!(
                                "constructor '{}' expects {} {}, but pattern has {}",
                                name,
                                expected,
                                plural(expected, "field", "fields"),
                                sub_pats.len()
                            ),
                            pattern.span,
                        );
                    }
                    // BROKEN (round 15): unify the scrutinee against
                    // `Generic(enum_name, fresh args)` BEFORE recursing,
                    // so `let Ok(x) = 42` is caught at typecheck rather
                    // than deferred to a runtime `DestructVariant` crash.
                    // Try to reuse existing type args if the scrutinee is
                    // already a Generic of the right enum.
                    let resolved_pre = self.apply(ty);
                    let type_args: Vec<Type> = match &resolved_pre {
                        Type::Generic(n, args) if *n == enum_name => args.clone(),
                        _ => enum_info.params.iter().map(|_| self.fresh_var()).collect(),
                    };
                    let enum_shape = Type::Generic(enum_name, type_args.clone());
                    self.unify(ty, &enum_shape, span);
                    for (i, sp) in sub_pats.iter().enumerate() {
                        if i < var_info.field_types.len() {
                            let field_ty = substitute_enum_params(
                                &var_info.field_types[i],
                                &enum_info.param_var_ids,
                                &type_args,
                            );
                            self.bind_pattern(sp, &field_ty, env, span);
                        } else {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        }
                    }
                    return;
                }
                // LATENT (round 26 L1): mirror round-23's check_pattern
                // behavior — if `name` refers to a declared record type,
                // emit the record-syntax hint instead of the generic
                // "undefined constructor" message. The previous fallback
                // only existed on check_pattern, so `let Circle(r) = c`
                // gave a confusing error when the real issue was shape,
                // not existence.
                // LATENT (round 26 L3): also point the caret at
                // `pattern.span`, not the outer `span` (the outer span
                // is the enclosing let/match scrutinee).
                if self.names_record(pattern.res, *name) {
                    self.error(Code::InvalidPatternUse,
                        format!(
                            "'{name}' is a record type; use record-pattern syntax `{name} {{ ... }}` instead of constructor-pattern syntax"
                        ),
                        pattern.span,
                    );
                } else {
                    self.error(
                        Code::UndefinedConstructor,
                        format!("undefined constructor '{name}' in pattern"),
                        pattern.span,
                    );
                }
                for sp in sub_pats {
                    let tv = self.fresh_var();
                    self.bind_pattern(sp, &tv, env, span);
                }
            }
            PatternKind::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(ty, &list_ty, span);
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.bind_pattern(p, &resolved_elem, env, span);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.bind_pattern(rest_pat, &rest_ty, env, span);
                }
            }
            PatternKind::Record { name, fields, .. } => {
                // BROKEN (round 52): duplicate field names in record
                // patterns slipped through — both the explicit-sub form
                // (`Point { x: a, x: b }` — distinct binders, so the
                // round-51 binder-dedup walk can't see the collision) and
                // any latent shorthand case. See the helper's rustdoc.
                self.check_record_pattern_duplicate_fields(fields, pattern.span);
                // BROKEN-4: `let Name { f } = v` used to silently bind `f`
                // to a fresh TyVar when the base wasn't a record, or when
                // the field didn't exist. Both were deferred to VM runtime
                // errors. Reject them at the type-check stage. The type
                // identity is the bare name (`util.Pt { x }` names `Pt`).
                if pattern.res == Some(crate::defs::Res::Error) {
                    self.bind_unresolved(pattern, env);
                    return;
                }
                let resolved = self.apply(ty);
                let looked = match name {
                    Some(rec_name) => self.named_record(pattern.res, *rec_name, span, true),
                    None => None,
                };
                let pattern_record: Option<(TypeRef, Vec<(Symbol, Type)>)> =
                    if let Some((rec_ty, rec_info, param_ids)) = looked {
                        let instantiated_fields =
                            self.instantiate_record_fields(&rec_info, param_ids.as_deref());
                        Some((rec_ty, instantiated_fields))
                    } else {
                        None
                    };
                if let Some((pname, pfields)) = &pattern_record {
                    let rec_ty = Type::Record(*pname, pfields.clone());
                    self.unify(ty, &rec_ty, span);
                }
                let resolved = self.apply(&resolved);

                // R1 (round 15): when the scrutinee's type surfaces as
                // `Type::Generic(name, args)` and `name` names a declared
                // record (common for records passed through fn boundaries
                // — `resolve_type_expr` maps user record annotations to
                // `Type::Generic`), instantiate the record's field
                // templates and bind sub-patterns directly. The named
                // pattern case — `let Pair { a, b } = p` — has already
                // computed these fields in `pattern_record`; prefer those
                // so the declared and inferred instantiations stay linked.
                let generic_record_fields: Option<(TypeRef, Vec<(Symbol, Type)>)> =
                    if let Type::Generic(type_name, type_args) = &resolved
                        && let Some(rec_info) = self.tables.records.get(type_name).cloned()
                    {
                        let fields = if let Some((pname, pfields)) = &pattern_record
                            && *pname == *type_name
                        {
                            pfields.clone()
                        } else if let Some(param_var_ids) =
                            self.tables.record_param_var_ids.get(type_name).cloned()
                        {
                            let mapping: HashMap<TyVar, Type> =
                                if type_args.len() == param_var_ids.len() {
                                    param_var_ids
                                        .iter()
                                        .zip(type_args.iter())
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
                            rec_info.fields.clone()
                        };
                        Some((*type_name, fields))
                    } else {
                        None
                    };

                if let Type::Record(rec_name, field_types) = &resolved {
                    for (field_name, _, sub_pat) in fields {
                        if let Some((_, ft)) = field_types.iter().find(|(n, _)| n == field_name) {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env, span);
                            } else {
                                env.define(*field_name, Scheme::mono(ft.clone()));
                            }
                        } else {
                            // GAP (round 26 L5): append a did-you-mean
                            // hint when a near edit-distance field
                            // exists on this record.
                            let base = format!("record '{rec_name}' has no field '{field_name}'");
                            self.error_help(
                                Code::NoSuchField,
                                format_record_field_suggestion(base, *field_name, field_types),
                                span,
                            );
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.bind_pattern(sp, &tv, env, span);
                            } else {
                                let tv = self.fresh_var();
                                env.define(*field_name, Scheme::mono(tv));
                            }
                        }
                    }
                } else if let Some((rec_name, field_types)) = generic_record_fields {
                    for (field_name, _, sub_pat) in fields {
                        if let Some((_, ft)) = field_types.iter().find(|(n, _)| n == field_name) {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env, span);
                            } else {
                                env.define(*field_name, Scheme::mono(ft.clone()));
                            }
                        } else {
                            // GAP (round 26 L5): same hint on the generic
                            // resolution path.
                            let base = format!("record '{rec_name}' has no field '{field_name}'");
                            self.error_help(
                                Code::NoSuchField,
                                format_record_field_suggestion(base, *field_name, &field_types),
                                span,
                            );
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.bind_pattern(sp, &tv, env, span);
                            } else {
                                let tv = self.fresh_var();
                                env.define(*field_name, Scheme::mono(tv));
                            }
                        }
                    }
                } else if matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                    for (field_name, _, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                } else {
                    self.error(Code::TypeMismatch,
                        format!(
                            "record pattern requires a record value, but '{resolved}' is not a record type"
                        ),
                        span,
                    );
                    for (field_name, _, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            // BROKEN (round 26 B2): `{:?}` on a BTreeSet<Symbol>
                            // leaks `Symbol(N: "x")` debug output into a
                            // user-facing diagnostic. Render the sets as
                            // sorted comma-separated lists of resolved names.
                            self.error(
                                Code::InvalidPatternUse,
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {}, alternative {} binds {}",
                                    format_symbol_set(&first_vars),
                                    i + 1,
                                    format_symbol_set(&alt_vars)
                                ),
                                span,
                            );
                        }
                    }
                }
                // Bind each alternative into a scratch sub-environment so we
                // can collect the per-alternative type for every variable the
                // or-pattern binds, then unify those types pairwise. This
                // enforces that the alternatives agree on each binding's
                // type (e.g. `Left(x) | Right(x)` where `x: Int` on one side
                // and `x: String` on the other must be rejected).
                let mut per_alt_types: Vec<HashMap<Symbol, Type>> = Vec::with_capacity(alts.len());
                for alt in alts {
                    let mut alt_env = env.child();
                    self.bind_pattern(alt, ty, &mut alt_env, span);
                    let mut names: HashMap<Symbol, Type> = HashMap::new();
                    for name in collect_pattern_vars(alt) {
                        if let Some(scheme) = alt_env.bindings.get(&name) {
                            names.insert(name, scheme.ty.clone());
                        }
                    }
                    per_alt_types.push(names);
                }
                // Pairwise-unify the first alt's types with each other alt.
                if per_alt_types.len() >= 2 {
                    let (first, rest) = per_alt_types.split_first().unwrap();
                    for other in rest {
                        for (name, first_ty) in first {
                            if let Some(other_ty) = other.get(name) {
                                let a = self.apply(first_ty);
                                let b = self.apply(other_ty);
                                if a != b {
                                    // Try to unify — if they're still
                                    // incompatible, report a targeted error.
                                    let err_count = self.errors.len();
                                    self.unify(&a, &b, span);
                                    if self.errors.len() > err_count {
                                        // Replace the generic unify error with
                                        // a clearer or-pattern-specific one.
                                        self.errors.truncate(err_count);
                                        self.error(Code::TypeMismatch,
                                            format!(
                                                "or-pattern alternatives bind '{}' to conflicting types: {} vs {}",
                                                name, a, b
                                            ),
                                            span,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // Finally, bind the first alternative's variables into the
                // real environment so downstream code sees them.
                if let Some(first_alt) = alts.first() {
                    self.bind_pattern(first_alt, ty, env, span);
                }
            }
            PatternKind::Range(_, _) => {
                self.unify(ty, &Type::Int, span);
            }
            PatternKind::FloatRange(_, _) => {
                self.unify(ty, &Type::Float, span);
            }
            PatternKind::Map(entries) => {
                // L3: Map patterns are currently restricted to String keys at
                // parse time — `PatternKind::Map(Vec<(String, Pattern)>)` in
                // src/ast.rs. If the scrutinee has a non-String key type, give
                // a targeted error rather than the cryptic unification failure.
                let val_ty = self.fresh_var();
                let resolved_scrutinee = self.apply(ty);
                if let Type::Map(existing_key, _) = &resolved_scrutinee {
                    let existing_key = self.apply(existing_key);
                    if !matches!(existing_key, Type::String | Type::Var(_) | Type::Error) {
                        self.error(Code::InvalidPatternUse,
                            format!(
                                "map patterns currently only match string keys; your scrutinee has key type '{existing_key}'"
                            ),
                            span,
                        );
                    }
                }
                let key_ty = Type::String;
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(ty, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.bind_pattern(pat, &resolved_val, env, span);
                }
            }
            PatternKind::Pin(name) => {
                // Pin does not introduce a new binding — it checks against an
                // existing variable.  Look it up in the parent (pre-match) scope
                // first, then fall back to the current scope for when/let contexts.
                let found = env
                    .parent
                    .as_ref()
                    .and_then(|p| p.lookup(*name).cloned())
                    .or_else(|| env.lookup(*name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(ty, &pinned_ty, span);
                } else {
                    // LATENT (round 26 L4): point the caret at the pin
                    // pattern, not the enclosing match/let scrutinee.
                    let msg = format_undefined_variable_message(*name, env, "in pin pattern");
                    self.error_help(Code::UndefinedVariable, msg, pattern.span);
                }
            }
            PatternKind::AnonRecord { fields, rest } => {
                // For each field, allocate a fresh field type. Build an
                // open anon-record type and unify with the scrutinee —
                // unify handles widening from a nominal record so
                // `match person { {name: n, ...rest} -> ... }` works on
                // a `type Person { ... }` value too.
                use std::collections::BTreeMap;
                let mut field_tys: BTreeMap<Symbol, Type> = BTreeMap::new();
                for (fname, _, _) in fields.iter() {
                    field_tys.insert(*fname, self.fresh_var());
                }
                // The row tail is open (a fresh row variable) regardless
                // of whether the pattern has a `...rest` binding — when
                // there's no rest, the leftover fields just aren't named.
                let row_var = self.fresh_tyvar_id();
                let anon_ty = Type::AnonRecord {
                    fields: field_tys.clone(),
                    tail: RowTail::Var(row_var),
                };
                self.unify(ty, &anon_ty, span);
                // Resolve to get post-unification field types.
                let resolved = self.apply(&anon_ty);
                let resolved_fields: BTreeMap<Symbol, Type> =
                    if let Type::AnonRecord { fields: rf, .. } = &resolved {
                        rf.clone()
                    } else {
                        field_tys.clone()
                    };
                for (fname, _, sub) in fields.iter() {
                    let ft = resolved_fields
                        .get(fname)
                        .cloned()
                        .unwrap_or_else(|| self.fresh_var());
                    let ft = self.apply(&ft);
                    match sub {
                        Some(p) => self.bind_pattern(p, &ft, env, span),
                        None => {
                            // Shorthand `{name}` binds `name` directly.
                            env.define(*fname, Scheme::mono(ft));
                        }
                    }
                }
                if let Some((rest_name, _)) = rest {
                    // Bind rest to a record carrying just the row var —
                    // unification will plug it in to the leftover row.
                    let rest_ty = Type::AnonRecord {
                        fields: BTreeMap::new(),
                        tail: RowTail::Var(row_var),
                    };
                    let rest_ty = self.apply(&rest_ty);
                    env.define(*rest_name, Scheme::mono(rest_ty));
                }
            }
        }
    }

    // ── Pattern checking (type check, not just bind) ────────────────

    pub(in crate::typechecker) fn check_pattern(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        span: Span,
    ) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Ident(name) => {
                env.define(*name, Scheme::mono(expected.clone()));
            }
            PatternKind::Int(_) => {
                self.unify(expected, &Type::Int, span);
            }
            PatternKind::Float(_) => {
                self.unify(expected, &Type::Float, span);
            }
            PatternKind::Bool(_) => {
                self.unify(expected, &Type::Bool, span);
            }
            PatternKind::StringLit(..) => {
                self.unify(expected, &Type::String, span);
            }
            PatternKind::Tuple(pats) => {
                // BROKEN (round 23 #1): mirror bind_pattern — `()` is the
                // unit pattern, not a zero-arity tuple. See the comment on
                // PatternKind::Tuple in bind_pattern for background.
                if pats.is_empty() {
                    self.unify(expected, &Type::Unit, span);
                } else {
                    let elem_types: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                    let tuple_ty = Type::Tuple(elem_types.clone());
                    self.unify(expected, &tuple_ty, span);

                    for (p, t) in pats.iter().zip(elem_types.iter()) {
                        self.check_pattern(p, t, env, span);
                    }
                }
            }
            PatternKind::Constructor {
                name,
                args: sub_pats,
                ..
            } => {
                // The variant's constructor is its definition's scheme
                // (in the builtin environment, whose derived impls are
                // not resolved, bound as `Enum.Variant`). A name that is
                // no variant is looked up bare, for the hints below.
                let scheme = match self.ctor_target(pattern) {
                    CtorTarget::Silent => {
                        self.bind_unresolved(pattern, env);
                        return;
                    }
                    CtorTarget::Enum(enum_name, _) => self
                        .def_scheme(pattern.res, env)
                        .or_else(|| env.lookup(intern(&format!("{enum_name}.{name}"))).cloned()),
                    CtorTarget::Unknown => env.lookup(*name).cloned(),
                };
                // Look up the constructor type
                if let Some(scheme) = scheme {
                    let ctor_ty = self.instantiate(&scheme);
                    let ctor_ty = self.apply(&ctor_ty);

                    match &ctor_ty {
                        Type::Fun(params, ret) => {
                            self.unify(expected, ret, span);
                            if sub_pats.len() != params.len() {
                                let expected = params.len();
                                // Fix A: the arity error is about the
                                // pattern itself — point at the
                                // constructor pattern's own span rather
                                // than the enclosing match scrutinee.
                                // LATENT (round 26 L2): include the
                                // constructor name to match bind_pattern's
                                // wording ("constructor 'Some' expects ..."),
                                // otherwise the user has no idea which
                                // alternative arm is wrong when multiple
                                // constructors appear in a match.
                                self.error(
                                    Code::ArityMismatch,
                                    format!(
                                        "constructor '{}' expects {} {}, but pattern has {}",
                                        name,
                                        expected,
                                        plural(expected, "field", "fields"),
                                        sub_pats.len()
                                    ),
                                    pattern.span,
                                );
                            }
                            for (i, sp) in sub_pats.iter().enumerate() {
                                if i < params.len() {
                                    self.check_pattern(sp, &params[i], env, span);
                                }
                            }
                        }
                        _ => {
                            // Zero-arg constructor
                            if sub_pats.is_empty() {
                                self.unify(expected, &ctor_ty, span);
                            } else if self.names_record(pattern.res, *name) {
                                // GAP (round 23 #4): the user wrote
                                // `Circle(r)` where `Circle` is a record
                                // type, not an enum constructor. The old
                                // error said "expects 0 fields, but
                                // pattern has N", which is misleading —
                                // record types DO have fields, they just
                                // use `Circle { radius: r }` pattern
                                // syntax. Surface the real issue and
                                // point at the correct shape.
                                self.error(Code::InvalidPatternUse,
                                    format!(
                                        "'{name}' is a record type; use record-pattern syntax `{name} {{ ... }}` instead of constructor-pattern syntax"
                                    ),
                                    pattern.span,
                                );
                                for sp in sub_pats {
                                    let tv = self.fresh_var();
                                    self.check_pattern(sp, &tv, env, span);
                                }
                            } else {
                                self.error(
                                    Code::ArityMismatch,
                                    format!(
                                        "constructor '{}' expects 0 fields, but pattern has {}",
                                        name,
                                        sub_pats.len()
                                    ),
                                    pattern.span,
                                );
                            }
                        }
                    }
                } else {
                    // Unknown constructor — report error and bind sub-patterns with fresh vars.
                    // LATENT (round 26 L3): point the caret at the
                    // constructor pattern, not the enclosing match
                    // scrutinee — round-17 F4 threaded pattern.span
                    // through arity sites but missed this fallback.
                    self.error(
                        Code::UndefinedConstructor,
                        format!("undefined constructor '{name}' in pattern"),
                        pattern.span,
                    );
                    for sp in sub_pats {
                        let tv = self.fresh_var();
                        self.check_pattern(sp, &tv, env, span);
                    }
                }
            }
            PatternKind::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(expected, &list_ty, span);
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.check_pattern(p, &resolved_elem, env, span);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.check_pattern(rest_pat, &rest_ty, env, span);
                }
            }
            PatternKind::Record { name, fields, .. } => {
                // BROKEN (round 52): same duplicate-field guard as in
                // `bind_pattern`'s Record arm — match-arm record patterns
                // flow through `check_pattern`, so the check has to fire
                // on both paths.
                self.check_record_pattern_duplicate_fields(fields, pattern.span);
                if pattern.res == Some(crate::defs::Res::Error) {
                    self.bind_unresolved(pattern, env);
                    return;
                }
                if let Some(rec_name) = name {
                    let looked = self.named_record(pattern.res, *rec_name, span, true);
                    if let Some((rec_ref, rec_info, param_ids)) = looked {
                        let instantiated_fields =
                            self.instantiate_record_fields(&rec_info, param_ids.as_deref());

                        let rec_ty = Type::Record(rec_ref, instantiated_fields.clone());
                        self.unify(expected, &rec_ty, span);

                        for (field_name, _, sub_pat) in fields {
                            if let Some((_, ft)) =
                                instantiated_fields.iter().find(|(n, _)| n == field_name)
                            {
                                if let Some(sp) = sub_pat {
                                    self.check_pattern(sp, ft, env, span);
                                } else {
                                    env.define(*field_name, Scheme::mono(ft.clone()));
                                }
                            } else {
                                // BROKEN-3: Reject unknown field names in
                                // match record patterns at compile time.
                                // GAP (round 26 L5): append a did-you-mean
                                // hint when a near edit-distance field
                                // exists on the record.
                                let base =
                                    format!("record '{rec_name}' has no field '{field_name}'");
                                self.error_help(
                                    Code::NoSuchField,
                                    format_record_field_suggestion(
                                        base,
                                        *field_name,
                                        &instantiated_fields,
                                    ),
                                    span,
                                );
                                if let Some(sp) = sub_pat {
                                    let tv = self.fresh_var();
                                    self.check_pattern(sp, &tv, env, span);
                                }
                            }
                        }
                    } else {
                        for (_, _, sub_pat) in fields {
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.check_pattern(sp, &tv, env, span);
                            }
                        }
                    }
                } else {
                    for (field_name, _, sub_pat) in fields {
                        let tv = self.fresh_var();
                        if let Some(sp) = sub_pat {
                            self.check_pattern(sp, &tv, env, span);
                        } else {
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            // BROKEN (round 26 B2): `{:?}` on a BTreeSet<Symbol>
                            // leaks `Symbol(N: "x")` debug output into a
                            // user-facing diagnostic. Render the sets as
                            // sorted comma-separated lists of resolved names.
                            self.error(
                                Code::InvalidPatternUse,
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {}, alternative {} binds {}",
                                    format_symbol_set(&first_vars),
                                    i + 1,
                                    format_symbol_set(&alt_vars)
                                ),
                                span,
                            );
                        }
                    }
                }
                // Check each alternative into a scratch sub-environment so
                // we can collect the per-alternative type for every variable
                // the or-pattern binds, then unify those types pairwise.
                let mut per_alt_types: Vec<HashMap<Symbol, Type>> = Vec::with_capacity(alts.len());
                for alt in alts {
                    let mut alt_env = env.child();
                    self.check_pattern(alt, expected, &mut alt_env, span);
                    let mut names: HashMap<Symbol, Type> = HashMap::new();
                    for name in collect_pattern_vars(alt) {
                        if let Some(scheme) = alt_env.bindings.get(&name) {
                            names.insert(name, scheme.ty.clone());
                        }
                    }
                    per_alt_types.push(names);
                }
                if per_alt_types.len() >= 2 {
                    let (first, rest) = per_alt_types.split_first().unwrap();
                    for other in rest {
                        for (name, first_ty) in first {
                            if let Some(other_ty) = other.get(name) {
                                let a = self.apply(first_ty);
                                let b = self.apply(other_ty);
                                if a != b {
                                    let err_count = self.errors.len();
                                    self.unify(&a, &b, span);
                                    if self.errors.len() > err_count {
                                        self.errors.truncate(err_count);
                                        self.error(Code::TypeMismatch,
                                            format!(
                                                "or-pattern alternatives bind '{}' to conflicting types: {} vs {}",
                                                name, a, b
                                            ),
                                            span,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(first_alt) = alts.first() {
                    self.check_pattern(first_alt, expected, env, span);
                }
            }
            PatternKind::Range(_, _) => {
                self.unify(expected, &Type::Int, span);
            }
            PatternKind::FloatRange(_, _) => {
                self.unify(expected, &Type::Float, span);
            }
            PatternKind::Map(entries) => {
                // L3: Map patterns are restricted to String keys (parser
                // invariant — see PatternKind::Map in src/ast.rs). Give a
                // targeted error if the scrutinee has a non-String key type.
                let val_ty = self.fresh_var();
                let resolved_scrutinee = self.apply(expected);
                if let Type::Map(existing_key, _) = &resolved_scrutinee {
                    let existing_key = self.apply(existing_key);
                    if !matches!(existing_key, Type::String | Type::Var(_) | Type::Error) {
                        self.error(Code::InvalidPatternUse,
                            format!(
                                "map patterns currently only match string keys; your scrutinee has key type '{existing_key}'"
                            ),
                            span,
                        );
                    }
                }
                let key_ty = Type::String;
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(expected, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.check_pattern(pat, &resolved_val, env, span);
                }
            }
            PatternKind::Pin(name) => {
                // Look up the pinned variable in the parent (pre-match) scope,
                // falling back to current scope for when/let contexts.
                let found = env
                    .parent
                    .as_ref()
                    .and_then(|p| p.lookup(*name).cloned())
                    .or_else(|| env.lookup(*name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(expected, &pinned_ty, span);
                } else {
                    // LATENT (round 26 L4): point the caret at the pin
                    // pattern, not the enclosing match scrutinee.
                    let msg = format_undefined_variable_message(*name, env, "in pin pattern");
                    self.error_help(Code::UndefinedVariable, msg, pattern.span);
                }
            }
            PatternKind::AnonRecord { fields, rest } => {
                use std::collections::BTreeMap;
                let mut field_tys: BTreeMap<Symbol, Type> = BTreeMap::new();
                for (fname, _, _) in fields.iter() {
                    field_tys.insert(*fname, self.fresh_var());
                }
                let row_var = self.fresh_tyvar_id();
                let anon_ty = Type::AnonRecord {
                    fields: field_tys.clone(),
                    tail: RowTail::Var(row_var),
                };
                self.unify(expected, &anon_ty, span);
                let resolved = self.apply(&anon_ty);
                let resolved_fields: BTreeMap<Symbol, Type> =
                    if let Type::AnonRecord { fields: rf, .. } = &resolved {
                        rf.clone()
                    } else {
                        field_tys.clone()
                    };
                for (fname, _, sub) in fields.iter() {
                    let ft = resolved_fields
                        .get(fname)
                        .cloned()
                        .unwrap_or_else(|| self.fresh_var());
                    let ft = self.apply(&ft);
                    match sub {
                        Some(p) => self.check_pattern(p, &ft, env, span),
                        None => {
                            env.define(*fname, Scheme::mono(ft));
                        }
                    }
                }
                if let Some((rest_name, _)) = rest {
                    let rest_ty = Type::AnonRecord {
                        fields: BTreeMap::new(),
                        tail: RowTail::Var(row_var),
                    };
                    let rest_ty = self.apply(&rest_ty);
                    env.define(*rest_name, Scheme::mono(rest_ty));
                }
            }
        }
    }
}
