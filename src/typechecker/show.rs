use super::*;

impl TypeChecker {
    /// The signature of the function `f`, of the scheme `scheme`, as an
    /// editor's hover writes one over type constructors (HK8): `fn
    /// twice(xs: m(Int)) -> m(Int) where m: Functor`, a type constructor
    /// variable by the name its annotation gives it (else `m`, `n`, ...),
    /// a type variable by a letter; and the function's type with its
    /// variables so named. `None` for a function whose type holds no type
    /// constructor variable.
    pub(super) fn written_signature(&self, f: &FnDecl, scheme: &Scheme) -> Option<Signature> {
        let ty = self.apply(&scheme.ty);
        let Type::Fun(params, ret) = &ty else {
            return None;
        };
        let heads: Vec<TyVar> = (crate::types::constructor_heads(&ty).into_iter())
            .filter(|v| scheme.vars.contains(v))
            .collect();
        if heads.is_empty() || params.len() != f.params.len() {
            return None;
        }
        let mut taken: Vec<String> = Vec::new();
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        let mut constructors = ["m", "n", "o", "p", "q", "r"].into_iter().map(String::from);
        let mut letters = ('a'..='z').map(|c| c.to_string());
        for v in free_vars_in(&ty) {
            if !scheme.vars.contains(&v) {
                continue;
            }
            let written = (self.tables.constructor_names.get(&v))
                .map(|name| resolve(*name))
                .filter(|name| heads.contains(&v) && !taken.contains(name));
            let name = match written {
                Some(name) => Some(name),
                None if heads.contains(&v) => constructors.find(|n| !taken.contains(n)),
                None => letters.find(|n| !taken.contains(n)),
            }
            .unwrap_or_else(|| "_".to_string());
            taken.push(name.clone());
            let id = RigidId {
                var: v,
                name: intern(&name),
            };
            mapping.insert(v, Type::Rigid(id));
        }
        let named = |t: &Type| self.show_type(&substitute_vars(t, &mapping));
        let written_params: Vec<String> = (f.params.iter().zip(params))
            .map(|(param, ty)| {
                let name = match &param.pattern.kind {
                    PatternKind::Ident(n) => resolve(*n),
                    _ => "_".to_string(),
                };
                match param.kind {
                    ParamKind::Type => format!("type {name}"),
                    ParamKind::Data => format!("{name}: {}", named(ty)),
                }
            })
            .collect();
        let mut text = format!("fn {}({})", f.name, written_params.join(", "));
        if !matches!(**ret, Type::Unit) {
            text.push_str(&format!(" -> {}", named(ret)));
        }
        // Each variable's bounds, by the variable's name, as `where`
        // writes them.
        let mut bounds: Vec<(String, Vec<String>)> = Vec::new();
        for pred in &scheme.preds {
            let Pred::Trait {
                tr, args, subject, ..
            } = pred
            else {
                continue;
            };
            let Type::Rigid(subject) = substitute_vars(&self.apply(subject), &mapping) else {
                continue;
            };
            let args: Vec<Type> = (args.iter())
                .map(|arg| substitute_vars(&self.apply(arg), &mapping))
                .collect();
            let bound = self.show_bound(*tr, &args);
            let subject = resolve(subject.name);
            match bounds.iter_mut().find(|(s, _)| *s == subject) {
                Some((_, traits)) if traits.contains(&bound) => {}
                Some((_, traits)) => traits.push(bound),
                None => bounds.push((subject, vec![bound])),
            }
        }
        if !bounds.is_empty() {
            let bounds: Vec<String> = (bounds.iter())
                .map(|(subject, traits)| format!("{subject}: {}", traits.join(" + ")))
                .collect();
            text.push_str(&format!(" where {}", bounds.join(", ")));
        }
        Some(Signature {
            text,
            ty: substitute_vars(&ty, &mapping),
        })
    }

    // ── Error reporting ─────────────────────────────────────────────

    /// The two types of a message, rendered: two types of one name are
    /// told apart by their modules (`a.Pt` and `b.Pt`); the module's own
    /// type keeps its bare name.
    pub(super) fn show_apart(&self, a: &Type, b: &Type) -> (String, String) {
        let mut shown = self.show_types(&[a, b], false).into_iter();
        let a = shown.next().unwrap_or_default();
        (a, shown.next().unwrap_or_default())
    }

