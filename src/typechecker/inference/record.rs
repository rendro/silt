//! The type of a record literal and of a record update (arms of
//! `infer_expr_kind`).

use super::*;

impl TypeChecker {
    /// `infer_expr_kind` for a record literal. `Break` is a type
    /// `infer_expr_kind` returns as it is, without writing it on the expression.
    pub(super) fn infer_record_create(
        &mut self,
        expr: &mut Expr,
        env: &mut TypeEnv,
        span: Span,
    ) -> ControlFlow<Type, Type> {
        let ExprKind::RecordCreate { name, fields, .. } = &mut expr.kind else {
            unreachable!("infer_record_create of another form");
        };
        ControlFlow::Continue({
            let name = *name;
            // A field written twice in a record literal
            // (e.g. `User { name: "a", name: "b" }`) is an error:
            // downstream processing goes through a HashSet that would
            // silently dedupe them. Mirror the record type
            // declaration's duplicate-field check: walk once and
            // emit a diagnostic per duplicate.
            {
                let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
                for (field_name, _) in fields.iter() {
                    if !seen.insert(*field_name) {
                        self.error(
                            Code::DuplicateRecordField,
                            format!(
                                "duplicate field '{}' in record literal for '{}'",
                                field_name, name
                            ),
                            span,
                        );
                    }
                }
            }
            // The type identity is the bare name: `util.Pt { x: 1 }`
            // builds a `Pt`.
            let looked = self.named_record(expr.res, name, span, false);
            if let Some((rec_ty, ..)) = &looked
                && let Some(module) = self.opaque_outside(*rec_ty)
            {
                let message = format!(
                    "cannot build a '{}' outside module '{module}': the type is opaque",
                    super::opaque::opaque_name(*rec_ty, module)
                );
                self.opaque_error(message, module, span);
                for (_, e) in fields.iter_mut() {
                    let _ = self.infer_expr(e, env);
                }
                expr.ty = Some(Type::Error);
                return ControlFlow::Break(Type::Error);
            }
            if let Some((rec_ty, rec_info, param_ids, fixed)) = looked {
                // For parameterized record types, create fresh type variables
                // for each type parameter and substitute them into field types.
                // This prevents different instantiations from sharing the same
                // template variables (e.g., Box { value: 42 } and Box { value: "hi" }).
                let (instantiated_fields, type_args) =
                    self.instantiate_record_fields(&rec_info, param_ids.as_deref());
                // An alias's arguments (`BI { .. }` for `type BI = B(Int)`).
                for (arg, fixed) in type_args.iter().zip(fixed.iter().flatten()) {
                    self.unify(arg, fixed, span);
                }

                let field_types: Vec<(Symbol, Type)> = fields
                    .iter_mut()
                    .map(|(n, e)| {
                        let ty = self.infer_expr(e, env);
                        (*n, ty)
                    })
                    .collect();

                // Unify with declared field types (using instantiated
                // copies), each at its value.
                for ((field_name, inferred_ty), (_, value)) in field_types.iter().zip(&*fields) {
                    if let Some((_, declared_ty)) =
                        instantiated_fields.iter().find(|(n, _)| n == field_name)
                    {
                        self.unify_field(*field_name, inferred_ty, declared_ty, value.span);
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
                    let missing_str: Vec<String> = missing.iter().map(|s| format!("{s}")).collect();
                    self.error(
                        Code::MissingField,
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
                        // Append a did-you-mean
                        // hint for record-literal typos — e.g.
                        // `User { nam: ... }` → `did you mean \`name\`?`.
                        let base = format!("unknown field '{}' in {}", field_name, name);
                        self.error_help(
                            Code::UnknownField,
                            format_record_field_suggestion(base, *field_name, &rec_info.fields),
                            span,
                        );
                    }
                }

                Type::Generic(rec_ty, type_args)
            } else {
                // An unknown record type (reported by `named_record`
                // or the resolver). The field expressions are still
                // walked so nested errors are reported, but the type
                // is Type::Error to prevent downstream cascades.
                for (_, e) in fields.iter_mut() {
                    let _ = self.infer_expr(e, env);
                }
                Type::Error
            }
        })
    }

    /// `infer_expr_kind` for a record update. `Break` is a type
    /// `infer_expr_kind` returns as it is, without writing it on the expression.
    pub(super) fn infer_record_update(
        &mut self,
        expr: &mut Expr,
        env: &mut TypeEnv,
        span: Span,
    ) -> ControlFlow<Type, Type> {
        let ExprKind::RecordUpdate {
            expr: base, fields, ..
        } = &mut expr.kind
        else {
            unreachable!("infer_record_update of another form");
        };
        ControlFlow::Continue({
            let base_span = base.span;
            let base_ty = self.infer_expr(base, env);
            let resolved = self.apply(&base_ty);
            if let Some((ty, module)) = self.opaque_value(&resolved) {
                let message = format!(
                    "cannot update a '{}' outside module '{module}': the type is opaque",
                    super::opaque::opaque_name(ty, module)
                );
                self.opaque_error(message, module, span);
                for (_, e) in fields.iter_mut() {
                    let _ = self.infer_expr(e, env);
                }
                expr.ty = Some(Type::Error);
                return ControlFlow::Break(Type::Error);
            }
            // A field written twice in a record-update
            // expression (e.g. `r.{ age: 1, age: 2 }`) would be
            // silently deduped downstream. Emit a diagnostic per
            // duplicate so the typo is surfaced at compile time.
            {
                let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
                for (field_name, _) in fields.iter() {
                    if !seen.insert(*field_name) {
                        self.error(
                            Code::DuplicateRecordField,
                            format!("duplicate field '{}' in record update", field_name),
                            span,
                        );
                    }
                }
            }
            // Two cases:
            //  1. `Type::Generic(name, args)`, a declared record —
            //     validate against the declared fields.
            //  2. Anything else — compile-time reject.
            let mut handled = false;
            if let Type::Generic(type_name, type_args) = &resolved
                && self.tables.records.contains_key(type_name)
            {
                let instantiated_fields =
                    self.instantiate_record_fields_with_args(*type_name, type_args);
                let declared: std::collections::HashMap<Symbol, Type> = instantiated_fields
                    .iter()
                    .map(|(n, t)| (*n, t.clone()))
                    .collect();
                for (field_name, field_expr) in &mut *fields {
                    let ft = self.infer_expr(field_expr, env);
                    if let Some(declared_ty) = declared.get(field_name) {
                        self.unify_field(*field_name, &ft, declared_ty, field_expr.span);
                    } else {
                        // Did-you-mean on the
                        // generic-record update path.
                        let base = format!("unknown field '{field_name}' in {type_name}");
                        self.error_help(
                            Code::UnknownField,
                            format_record_field_suggestion(base, *field_name, &instantiated_fields),
                            span,
                        );
                    }
                }
                handled = true;
            }
            if !handled {
                // The base's type is unknown: each field waits for
                // it (`Goal::Update`).
                let waits = matches!(resolved, Type::Var(_));
                for (field_name, field_expr) in &mut *fields {
                    let value = self.infer_expr(field_expr, env);
                    if waits {
                        self.want_goal(
                            Goal::Update {
                                base: base_ty.clone(),
                                field: *field_name,
                                value,
                            },
                            Origin {
                                span,
                                callee: None,
                                op: None,
                                header: None,
                            },
                        );
                    }
                }
                if !matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                    let message = self.not_a_record_base(&resolved);
                    self.error(Code::TypeMismatch, message, base_span);
                }
            }
            base_ty
        })
    }
}
