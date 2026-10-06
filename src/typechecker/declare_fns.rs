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
                    let var = param_map
                        .entry(name)
                        .or_insert_with(|| self.fresh_var())
                        .clone();
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

        // Binding rule: every lowercase type variable that appears in the
        // user-written return annotation must also be introduced by some
        // parameter (regular annotation or `type a`). Detected by
        // snapshotting `param_map` before resolving the return annotation —
        // any new key added afterwards is a variable that only appears in
        // the return, with no anchor.
        let pre_return_keys: std::collections::HashSet<Symbol> =
            param_map.keys().copied().collect();
        let ret_type = if let Some(te) = &f.return_type {
            let resolved = self.resolve_type_expr(te, &mut param_map);
            for name in param_map.keys() {
                if !pre_return_keys.contains(name) && !self.registry_rows {
                    let n = resolve(*name);
                    self.error(Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in return type is not introduced by any parameter; \
                             add a `type {}` parameter or anchor it on an existing parameter's type",
                            n, n
                        ),
                        f.span,
                    );
                }
            }
            resolved
        } else {
            self.fresh_var()
        };
        self.current_type_anno_span = prev_type_span;

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
            if let Some(ty) = param_map.get(type_param) {
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

        // The annotation variables: rigid in the body. A row variable
        // (`{name: String, ...r}`) is a variable of the body instead.
        let mut rigid: Vec<RigidId> = Vec::new();
        let mut names: HashMap<Symbol, Type> = HashMap::new();
        let mut body_view: HashMap<TyVar, Type> = HashMap::new();
        for (name, ty) in &param_map {
            let Type::Var(var) = ty else { continue };
            if resolve(*name).starts_with("__row__") {
                body_view.insert(*var, self.fresh_var());
                continue;
            }
            let id = RigidId {
                var: *var,
                name: *name,
            };
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
            for Pred::Trait { args, .. } in &bounds {
                for v in args.iter().flat_map(free_vars_in) {
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
                    optional_last_param: false,
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
