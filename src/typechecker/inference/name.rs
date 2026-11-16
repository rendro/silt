//! The type of a name and of a field access (arms of `infer_expr_kind`).

use super::*;

impl TypeChecker {
    /// `infer_expr_kind` for a name.
    pub(super) fn infer_ident(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Ident(name) = &mut expr.kind else {
            unreachable!("infer_ident of another form");
        };
        let name = *name;
        if expr.res == Some(crate::defs::Res::Error) {
            // The resolver reported the name, or it comes from a
            // module that failed to load.
            Type::Error
        } else if let Some(scheme) = self
            .def_scheme(expr.res, env)
            .or_else(|| env.lookup(name).cloned())
        {
            // A name used as a value owes what a call of it does.
            self.named_use.get_or_insert(Origin {
                span,
                callee: Some(name),
                op: None,
                header: None,
            });
            let ty = self.instantiate_def(&scheme, expr.res);
            // (A generic type's name, `List`: its value names the
            // type alone.)
            if self.names_generic_type(expr) {
                self.open_vars.extend(free_vars_in(&ty));
            }
            ty
        } else if name == intern("self") {
            // `self` is resolved at runtime — allow without error
            self.fresh_var()
        } else if name == intern("Self")
            && expr.res == Some(crate::defs::Res::Local)
            && let Some(ty) = self.sig_type_var(name)
        {
            // A trait's default body gives `Self` to a call as a
            // type (the resolver allows it there alone).
            Type::type_of(ty)
        } else if expr.res == Some(crate::defs::Res::Local)
            && self.decode_params.contains(&name)
            && let Some(ty) = self.sig_type_var(name)
        {
            // A type parameter of an impl's header the call gives
            // (`decode_params`), as a type: `json.decode(v, Tree(a))`.
            Type::type_of(ty)
        } else if self.type_receiver
            && expr.res == Some(crate::defs::Res::Local)
            && let Some(ty) = self.sig_type_var(name)
        {
            // A type variable of the signature among the arguments
            // of a type before a dot, as a type: `Box(a).zero()`.
            Type::type_of(ty)
        } else if let Some(ty) = self.alias_value(expr.res, env) {
            ty
        } else if self.res_def(expr.res).is_some_and(|def| {
            matches!(
                def.kind,
                crate::defs::DefKind::TypeAlias | crate::defs::DefKind::Trait(_)
            )
        }) {
            // A type alias or a trait used as a value.
            self.error(
                Code::UndefinedVariable,
                format!("'{name}' is not a value"),
                span,
            );
            Type::Error
        } else {
            let (message, mut help) = format_undefined_variable_message(name, env, "");
            if let Some((at, tuple)) = &self.tuple_spelling
                && *at == span
            {
                help = Some(format!("a tuple type before a dot is written `{tuple}`"));
            }
            self.error_help(Code::UndefinedVariable, (message, help), span);
            self.fresh_var()
        }
    }

