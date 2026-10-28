use super::*;

/// What a function's declaration says of its type, as its body sees it:
/// each annotation variable is rigid, each part the declaration leaves
/// out is a variable the body decides.
#[derive(Debug, Clone)]
pub(crate) struct FnSig {
    pub(super) params: Vec<Type>,
    pub(super) ret: Type,
    /// The annotation variables, by name: an annotation in the body that
    /// writes one of the names means the same variable.
    pub(super) names: HashMap<Symbol, Type>,
    /// The annotation variables.
    pub(super) rigid: Vec<RigidId>,
    /// The bounds the `where` clauses declare, each on the variable an
    /// annotation variable is in the function's scheme.
    pub(super) bounds: Vec<Pred>,
    /// Whether the declaration leaves nothing out: every parameter and
    /// the result are annotated, and no annotation has a hole (a generic
    /// type written without its arguments). The function's scheme is
    /// then known before any body is checked.
    pub(super) complete: bool,
}

impl FnSig {
    /// The function's type, as its body sees it.
    pub(super) fn ty(&self) -> Type {
        Type::Fun(self.params.clone(), Box::new(self.ret.clone()))
    }
}

impl TypeChecker {
    // ── Register function declarations ──────────────────────────────

    /// The error of a function whose return type has the type variable
    /// `name` that no parameter's type has and a bound names: written so
    /// (`fn zero() -> a where a: Monoid`), or inferred so (`fn cx(w) {
    /// Conv.conv(w) }`, whose `b` the bound `a: Conv(b)` names). Which
    /// impl the bound means would be what each caller asks of it.
    pub(super) fn return_only_variable(&mut self, name: Symbol, span: Span) {
        let n = resolve(name);
        self.error(
            Code::InvalidTypeAnnotation,
            format!(
                "type variable '{n}' in return type is not introduced by any parameter; \
                 add a `type {n}` parameter or anchor it on an existing parameter's type"
            ),
            span,
        );
    }

    /// Hold `scheme`, the inferred type of the function at `span`, to
    /// the rule its written return type is held to: the error of each
    /// type variable that its return type has, no parameter's type has
    /// and a bound names, named as `:type` writes it. One that no bound
    /// names is general, as `fn e() { [] }` gives every caller the list
    /// it wants; and a return type that is a variable nothing else names
    /// is that of a body that never returns (`panic`, `process.exit`, a
    /// call of itself): `Never`, as the program would write it. A body
    /// that has an error of its own is not judged: its type is not
    /// known.
    pub(super) fn return_only_variables(&mut self, scheme: &mut Scheme, span: Span) {
        let Type::Fun(params, ret) = &scheme.ty else {
            return;
        };
        let erred = self.errors.iter().any(|d| {
            d.severity == crate::diagnostic::Severity::Error
                && d.span.file == span.file
                && d.span.start >= span.start
                && d.span.end <= span.end
        });
        if erred || scheme.ty.holds_error() {
            return;
        }
        let given: Vec<TyVar> = params.iter().flat_map(free_vars_in).collect();
        // (A variable the scheme does not quantify is an outer
        // definition's, which a later one may decide.)
        if let Type::Var(v) = **ret
            && scheme.vars.contains(&v)
            && !given.contains(&v)
            && !scheme.preds.iter().any(|pred| pred.mentions(v))
        {
            scheme.ty = Type::Fun(params.clone(), Box::new(Type::Never));
            scheme.vars.retain(|var| *var != v);
            return;
        }
        let letters = scheme.letters();
        for var in return_only(params, ret) {
            let bounded = scheme.preds.iter().any(|pred| {
                matches!(pred, Pred::Trait { .. } | Pred::Via { .. }) && pred.mentions(var)
            });
            if bounded && let Some(&name) = letters.get(&var) {
                self.return_only_variable(name, span);
            }
        }
    }

