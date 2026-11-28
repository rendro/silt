//! The type of a literal that holds expressions: an interpolated string, a
//! list, a map, a set (arms of `infer_expr_kind`).

use super::*;

impl TypeChecker {
    /// `infer_expr_kind` for an interpolated string.
    pub(super) fn infer_string_interp(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let ExprKind::StringInterp(parts) = &mut expr.kind else {
            unreachable!("infer_string_interp of another form");
        };
        // Each part is either a literal or an expression
        for part in parts {
            if let StringPart::Expr(e) = part {
                let expr_span = e.span;
                let t = self.infer_expr(e, env);
                let resolved = self.apply(&t);
                self.want(
                    Pred::Trait {
                        tr: TraitKey::builtin("Display"),
                        args: Vec::new(),
                        subject: resolved,
                        asker: None,
                    },
                    Origin {
                        span: expr_span,
                        callee: None,
                        op: None,
                        header: None,
                    },
                );
            }
        }
        Type::String
    }

    /// `infer_expr_kind` for a list literal.
    pub(super) fn infer_list(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let ExprKind::List(elems) = &mut expr.kind else {
            unreachable!("infer_list of another form");
        };
        if elems.is_empty() {
            let tv = self.fresh_var();
            Type::Container(Container::List(Box::new(tv)))
        } else {
            // Each element is inferred first (without unifying),
            // so that one "list elements must have the same type"
            // error points at the first element that differs. What
            // `..xs` adds are the elements of `xs`, which must be
            // a list: a spread of anything else is reported as
            // that, and says nothing of the elements.
            let mut elem_infos: Vec<(usize, Type, Span)> = Vec::with_capacity(elems.len());
            for (idx, elem) in elems.iter_mut().enumerate() {
                match elem {
                    ListElem::Single(e) => {
                        let t = self.infer_expr(e, env);
                        elem_infos.push((idx, t, e.span));
                    }
                    ListElem::Spread(e) => {
                        let t = self.infer_expr(e, env);
                        let inner = self.fresh_var();
                        let list = Type::Container(Container::List(Box::new(inner.clone())));
                        if self.unify_types(&t, &list).is_ok() {
                            elem_infos.push((idx, inner, e.span));
                        } else {
                            let shown = self.show_type(&self.apply(&t));
                            self.error(
                                Code::TypeMismatch,
                                format!("`..` needs a list to spread, got {shown}"),
                                e.span,
                            );
                        }
                    }
                }
            }

            let elem_type = self.fresh_var();
            if let Some((_, first_ty, first_span)) = elem_infos.first().cloned() {
                self.unify(&elem_type, &first_ty, first_span);
                for (idx, t, espan) in &elem_infos {
                    if self.unify_types(&elem_type, t).is_err() {
                        // A list-level message, clearer than the
                        // mismatch of the two types.
                        let (first, elem) = (self.apply(&first_ty), self.apply(t));
                        let (first_shown, elem_shown) = self.show_apart(&first, &elem);
                        // Two channel operations of different
                        // message types: what `channel.select`
                        // is given, which takes one.
                        let channel_op = |ty: &Type| matches!(ty, Type::Generic(name, _) if name.is_builtin("ChannelOp"));
                        let help = (channel_op(&first) && channel_op(&elem)).then(|| {
                            format!(
                                "{} takes channels of one message type: give the signal \
                                 channel the same type and close it to signal (a closed \
                                 channel is received as None), or wrap the messages in one enum",
                                self.written_builtin("channel.select")
                            )
                        });
                        self.error_help(
                            Code::TypeMismatch,
                            (
                                format!(
                                    "list elements must have the same type: first element is {}, but element {} is {}",
                                    first_shown,
                                    idx + 1,
                                    elem_shown
                                ),
                                help,
                            ),
                            *espan,
                        );
                    }
                }
            }

            Type::Container(Container::List(Box::new(elem_type)))
        }
    }