    /// `infer_expr_kind` for a field access (`p.x`, `m.f`, `Shape.Circle`, `x.method`).
    /// `Break` is a type `infer_expr_kind` returns as it is, without writing it
    /// on the expression.
    pub(super) fn infer_field_access(
        &mut self,
        expr: &mut Expr,
        env: &mut TypeEnv,
        span: Span,
        called: bool,
    ) -> ControlFlow<Type, Type> {
        let ExprKind::FieldAccess(obj, field, _) = &mut expr.kind else {
            unreachable!("infer_field_access of another form");
        };
        self.receiver_taken = false;
        let field = *field;
        // (An access that is no callee is a value: a message about
        // it writes it as it is written.)
        if !called && let ExprKind::Ident(name) = obj.kind {
            self.method_values.insert(span, format!("{name}.{field}"));
        }
        // What the resolver said the access names: nothing (it
        // reported why, or the module failed to load), a variant
        // (`m.Red`, `Shape.Red`, `m.Shape.Red`), whose constructor
        // is bound as `Enum.Variant`, or a member of a module
        // (`m.f`, `m.Pt`), bound as `m.f`.
        if expr.res == Some(crate::defs::Res::Error) {
            expr.ty = Some(Type::Error);
            return ControlFlow::Break(Type::Error);
        }
        let names_member = self.res_variant_enum(expr.res).is_some()
            || matches!(obj.res, Some(crate::defs::Res::Module(_)));
        if names_member {
            let ty = match self.def_scheme(expr.res, env) {
                Some(scheme) => {
                    let ty = self.instantiate_def(&scheme, expr.res);
                    self.apply(&ty)
                }
                None if let Some(ty) = self.alias_value(expr.res, env) => ty,
                // A member the resolver found that has no
                // scheme: a type alias or a trait used as a
                // value.
                None => {
                    let module = match &obj.kind {
                        ExprKind::Ident(m) => format!("{m}."),
                        _ => String::new(),
                    };
                    let message = format!("'{module}{field}' is not a value");
                    self.error(Code::UndefinedVariable, message, span);
                    Type::Error
                }
            };
            expr.ty = Some(ty.clone());
            return ControlFlow::Break(ty);
        }
        // `Trait.method`: a method of a trait, called through it
        // (`Greet.greet(x)`, `m.Greet.greet`, `Compare.compare`).
        if let Some(tr) = self.trait_named(obj) {
            let receiver = self.trait_receiver.take();
            let ty = self.trait_method(tr, field, receiver, span);
            expr.ty = Some(ty.clone());
            return ControlFlow::Break(ty);
        }
        // `Type.method`: a method that makes a value, called
        // through its type (`Int.empty`, `m.Shape.default`).
        let type_ref = match (&obj.kind, obj.res) {
            (ExprKind::Ident(name), None) => self.named_type(None, *name),
            (ExprKind::Ident(_) | ExprKind::FieldAccess(..), res) => self.res_type(res),
            _ => None,
        };
        if let Some(ty) = type_ref
            && let Some(found) = self.field_of_type(obj, &mut expr.ty, ty, field, env, span)
        {
            return ControlFlow::Break(found);
        }

        // `a.empty()`: a type variable of the signature (or the
        // impl's header) that no value is named, as a type: the
        // method of a bound of it that makes a value.
        if let ExprKind::Ident(name) = obj.kind
            && obj.res == Some(crate::defs::Res::Local)
            && env.lookup(name).is_none()
            && let Some(ty) = self.sig_type_var(name)
        {
            self.var_as_type = Some(name);
            let resolved = self.resolve_type_descriptor_method(&ty, field, span);
            self.var_as_type = None;
            // (`Self` in a trait's default method is a type held
            // where the code runs, as a `type a` is: the method is
            // found by it, `Self.decode(v)`.)
            if name == intern("Self") && self.default_body_of.is_some() {
                obj.ty = Some(Type::type_of(ty));
            }
            let ty = resolved.unwrap_or(Type::Error);
            expr.ty = Some(ty.clone());
            return ControlFlow::Break(ty);
        }

        // A type named like its variant, given types before a dot,
        // is the type (`Cell(Int).default()`), as it is alone.
        // (So is a type of the module named like a variant of
        // another type.)
        if let ExprKind::Call(callee, args) = &mut obj.kind
            && args.iter().all(|arg| self.writes_types(arg))
        {
            self.type_named_like_its_variant(callee);
            if let ExprKind::Ident(name) = callee.kind
                && let Some(crate::defs::Res::Def(id)) = callee.res
                && (self.defs.as_ref()).is_some_and(|defs| defs.variant_type(id).is_some())
                && let Some(ty) = self.own_types.get(&name)
            {
                callee.res = Some(crate::defs::Res::Def(ty.id.0));
            }
        }

        // Could be record.field — infer the object type
        let around = std::mem::replace(
            &mut self.type_receiver,
            matches!(obj.kind, ExprKind::Call(..)),
        );
        // A tuple of types before a dot is the tuple type
        // (`(Int, Int).empty()`), as `Box(Int)` is that type.
        let spelled = match obj.kind {
            ExprKind::Tuple(_) => self.spelled_type(obj, env),
            _ => None,
        };
        // `Tuple(Int, Int).empty()` is no spelling of the tuple
        // type: the help of its `Tuple` writes the one that is.
        // (The help is the outer `Tuple`'s, by its place: one in
        // its arguments is a part of the type it writes. None when
        // a part is no type.)
        self.tuple_spelling = match &obj.kind {
            ExprKind::Call(callee, _) if super::names::is_tuple_name(callee) => {
                super::names::shown_type(obj).map(|tuple| (callee.span, tuple))
            }
            _ => None,
        };
        let obj_ty = match spelled {
            Some(ty) => Type::type_of(ty),
            None => self.infer_expr(obj, env),
        };
        self.tuple_spelling = None;
        self.type_receiver = around;
        self.receiver_taken = false;
        let obj_ty = self.apply(&obj_ty);
        // Canonicalise before dispatch so a receiver whose type
        // is written through an alias lands in the arm of the
        // type it stands for.
        let obj_ty = crate::types::canonical::canonicalize(&self.tables.resolver, &obj_ty);

        // A dot after a value is a field of its record type, or,
        // on a value whose type is still unknown, a field read
        // when the type is known (`Goal::Select`).
        //
        // `TypeOf(inner)` values (descriptors produced by `type a`
        // parameters or bare type names like `Int`) call the
        // methods of `inner` that make a value of it.
        if let Type::Generic(gname, gargs) = &obj_ty
            && gname.is_builtin(crate::defs::TYPE_OF)
            && gargs.len() == 1
        {
            return ControlFlow::Break(self.method_of_type_value(
                obj,
                &mut expr.ty,
                &obj_ty,
                gargs,
                field,
                span,
                called,
            ));
        }
        self.field_of_value(obj, &mut expr.ty, obj_ty, field, span, called)
    }