    /// The type of a message that names one type: a named type is
    /// qualified by its module when another type the session knows has
    /// its name (`other.Shape`, with `shapes.Shape` imported too).
    pub(super) fn show_type(&self, ty: &Type) -> String {
        self.show_types(&[ty], true).pop().unwrap_or_default()
    }

    /// The types of one message (see `show_apart`, `show_type`): a type
    /// of another module is written as a program writes it, with its
    /// module (`json.Value`, `time.Date`, `shapes.Shape`); a prelude
    /// type and the module's own keep their bare names, unless another
    /// type of the message, or with `session` another type the session
    /// knows, has the name (`prelude.Option`, `a.Pt` and `b.Pt`). In a
    /// REPL cell a type of an earlier cell declared again since, which
    /// the cell cannot name, is written with its cell (`<repl:1>.H`).
    fn show_types(&self, types: &[&Type], session: bool) -> Vec<String> {
        // (A type constructor variable not decided yet is written by its
        // name, `m(Int)`.)
        let named: Vec<Type> = types.iter().map(|ty| self.name_constructors(ty)).collect();
        let types: Vec<&Type> = named.iter().collect();
        let types = types.as_slice();
        Type::show_all(types, |r, clash| {
            (clash
                || (session && self.type_name_clashes(r))
                || self.other_module(r).is_some()
                || self.shadowed_cell_type(r))
            .then(|| self.qualified_type(r))
        })
    }

    /// `ty` with each type constructor variable that is still a
    /// unification variable written by the name of the annotation's
    /// variable it is a use of, for a message.
    fn name_constructors(&self, ty: &Type) -> Type {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        collect_app_heads(ty, &mut |v| {
            if let Some(name) = self.tables.constructor_names.get(&v) {
                mapping.insert(
                    v,
                    Type::Rigid(RigidId {
                        var: v,
                        name: *name,
                    }),
                );
            }
        });
        match mapping.is_empty() {
            true => ty.clone(),
            false => substitute_vars(ty, &mapping),
        }
    }

    /// Whether `r` is, in a REPL cell, a type of an earlier cell that the
    /// cell does not name by its name: a later cell declared it again.
    fn shadowed_cell_type(&self, r: TypeRef) -> bool {
        self.is_cell
            && !self.named_types.contains(&r.id.0)
            && self
                .def(r.id.0)
                .is_some_and(|def| self.cells.contains(&def.module))
    }

