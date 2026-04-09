//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! bind_pattern, check_pattern, and check_fn_body.

use super::*;

impl TypeChecker {
    // ── Check function body ─────────────────────────────────────────

    pub(super) fn check_fn_body(&mut self, f: &mut FnDecl, env: &TypeEnv) {
        let mut local_env = env.child();

        // Validate where clauses
        for (type_param, trait_name) in &f.where_clauses {
            if !self.traits.contains_key(trait_name) {
                self.error(
                    format!(
                        "unknown trait '{}' in where clause for '{}'",
                        trait_name, type_param
                    ),
                    f.span,
                );
            }
        }

        // Look up the function's registered type and instantiate it
        let fn_scheme = match env.lookup(f.name) {
            Some(s) => s.clone(),
            None => return, // already reported
        };
        let (fn_type, constraints) = self.instantiate_with_constraints(&fn_scheme);
        let fn_type = self.apply(&fn_type);

        let (param_types, ret_type) = match &fn_type {
            Type::Fun(params, ret) => (params.clone(), *ret.clone()),
            _ => return,
        };

        // Populate active constraints so method resolution on type variables
        // can check trait methods during body inference.
        let prev_constraints = std::mem::take(&mut self.active_constraints);
        for (tv, trait_name) in &constraints {
            self.active_constraints
                .entry(*tv)
                .or_default()
                .push(*trait_name);
        }

        // Bind parameters
        for (i, param) in f.params.iter().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.bind_pattern(&param.pattern, ty, &mut local_env);
            }
        }

        // Infer the body and unify with declared return type
        let body_type = self.infer_expr(&mut f.body, &mut local_env);
        self.unify(&body_type, &ret_type, f.body.span);

        // Restore previous constraints
        self.active_constraints = prev_constraints;
    }

    // ── Pattern type binding ────────────────────────────────────────

    /// Bind names in a pattern to their types in the environment.
    pub(super) fn bind_pattern(&mut self, pattern: &Pattern, ty: &Type, env: &mut TypeEnv) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Ident(name) => {
                env.define(*name, Scheme::mono(ty.clone()));
            }
            Pattern::Int(_) => {}
            Pattern::Float(_) => {}
            Pattern::Bool(_) => {}
            Pattern::StringLit(_) => {}
            Pattern::Tuple(pats) => {
                let resolved = self.apply(ty);
                match &resolved {
                    Type::Tuple(elems) => {
                        for (p, t) in pats.iter().zip(elems.iter()) {
                            self.bind_pattern(p, t, env);
                        }
                    }
                    _ => {
                        // Create fresh vars for each sub-pattern
                        for p in pats {
                            let tv = self.fresh_var();
                            self.bind_pattern(p, &tv, env);
                        }
                    }
                }
            }
            Pattern::Constructor(name, sub_pats) => {
                // Look up the constructor to find inner types
                let resolved = self.apply(ty);
                if let Some(enum_name) = self.variant_to_enum.get(name).cloned()
                    && let Some(enum_info) = self.enums.get(&enum_name).cloned()
                    && let Some(var_info) = enum_info.variants.iter().find(|v| v.name == *name)
                {
                    // For parameterized types we need to figure out type args
                    // from the outer type and substitute them in
                    let type_args = match &resolved {
                        Type::Generic(_, args) => args.clone(),
                        _ => enum_info.params.iter().map(|_| self.fresh_var()).collect(),
                    };
                    for (i, sp) in sub_pats.iter().enumerate() {
                        if i < var_info.field_types.len() {
                            let field_ty = substitute_enum_params(
                                &var_info.field_types[i],
                                &enum_info.params,
                                &type_args,
                            );
                            self.bind_pattern(sp, &field_ty, env);
                        } else {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env);
                        }
                    }
                    return;
                }
                // Fallback: bind sub-patterns with fresh vars
                for sp in sub_pats {
                    let tv = self.fresh_var();
                    self.bind_pattern(sp, &tv, env);
                }
            }
            Pattern::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(
                    ty,
                    &list_ty,
                    Span {
                        line: 0,
                        col: 0,
                        offset: 0,
                    },
                );
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.bind_pattern(p, &resolved_elem, env);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.bind_pattern(rest_pat, &rest_ty, env);
                }
            }
            Pattern::Record { fields, .. } => {
                let resolved = self.apply(ty);
                if let Type::Record(_, field_types) = &resolved {
                    for (field_name, sub_pat) in fields {
                        if let Some((_, ft)) = field_types.iter().find(|(n, _)| n == field_name) {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env);
                            } else {
                                // Shorthand: field name is also the binding
                                env.define(*field_name, Scheme::mono(ft.clone()));
                            }
                        } else if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                } else {
                    // Bind with fresh vars
                    for (field_name, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            Pattern::Or(alts) => {
                for alt in alts {
                    self.bind_pattern(alt, ty, env);
                }
            }
            Pattern::Range(_, _) => {
                self.unify(
                    ty,
                    &Type::Int,
                    Span {
                        line: 0,
                        col: 0,
                        offset: 0,
                    },
                );
            }
            Pattern::FloatRange(_, _) => {
                self.unify(
                    ty,
                    &Type::Float,
                    Span {
                        line: 0,
                        col: 0,
                        offset: 0,
                    },
                );
            }
            Pattern::Map(entries) => {
                let key_ty = self.fresh_var();
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(
                    ty,
                    &map_ty,
                    Span {
                        line: 0,
                        col: 0,
                        offset: 0,
                    },
                );
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.bind_pattern(pat, &resolved_val, env);
                }
            }
            Pattern::Pin(name) => {
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
                    self.unify(
                        ty,
                        &pinned_ty,
                        Span {
                            line: 0,
                            col: 0,
                            offset: 0,
                        },
                    );
                } else {
                    self.error(
                        format!("undefined variable '{name}' in pin pattern"),
                        Span {
                            line: 0,
                            col: 0,
                            offset: 0,
                        },
                    );
                }
            }
        }
    }

    // ── Expression type inference ───────────────────────────────────

    pub(super) fn infer_expr(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let span = expr.span;
        let ty = match &mut expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::StringLit(_) => Type::String,
            ExprKind::Unit => Type::Unit,

            ExprKind::StringInterp(parts) => {
                // Each part is either a literal or an expression
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        let expr_span = e.span;
                        let t = self.infer_expr(e, env);
                        let resolved = self.apply(&t);
                        if let Some(type_name) = self.type_name_for_impl(&resolved)
                            && !self
                                .trait_impl_set
                                .contains(&(intern("Display"), type_name))
                        {
                            self.error(
                                format!(
                                    "type '{}' does not implement Display (required for string interpolation)",
                                    type_name
                                ),
                                expr_span,
                            );
                        }
                    }
                }
                Type::String
            }

            ExprKind::List(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::List(Box::new(tv))
                } else {
                    let elem_type = self.fresh_var();
                    for elem in elems.iter_mut() {
                        match elem {
                            ListElem::Single(e) => {
                                let t = self.infer_expr(e, env);
                                self.unify(&elem_type, &t, e.span);
                            }
                            ListElem::Spread(e) => {
                                let t = self.infer_expr(e, env);
                                let expected = Type::List(Box::new(elem_type.clone()));
                                self.unify(&expected, &t, e.span);
                            }
                        }
                    }
                    Type::List(Box::new(elem_type))
                }
            }

            ExprKind::Map(entries) => {
                if entries.is_empty() {
                    let k = self.fresh_var();
                    let v = self.fresh_var();
                    Type::Map(Box::new(k), Box::new(v))
                } else {
                    let mut iter = entries.iter_mut();
                    let first_entry = iter.next().unwrap();
                    let first_k = self.infer_expr(&mut first_entry.0, env);
                    let first_v = self.infer_expr(&mut first_entry.1, env);
                    for (k, v) in iter {
                        let k_span = k.span;
                        let v_span = v.span;
                        let kt = self.infer_expr(k, env);
                        let vt = self.infer_expr(v, env);
                        self.unify(&first_k, &kt, k_span);
                        self.unify(&first_v, &vt, v_span);
                    }
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
                } else {
                    let elem_type = self.fresh_var();
                    for e in elems.iter_mut() {
                        let t = self.infer_expr(e, env);
                        self.unify(&elem_type, &t, e.span);
                    }
                    Type::Set(Box::new(elem_type))
                }
            }

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems.iter_mut().map(|e| self.infer_expr(e, env)).collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(name) => {
                let name = *name;
                if let Some(scheme) = env.lookup(name) {
                    let scheme = scheme.clone();
                    self.instantiate(&scheme)
                } else if name == intern("self") {
                    // `self` is resolved at runtime — allow without error
                    self.fresh_var()
                } else {
                    self.error(format!("undefined variable '{name}'"), span);
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field) => {
                let field = *field;
                // Capture module name before mutable borrow for inference
                let module_name = if let ExprKind::Ident(n) = &obj.kind {
                    Some(*n)
                } else {
                    None
                };

                // Check for module-style access first (e.g., string.split)
                // Do this BEFORE inferring obj to avoid false "possibly undefined variable" warnings
                // for stdlib module names like list, string, map, io, etc.
                if let Some(module_name) = module_name {
                    let qualified = intern(&format!("{module_name}.{field}"));
                    if let Some(scheme) = env.lookup(qualified) {
                        let scheme = scheme.clone();
                        let result = self.instantiate(&scheme);
                        let resolved = self.apply(&result);
                        expr.ty = Some(resolved.clone());
                        return resolved;
                    }
                }

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);

                // Field / method access
                match &obj_ty {
                    Type::Record(rec_name, fields) => {
                        // Direct field access first
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| *n == field) {
                            ft.clone()
                        } else if let Some(entry) =
                            self.method_table.get(&(*rec_name, field)).cloned()
                        {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        } else {
                            self.error(
                                format!("record {rec_name} has no field or method '{field}'"),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, _) => {
                        // Check record field definitions
                        if let Some(rec_info) = self.records.get(type_name).cloned()
                            && let Some((_, ft)) = rec_info.fields.iter().find(|(n, _)| *n == field)
                        {
                            let resolved = self.apply(ft);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Check method table (trait methods)
                        if let Some(entry) = self.method_table.get(&(*type_name, field)).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Legacy fallback: check TypeEnv for "TypeName.method"
                        let key = intern(&format!("{type_name}.{field}"));
                        if let Some(scheme) = env.lookup(key) {
                            let scheme = scheme.clone();
                            let result = self.instantiate(&scheme);
                            let resolved = self.apply(&result);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown field or method '{field}' on type {type_name}"),
                            span,
                        );
                        Type::Error
                    }
                    // Primitive types — check method table for trait methods
                    Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit => {
                        let type_name = match &obj_ty {
                            Type::Int => intern("Int"),
                            Type::Float => intern("Float"),
                            Type::Bool => intern("Bool"),
                            Type::String => intern("String"),
                            Type::Unit => intern("()"),
                            _ => unreachable!(),
                        };
                        if let Some(entry) = self.method_table.get(&(type_name, field)).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown method '{field}' on type {type_name}"),
                            span,
                        );
                        Type::Error
                    }
                    // Collection types
                    Type::List(_) => {
                        if let Some(entry) =
                            self.method_table.get(&(intern("List"), field)).cloned()
                        {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on List"), span);
                        Type::Error
                    }
                    Type::Tuple(_) => {
                        if let Some(entry) =
                            self.method_table.get(&(intern("Tuple"), field)).cloned()
                        {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Tuple"), span);
                        Type::Error
                    }
                    Type::Map(_, _) => {
                        if let Some(entry) = self.method_table.get(&(intern("Map"), field)).cloned()
                        {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Map"), span);
                        Type::Error
                    }
                    Type::Set(_) => {
                        if let Some(entry) = self.method_table.get(&(intern("Set"), field)).cloned()
                        {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Set"), span);
                        Type::Error
                    }
                    // Variant types — look up parent enum
                    Type::Variant(variant_name, _) => {
                        let parent = self
                            .variant_to_enum
                            .get(variant_name)
                            .copied()
                            .unwrap_or(*variant_name);
                        if let Some(entry) = self.method_table.get(&(parent, field)).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on type {parent}"), span);
                        Type::Error
                    }
                    Type::Var(v) => {
                        // Check if this type variable has trait constraints
                        if let Some(trait_names) = self.active_constraints.get(v).cloned() {
                            // Collect all traits that provide this method
                            let mut matches: Vec<(Symbol, Type)> = Vec::new();
                            for trait_name in &trait_names {
                                if let Some(trait_info) = self.traits.get(trait_name).cloned()
                                    && let Some((_, method_ty)) =
                                        trait_info.methods.iter().find(|(n, _)| *n == field)
                                {
                                    matches.push((*trait_name, method_ty.clone()));
                                }
                            }
                            if matches.len() > 1 {
                                let trait_list = matches
                                    .iter()
                                    .map(|(name, _)| format!("{name}"))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                self.error(
                                    format!(
                                        "ambiguous method '{field}': provided by multiple traits ({trait_list})"
                                    ),
                                    span,
                                );
                                Type::Error
                            } else if let Some((_, method_ty)) = matches.first() {
                                let resolved = self.apply(method_ty);
                                expr.ty = Some(resolved.clone());
                                return resolved;
                            } else {
                                // Method not found on any constrained trait — error
                                let traits_str = trait_names
                                    .iter()
                                    .map(|s| format!("{s}"))
                                    .collect::<Vec<_>>()
                                    .join(" + ");
                                self.error(
                                    format!(
                                        "no method '{field}' found in trait constraints ({traits_str})"
                                    ),
                                    span,
                                );
                                Type::Error
                            }
                        } else {
                            // Unconstrained type variable — stay lenient (may resolve later)
                            self.fresh_var()
                        }
                    }
                    Type::Error => {
                        // Prior error — propagate to prevent cascading false positives
                        Type::Error
                    }
                    _ => {
                        self.error(
                            format!("unknown field or method '{field}' on type {obj_ty}"),
                            span,
                        );
                        Type::Error
                    }
                }
            }

            ExprKind::Binary(lhs, op, rhs) => {
                let op = *op;
                let lhs_span = lhs.span;
                let rhs_span = rhs.span;
                let lt = self.infer_expr(lhs, env);
                let rt = self.infer_expr(rhs, env);

                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod => {
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::ExtFloat)
                            | (Type::ExtFloat, Type::Float)
                            | (Type::ExtFloat, Type::ExtFloat) => Type::ExtFloat,
                            _ => {
                                self.unify(&lt, &rt, span);
                                lt
                            }
                        }
                    }
                    BinOp::Div => {
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::Float)
                            | (Type::Float, Type::ExtFloat)
                            | (Type::ExtFloat, Type::Float)
                            | (Type::ExtFloat, Type::ExtFloat) => Type::ExtFloat,
                            _ => {
                                self.unify(&lt, &rt, span);
                                lt
                            }
                        }
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                        self.unify(&lt, &rt, span);
                        Type::Bool
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&lt, &Type::Bool, lhs_span);
                        self.unify(&rt, &Type::Bool, rhs_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Unary(op, operand) => {
                let op = *op;
                let operand_span = operand.span;
                let t = self.infer_expr(operand, env);
                match op {
                    UnaryOp::Neg => t,
                    UnaryOp::Not => {
                        self.unify(&t, &Type::Bool, operand_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Pipe(lhs, rhs) => {
                let lhs_span = lhs.span;
                let arg_type = self.infer_expr(lhs, env);

                // Pipe semantics: a |> f(b) means f(a, b)
                // If the RHS is a Call, we prepend the pipe LHS as the first argument.
                // Check if rhs is a Call before mutable borrow
                let rhs_is_call = matches!(&rhs.kind, ExprKind::Call(..));

                if rhs_is_call {
                    // Destructure rhs.kind mutably to get at callee and call_args
                    if let ExprKind::Call(callee, call_args) = &mut rhs.kind {
                        // Capture callee name for where clause check
                        let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind {
                            Some(*n)
                        } else {
                            None
                        };
                        // Capture arg spans before mutable inference
                        let arg_spans: Vec<Span> = call_args.iter().map(|a| a.span).collect();

                        // If callee is a named function, use instantiate_with_constraints
                        let (callee_ty, where_constraints) = if let Some(name) = callee_fn_name {
                            if let Some(scheme) = env.lookup(name).cloned() {
                                let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                                (self.apply(&ty), constraints)
                            } else {
                                let ty = self.infer_expr(callee, env);
                                (self.apply(&ty), vec![])
                            }
                        } else {
                            let ty = self.infer_expr(callee, env);
                            (self.apply(&ty), vec![])
                        };

                        // Infer types for the explicit call args
                        let explicit_arg_types: Vec<Type> = call_args
                            .iter_mut()
                            .map(|a| self.infer_expr(a, env))
                            .collect();

                        // All args = [pipe_lhs, ...explicit_args]
                        let mut all_arg_types = vec![arg_type];
                        all_arg_types.extend(explicit_arg_types);

                        let result_ty = match &callee_ty {
                            Type::Fun(params, ret) => {
                                let min_len = params.len().min(all_arg_types.len());
                                for i in 0..min_len {
                                    let s = if i == 0 { lhs_span } else { arg_spans[i - 1] };
                                    self.unify(&all_arg_types[i], &params[i], s);
                                }
                                *ret.clone()
                            }
                            Type::Var(_) => {
                                let ret = self.fresh_var();
                                let fn_ty = Type::Fun(all_arg_types.clone(), Box::new(ret.clone()));
                                self.unify(&callee_ty, &fn_ty, span);
                                ret
                            }
                            _ => self.fresh_var(),
                        };

                        // Check where clause constraints using instantiated TyVars
                        for (tyvar, trait_name) in &where_constraints {
                            let resolved = self.apply(&Type::Var(*tyvar));
                            if let Some(type_name) = self.type_name_for_impl(&resolved)
                                && !self.trait_impl_set.contains(&(*trait_name, type_name))
                            {
                                self.error(
                                    format!(
                                        "type '{}' does not implement trait '{}'",
                                        type_name, trait_name
                                    ),
                                    span,
                                );
                            }
                        }

                        result_ty
                    } else {
                        unreachable!()
                    }
                } else {
                    // RHS is a plain function/lambda, not a call
                    let fn_type = self.infer_expr(rhs, env);
                    let fn_type = self.apply(&fn_type);

                    match &fn_type {
                        Type::Fun(params, ret) => {
                            if !params.is_empty() {
                                self.unify(&arg_type, &params[0], span);
                            }
                            *ret.clone()
                        }
                        Type::Var(_) => {
                            let ret = self.fresh_var();
                            let fn_ty = Type::Fun(vec![arg_type], Box::new(ret.clone()));
                            self.unify(&fn_type, &fn_ty, span);
                            ret
                        }
                        _ => self.fresh_var(),
                    }
                }
            }

            ExprKind::Range(start, end) => {
                let start_span = start.span;
                let end_span = end.span;
                let st = self.infer_expr(start, env);
                let et = self.infer_expr(end, env);
                self.unify(&st, &Type::Int, start_span);
                self.unify(&et, &Type::Int, end_span);
                Type::List(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                match &inner_ty {
                    Type::Generic(name, args) if *name == intern("Result") && args.len() == 2 => {
                        args[0].clone()
                    }
                    Type::Generic(name, args) if *name == intern("Option") && args.len() == 1 => {
                        args[0].clone()
                    }
                    Type::Var(_) => {
                        // Unresolved type variable — stay lenient
                        self.fresh_var()
                    }
                    _ => {
                        self.error(
                            format!(
                                "'?' operator requires Result or Option type, got '{inner_ty}'"
                            ),
                            span,
                        );
                        self.fresh_var()
                    }
                }
            }

            ExprKind::Ascription(inner, type_expr) => {
                let inner_ty = self.infer_expr(inner, env);
                let declared = self.resolve_type_expr(type_expr, &mut HashMap::new());
                self.unify(&inner_ty, &declared, span);
                declared
            }

            ExprKind::Call(callee, args) => {
                // Capture callee name and arg spans before mutable inference
                let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind {
                    Some(*n)
                } else {
                    None
                };
                let is_method_call = matches!(&callee.kind, ExprKind::FieldAccess(..));
                let arg_spans: Vec<Span> = args.iter().map(|a| a.span).collect();

                // If callee is a named function, use instantiate_with_constraints
                // to get where clause constraints with remapped type variables.
                let (callee_ty, where_constraints) = if let Some(name) = callee_fn_name {
                    if let Some(scheme) = env.lookup(name).cloned() {
                        let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                        (self.apply(&ty), constraints)
                    } else {
                        let ty = self.infer_expr(callee, env);
                        (self.apply(&ty), vec![])
                    }
                } else {
                    let ty = self.infer_expr(callee, env);
                    (self.apply(&ty), vec![])
                };

                let arg_types: Vec<Type> =
                    args.iter_mut().map(|a| self.infer_expr(a, env)).collect();

                let result_ty = match &callee_ty {
                    Type::Fun(params, ret) => {
                        // Unify argument types with parameter types
                        let min_len = params.len().min(arg_types.len());
                        for i in 0..min_len {
                            self.unify(&arg_types[i], &params[i], arg_spans[i]);
                        }
                        // Check arity. For method calls (obj.method(...)),
                        // the type signature includes `self` but the call
                        // site does not, so allow a difference of exactly 1.
                        let arity_ok = if is_method_call {
                            arg_types.len() == params.len()
                                || arg_types.len() + 1 == params.len()
                                || arg_types.len() == params.len() + 1
                        } else {
                            arg_types.len() == params.len()
                        };
                        if !arity_ok {
                            self.error(
                                format!(
                                    "function expects {} argument(s), got {}",
                                    params.len(),
                                    arg_types.len()
                                ),
                                span,
                            );
                        }
                        *ret.clone()
                    }
                    Type::Var(_) => {
                        // The callee is an unresolved type variable - create a function type
                        let ret = self.fresh_var();
                        let fn_ty = Type::Fun(arg_types.clone(), Box::new(ret.clone()));
                        self.unify(&callee_ty, &fn_ty, span);
                        ret
                    }
                    Type::Error => Type::Error,
                    Type::Never => Type::Never,
                    _ => {
                        self.error(format!("type '{}' is not callable", callee_ty), span);
                        self.fresh_var()
                    }
                };

                // Check where clause constraints using instantiated TyVars
                for (tyvar, trait_name) in &where_constraints {
                    let resolved = self.apply(&Type::Var(*tyvar));
                    if let Some(type_name) = self.type_name_for_impl(&resolved)
                        && !self.trait_impl_set.contains(&(*trait_name, type_name))
                    {
                        self.error(
                            format!(
                                "type '{}' does not implement trait '{}'",
                                type_name, trait_name
                            ),
                            span,
                        );
                    }
                }

                result_ty
            }

            ExprKind::Lambda { params, body } => {
                let mut local_env = env.child();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = if let Some(te) = &p.ty {
                            self.resolve_type_expr(te, &mut HashMap::new())
                        } else {
                            self.fresh_var()
                        };
                        self.bind_pattern(&p.pattern, &ty, &mut local_env);
                        ty
                    })
                    .collect();

                let body_type = self.infer_expr(body, &mut local_env);
                Type::Fun(param_types, Box::new(body_type))
            }

            ExprKind::RecordCreate { name, fields } => {
                let name = *name;
                if let Some(rec_info) = self.records.get(&name).cloned() {
                    let field_types: Vec<(Symbol, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (*n, ty)
                        })
                        .collect();

                    // Unify with declared field types
                    for (field_name, inferred_ty) in &field_types {
                        if let Some((_, declared_ty)) =
                            rec_info.fields.iter().find(|(n, _)| n == field_name)
                        {
                            self.unify(inferred_ty, declared_ty, span);
                        }
                    }

                    // Check for missing fields
                    let provided: std::collections::HashSet<Symbol> =
                        field_types.iter().map(|(n, _)| *n).collect();
                    let missing: Vec<Symbol> = rec_info
                        .fields
                        .iter()
                        .filter(|(n, _)| !provided.contains(n))
                        .map(|(n, _)| *n)
                        .collect();
                    if !missing.is_empty() {
                        let missing_str: Vec<String> =
                            missing.iter().map(|s| format!("{s}")).collect();
                        self.error(
                            format!(
                                "missing field{} in {}: {}",
                                if missing.len() > 1 { "s" } else { "" },
                                name,
                                missing_str.join(", "),
                            ),
                            span,
                        );
                    }

                    // Check for extra fields not in the record type
                    let declared: std::collections::HashSet<Symbol> =
                        rec_info.fields.iter().map(|(n, _)| *n).collect();
                    for (field_name, _) in &field_types {
                        if !declared.contains(field_name) {
                            self.error(format!("unknown field '{}' in {}", field_name, name), span);
                        }
                    }

                    Type::Record(name, rec_info.fields.clone())
                } else {
                    // Unknown record type - infer from fields
                    let field_types: Vec<(Symbol, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (*n, ty)
                        })
                        .collect();
                    Type::Record(name, field_types)
                }
            }

            ExprKind::RecordUpdate { expr: base, fields } => {
                let base_ty = self.infer_expr(base, env);
                let resolved = self.apply(&base_ty);
                if let Type::Record(ref rec_name, ref rec_fields) = resolved {
                    let declared: std::collections::HashMap<Symbol, &Type> =
                        rec_fields.iter().map(|(n, t)| (*n, t)).collect();
                    for (field_name, field_expr) in fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(&declared_ty) = declared.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else {
                            self.error(
                                format!("unknown field '{}' in {}", field_name, rec_name),
                                span,
                            );
                        }
                    }
                } else {
                    // Base type not resolved to a record — still infer field exprs
                    for (_, field_expr) in fields {
                        let _ft = self.infer_expr(field_expr, env);
                    }
                }
                base_ty
            }

            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                match scrutinee {
                    Some(scrutinee) => {
                        let scrutinee_span = scrutinee.span;
                        let scrutinee_ty = self.infer_expr(scrutinee, env);
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();
                            self.check_pattern(
                                &arm.pattern,
                                &scrutinee_ty,
                                &mut arm_env,
                                scrutinee_span,
                            );

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        self.check_exhaustiveness(arms, &resolved_scrutinee_ty, scrutinee_span);

                        result_ty
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean condition
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // No exhaustiveness checking for guardless match
                        result_ty
                    }
                }
            }

            ExprKind::Return(maybe_expr) => {
                if let Some(e) = maybe_expr {
                    self.infer_expr(e, env);
                }
                Type::Never
            }

            ExprKind::Block(stmts) => {
                let mut last_ty = Type::Unit;
                let mut block_env = env.child();

                for stmt in stmts {
                    last_ty = self.infer_stmt(stmt, &mut block_env);
                }

                last_ty
            }

            ExprKind::Loop { bindings, body } => {
                let mut loop_env = env.child();
                let binding_count = bindings.len();
                for (name, value) in bindings.iter_mut() {
                    let ty = self.infer_expr(value, env);
                    loop_env.define(*name, Scheme::mono(ty));
                }
                let prev_loop = self.loop_binding_count;
                self.loop_binding_count = Some(binding_count);
                let result = self.infer_expr(body, &mut loop_env);
                self.loop_binding_count = prev_loop;
                result
            }

            ExprKind::Recur(args) => {
                let recur_count = args.len();
                for arg in args {
                    let _ty = self.infer_expr(arg, env);
                }
                if let Some(expected) = self.loop_binding_count
                    && recur_count != expected
                {
                    self.warning(
                        format!(
                            "loop has {} binding(s), but recur supplies {} argument(s)",
                            expected, recur_count
                        ),
                        span,
                    );
                }
                self.fresh_var()
            }

            ExprKind::FloatElse(expr, fallback) => {
                let expr_ty = self.infer_expr(expr, env);
                let fallback_ty = self.infer_expr(fallback, env);
                self.unify(&expr_ty, &Type::ExtFloat, expr.span);
                self.unify(&fallback_ty, &Type::Float, fallback.span);
                Type::Float
            }
        };
        let resolved = self.apply(&ty);
        expr.ty = Some(resolved.clone());
        resolved
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                let is_value = is_syntactic_value(&value.kind);
                let val_ty = self.infer_expr(value, env);

                if let Some(te) = &ty {
                    let declared = self.resolve_type_expr(te, &mut HashMap::new());
                    self.unify(&val_ty, &declared, value_span);
                }

                // Generalize for let-polymorphism, but apply the value
                // restriction: only generalize syntactic values (literals,
                // lambdas, identifiers). Function calls may return types
                // with mutable state (e.g. channels) that must remain
                // monomorphic so that the element type is shared across
                // all uses.
                let scheme = if is_value {
                    self.generalize(env, &val_ty)
                } else {
                    Scheme::mono(self.apply(&val_ty))
                };

                // Bind names in the pattern
                // For let-polymorphism we need to bind with the generalized scheme
                match pattern {
                    Pattern::Ident(name) => {
                        env.define(*name, scheme);
                    }
                    _ => {
                        self.bind_pattern(pattern, &val_ty, env);
                    }
                }

                Type::Unit
            }

            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                let expr_ty = self.infer_expr(expr, env);

                // Type check the else body
                let _else_ty = self.infer_expr(else_body, env);

                // Bind the pattern in the current scope (type narrowing)
                self.bind_pattern(pattern, &expr_ty, env);

                // For constructor patterns, narrow the type
                // e.g., when Ok(value) = expr, value has the inner type
                if let Pattern::Constructor(name, sub_pats) = pattern {
                    let expr_ty = self.apply(&expr_ty);
                    if let Some(enum_name) = self.variant_to_enum.get(name).cloned()
                        && let Some(enum_info) = self.enums.get(&enum_name).cloned()
                        && let Some(var_info) = enum_info.variants.iter().find(|v| v.name == *name)
                    {
                        let type_args = match &expr_ty {
                            Type::Generic(_, args) => args.clone(),
                            _ => enum_info.params.iter().map(|_| self.fresh_var()).collect(),
                        };
                        for (i, sp) in sub_pats.iter().enumerate() {
                            if i < var_info.field_types.len() {
                                let field_ty = substitute_enum_params(
                                    &var_info.field_types[i],
                                    &enum_info.params,
                                    &type_args,
                                );
                                self.bind_pattern(sp, &field_ty, env);
                            }
                        }
                    }
                }

                Type::Unit
            }

            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                let cond_ty = self.infer_expr(condition, env);
                self.unify(&cond_ty, &Type::Bool, condition.span);

                // Type check the else body
                let _else_ty = self.infer_expr(else_body, env);

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    // ── Pattern checking (type check, not just bind) ────────────────

    fn check_pattern(&mut self, pattern: &Pattern, expected: &Type, env: &mut TypeEnv, span: Span) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Ident(name) => {
                env.define(*name, Scheme::mono(expected.clone()));
            }
            Pattern::Int(_) => {
                self.unify(expected, &Type::Int, span);
            }
            Pattern::Float(_) => {
                self.unify(expected, &Type::Float, span);
            }
            Pattern::Bool(_) => {
                self.unify(expected, &Type::Bool, span);
            }
            Pattern::StringLit(_) => {
                self.unify(expected, &Type::String, span);
            }
            Pattern::Tuple(pats) => {
                let elem_types: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                let tuple_ty = Type::Tuple(elem_types.clone());
                self.unify(expected, &tuple_ty, span);

                for (p, t) in pats.iter().zip(elem_types.iter()) {
                    self.check_pattern(p, t, env, span);
                }
            }
            Pattern::Constructor(name, sub_pats) => {
                // Look up the constructor type
                if let Some(scheme) = env.lookup(*name).cloned() {
                    let ctor_ty = self.instantiate(&scheme);
                    let ctor_ty = self.apply(&ctor_ty);

                    match &ctor_ty {
                        Type::Fun(params, ret) => {
                            self.unify(expected, ret, span);
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
                            }
                        }
                    }
                } else {
                    // Unknown constructor — bind sub-patterns with fresh vars
                    for sp in sub_pats {
                        let tv = self.fresh_var();
                        self.check_pattern(sp, &tv, env, span);
                    }
                }
            }
            Pattern::List(pats, rest) => {
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
            Pattern::Record { name, fields, .. } => {
                if let Some(rec_name) = name {
                    if let Some(rec_info) = self.records.get(rec_name).cloned() {
                        let rec_ty = Type::Record(*rec_name, rec_info.fields.clone());
                        self.unify(expected, &rec_ty, span);

                        for (field_name, sub_pat) in fields {
                            if let Some((_, ft)) =
                                rec_info.fields.iter().find(|(n, _)| n == field_name)
                            {
                                if let Some(sp) = sub_pat {
                                    self.check_pattern(sp, ft, env, span);
                                } else {
                                    env.define(*field_name, Scheme::mono(ft.clone()));
                                }
                            }
                        }
                    }
                } else {
                    // Anonymous record pattern — bind fields
                    for (field_name, sub_pat) in fields {
                        let tv = self.fresh_var();
                        if let Some(sp) = sub_pat {
                            self.check_pattern(sp, &tv, env, span);
                        } else {
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            Pattern::Or(alts) => {
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            self.error(
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {:?}, alternative {} binds {:?}",
                                    first_vars,
                                    i + 1,
                                    alt_vars
                                ),
                                span,
                            );
                        }
                    }
                }
                for alt in alts {
                    self.check_pattern(alt, expected, env, span);
                }
            }
            Pattern::Range(_, _) => {
                self.unify(expected, &Type::Int, span);
            }
            Pattern::FloatRange(_, _) => {
                self.unify(expected, &Type::Float, span);
            }
            Pattern::Map(entries) => {
                let key_ty = self.fresh_var();
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(expected, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.check_pattern(pat, &resolved_val, env, span);
                }
            }
            Pattern::Pin(name) => {
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
                    self.error(format!("undefined variable '{name}' in pin pattern"), span);
                }
            }
        }
    }
}