    /// `infer_expr_kind` for a map literal.
    pub(super) fn infer_map(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Map(entries) = &mut expr.kind else {
            unreachable!("infer_map of another form");
        };
        // A key written twice as the same constant: the first value
        // would be lost without a word. (A computed key is not looked
        // at.)
        let mut seen = Vec::new();
        for (key, _) in entries.iter() {
            let Some(constant) = MapKey::of(key) else {
                continue;
            };
            if seen.contains(&constant) {
                self.error(
                    Code::DuplicateRecordField,
                    format!("duplicate key {constant} in map literal"),
                    key.span,
                );
            } else {
                seen.push(constant);
            }
        }
        if entries.is_empty() {
            let k = self.fresh_var();
            let v = self.fresh_var();
            Type::Container(Container::Map(Box::new(k), Box::new(v)))
        } else {
            let mut iter = entries.iter_mut();
            let first_entry = iter.next().unwrap();
            let first_k = self.infer_expr(&mut first_entry.0, env);
            let first_k = self.kept(first_k);
            let first_v = self.infer_expr(&mut first_entry.1, env);
            let first_v = self.kept(first_v);
            // The unify calls here pass the deviant entry as t1 and
            // the established type as t2, the documented
            // t1=got/t2=expected convention (`#{ "a": 1, 2: 3 }`
            // expects String, got Int), and, mirroring the list-
            // literal arm above, replace the raw unify mismatch
            // with a curated map-level message.
            for (idx, (k, v)) in iter.enumerate() {
                let entry_num = idx + 2; // 1-based; entry 1 set the type
                let k_span = k.span;
                let v_span = v.span;
                let kt = self.infer_expr(k, env);
                let vt = self.infer_expr(v, env);
                if self.unify_types(&kt, &first_k).is_err() {
                    let first_resolved = self.apply(&first_k);
                    let kt_resolved = self.apply(&kt);
                    self.error(Code::TypeMismatch,
                        format!(
                            "map keys must have the same type: first key is {first_resolved}, but key {entry_num} is {kt_resolved}"
                        ),
                        k_span,
                    );
                }
                if self.unify_types(&vt, &first_v).is_err() {
                    let first_resolved = self.apply(&first_v);
                    let vt_resolved = self.apply(&vt);
                    self.error(Code::TypeMismatch,
                        format!(
                            "map values must have the same type: first value is {first_resolved}, but value {entry_num} is {vt_resolved}"
                        ),
                        v_span,
                    );
                }
            }
            // A map hashes its keys.
            self.want_hash(&first_k, span);
            Type::Container(Container::Map(Box::new(first_k), Box::new(first_v)))
        }
    }

    /// `infer_expr_kind` for a set literal.
    pub(super) fn infer_set(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::SetLit(elems) = &mut expr.kind else {
            unreachable!("infer_set of another form");
        };
        if elems.is_empty() {
            let tv = self.fresh_var();
            Type::Container(Container::Set(Box::new(tv)))
        } else {
            // The same direction and curated message as the map arm
            // above (`#[1, 2, "x"]` expects Int, got String), in the
            // list-literal wording for set elements.
            let elem_type = self.fresh_var();
            for (idx, e) in elems.iter_mut().enumerate() {
                let espan = e.span;
                let t = self.infer_expr(e, env);
                if self.unify_types(&t, &elem_type).is_err() {
                    let first_resolved = self.apply(&elem_type);
                    let t_resolved = self.apply(&t);
                    self.error(Code::TypeMismatch,
                        format!(
                            "set elements must have the same type: first element is {}, but element {} is {}",
                            first_resolved,
                            idx + 1,
                            t_resolved
                        ),
                        espan,
                    );
                }
            }
            // A set hashes its elements.
            self.want_hash(&elem_type, span);
            Type::Container(Container::Set(Box::new(elem_type)))
        }
    }
}

/// A map key written as a constant: a text, a number, a Bool or a
/// variant without fields (`Red`, `Color.Red`). Two keys of one map
/// literal written as the same constant are the same key.
#[derive(PartialEq)]
enum MapKey {
    Text(String),
    Int(i64),
    /// By its bits, a zero of either sign as `0.0`: two floats are
    /// one key when the map takes them for one (`-0.0 == 0.0`).
    Float(u64),
    Bool(bool),
    Variant(Symbol),
}

impl MapKey {
    fn float(x: f64) -> MapKey {
        MapKey::Float(if x == 0.0 { 0.0f64 } else { x }.to_bits())
    }

    fn of(key: &Expr) -> Option<MapKey> {
        Some(match &key.kind {
            ExprKind::StringLit(text) => MapKey::Text(text.clone()),
            ExprKind::Int(n) => MapKey::Int(*n),
            ExprKind::Float(x) => MapKey::float(*x),
            ExprKind::Bool(b) => MapKey::Bool(*b),
            ExprKind::Unary(UnaryOp::Neg, inner) => match inner.kind {
                ExprKind::Int(n) => MapKey::Int(n.checked_neg()?),
                ExprKind::Float(x) => MapKey::float(-x),
                _ => return None,
            },
            // A capital names a variant (`Red`, `shapes.Red`,
            // `Color.Red`): one with fields is no key a map can have.
            ExprKind::Ident(name) | ExprKind::FieldAccess(_, name, _)
                if resolve(*name).starts_with(char::is_uppercase) =>
            {
                MapKey::Variant(*name)
            }
            _ => return None,
        })
    }
}

impl std::fmt::Display for MapKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapKey::Text(text) => write!(f, "{text:?}"),
            MapKey::Int(n) => write!(f, "{n}"),
            MapKey::Float(bits) => write!(f, "{:?}", f64::from_bits(*bits)),
            MapKey::Bool(b) => write!(f, "{b}"),
            MapKey::Variant(name) => write!(f, "{}", resolve(*name)),
        }
    }
}