    /// The module of `r` if it is another module than the one being
    /// checked, as a program names it: a builtin module (`json`,
    /// `time`) or a module of the program's. `None` for a prelude type,
    /// for one of the module's own, and in a REPL cell for one of an
    /// earlier cell, which the cell writes by its name.
    pub(super) fn other_module(&self, r: TypeRef) -> Option<Symbol> {
        match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, module)) => module.map(intern),
            None => self
                .def(r.id.0)
                .filter(|def| def.module != self.module)
                .and_then(|def| self.tables.module_names.get(&def.module).copied())
                .filter(|module| !resolve(*module).starts_with('<')),
        }
    }

    /// The variant `name` of the enum `enum_ty` as a program writes it:
    /// with its module where the enum is another module's (`json.Bool`,
    /// `shapes.Circle`), bare otherwise (`Some`, a variant of the
    /// module's own).
    pub(super) fn written_variant(&self, enum_ty: TypeRef, name: Symbol) -> String {
        match self.other_module(enum_ty) {
            Some(module) => format!("{module}.{name}"),
            None => name.to_string(),
        }
    }

    /// Whether another type the session knows has the name of `r`.
    fn type_name_clashes(&self, r: TypeRef) -> bool {
        let other = |o: &TypeRef| o.name == r.name && o.id != r.id;
        self.tables.enums.keys().any(other)
            || self.tables.records.keys().any(other)
            || crate::defs::builtin_type_id(&resolve(r.name)).is_some_and(|id| id != r.id)
    }

    /// The name of `r` written with its module: `a.Pt`, `time.Weekday`,
    /// `prelude.Option` for a prelude type; a module's own type keeps its
    /// bare name, and so in a REPL cell does a type of an earlier cell
    /// that the cell names by its name (`<repl:1>.H` is the `H` an
    /// earlier cell declared again).
    fn qualified_type(&self, r: TypeRef) -> String {
        let module = match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, module)) => Some(intern(module.unwrap_or("prelude"))),
            None => self
                .def(r.id.0)
                .filter(|def| def.module != self.module)
                .filter(|def| {
                    !(self.is_cell
                        && self.cells.contains(&def.module)
                        && self.named_types.contains(&r.id.0))
                })
                .and_then(|def| self.tables.module_names.get(&def.module).copied()),
        };
        match module {
            Some(module) => format!("{module}.{}", r.name),
            None => r.name.to_string(),
        }
    }

    /// The name of `r` as the module being checked writes it, for a
    /// hint that spells code: a type of another module with the module
    /// (`time.Date`); in a REPL cell the types of the earlier cells are
    /// in scope by name.
    pub(super) fn written_type(&self, r: TypeRef) -> String {
        let qualified = self.qualified_type(r);
        // (An earlier cell's "module" has no name a program writes.)
        match self.is_cell && qualified.starts_with('<') {
            true => r.name.to_string(),
            false => qualified,
        }
    }

    /// The name of the trait `t` in a message, as a program writes it:
    /// a trait of another module with its module (`json.Encode`,
    /// `a.Show`); a prelude trait and the module's own bare (`Display`),
    /// unless another trait the session knows has its name
    /// (`prelude.Display` beside a program's `Display`), or in a REPL
    /// cell, of an earlier cell's, unless the cell names it by its name
    /// (`<repl:1>.Greet` is the `Greet` a later cell declared again).
    pub(super) fn show_trait(&self, t: TraitKey) -> String {
        let of_another_module = match crate::defs::builtin_trait_of_id(t.id.0) {
            Some((_, module)) => module.is_some(),
            None => self
                .def(t.id.0)
                .filter(|def| def.module != self.module)
                .and_then(|def| self.tables.module_names.get(&def.module))
                .is_some_and(|module| !resolve(*module).starts_with('<')),
        };
        let clashes = self
            .tables
            .traits
            .keys()
            .any(|o| o.name == t.name && o.id != t.id);
        let named_by_cell = self.is_cell
            && self.named_types.contains(&t.id.0)
            && (self.def(t.id.0)).is_some_and(|def| self.cells.contains(&def.module));
        match of_another_module || (clashes && !named_by_cell) {
            true => self.show_trait_in_module(t),
            false => t.name.to_string(),
        }
    }

    /// The name of the trait `t` as the module being checked writes it:
    /// with its module where another module declares it (`m.Show`,
    /// `json.Encode`), a prelude trait by its name (`Display`); in a
    /// REPL cell the traits of the earlier cells are in scope by name.
    pub(super) fn written_trait(&self, t: TraitKey) -> String {
        if let Some((name, None)) = crate::defs::builtin_trait_of_id(t.id.0) {
            return name.to_string();
        }
        let qualified = self.show_trait_in_module(t);
        // (An earlier cell's "module" has no name a program writes.)
        match self.is_cell && qualified.starts_with('<') {
            true => t.name.to_string(),
            false => qualified,
        }
    }

    /// The trait with the module that declares it, where that is not
    /// the module being checked: `a.Show`, `prelude.Display`.
    pub(super) fn show_trait_in_module(&self, t: TraitKey) -> String {
        let module = if let Some((_, module)) = crate::defs::builtin_trait_of_id(t.id.0) {
            Some(intern(module.unwrap_or("prelude")))
        } else {
            self.def(t.id.0)
                .filter(|def| def.module != self.module)
                .and_then(|def| self.tables.module_names.get(&def.module).copied())
        };
        match module {
            Some(module) => format!("{module}.{}", t.name),
            None => t.name.to_string(),
        }
    }

    /// The quick fix for a value where a `Result` is expected: wrap the
    /// expression in `Ok(...)`.
    pub(super) fn add_ok_wrap_fix(d: &mut Diagnostic, got: &Type, expected: &Type) {
        let is_result = |t: &Type| matches!(t, Type::Generic(n, _) if n.is_builtin("Result"));
        if is_result(expected) && !is_result(got) && d.span.is_in_source() {
            let (start, end) = (
                Span::point(d.span.file, d.span.start),
                Span::point(d.span.file, d.span.end),
            );
            d.fixes.push(crate::diagnostic::Fix {
                title: "Wrap expression in `Ok(...)`".to_string(),
                edits: vec![(start, "Ok(".to_string()), (end, ")".to_string())],
            });
        }
    }

    /// If `got` is a `Result(_, _)` or `Option(_)` but `expected` is
    /// not, the help line that explains how to thread the value
    /// through: the raw "expected String, got Result(String, _)" is
    /// correct but doesn't say how to fix it; the help points at `?` and
    /// `result.flat_map` / `option.flat_map`.
    pub(super) fn chain_hint(got: &Type, expected: &Type) -> Option<std::string::String> {
        let is_wrapper = |t: &Type, name: &str| -> bool { t.is_builtin(name) };
        // (A type constructor, `Option(_)`, holds no value to chain.)
        if matches!(got, Type::Con(..)) {
            return None;
        }
        if is_wrapper(expected, "Result") || is_wrapper(expected, "Option") {
            return None;
        }
        if is_wrapper(got, "Result") {
            return Some(
                "to chain through a `Result`, use `?` to propagate the \
                 error, or `|> result.flat_map { x -> ... }` to continue the \
                 pipeline on the Ok value"
                    .to_string(),
            );
        }
        if is_wrapper(got, "Option") {
            return Some(
                "to chain through an `Option`, use `?` to propagate \
                 `None`, or `|> option.flat_map { x -> ... }` to continue the \
                 pipeline on the Some value"
                    .to_string(),
            );
        }
        None
    }

    pub(super) fn error(
        &mut self,
        code: Code,
        message: impl Into<std::string::String>,
        span: Span,
    ) {
        self.errors.push(Diagnostic::error(code, span, message));
    }

    /// An error with a message and, when there is one, a help line: what
    /// the `*_message` helpers that suggest a close name return.
    pub(super) fn error_help(
        &mut self,
        code: Code,
        (message, help): (std::string::String, Option<std::string::String>),
        span: Span,
    ) {
        let mut d = Diagnostic::error(code, span, message);
        d.help.extend(help);
        self.errors.push(d);
    }

    /// The known type that `name`, a name in a type annotation, spells
    /// in the wrong case: `int` → `Int`, `INT` → `Int`, `option` →
    /// `Option`. Type names are case-sensitive and declarations must be
    /// capitalised, so such a name can only be a typo for that type.
    /// Unless `name` is applied to arguments (`q(Int)`), one-letter
    /// names are left alone: `a`, `e`, `t` are the usual type variables,
    /// whatever types a program declares.
    pub(super) fn case_mismatched_type_name(&self, name: &str, applied: bool) -> Option<String> {
        if !applied && name.chars().count() < 2 {
            return None;
        }
        let matches = |candidate: &str| candidate != name && candidate.eq_ignore_ascii_case(name);
        crate::types::builtins::BUILTIN_TYPES
            .iter()
            .map(|t| t.name.to_string())
            .chain(self.tables.records.keys().map(|t| resolve(t.name)))
            .chain(self.tables.enums.keys().map(|t| resolve(t.name)))
            .chain(self.tables.type_aliases.iter().map(|t| resolve(t.name)))
            .filter(|candidate| matches(candidate))
            .min()
    }

    /// The "unknown type" error for `name`, with a hint when it is a
    /// known type in the wrong case (see `case_mismatched_type_name`).
    pub(super) fn unknown_type_message(&self, name: &str, applied: bool) -> String {
        match self.case_mismatched_type_name(name, applied) {
            Some(type_name) => format!(
                "unknown type '{name}' — did you mean `{type_name}`? (type names are \
                 case-sensitive and start with a capital letter; a lowercase name in a \
                 type is a type variable)"
            ),
            None => format!("unknown type '{name}'"),
        }
    }
}

/// Call `f` with each unification variable of `ty` (those of type
/// constructor variables have names).
fn collect_app_heads(ty: &Type, f: &mut impl FnMut(TyVar)) {
    match ty {
        Type::Var(v) => f(*v),
        Type::App(head, arg) => {
            collect_app_heads(head, f);
            collect_app_heads(arg, f);
        }
        Type::Fun(params, ret) => {
            params.iter().for_each(|t| collect_app_heads(t, f));
            collect_app_heads(ret, f);
        }
        Type::List(t) | Type::Set(t) | Type::Channel(t) => collect_app_heads(t, f),
        Type::Map(k, v) => {
            collect_app_heads(k, f);
            collect_app_heads(v, f);
        }
        Type::Tuple(ts) | Type::Generic(_, ts) | Type::Con(_, ts) => {
            ts.iter().for_each(|t| collect_app_heads(t, f))
        }
        Type::AnonRecord { fields, .. } => fields.values().for_each(|t| collect_app_heads(t, f)),
        Type::AssocProj { receiver, .. } => collect_app_heads(receiver, f),
        _ => {}
    }
}