    /// Read the signature of `f`. A function with a complete signature
    /// (see [`FnSig::complete`]) is bound in `env` with its scheme; any
    /// other is bound when its body is checked, in the order the
    /// definitions refer to each other.
    pub(super) fn register_fn_decl(&mut self, f: &FnDecl, env: &mut TypeEnv) -> FnSig {
        // Recovery-stub special case (Option B): record the name and bind
        // its signature just like a real fn, so downstream references in
        // unrelated code do not cascade into "undefined variable" errors.
        if f.is_recovery_stub {
            self.recovery_stub_names.insert(f.name);
        }
        let applied = self.check_signature_kinds(f);
        let outer_signature = std::mem::replace(&mut self.resolving_signature, true);
        let mut param_map = HashMap::new();
        let mut param_types = Vec::new();

        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on parameter / return type annotations.
        let prev_type_span = self.current_type_anno_span.replace(f.span);
        for param in &f.params {
            let ty = match param.kind {
                ParamKind::Type => {
                    // `type a` parameter — seed `a` as a fresh type variable
                    // in `param_map` so it is in scope for the rest of the
                    // signature and any where clauses. The parameter's own
                    // compile-time type is the runtime type descriptor
                    // `TypeOf(a)`, which unifies with record / primitive
                    // descriptors at call sites.
                    let name = match &param.pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!("parser guarantees `type` params use an Ident pattern"),
                    };
                    let var = self.type_param_var(&mut param_map, name, param.pattern.span);
                    Type::type_of(var)
                }
                ParamKind::Data => {
                    if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &mut param_map)
                    } else {
                        self.fresh_var()
                    }
                }
            };
            param_types.push(ty);
        }

        // Binding rule: a lowercase type variable of the written return
        // annotation that no parameter introduces (regular annotation or
        // `type a`) and a where clause names is an error, once the where
        // clauses are read. Detected by snapshotting `param_map` before
        // resolving the return annotation: any new key added afterwards
        // is a variable that only appears in the return.
        let pre_return_keys: std::collections::HashSet<Symbol> =
            param_map.keys().copied().collect();
        let ret_type = if let Some(te) = &f.return_type {
            // `-> Never`, the whole annotation, is the one place a
            // program writes `Never`.
            let never = matches!(&te.kind, TypeExprKind::Named { module: None, name, .. }
                if resolve(*name) == "Never");
            let prev_never = std::mem::replace(&mut self.never_return, never);
            let resolved = self.resolve_type_expr(te, &mut param_map);
            self.never_return = prev_never;
            resolved
        } else {
            self.fresh_var()
        };
        self.current_type_anno_span = prev_type_span;
        self.resolving_signature = outer_signature;
        self.remember_constructor_labels(applied, &param_map);

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type.clone()));
        let mut bounds: Vec<Pred> = Vec::new();

        // Each where clause is a predicate of the scheme. Its type
        // variable must be one the signature's annotations write; its
        // trait arguments (`a: TryInto(b)`) are resolved through
        // `param_map`.
        for wc in &f.where_clauses {
            let type_param = &wc.type_param;
            let trait_name = &wc.trait_name;
            let trait_args = &wc.trait_args;
            // (A bound may be on a row variable: `where r: Display`.)
            let row_key = intern(&format!("__row__{type_param}"));
            if let Some(ty) = param_map
                .get(type_param)
                .or_else(|| param_map.get(&row_key))
            {
                let resolved = self.apply(ty);
                // An unknown trait is reported when the body is checked
                // (or was, by the resolver); what the variable is bounded
                // by is then not known.
                if let Type::Var(tv) = resolved
                    && self
                        .named_trait(wc.trait_res, *trait_name)
                        .is_none_or(|t| !self.tables.traits.contains_key(&t))
                {
                    self.unknown_bounds.insert(tv);
                }
                // A row variable stands for the rest of a record's
                // fields, not for a type: the one thing a bound can ask
                // of it is a trait a record has by its fields.
                let on_row = !param_map.contains_key(type_param);
                if on_row
                    && let Some(tr) = self.named_trait(wc.trait_res, *trait_name)
                    && !["Display", "Equal", "Hash"]
                        .iter()
                        .any(|name| tr.is_builtin(name))
                {
                    self.errors.push(
                        Diagnostic::error(
                            Code::InvalidTypeAnnotation,
                            wc.trait_name_span,
                            format!(
                                "the row variable '{type_param}' cannot be bounded by '{}': it stands for the rest of a record's fields, not for a type that implements a trait",
                                self.show_trait(tr)
                            ),
                        )
                        .with_help(
                            "a row can be bounded by Display, Equal or Hash, which a record has when its fields do",
                        ),
                    );
                    continue;
                }
                // A type is bounded by a trait of types, a type
                // constructor by a trait of type constructors.
                if let Type::Var(tv) = resolved
                    && let Some(tr) = self.named_trait(wc.trait_res, *trait_name)
                    && !self.bound_fits_kind(&resolved, (*type_param, true), tr, wc.trait_name_span)
                {
                    self.unknown_bounds.insert(tv);
                    continue;
                }
                if let Type::Var(tv) = resolved
                    && let Some(trait_name) = self.named_trait(wc.trait_res, *trait_name)
                {
                    let args: Vec<Type> = trait_args
                        .iter()
                        .map(|te| self.resolve_type_expr(te, &mut param_map))
                        .collect();
                    bounds.push(Pred::Trait {
                        tr: trait_name,
                        args,
                        subject: Type::Var(tv),
                        asker: Asker::of_bound(trait_name, tv).map(|asker| Asker {
                            passer: Some(f.name),
                            ..asker
                        }),
                    });
                }
            } else {
                let first_param_name = f
                    .params
                    .first()
                    .map(|p| match &p.pattern.kind {
                        PatternKind::Ident(n) => resolve(*n),
                        _ => "_".to_string(),
                    })
                    .unwrap_or_else(|| "_".to_string());
                self.error(Code::InvalidTypeAnnotation,
                    format!(
                        "type variable '{}' in where clause is not introduced in the function signature; \
                         use an explicit type annotation, e.g.: fn {}({}: {}) where {}: {}",
                        type_param, f.name,
                        first_param_name,
                        type_param, type_param, trait_name
                    ),
                    f.span,
                );
            }
        }

        let written_return_only: Vec<(Symbol, TyVar)> = (param_map.iter())
            .filter(|(name, _)| !pre_return_keys.contains(*name))
            .filter_map(|(name, ty)| match ty {
                Type::Var(v) => Some((*name, *v)),
                _ => None,
            })
            .collect();
        let result_only = return_only(&param_types, &ret_type);
        for (name, var) in written_return_only {
            if !self.registry_rows
                && result_only.contains(&var)
                && bounds.iter().any(|pred| pred.mentions(var))
            {
                self.return_only_variable(name, f.span);
            }
        }

        // The annotation variables: rigid in the body, a row variable
        // (`{name: String, ...r}`) like any other. (A row variable is
        // kept under `__row__r`, apart from a type variable `r`.)
        let mut rigid: Vec<RigidId> = Vec::new();
        let mut names: HashMap<Symbol, Type> = HashMap::new();
        let mut body_view: HashMap<TyVar, Type> = HashMap::new();
        for (name, ty) in &param_map {
            let Type::Var(var) = ty else { continue };
            let id = RigidId {
                var: *var,
                name: match resolve(*name).strip_prefix("__row__") {
                    Some(row) => intern(row),
                    None => *name,
                },
            };
            // A declared record may stand for a row variable only if
            // the body makes no anonymous record from it.
            if resolve(*name).starts_with("__row__") {
                self.sig_rows.insert(*var);
                bounds.push(Pred::Anon {
                    row: Type::Var(*var),
                    given: Some(*var),
                });
            } else if !self.registry_rows {
                // What the body asks of the variable for the builtins it
                // gives it to, each use asks of the type it gives.
                bounds.push(Pred::Given {
                    subject: Type::Var(*var),
                    given: *var,
                    written: None,
                    passer: f.name,
                    body: None,
                    group: None,
                });
            }
            rigid.push(id);
            names.insert(*name, Type::Rigid(id));
            body_view.insert(*var, Type::Rigid(id));
        }
        rigid.sort_by_key(|r| r.var);
        let annotated = f
            .params
            .iter()
            .all(|p| matches!(p.kind, ParamKind::Type) || p.ty.is_some())
            && f.return_type.is_some();
        let free = free_vars_in(&fn_type);
        // A recovery stub and a host function have no body: what they
        // leave out is never decided, so it is general.
        let bodiless = f.is_recovery_stub || self.signatures_only;
        let complete = bodiless || (annotated && free.iter().all(|v| body_view.contains_key(v)));
        if complete {
            // A variable only a bound's trait arguments name
            // (`where a: TryInto(b)`) is the scheme's as well.
            let mut scheme_vars = free;
            for pred in &bounds {
                for v in pred.args().iter().flat_map(free_vars_in) {
                    if !scheme_vars.contains(&v) {
                        scheme_vars.push(v);
                    }
                }
            }
            env.define(
                f.name,
                Scheme {
                    vars: scheme_vars,
                    preds: bounds.clone(),
                    ty: fn_type,
                },
            );
        }
        FnSig {
            params: param_types
                .iter()
                .map(|t| substitute_vars(t, &body_view))
                .collect(),
            ret: substitute_vars(&ret_type, &body_view),
            names,
            rigid,
            bounds,
            complete,
        }
    }
}

