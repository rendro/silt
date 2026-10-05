use super::*;

impl TypeChecker {
    // ── Register function declarations ──────────────────────────────

    pub(super) fn register_fn_decl(&mut self, f: &FnDecl, env: &mut TypeEnv) {
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
                if !pre_return_keys.contains(name) {
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

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type));
        let mut scheme = self.generalize(env, &fn_type);
        // Round 64 item 6B (annotated polymorphic recursion): record
        // whether the user's signature is fully annotated. A `Data`
        // parameter is annotated iff it carries an explicit `ty`;
        // `Type` parameters are annotated by construction (the binder
        // itself is the annotation). The return type is annotated iff
        // `return_type` is Some. When both hold for every parameter
        // and the return, the narrowing pass in `check_program` will
        // skip this fn — keeping its scheme polymorphic across all
        // recursive call sites in its own body.
        if !f.is_recovery_stub {
            let all_params_annotated = f
                .params
                .iter()
                .all(|p| matches!(p.kind, ParamKind::Type) || p.ty.is_some());
            if all_params_annotated && f.return_type.is_some() {
                self.fully_annotated_fn_names.insert(f.name);
            }
        }

        // Resolve where clauses to (TyVar, trait_name) using param_map.
        // Type variables must be introduced via explicit type annotations in the signature.
        // Trait args (for parameterized traits like `a: TryInto(b)`) are
        // resolved through `param_map` and stashed in `trait_arg_bindings`
        // so descriptor method resolution can substitute them later.
        for wc in &f.where_clauses {
            let type_param = &wc.type_param;
            let trait_name = &wc.trait_name;
            let trait_args = &wc.trait_args;
            if let Some(ty) = param_map.get(type_param) {
                let resolved = self.apply(ty);
                // An unknown trait is reported when the body is checked.
                if let Type::Var(tv) = resolved
                    && let Some(trait_name) = self.named_trait(wc.trait_res, *trait_name)
                {
                    scheme.constraints.push((tv, trait_name));
                    if !trait_args.is_empty() {
                        let resolved_args: Vec<Type> = trait_args
                            .iter()
                            .map(|te| self.resolve_type_expr(te, &mut param_map))
                            .collect();
                        self.trait_arg_bindings
                            .insert((tv, trait_name), resolved_args);
                    }
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

        env.define(f.name, scheme);
    }
}
