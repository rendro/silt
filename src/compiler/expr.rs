//! The longer arms of `compile_expr_kind`: a binary operator, a field
//! access, an interpolated string, a closure, a list literal.

use super::*;

impl Compiler<'_> {
    /// `compile_expr_kind` for a binary operator.
    pub(super) fn compile_binary(&mut self, expr: &Expr, span: Span) -> Result<(), Diagnostic> {
        let ExprKind::Binary(left, op, right) = &expr.kind else {
            unreachable!("compile_binary of another form");
        };
        match op {
            BinOp::And => {
                // Short-circuit: if left is false, skip right
                self.compile_expr(left)?;
                // Duplicate TOS so we can test and still have the value
                self.emit(Asm::Dup, span)?;
                let jump = self.jump_if_false(span)?;
                // Left was truthy, discard it and evaluate right
                self.emit(Asm::Pop, span)?;
                self.compile_expr(right)?;
                self.bind_over(jump, "bytes of code in the right operand of `&&`", span)?;
            }
            BinOp::Or => {
                // Short-circuit: if left is true, skip right
                self.compile_expr(left)?;
                // Duplicate TOS so we can test and still have the value
                self.emit(Asm::Dup, span)?;
                let jump = self.jump_if_true(span)?;
                // Left was falsy, discard it and evaluate right
                self.emit(Asm::Pop, span)?;
                self.compile_expr(right)?;
                self.bind_over(jump, "bytes of code in the right operand of `||`", span)?;
            }
            _ => {
                self.compile_operands([&**left, &**right])?;
                let instruction = match op {
                    BinOp::Add => Asm::Add,
                    BinOp::Sub => Asm::Sub,
                    BinOp::Mul => Asm::Mul,
                    BinOp::Div => Asm::Div,
                    BinOp::Mod => Asm::Mod,
                    BinOp::Eq => Asm::Eq,
                    BinOp::Neq => Asm::Neq,
                    BinOp::Lt => Asm::Lt,
                    BinOp::Gt => Asm::Gt,
                    BinOp::Leq => Asm::Leq,
                    BinOp::Geq => Asm::Geq,
                    BinOp::And | BinOp::Or => unreachable!(),
                };
                self.emit(instruction, span)?;
            }
        }
        Ok(())
    }

    /// `compile_expr_kind` for an interpolated string.
    pub(super) fn compile_string_interp(
        &mut self,
        expr: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let ExprKind::StringInterp(parts) = &expr.kind else {
            unreachable!("compile_string_interp of another form");
        };
        // The value of every expression, or its text
        // (`DisplayValue`), stays on the stack until
        // `Interpolate`; a literal part is a constant it reads
        // where it is.
        let mut segments = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                StringPart::Literal(s) => {
                    let k = self.add_constant(Value::string(s.clone()), span)?;
                    segments.push(Segment::Literal(k));
                }
                StringPart::Expr(e) => {
                    self.compile_expr(e)?;
                    self.emit(Asm::DisplayValue, span)?;
                    segments.push(Segment::Value);
                }
            }
        }
        self.emit(
            Asm::Interpolate {
                segments: &segments,
            },
            span,
        )?;
        Ok(())
    }

    /// `compile_expr_kind` for a closure.
    pub(super) fn compile_lambda(&mut self, expr: &Expr, span: Span) -> Result<(), Diagnostic> {
        let ExprKind::Lambda { params, body, .. } = &expr.kind else {
            unreachable!("compile_lambda of another form");
        };
        let own_name = self.closure_name.take();
        let own_global = self.closure_global.take();
        let label = self.closure_label.take();
        // A closure is named after what it is written in, and
        // so are the closures in it; one a `let` binds directly
        // is named by its binding.
        let (closures, place) = match self.contexts.last() {
            Some(around) => (around.closures.clone(), around.place.clone()),
            None => return Err(checker_missed(span, "a closure outside any code")),
        };
        let name = label.unwrap_or_else(|| closures.clone());
        self.begin_function(name, params.len(), span)?;
        if let Some(closure) = self.contexts.last_mut() {
            closure.closures = closures;
            closure.place = place;
        }

        // A closure that calls itself by the name its `let`
        // gives it has itself in a local of that name, unless a
        // parameter takes the name; a tail call of it by that name
        // jumps back here, to the start of its code.
        let itself = own_name.filter(|&name| {
            reads_local(body, name)
                && !params
                    .iter()
                    .any(|p| Self::pattern_binding_names(&p.pattern).contains(&name))
        });
        // (One a top-level `let` binds calls itself by its global.)
        let start = match itself.is_some() || own_global.is_some() {
            true => {
                let start = self.label();
                self.bind(start, span)?;
                Some(start)
            }
            false => None,
        };
        if let (Some(slot), Some(start)) = (own_global, start) {
            self.ctx_mut().self_call = Some(SelfCall {
                by: SelfName::Global(slot),
                start,
                params: params.len(),
            });
        }

        self.compile_params(params, span)?;

        if let (Some(name), Some(start)) = (itself, start) {
            self.emit(Asm::GetClosure, span)?;
            let slot = self.add_local(name, span)?;
            self.emit(Asm::SetLocal { slot }, span)?;
            self.ctx_mut().self_call = Some(SelfCall {
                by: SelfName::Local(slot),
                start,
                params: params.len(),
            });
        }

        // Compile the lambda body in tail position for TCO.
        self.in_tail_position = true;
        self.compile_expr(body)?;
        self.in_tail_position = false;
        self.emit_body_return(span)?;

        let (func, upvalue_descs) = self.end_function(span)?;

        let vm_closure = Arc::new(VmClosure::new(Arc::new(func), vec![]));
        let closure_val = Value::VmClosure(vm_closure);
        let fi = self.add_constant(closure_val, span)?;

        // A closure is made each time the lambda is evaluated,
        // one that captures nothing too: then it is the
        // evaluating task's own, and the tasks that evaluate
        // one lambda (and call what they made, once for each
        // element of a list) do not all count references to
        // one constant.
        self.emit(
            Asm::MakeClosure {
                f: fi,
                captures: &upvalue_descs,
            },
            span,
        )?;
        Ok(())
    }

    /// `compile_expr_kind` for a list literal.
    pub(super) fn compile_list(&mut self, expr: &Expr, span: Span) -> Result<(), Diagnostic> {
        let ExprKind::List(elems) = &expr.kind else {
            unreachable!("compile_list of another form");
        };
        let has_spread = elems.iter().any(|e| matches!(e, ListElem::Spread(_)));
        if !has_spread {
            // Fast path: no spreads, just compile all singles
            self.compile_operands(elems.iter().filter_map(|elem| match elem {
                ListElem::Single(e) => Some(e),
                ListElem::Spread(_) => None,
            }))?;
            self.emit(Asm::MakeList { count: elems.len() }, span)?;
        } else {
            // Spread path: group consecutive singles into segments,
            // compile each spread, and ListConcat them together.
            //
            // While an element is compiled the stack holds the list
            // accumulated so far (if any) and the singles not yet
            // collected.
            let mut have_accumulated = false;
            let mut single_count: usize = 0;

            for elem in elems {
                match elem {
                    ListElem::Single(e) => {
                        self.compile_expr(e)?;
                        single_count += 1;
                    }
                    ListElem::Spread(e) => {
                        // Flush any pending singles as a MakeList
                        if single_count > 0 {
                            self.emit(
                                Asm::MakeList {
                                    count: single_count,
                                },
                                span,
                            )?;
                            if have_accumulated {
                                self.emit(Asm::ListConcat, span)?;
                            }
                            have_accumulated = true;
                            single_count = 0;
                        }
                        // Compile the spread expression (should be a list or range)
                        self.compile_expr(e)?;
                        if have_accumulated {
                            self.emit(Asm::ListConcat, span)?;
                        } else {
                            have_accumulated = true;
                        }
                    }
                }
            }
            // Flush any trailing singles
            if single_count > 0 {
                self.emit(
                    Asm::MakeList {
                        count: single_count,
                    },
                    span,
                )?;
                if have_accumulated {
                    self.emit(Asm::ListConcat, span)?;
                }
            } else if !have_accumulated {
                // Edge case: empty list with spreads (shouldn't happen, but be safe)
                self.emit(Asm::MakeList { count: 0 }, span)?;
            }
        }
        Ok(())
    }

    /// `compile_expr_kind` for a field access: a variant, a method of a
    /// trait or of a type, a function of a module, a record's field.
    pub(super) fn compile_field_access(
        &mut self,
        expr: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match &expr.kind {
            ExprKind::FieldAccess(..) if let Some(variant) = self.variant_value(expr) => {
                let idx = self.add_constant(variant, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            // `Tr.m` as a value: the method of the one impl the checker
            // selected, or the function `{ a, b -> Tr.m(a, b) }`, which
            // finds it where it runs. The program wrote no closure there:
            // the function has the method's name, `Tr.m`.
            // `Tr.m` of a trait a program declares, as a value: the
            // method of the dictionary the evidence names, with it bound.
            ExprKind::FieldAccess(of, method, _)
                if self.res_trait(of.res).is_some() && self.dict_call(expr).is_some() =>
            {
                let (tr, recv, own) = self.dict_call(expr).expect("a call through a dictionary");
                let (recv, own) = (recv.clone(), own.to_vec());
                self.compile_dict_method_value(tr, &recv, &own, *method, span)?;
            }

            ExprKind::FieldAccess(of, method, _) if self.res_trait(of.res).is_some() => {
                let tr = self.res_trait(of.res).expect("a trait");
                if let Some(crate::ast::Selection::Impl { tr, ty }) = expr.sel
                    && let Some(slot) = self.globals.method(tr, ty, &resolve(*method))
                {
                    self.emit(Asm::GetGlobal { slot }, span)?;
                } else {
                    let arity = match &expr.ty {
                        Some(Type::Fun(params, _)) => params.len(),
                        _ => return Err(checker_missed(span, "a trait method of no type")),
                    };
                    let names: Vec<Symbol> =
                        (0..arity).map(|k| intern(&format!("__arg{k}__"))).collect();
                    let ident = |name: Symbol| Expr::new(ExprKind::Ident(name), span);
                    let call = Expr::new(
                        ExprKind::Call(
                            Box::new(expr.clone()),
                            names.iter().map(|n| ident(*n)).collect(),
                        ),
                        span,
                    );
                    let lambda = Expr::new(
                        ExprKind::Lambda {
                            params: names
                                .iter()
                                .map(|n| crate::ast::Param {
                                    kind: crate::ast::ParamKind::Data,
                                    pattern: crate::ast::Pattern::new(
                                        crate::ast::PatternKind::Ident(*n),
                                        span,
                                    ),
                                    ty: None,
                                })
                                .collect(),
                            body: Box::new(call),
                        },
                        span,
                    );
                    let method_name = format!("{}.{method}", self.units.defs.get(tr.0).name);
                    let around = self
                        .contexts
                        .last_mut()
                        .map(|context| std::mem::replace(&mut context.closures, method_name));
                    let compiled = self.compile_expr(&lambda);
                    if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                        context.closures = around;
                    }
                    compiled?;
                }
            }

            // `T.decode` as a value, a method the VM has natively for the
            // type: the function `{ v -> T.decode(v) }`. The program wrote
            // no closure there: the function has the method's name,
            // `T.decode`, as a written method has.
            ExprKind::FieldAccess(of, method, _)
                if let Some(TypeMember::Native(_)) = self.type_member(expr)? =>
            {
                let ty = match &of.kind {
                    ExprKind::Ident(ty) | ExprKind::FieldAccess(_, ty, _) => *ty,
                    _ => return Err(checker_missed(span, "a method of no named type")),
                };
                let method_name = format!("{ty}.{method}");
                let names: &[&str] = &["__value__"];
                let ident = |name: &str| Expr::new(ExprKind::Ident(intern(name)), span);
                let call = Expr::new(
                    ExprKind::Call(Box::new(expr.clone()), vec![ident(names[0])]),
                    span,
                );
                let lambda = Expr::new(
                    ExprKind::Lambda {
                        params: names
                            .iter()
                            .map(|n| crate::ast::Param {
                                kind: crate::ast::ParamKind::Data,
                                pattern: crate::ast::Pattern::new(
                                    crate::ast::PatternKind::Ident(intern(n)),
                                    span,
                                ),
                                ty: None,
                            })
                            .collect(),
                        body: Box::new(call),
                    },
                    span,
                );
                let around = self
                    .contexts
                    .last_mut()
                    .map(|context| std::mem::replace(&mut context.closures, method_name));
                let compiled = self.compile_expr(&lambda);
                if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                    context.closures = around;
                }
                compiled?;
            }

            // `Int.empty` of a program's trait, as a value, or `t.empty` of
            // a `type t` bounded by one: through the dictionary, with it
            // bound. (The type named or held is no value of the method.)
            ExprKind::FieldAccess(_, method, _) if self.dict_call(expr).is_some() => {
                let (tr, recv, own) = self.dict_call(expr).expect("a call through a dictionary");
                let (recv, own) = (recv.clone(), own.to_vec());
                self.compile_dict_method_value(tr, &recv, &own, *method, span)?;
            }

            // `Box(Int).decode` as a value, a method of a type held as a
            // value (`let t = Box(Int); t.decode`) or of an alias whose
            // written `decode` takes its type arguments from it (`IB.decode`
            // for `type IB = Box(Int)`): the function that calls it, `{ v ->
            // Box(Int).decode(v) }`, named as the method.
            ExprKind::FieldAccess(of, method, _) if self.method_of_a_full_type(expr)? => {
                let arity = match &expr.ty {
                    Some(Type::Fun(params, _)) => params.len(),
                    _ => return Err(checker_missed(span, "a method of no type")),
                };
                let names: Vec<Symbol> =
                    (0..arity).map(|k| intern(&format!("__arg{k}__"))).collect();
                let ident = |name: Symbol| Expr::new(ExprKind::Ident(name), span);
                let call = Expr::new(
                    ExprKind::Call(
                        Box::new(expr.clone()),
                        names.iter().map(|n| ident(*n)).collect(),
                    ),
                    span,
                );
                let lambda = Expr::new(
                    ExprKind::Lambda {
                        params: names
                            .iter()
                            .map(|n| crate::ast::Param {
                                kind: crate::ast::ParamKind::Data,
                                pattern: crate::ast::Pattern::new(
                                    crate::ast::PatternKind::Ident(*n),
                                    span,
                                ),
                                ty: None,
                            })
                            .collect(),
                        body: Box::new(call),
                    },
                    span,
                );
                let shown = written_type_of(of).map_or_else(String::new, shown);
                let around = (self.contexts.last_mut()).map(|context| {
                    std::mem::replace(&mut context.closures, format!("{shown}.{method}"))
                });
                let compiled = self.compile_expr(&lambda);
                if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                    context.closures = around;
                }
                compiled?;
            }

            ExprKind::FieldAccess(..)
                if let Some(TypeMember::Slot(slot)) = self.type_member(expr)? =>
            {
                self.emit(Asm::GetGlobal { slot }, span)?;
            }

            // `m.f`, `m.limit`, `list.map` used as a value.
            ExprKind::FieldAccess(..) if let Some(def) = self.value_def(expr.res) => {
                self.emit_global_value(def, span)?;
            }

            // A record's field. (Tuple indexing, `t.0`, is rejected by
            // the checker.)
            ExprKind::FieldAccess(expr, field, _) => {
                self.compile_expr(expr)?;
                self.emit_get_field(self.receiver_field(expr, *field, span)?, span)?;
            }
            _ => unreachable!("compile_field_access of another form"),
        }
        Ok(())
    }
}