/// The type variables of the return type `ret` of a function of the
/// parameters `params` that only its results hold: none of `params` has
/// it, and no parameter of a function type inside `ret` has it either
/// (`a` of `Fn(String) -> Fn(List(a)) -> List(String)` is the returned
/// closure's caller's to give).
fn return_only(params: &[Type], ret: &Type) -> Vec<TyVar> {
    fn results(ty: &Type, given: &mut Vec<TyVar>, out: &mut Vec<TyVar>) {
        match ty {
            Type::Fun(params, ret) => {
                given.extend(params.iter().flat_map(free_vars_in));
                results(ret, given, out);
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => results(t, given, out),
            Type::Map(k, v) => {
                results(k, given, out);
                results(v, given, out);
            }
            Type::Generic(_, args) | Type::Tuple(args) => {
                args.iter().for_each(|t| results(t, given, out))
            }
            other => out.extend(free_vars_in(other)),
        }
    }
    let mut given: Vec<TyVar> = params.iter().flat_map(free_vars_in).collect();
    let mut out = Vec::new();
    results(ret, &mut given, &mut out);
    let mut only: Vec<TyVar> = Vec::new();
    for var in out {
        if !given.contains(&var) && !only.contains(&var) {
            only.push(var);
        }
    }
    only
}
