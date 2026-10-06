use super::*;

impl TypeChecker {
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

    /// The types of one message (see `show_apart`, `show_type`): a named
    /// type that another type of the message has the name of, or, with
    /// `session`, another type the session knows, is written with its
    /// module; a module's own type keeps its bare name unless the other
    /// is its own too.
    fn show_types(&self, types: &[&Type], session: bool) -> Vec<String> {
        Type::show_all(types, |r, clash| {
            (clash || (session && self.type_name_clashes(r))).then(|| self.qualified_type(r))
        })
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
    /// bare name.
    fn qualified_type(&self, r: TypeRef) -> String {
        let module = match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, module)) => Some(intern(module.unwrap_or("prelude"))),
            None => self
                .def(r.id.0)
                .filter(|def| def.module != self.module)
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

    /// The name of the trait `t` in a message: written with its module
    /// when another trait the session knows has its name (`a.Show`).
    pub(super) fn show_trait(&self, t: TraitKey) -> String {
        let clashes = self
            .tables
            .traits
            .keys()
            .any(|o| o.name == t.name && o.id != t.id);
        if !clashes {
            return t.name.to_string();
        }
        self.show_trait_in_module(t)
    }

    /// The trait with the module that declares it, where that is not
    /// the module being checked: `a.Show`, `prelude.Display`.
    pub(super) fn show_trait_in_module(&self, t: TraitKey) -> String {
        let first = crate::defs::builtin_types().len();
        let module = if (t.id.0.0 as usize)
            .checked_sub(first)
            .is_some_and(|k| k < crate::defs::BUILTIN_TRAITS.len())
        {
            Some(intern("prelude"))
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

    pub(super) fn warning(
        &mut self,
        code: Code,
        message: impl Into<std::string::String>,
        span: Span,
    ) {
        self.errors.push(Diagnostic::warning(code, span, message));
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