/// Returns true if an expression is a syntactic value for the purpose of the
/// value restriction on let-generalization. Syntactic values (literals,
/// lambdas, identifiers, constructors of values) are safe to generalize;
/// function applications are not, because they may produce types with
/// shared mutable state (e.g. channels) that must remain monomorphic.
fn is_syntactic_value(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::StringLit(_)
        | ExprKind::Unit
        | ExprKind::Ident(_)
        | ExprKind::Lambda { .. } => true,
        ExprKind::Tuple(elems) => elems.iter().all(|e| is_syntactic_value(&e.kind)),
        ExprKind::List(elems) => elems.iter().all(|e| match e {
            ListElem::Single(expr) => is_syntactic_value(&expr.kind),
            ListElem::Spread(_) => false,
        }),
        ExprKind::RecordCreate { fields, .. } => {
            fields.iter().all(|(_, e)| is_syntactic_value(&e.kind))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn assert_no_errors(input: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        let hard: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(
            hard.is_empty(),
            "expected no type errors, got:\n{}",
            hard.iter()
                .map(|e| format!("  {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    fn assert_has_error(input: &str, expected: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Unary operator inference ────────────────────────────────────

    #[test]
    fn test_unary_negate_int() {
        assert_no_errors(
            r#"
fn main() {
  let x = -42
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_negate_float() {
        assert_no_errors(
            r#"
fn main() {
  let x = -3.14
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_not_bool() {
        assert_no_errors(
            r#"
fn main() {
  let x = !true
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_not_non_bool() {
        assert_has_error(
            r#"
fn main() {
  !42
}
        "#,
            "type mismatch",
        );
    }

    // ── Or-pattern binding ──────────────────────────────────────────

    #[test]
    fn test_or_pattern_binds_variable() {
        assert_no_errors(
            r#"
fn classify(x) {
  match x {
    1 | 2 | 3 -> "small"
    _ -> "big"
  }
}
fn main() { classify(2) }
        "#,
        );
    }

    #[test]
    fn test_or_pattern_with_constructor_binding() {
        assert_no_errors(
            r#"
fn extract(x) {
  match x {
    Ok(v) | Err(v) -> v
  }
}
fn main() { extract(Ok(42)) }
        "#,
        );
    }

    // ── Map pattern binding ─────────────────────────────────────────

    #[test]
    fn test_map_pattern_in_match() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "x": 1, "y": 2 }
  match m {
    #{ "x": val } -> val
    _ -> 0
  }
}
        "#,
        );
    }

    // ── Pin pattern ─────────────────────────────────────────────────

    #[test]
    fn test_pin_pattern_matches_value() {
        assert_no_errors(
            r#"
fn main() {
  let expected = 42
  match 42 {
    ^expected -> "matched"
    _ -> "no match"
  }
}
        "#,
        );
    }

    // ── Return expression ───────────────────────────────────────────

    #[test]
    fn test_return_with_value() {
        assert_no_errors(
            r#"
fn early(x) {
  when x > 0 else { return 0 }
  x * 2
}
fn main() { early(5) }
        "#,
        );
    }

    #[test]
    fn test_return_no_value() {
        assert_no_errors(
            r#"
fn side_effect(x) {
  when x > 0 else { return () }
  println(x)
}
fn main() { side_effect(1) }
        "#,
        );
    }

    // ── String interpolation inference ──────────────────────────────

    #[test]
    fn test_string_interp_with_int_and_bool() {
        assert_no_errors(
            r#"
fn main() {
  let n = 42
  let b = true
  "n={n}, b={b}"
}
        "#,
        );
    }

    // ── Ascription ──────────────────────────────────────────────────

    #[test]
    fn test_ascription_correct_type() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42 as Int
  x
}
        "#,
        );
    }

    #[test]
    fn test_ascription_mismatch() {
        assert_has_error(
            r#"
fn main() {
  "hello" as Int
}
        "#,
            "type mismatch",
        );
    }

    // ── Pipe operator inference ─────────────────────────────────────

    #[test]
    fn test_pipe_chains_types() {
        assert_no_errors(
            r#"
fn double(x) = x * 2
fn add_one(x) = x + 1
fn main() {
  5 |> double |> add_one
}
        "#,
        );
    }

    // ── Record update inference ─────────────────────────────────────

    #[test]
    fn test_record_update_preserves_type() {
        assert_no_errors(
            r#"
type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  let q = p.{ x: 10 }
  q.y
}
        "#,
        );
    }

    #[test]
    fn test_record_create_wrong_field_type() {
        assert_has_error(
            r#"
type Point { x: Int, y: Int }
fn main() {
  Point { x: "hello", y: 2 }
}
        "#,
            "type mismatch",
        );
    }

    // ── check_pattern error cases ───────────────────────────────────

    #[test]
    fn test_check_pattern_wrong_type() {
        assert_has_error(
            r#"
fn main() {
  match 42 {
    true -> "yes"
    false -> "no"
  }
}
        "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_list_rest_pattern_binds() {
        assert_no_errors(
            r#"
fn sum_list(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum_list(tail)
  }
}
fn main() { sum_list([1, 2, 3]) }
        "#,
        );
    }

    // ── Range pattern type checking ─────────────────────────────────

    #[test]
    fn test_range_pattern_int() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    1..10 -> "small"
    _ -> "big"
  }
}
fn main() { classify(5) }
        "#,
        );
    }

    // ── When-bool statement ─────────────────────────────────────────

    #[test]
    fn test_when_bool_condition_must_be_bool() {
        assert_has_error(
            r#"
fn check(x) {
  when 42 else { return 0 }
  x
}
fn main() { check(1) }
        "#,
            "type mismatch",
        );
    }

    // ── Loop/recur type inference ───────────────────────────────────

    #[test]
    fn test_loop_bindings_inferred() {
        assert_no_errors(
            r#"
fn factorial(n) {
  loop i = n, acc = 1 {
    match i <= 1 {
      true -> acc
      false -> loop(i - 1, acc * i)
    }
  }
}
fn main() { factorial(5) }
        "#,
        );
    }

    // ── Trait constraint checking at definition ────────────────────

    #[test]
    fn test_trait_constraint_method_resolved() {
        // A constrained type variable should allow calling trait methods
        assert_no_errors(
            r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.display()
}
fn main() { show(42) }
        "#,
        );
    }

    #[test]
    fn test_trait_constraint_unknown_method_errors() {
        // A constrained type variable should NOT allow calling methods not in the trait
        assert_has_error(
            r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.nonexistent()
}
fn main() { show(42) }
        "#,
            "no method 'nonexistent' found in trait constraints",
        );
    }

    // ── Error type propagation ─────────────────────────────────────

    #[test]
    fn test_error_type_does_not_produce_fresh_var() {
        // Accessing a field on an error type should propagate the error,
        // not create a new unresolved type variable
        assert_has_error(
            r#"
fn main() {
  let x = undefined_var
  x.field
}
        "#,
            "undefined",
        );
    }
}