    /// `infer_field_access` for an access through a type (`Int.empty`, `Pt.make`,
    /// `m.Shape.default`): `Some` of the type the access ends with, `None` when the
    /// type has no such method and its name is a value (the access goes on).
    fn field_of_type(
        &mut self,
        obj: &mut Expr,
        expr_ty: &mut Option<Type>,
        ty: TypeRef,
        field: Symbol,
        env: &mut TypeEnv,
        span: Span,
    ) -> Option<Type> {
        let key = (canonical_head(&self.tables.resolver, ty), field);
        // A trait with an impl for the type at each of several
        // trait arguments (`From`): the method as the trait
        // declares it; which impl the use reaches its trait
        // arguments say.
        if let Some(tr) = self.trait_of_impls_by_args(key.0, field)
            && self.self_param(tr, field).is_none()
            && self.private_owner(tr).is_none()
        {
            if self.bare_type_receiver(obj, ty, Some(tr)) {
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            let instance =
                (self.type_instance(ty)).unwrap_or_else(|| Type::Generic(key.0, Vec::new()));
            // (The receiver is the value of that type, an
            // alias's in full: `BI.decode` of `type BI =
            // Box(Int)` is `Box(Int)`'s.)
            let instance = crate::types::canonical::canonicalize(&self.tables.resolver, &instance);
            obj.ty = Some(Type::type_of(instance.clone()));
            self.method_trait = Some(tr);
            let ty = self.through_trait(tr, field, instance, None, span);
            *expr_ty = Some(ty.clone());
            return Some(ty);
        }
        if let Some(entry) = self.method_entry(key.0, key.1) {
            if let Some(trait_name) = entry.trait_name
                && self.private_owner(trait_name).is_some()
            {
                self.private_method(trait_name, field, span);
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            // A method that takes a value of the type is
            // called through its trait.
            if let Some(tr) = self.entry_trait(&entry, field)
                && self.self_param(tr, field).is_some()
            {
                // (The help names the traits whose method is
                // called: a program's, not `Display`'s.)
                let mut traits = self.method_providers(key.0, field);
                if traits.is_empty() {
                    traits.push(tr);
                }
                let on = self.show_type(&Type::Generic(key.0, Vec::new()));
                self.through_its_trait(&on, &traits, field, span);
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            if self.ambiguous_method_call(key.0, field, span) {
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            if self.of_every_shape(key.0, &entry, field, span) {
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            if self.bare_type_receiver(obj, ty, self.entry_trait(&entry, field)) {
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            // Written through an alias, the instance is the
            // alias's (`Ints` for `type Ints = List(Int)` is
            // a `List(Int)`), with its arguments.
            let instance = self.type_instance(ty).map(|instance| {
                crate::types::canonical::canonicalize(&self.tables.resolver, &instance)
            });
            // (The receiver is the value of that type.)
            obj.ty = instance.clone().map(Type::type_of);
            if entry.structural {
                let ty = match instance {
                    Some(inner) => self.structural_type_method(key.0, &inner, field, span),
                    // A type of every shape (`Tuple`): its
                    // values have the method, not the type.
                    None => {
                        self.of_every_shape_error(key.0, field, span);
                        Type::Error
                    }
                };
                *expr_ty = Some(ty.clone());
                return Some(ty);
            }
            self.method_trait = self.entry_trait(&entry, field);
            // (The type is given for `Self`, as to a bound of
            // the trait.)
            if let Some(tr) = self.method_trait
                && let Some(refusal) =
                    self.opaque_through_trait(tr, &Type::Generic(key.0, Vec::new()), span)
            {
                self.refuse_opaque(span, refusal);
                *expr_ty = Some(Type::Error);
                return Some(Type::Error);
            }
            let scheme = self.method_scheme(&entry);
            self.instance_header = self.entry_impl(key.0, &entry, field);
            let ty = self.instantiate_method(&scheme, field, span);
            if let (Some(tr), Some(named)) = (self.method_trait, &instance) {
                // (An alias of a type with its arguments.)
                let written = (self.res_def(obj.res))
                    .is_some_and(|def| def.kind == crate::defs::DefKind::TypeAlias)
                    && free_vars_in(named).is_empty();
                self.self_is(tr, field, (&ty, named, written), span);
                let callee = format!("{}.{field}", key.0.name);
                self.decoded_through_type(tr, named, callee, span);
            }
            let full = self.full_self(key.0, &entry, field, &ty);
            self.type_method_site(key.0, &entry, field, full, span);
            let ty = self.apply(&ty);
            *expr_ty = Some(ty.clone());
            return Some(ty);
        }
        // A type with no such method, whose name is no value
        // either (`Option.compare`, `time.Weekday.nope`): the
        // method is what is missing.
        if matches!(obj.kind, ExprKind::FieldAccess(..)) || self.def_scheme(obj.res, env).is_none()
        {
            // (An alias is named as written: `L` of `type L(a)
            // = List(a)`.)
            match self.res_def(obj.res) {
                Some(def) if def.kind == crate::defs::DefKind::TypeAlias => {
                    let written = super::names::shown_arg(obj);
                    self.no_type_method(&written, field, span, None);
                }
                _ => self.no_type_method(&resolve(ty.name), field, span, Some(key.0)),
            }
            *expr_ty = Some(Type::Error);
            return Some(Type::Error);
        }
        None
    }

    /// `infer_field_access` for an access on a type held as a value
    /// (`TypeOf(inner)`: a `type a` parameter, `Int`, `Box(Int)`): the method of
    /// `inner` that makes a value of it. It ends the access.
    #[allow(clippy::too_many_arguments)]
    fn method_of_type_value(
        &mut self,
        obj: &mut Expr,
        expr_ty: &mut Option<Type>,
        obj_ty: &Type,
        gargs: &[Type],
        field: Symbol,
        span: Span,
        called: bool,
    ) -> Type {
        // A type not known yet (a parameter an earlier
        // `t.zero()`, or a call that takes a type, made a type):
        // its call of a method that makes a value of the one
        // trait in sight with the name owes that trait too.
        let inner = self.apply(&gargs[0]);
        let (makers, taking) = match (called, &inner) {
            (true, Type::Var(_)) => self.value_makers(field),
            _ => (Vec::new(), Vec::new()),
        };
        if let ([tr], []) = (makers.as_slice(), taking.as_slice()) {
            let tr = *tr;
            self.note_reading(obj, obj_ty, |receiver| Reading::Type {
                receiver,
                tr,
                method: field,
            });
            self.method_trait = Some(tr);
            self.note_type_holder(obj, &inner);
            let ty = self.through_trait(tr, field, inner, None, span);
            *expr_ty = Some(ty.clone());
            return ty;
        }
        // (Of a type not known yet: what the call names, by the
        // receiver as written, for none of the method's traits
        // is decided by it.)
        if let (true, Type::Var(_)) = (called, &inner) {
            let receiver = written_receiver(obj).unwrap_or_else(|| "_".to_string());
            match (makers.len(), taking.is_empty()) {
                (0, true) => self.no_type_method(&receiver, field, span, None),
                (0, false) => self.through_its_trait(&receiver, &taking, field, span),
                _ => self.ambiguous_method(field, &format!("`{receiver}`"), &makers, span),
            }
            return Type::Error;
        }
        // (A field of a type not known yet, which has none:
        // named by the parameter that holds it, or as written.)
        if let (false, Type::Var(v)) = (called, &inner) {
            let name = (self.type_holder(*v))
                .or_else(|| written_receiver(obj).map(|shown| intern(&shown)))
                .unwrap_or_else(|| intern("_"));
            let message = format!(
                "type mismatch: expected a value with a field `{field}`, got the type \
             `{name}`"
            );
            self.error(Code::TypeMismatch, message, span);
            return Type::Error;
        }
        let resolved = self.resolve_type_descriptor_method(&gargs[0], field, span);
        if let Some(ty) = resolved {
            *expr_ty = Some(ty.clone());
            return ty;
        }
        // A concrete type with no such method (a type
        // variable's case is reported above).
        let inner = self.apply(&gargs[0]);
        if !matches!(inner, Type::Var(_) | Type::Rigid(_) | Type::Error) {
            // (A generic type or alias written alone, `id(L)`:
            // named as written, not with its open arguments.)
            let alone = (free_vars_in(&inner).iter())
                .find_map(|v| self.open_var_names.get(v))
                .map(|name| resolve(*name))
                .or_else(|| match &inner {
                    _ if let Some(head) = inner.container_head()
                        && self.holds_open_vars(&inner) =>
                    {
                        head.container_name().map(str::to_string)
                    }
                    Type::Generic(name, _) if self.holds_open_vars(&inner) => {
                        Some(self.show_type(&Type::Generic(*name, Vec::new())))
                    }
                    _ => None,
                });
            let shown = alone.unwrap_or_else(|| self.show_type(&inner));
            self.no_type_method(&shown, field, span, None);
        }
        Type::Error
    }

    /// `infer_field_access` for an access on a value: a field of its record, a
    /// field read when its type is known, a method value, or what is wrong.
    fn field_of_value(
        &mut self,
        obj: &mut Expr,
        expr_ty: &mut Option<Type>,
        obj_ty: Type,
        field: Symbol,
        span: Span,
        called: bool,
    ) -> ControlFlow<Type, Type> {
        ControlFlow::Continue(match &obj_ty {
            Type::Generic(type_name, type_args)
                if let Some(module) = self.opaque_outside(*type_name)
                    && (self.tables.records.get(type_name))
                        .is_some_and(|rec| rec.fields.iter().any(|(n, _)| *n == field)) =>
            {
                let _ = type_args;
                let message = format!(
                    "cannot read field '{field}' of a '{}' outside module '{module}': \
                 the type is opaque",
                    super::opaque::opaque_name(*type_name, module)
                );
                self.opaque_error(message, module, span);
                Type::Error
            }
            Type::Generic(type_name, type_args) => {
                // A field of a declared record, at the record's type
                // arguments.
                if self.tables.records.contains_key(type_name)
                    && let Some((_, ft)) = self
                        .instantiate_record_fields_with_args(*type_name, type_args)
                        .into_iter()
                        .find(|(n, _)| *n == field)
                {
                    let resolved = self.apply(&ft);
                    *expr_ty = Some(resolved.clone());
                    return ControlFlow::Break(resolved);
                }
                let fields = (self.tables.records.get(type_name))
                    .map(|rec| rec.fields.clone())
                    .unwrap_or_default();
                self.no_field(&obj_ty, field, Some(obj), &fields, span);
                Type::Error
            }
            // `t.0` parses as a FieldAccess with an
            // all-numeric field name, but tuple indexing is
            // intentionally unsupported: the
            // targeted diagnostic points at destructuring instead.
            Type::Tuple(_)
                if !resolve(field).is_empty()
                    && resolve(field).bytes().all(|b| b.is_ascii_digit()) =>
            {
                self.error_help(
                    Code::UnsupportedOperation,
                    (
                        format!(
                            "tuple indexing ('t.{field}') is not supported; \
                         destructure instead: 'let (a, b) = t'"
                        ),
                        Some(crate::parser::TUPLE_INDEX_HELP.to_string()),
                    ),
                    span,
                );
                Type::Error
            }
            // A value of the type a parameter holds (`t.zero().show2()`):
            // its dot names a field, which it has none of, as for a
            // `type t` parameter.
            Type::Var(r) if self.type_holder(*r).is_some() => {
                let name = self.type_holder(*r).unwrap_or_else(|| intern("_"));
                let held = Type::Rigid(RigidId { var: *r, name });
                self.no_field(&held, field, Some(obj), &[], span);
                Type::Error
            }
            Type::Var(_) => {
                let (makers, taking) = match called {
                    true => self.value_makers(field),
                    false => (Vec::new(), Vec::new()),
                };
                if let ([tr], []) = (makers.as_slice(), taking.as_slice())
                    && !self.read_as_record(&obj_ty)
                {
                    // `t.mk(s)` where one trait in sight has a
                    // method `mk`, which makes a value: `t` is a
                    // type, which owes the trait, as `json.parse(s,
                    // t)` makes `t` a type that owes `json.Decode`.
                    let tr = *tr;
                    self.note_reading(obj, &obj_ty, |receiver| Reading::Type {
                        receiver,
                        tr,
                        method: field,
                    });
                    let inner = self.fresh_var();
                    self.note_type_holder(obj, &inner);
                    self.unify(&obj_ty, &Type::type_of(inner.clone()), span);
                    self.method_trait = Some(tr);
                    self.through_trait(tr, field, inner, None, span)
                } else if called {
                    // (Of several traits' methods that make a value
                    // the call does not say which, and one of a trait
                    // of type constructors is not taken for one: a
                    // field.)
                    let makers = match (makers.len(), taking.is_empty()) {
                        (0, true) => self.constructor_makers(field),
                        _ => makers,
                    };
                    let read_as_field = makers.len() > 1
                        || (!makers.is_empty() && self.of_constructors_trait(makers[0]));
                    if read_as_field {
                        self.note_reading(obj, &obj_ty, |receiver| Reading::Field {
                            receiver,
                            traits: makers.clone(),
                            method: field,
                        });
                    }
                    // `x.f(..)`: what it calls is decided with
                    // the type of `x` (`check_call`).
                    let traits = if read_as_field { makers } else { Vec::new() };
                    let at = self.read_at(obj, &obj_ty, traits);
                    self.unknown_receiver = Some((obj_ty.clone(), field, at));
                    self.fresh_var()
                } else {
                    // `x.f`: the field is read when the type of
                    // `x` is known.
                    let result = self.fresh_var();
                    self.want_goal(
                        Goal::Select {
                            recv: obj_ty.clone(),
                            name: field,
                            args: None,
                            result: result.clone(),
                            at: self.read_at(obj, &obj_ty, Vec::new()),
                        },
                        Origin {
                            span,
                            callee: None,
                            op: None,
                            header: None,
                        },
                    );
                    result
                }
            }
            // Prior error — propagate to prevent cascading false positives
            Type::Error => Type::Error,
            // A value that never is (`panic("a").x`): its field
            // never is either, and meets every type, as the
            // value does (`v.x.y`, `let w = v.x` then `w.y`).
            Type::Never => Type::Never,
            // Any other type has no fields.
            _ => {
                self.no_field(&obj_ty, field, Some(obj), &[], span);
                Type::Error
            }
        })
    }
}
