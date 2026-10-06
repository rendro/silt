//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! and check_body.

use super::infer::pattern::*;
use super::suggest::suggest_similar;
use super::*;

/// Where an expression stands relative to its enclosing `loop`, for the
/// check that `loop(...)` is in tail position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurPos {
    /// The loop's result: a `loop(...)` here restarts the loop.
    Tail,
    /// Somewhere whose value is used.
    NotTail,
    /// Inside a closure in the loop body.
    InClosure,
}

impl RecurPos {
    /// The position of a sub-expression whose value is used.
    fn inner(self) -> RecurPos {
        match self {
            RecurPos::InClosure => RecurPos::InClosure,
            RecurPos::Tail | RecurPos::NotTail => RecurPos::NotTail,
        }
    }
}

/// GAP (round 17 F5): pick the singular or plural form of a word
/// based on `n`. Used to render arity/field/binding counts in
/// typechecker diagnostics without the awkward "1 argument(s)" that
/// tooling and users had been complaining about.
pub(super) fn plural<'a>(n: usize, singular: &'a str, plural_form: &'a str) -> &'a str {
    if n == 1 { singular } else { plural_form }
}

/// How a call is written, for what it reports: `f(a)` and `a |> f(b)`
/// are calls with a written argument list; `a |> f` has none.
#[derive(Clone, Copy)]
enum CallForm {
    Call,
    BarePipe,
}

/// The arity rule of every call form — `f(a, b)`, `a |> f(b)` and
/// `a |> f`: a call supplies exactly as many arguments as the callee has
/// parameters, or one fewer when the callee's signature declares its last
/// parameter optional (`Scheme::optional_last_param`).
///
/// `supplied` counts every argument the callee receives, including a
/// piped value and the implicit `self` of a method call.
pub(super) fn call_arity_matches(
    params: usize,
    optional_last_param: bool,
    supplied: usize,
) -> bool {
    supplied == params || (optional_last_param && supplied + 1 == params)
}

/// The argument counts a callee accepts, worded for a diagnostic:
/// "1 argument", "2 arguments", or "2 or 3 arguments" for a callee whose
/// last parameter is optional.
pub(super) fn accepted_arity_text(params: usize, optional_last_param: bool) -> String {
    if optional_last_param && params > 0 {
        format!("{} or {} arguments", params - 1, params)
    } else {
        format!("{} {}", params, plural(params, "argument", "arguments"))
    }
}

/// BROKEN (round 26 B2): render a set of symbols for a user-facing
/// diagnostic. `BTreeSet<Symbol>` formatted with `{:?}` leaks the
/// interner's Debug form (`Symbol(6: "x")`) — awful to read and exposes
/// implementation detail. This helper resolves each symbol to its
/// source-level name and joins them inside `{}` braces in sorted order
/// (the BTreeSet iteration order is already lexicographic on symbol
/// id, so we sort by the resolved string to keep output stable across
/// interning permutations). Example output: `{x}`, `{a, b, c}`, `{}`.
pub(super) fn format_symbol_set(set: &BTreeSet<Symbol>) -> String {
    let mut names: Vec<String> = set.iter().map(|s| resolve(*s)).collect();
    names.sort();
    format!("{{{}}}", names.join(", "))
}

/// Format an "undefined variable '<typo>'" error message, with an
/// optional "did you mean `<cand>`?" help line. Sourced candidates come from every in-scope name
/// the typechecker's env chain exposes (locals, fn params, top-level
/// decls, stdlib builtins) — the caller hands us `env`. If no candidate
/// passes the suggest-similar threshold, we emit the plain error.
///
/// Closes round-17 deferred finding #4: see `src/typechecker/suggest.rs`.
/// Lock: tests/lang/diagnostic_suggestion_tests.rs.
pub(super) fn format_undefined_variable_message(
    name: Symbol,
    env: &TypeEnv,
    suffix: &str,
) -> (String, Option<String>) {
    let name_str = resolve(name);
    let base = if suffix.is_empty() {
        format!("undefined variable '{name_str}'")
    } else {
        format!("undefined variable '{name_str}' {suffix}")
    };
    // If the identifier is a keyword borrowed from another language
    // that silt has no equivalent for, the edit-distance suggestion
    // isn't useful — attach a targeted recommendation instead. These
    // hints used to live in the parser's G1 guard, but that fired on
    // any parenthesized reference and broke formatter roundtrip;
    // resolving them here keeps the UX while staying syntax-neutral.
    let foreign_keyword_hint = match name_str.as_str() {
        "break" | "continue" => {
            Some("silt has no 'break'/'continue' — return early or restructure the recursion")
        }
        // F12 (round 67): mirror parser.rs G1 (parser.rs:2245-2255) for
        // the expression-position case. The parser only catches these
        // at statement position (and gates on a "looks like a mistake"
        // lookahead); in expression position the parser parses `if` /
        // `while` / `for` as bare identifiers and we land here with an
        // "undefined variable" diagnostic. Wording is copied verbatim
        // from parser.rs so both paths give the user identical advice.
        "if" => Some("silt has no 'if' keyword — use 'match cond { true -> ..., false -> ... }'"),
        "while" | "for" => Some(
            "silt has no 'while'/'for' keywords — use tail-recursive 'loop' or 'list.each' / 'list.map'",
        ),
        _ => None,
    };
    if let Some(hint) = foreign_keyword_hint {
        return (base, Some(hint.to_string()));
    }
    let mut candidates = BTreeSet::new();
    env.collect_names(&mut candidates);
    // Strip fully-qualified builtin names like `list.map` — those are
    // not useful suggestions for a bare identifier typo. Also drop the
    // pseudo-binding for `self` which is handled by the Ident arm.
    let candidate_strs: Vec<String> = candidates
        .iter()
        .map(|s| resolve(*s))
        .filter(|s| !s.contains('.') && s != "self")
        .collect();
    let help = suggest_similar(&name_str, candidate_strs.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
}

/// GAP (round 26 L5): append a "did you mean `<cand>`?" hint when a
/// record-field diagnostic mentions a name that's close in edit
/// distance to one of the record's declared fields. Used by every
/// "record 'X' has no field 'Y'" / "unknown field 'Y' in X" site so
/// `u.nam` on `type User { name, age }` gets `did you mean \`name\`?`.
/// Delegates to `suggest::suggest_similar` for the threshold policy
/// (matches the round-24 short-name tightening — single-edit only for
/// names up to 5 chars; scaled for longer names).
pub(super) fn format_record_field_suggestion(
    base: String,
    field: Symbol,
    record_fields: &[(Symbol, Type)],
) -> (String, Option<String>) {
    let field_str = resolve(field);
    let candidates: Vec<String> = record_fields.iter().map(|(n, _)| resolve(*n)).collect();
    let help = suggest_similar(&field_str, candidates.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
}

/// GAP (round 23 #3): append a "did you mean `<cand>`?" hint to an
/// "unknown method '<field>' on <Type>" diagnostic when the method table
/// has a close edit-distance match for the given type name. The
/// method_table is keyed on `(type_name, method_name)`; we walk it once
/// to collect every method registered on the target type and feed them
/// to the existing `suggest::suggest_similar` policy.
pub(super) fn format_unknown_method_message(
    field: Symbol,
    display_type_name: &str,
    method_table: &HashMap<(TypeRef, Symbol), MethodEntry>,
    table_key: TypeRef,
) -> (String, Option<String>) {
    let field_str = resolve(field);
    let base = format!("unknown method '{field_str}' on {display_type_name}");
    let candidates: Vec<String> = method_table
        .keys()
        .filter(|(ty, _)| *ty == table_key)
        .map(|(_, m)| resolve(*m).to_string())
        .collect();
    let help = suggest_similar(&field_str, candidates.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
}

impl TypeChecker {
    /// Put `var: trait_name` (at the trait arguments `args`) in scope,
    /// and with it each supertrait, at the arguments the trait's
    /// declaration gives it: `where a: Ordered` with
    /// `trait Ordered: Equal` makes both traits' methods callable on `a`,
    /// and `v: Sub(Int)` with `trait Sub(a): Super(a)` says
    /// `v: Super(Int)`.
    pub(super) fn declare_bound(&mut self, var: TyVar, trait_name: TraitKey, args: Vec<Type>) {
        let in_scope = self.bounds.entry(var).or_default();
        if in_scope.iter().any(|(t, _)| *t == trait_name) {
            return;
        }
        in_scope.push((trait_name, args.clone()));
        let Some(info) = self.tables.traits.get(&trait_name).cloned() else {
            return;
        };
        for (i, super_name) in info.supertraits.iter().enumerate() {
            let super_args: Vec<Type> = match info.supertrait_args.get(i) {
                Some(exprs) if !exprs.is_empty() => {
                    // LATENT (round 88): a bare parametric type name
                    // (`Box` where `type Box(a) { ... }`) among the
                    // supertrait's arguments would silently produce a
                    // 0-arity type that matches no impl.
                    for te in exprs.iter() {
                        self.check_supertrait_arg_parametric_arity(te, &info);
                    }
                    exprs
                        .iter()
                        .map(|te| resolve_supertrait_arg(te, &info, &args))
                        .collect()
                }
                _ => Vec::new(),
            };
            self.declare_bound(var, *super_name, super_args);
        }
    }

    /// `recv.method` where `method` is the method of a builtin
    /// structural trait (`compare`, `equal`, `hash`, `display`) and the
    /// receiver's type has no entry for it in the method table: the
    /// type has the method when it has the trait by its structure,
    /// which the access owes.
    pub(super) fn structural_method(
        &mut self,
        recv: &Type,
        method: Symbol,
        span: Span,
    ) -> Option<Type> {
        let (tr, others, result) = match resolve(method).as_str() {
            "compare" => ("Compare", 1, Type::Int),
            "equal" => ("Equal", 1, Type::Bool),
            "hash" => ("Hash", 0, Type::Int),
            "display" => ("Display", 0, Type::String),
            _ => return None,
        };
        let tr = TraitKey::builtin(tr);
        let head = self.type_name_for_impl(&self.apply(recv))?;
        if !self.by_structure(tr, head) {
            return None;
        }
        self.want(
            Pred::Trait {
                tr,
                args: Vec::new(),
                subject: recv.clone(),
            },
            Origin {
                span,
                callee: Some(method),
                op: None,
            },
        );
        self.last_field_access_was_method = true;
        self.method_trait = Some(tr);
        let mut params = vec![recv.clone()];
        params.extend(std::iter::repeat_n(recv.clone(), others));
        Some(Type::Fun(params, Box::new(result)))
    }

    /// The map or set literal at `span` hashes values of the type
    /// `key`.
    fn want_hash(&mut self, key: &Type, span: Span) {
        self.want(
            Pred::Trait {
                tr: TraitKey::builtin("Hash"),
                args: Vec::new(),
                subject: key.clone(),
            },
            Origin {
                span,
                callee: None,
                op: None,
            },
        );
    }

    /// The operator `op` at `span` needs its operand's type, still
    /// unknown or an annotation variable, to implement the builtin trait
    /// `tr`: `+` needs `Number`, `==` `Equal`, `<` `Compare`.
    pub(super) fn want_operand(&mut self, tr: &str, operand: &Type, op: &'static str, span: Span) {
        if matches!(self.apply(operand), Type::Error | Type::Never) {
            return;
        }
        self.want(
            Pred::Trait {
                tr: TraitKey::builtin(tr),
                args: Vec::new(),
                subject: operand.clone(),
            },
            Origin {
                span,
                callee: None,
                op: Some(op),
            },
        );
    }

    /// Report that the declaration being checked does not declare
    /// `r: trait_name(args)`, which the use `origin` in it needs, unless
    /// it does. The bound it declares for the trait is the only one `r`
    /// has, so its arguments are the ones needed.
    pub(super) fn require_declared_bound(
        &mut self,
        r: RigidId,
        trait_name: TraitKey,
        args: &[Type],
        origin: Origin,
    ) {
        if self.unknown_bounds.contains(&r.var) {
            return;
        }
        let declared = self
            .bounds
            .get(&r.var)
            .and_then(|bounds| bounds.iter().find(|(t, _)| *t == trait_name))
            .map(|(_, declared)| declared.clone());
        if let Some(declared) = declared {
            if args.is_empty() || declared.len() != args.len() {
                return;
            }
            let declared: Vec<Type> = declared
                .iter()
                .map(|t| substitute_vars(t, &self.rigid_of))
                .collect();
            let agree = args
                .iter()
                .zip(&declared)
                .all(|(needed, declared)| self.unify_types(needed, declared).is_ok());
            if !agree {
                self.error(
                    Code::MissingConstraint,
                    format!(
                        "type variable `{}` is declared to implement '{}', not '{}'",
                        r.name,
                        self.show_bound(trait_name, &declared),
                        self.show_bound(trait_name, args)
                    ),
                    origin.span,
                );
            }
            return;
        }
        let bound = self.show_bound(trait_name, args);
        if self.let_vars.contains(&r.var) {
            let needs = match origin.callee {
                Some(callee) => format!("'{callee}' needs"),
                None => "the value needs".to_string(),
            };
            self.errors.push(
                Diagnostic::error(
                    Code::MissingConstraint,
                    origin.span,
                    format!(
                        "{needs} `{}: {bound}`, and the type variable of a `let` annotation cannot have a bound",
                        r.name
                    ),
                )
                .with_help(format!(
                    "write the type the value is used at in place of `{}`, or leave the annotation out",
                    r.name
                )),
            );
            return;
        }
        let needs = match (origin.callee, origin.op) {
            (Some(callee), _) => format!(", which '{callee}' needs"),
            (None, Some(op)) => format!(", which {op} needs"),
            (None, None) => String::new(),
        };
        let diagnostic = Diagnostic::error(
            Code::MissingConstraint,
            origin.span,
            format!(
                "type variable `{}` is not known to implement trait '{bound}'{needs}",
                r.name
            ),
        )
        .with_help(format!("add `where {}: {bound}`", r.name));
        self.errors.push(diagnostic);
    }

    /// The methods named `field` that the bounds in scope give the
    /// annotation variable `r`, each with its trait and its type: `Self`
    /// is `r`, and the trait's parameters are what the bound says
    /// (`where a: TryInto(Int)`).
    pub(super) fn bound_methods(&mut self, r: RigidId, field: Symbol) -> Vec<(TraitKey, Scheme)> {
        let in_scope = self.bounds.get(&r.var).cloned().unwrap_or_default();
        let mut matches: Vec<(TraitKey, Scheme)> = Vec::new();
        for (trait_name, bound_args) in in_scope {
            let Some(info) = self.tables.traits.get(&trait_name) else {
                continue;
            };
            let Some((_, method_ty)) = info.methods.iter().find(|(n, _)| *n == field) else {
                continue;
            };
            let mut mapping: HashMap<TyVar, Type> = HashMap::new();
            mapping.insert(info.self_var, Type::Rigid(r));
            if bound_args.len() == info.param_var_ids.len() {
                for (&tv, arg) in info.param_var_ids.iter().zip(&bound_args) {
                    mapping.insert(tv, substitute_vars(arg, &self.rigid_of));
                }
            }
            // What the method leaves general (its own type variables)
            // is new at each call, which owes the method's own bounds.
            let ty = substitute_vars(method_ty, &mapping);
            let preds = info
                .method_bounds
                .get(&field)
                .into_iter()
                .flatten()
                .map(|pred| pred.substitute(&mapping))
                .collect();
            matches.push((
                trait_name,
                Scheme {
                    vars: free_vars_in(&ty),
                    preds,
                    ty,
                    optional_last_param: false,
                },
            ));
        }
        if let Some(t) = self.forced_trait {
            matches.retain(|(n, ..)| *n == t);
        }
        matches
    }

    /// The type of a use of the method `method` at `span`, from its
    /// scheme: the use owes the scheme's predicates.
    pub(super) fn instantiate_method(
        &mut self,
        scheme: &Scheme,
        method: Symbol,
        span: Span,
    ) -> Type {
        self.named_use = Some(Origin {
            span,
            callee: Some(method),
            op: None,
        });
        self.instantiate(scheme)
    }

    /// Report a call of `method`, of the trait `trait_name` another module
    /// declares without `pub`: its methods can be called only there.
    pub(super) fn private_method(&mut self, trait_name: TraitKey, method: Symbol, span: Span) {
        let module = self.private_owner(trait_name).unwrap_or(self.module_name);
        self.errors.push(
            Diagnostic::error(
                Code::PrivateItem,
                span,
                format!(
                    "method `{method}` belongs to trait '{trait_name}', which is private to \
                     module '{module}'"
                ),
            )
            .with_help(format!(
                "mark it `pub trait {trait_name}` in module '{module}' to call its methods \
                 from another module"
            )),
        );
    }

    /// The module that declares the trait `trait_name` without `pub`, by
    /// name, when it is another module: the trait's methods cannot be
    /// called here.
    pub(super) fn private_owner(&self, trait_name: TraitKey) -> Option<Symbol> {
        let (owner, module) = self.tables.traits.get(&trait_name)?.private_to?;
        (owner != self.module).then_some(module)
    }

    /// The private trait of another module that alone provides `method`:
    /// every impl that has a method of that name is of such a trait, and
    /// no trait this module may name declares it.
    fn only_private_provider(&self, method: Symbol) -> Option<TraitKey> {
        let mut providers = self
            .tables
            .method_table
            .iter()
            .filter(|((_, m), _)| *m == method)
            .map(|(_, entry)| entry.trait_name);
        let first = providers.next()??;
        if self.private_owner(first).is_none()
            || providers.any(|t| t.is_none_or(|t| self.private_owner(t).is_none()))
        {
            return None;
        }
        let visible_declares = self.tables.traits.iter().any(|(key, info)| {
            self.private_owner(*key).is_none() && info.methods.iter().any(|(n, _)| *n == method)
        });
        (!visible_declares).then_some(first)
    }

    /// The type of `receiver.method` for the impl method `entry`: the
    /// method's scheme instantiated (`instantiate`), with the receiver
    /// unified with its `self` parameter, so that what the impl's header
    /// and the method ask of the receiver's parts is checked against the
    /// receiver's type; on a part still unknown it waits like any
    /// predicate owed.
    pub(super) fn dispatch_method_entry(
        &mut self,
        entry: &MethodEntry,
        method_name: Symbol,
        receiver_ty: &Type,
        span: Span,
    ) -> Type {
        self.last_field_access_was_method = true;
        let head = self.type_name_for_impl(&self.apply(receiver_ty));
        // A call that names its trait (a derived impl's body) calls that
        // trait's method.
        let forced_entry = match (self.forced_trait, head) {
            (Some(t), Some(head)) if self.entry_trait(entry, method_name) != Some(t) => {
                self.trait_method_entry(head, method_name, t)
            }
            _ => None,
        };
        let entry = forced_entry.as_ref().unwrap_or(entry);
        // A method of a trait another module declares without `pub` can
        // be called only in that module.
        if let Some(trait_name) = entry.trait_name
            && self.private_owner(trait_name).is_some()
        {
            self.private_method(trait_name, method_name, span);
            return Type::Error;
        }
        if self.forced_trait.is_none()
            && let Some(head) = head
            && self.ambiguous_method_call(head, method_name, span)
        {
            return Type::Error;
        }
        self.method_trait = self
            .forced_trait
            .or_else(|| self.entry_trait(entry, method_name));
        // What the impl and the method ask of the receiver's parts and
        // of the method's own type variables is owed, and checked once
        // the receiver is unified with the method's `self` below.
        let owed_before = self.wanted.len();
        let scheme = self.method_scheme(entry);
        let instantiated_ty = self.instantiate_method(&scheme, method_name, span);
        // Reject value-receiver calls on no-self trait methods (`empty`,
        // `default`, etc.). The method has no slot for the receiver, so
        // invoking it via `instance.method()` is meaningless. Point the
        // user at the type-level form `TypeName.method()` and return
        // `Type::Error` so the downstream Call arm doesn't pile an arity
        // mismatch on top of the real diagnostic.
        if let Type::Fun(params, _) = &instantiated_ty
            && params.is_empty()
        {
            let suggestion = self
                .type_name_for_impl(&self.apply(receiver_ty))
                .map(|ty| format!("`{ty}.{method_name}()`"))
                .unwrap_or_else(|| format!("`SomeType.{method_name}()`"));
            self.error(
                Code::InvalidMethodCall,
                format!(
                    "method `{method_name}` takes no `self` — \
                     call it on the type instead: {suggestion}"
                ),
                span,
            );
            return Type::Error;
        }
        // Unify the receiver with the method's self param so concrete
        // receiver element types flow into the impl's tyvars before the
        // constraint check below.
        if let Type::Fun(params, _) = &instantiated_ty
            && let Some(self_param) = params.first()
        {
            self.unify(receiver_ty, self_param, span);
        }
        // A method of a structural trait (`.compare()`, `.equal()`,
        // `.hash()`, a derived `.display()`) is the receiver's when its
        // parts have the trait too.
        if let Some(tr) = self.entry_trait(entry, method_name)
            && let Some(head) = head
            && crate::defs::builtin_trait_id(&resolve(tr.name)) == Some(tr.id)
            && self.by_structure(tr, head)
        {
            self.want(
                Pred::Trait {
                    tr,
                    args: Vec::new(),
                    subject: receiver_ty.clone(),
                },
                Origin {
                    span,
                    callee: Some(method_name),
                    op: None,
                },
            );
        }
        self.solve_wanted(owed_before);
        self.apply(&instantiated_ty)
    }

    /// Resolve a method on a type descriptor (`TypeOf(inner)`). The
    /// descriptor is a type carrier — lookup uses `inner`'s effective type
    /// name (for concrete inners) or the active trait constraints (for a
    /// type variable inner). Returns the method's function type with all
    /// `Self` references substituted to `inner`.
    ///
    /// Unlike value-receiver dispatch, the descriptor does NOT occupy an
    /// argument slot of the method. Callers signal this by leaving
    /// `last_field_access_was_method = false` after this returns, so the
    /// downstream Call arm unifies args with params[0..] rather than
    /// params[1..].
    pub(super) fn resolve_type_descriptor_method(
        &mut self,
        inner: &Type,
        field: Symbol,
        span: Span,
    ) -> Option<Type> {
        let inner = self.apply(inner);
        match &inner {
            // A type still unknown has no methods to look up.
            Type::Var(_) => {
                self.error(
                    Code::UnknownMethod,
                    format!(
                        "no method '{field}' on `type {inner}` — \
                         the type variable has no trait constraints. \
                         Add a `where` clause such as `where {inner}: SomeTrait`."
                    ),
                    span,
                );
                None
            }
            Type::Rigid(r) => {
                // The methods of the variable's bounds. The `where`
                // clause promises an impl at every call; the call finds
                // it at run time by the type the descriptor carries.
                let Some(trait_names) = self
                    .bounds
                    .get(&r.var)
                    .map(|bounds| bounds.iter().map(|(t, _)| *t).collect::<Vec<TraitKey>>())
                else {
                    self.error(
                        Code::UnknownMethod,
                        format!(
                            "no method '{field}' on `type {inner}` — \
                             the type variable has no trait constraints. \
                             Add a `where` clause such as `where {inner}: SomeTrait`."
                        ),
                        span,
                    );
                    return None;
                };
                let matches = self.bound_methods(*r, field);
                if matches.is_empty() {
                    let traits_str = trait_names
                        .iter()
                        .map(|s| format!("{s}"))
                        .collect::<Vec<_>>()
                        .join(" + ");
                    self.error(
                        Code::UnknownMethod,
                        format!(
                            "no method '{field}' found on `type {inner}` \
                             in trait constraints ({traits_str})"
                        ),
                        span,
                    );
                    return None;
                }
                if matches.len() > 1 {
                    let traits: Vec<TraitKey> = matches.iter().map(|(t, _)| *t).collect();
                    self.ambiguous_method(field, &format!("`type {inner}`"), &traits, span);
                    return None;
                }
                self.method_trait = Some(matches[0].0);
                let instantiated = self.instantiate_method(&matches[0].1, field, span);
                Some(self.apply(&instantiated))
            }
            _ => {
                // Concrete inner — look up via the method table, keyed on
                // the effective type name (same path as
                // `type_name_for_impl`).
                let name = self.type_name_for_impl(&inner)?;
                let entry = self.tables.method_table.get(&(name, field)).cloned()?;
                if let Some(trait_name) = entry.trait_name
                    && self.private_owner(trait_name).is_some()
                {
                    self.private_method(trait_name, field, span);
                    return Some(Type::Error);
                }
                if self.ambiguous_method_call(name, field, span) {
                    return Some(Type::Error);
                }
                self.method_trait = self.entry_trait(&entry, field);
                let scheme = self.method_scheme(&entry);
                let instantiated = self.instantiate_method(&scheme, field, span);
                Some(self.apply(&instantiated))
            }
        }
    }

    /// Report `T.field` where the type `T` has no method `field`. A builtin
    /// module named like the type with a function of that name (`int.parse`
    /// for `Int.parse`) is suggested.
    fn no_type_method(&mut self, type_name: &str, field: Symbol, span: Span) {
        let module = type_name.to_lowercase();
        let mut d = Diagnostic::error(
            Code::UnresolvedName,
            span,
            format!("type '{type_name}' has no method '{field}'"),
        );
        let qualified = intern(&format!("{module}.{field}"));
        if crate::module::is_builtin_module(&module) && builtin_env_has(qualified) {
            d = d.with_help(format!(
                "did you mean `{module}.{field}`, a function of module `{module}`?"
            ));
        }
        self.errors.push(d);
    }

    // ── Check function body ─────────────────────────────────────────

    /// Check the body of `f` against `sig`, its signature as the body
    /// sees it: the parameters are bound to the signature's types, the
    /// annotation variables are rigid and bounded as the `where` clauses
    /// say, and the body's type is the signature's result.
    pub(super) fn check_body(&mut self, f: &mut FnDecl, sig: &FnSig, env: &mut TypeEnv) {
        // Validate where clauses
        for wc in &f.where_clauses {
            // A bound the resolver resolved to nothing: it reported why.
            if wc.trait_res == Some(crate::defs::Res::Error) {
                continue;
            }
            let type_param = &wc.type_param;
            let trait_args = &wc.trait_args;
            let Some(trait_name) = self
                .named_trait(wc.trait_res, wc.trait_name)
                .filter(|t| self.tables.traits.contains_key(t))
            else {
                self.error(
                    Code::UnknownTrait,
                    format!(
                        "unknown trait '{}' in where clause for '{}'",
                        wc.trait_name, type_param
                    ),
                    f.span,
                );
                continue;
            };
            // G1 (round 60, extended round 101): a where-clause bound
            // must supply exactly the trait's declared number of type
            // arguments. Zero args on a parameterized trait
            // (`trait Cast(to)` + `where a: Cast`) leaves the implied
            // `to` unresolved; a nonzero length MISMATCH
            // (`where a: Cast(Int, String)`) is worse — the
            // parameterized-trait verification in
            // `verify_trait_obligation` only runs its round-58
            // positional arg-compatibility zip when the bound's and
            // the impl's arg lists have equal length, so a mismatched
            // bound silently degraded to a bare "implements Cast"
            // check and matched ANY `Cast(*)` impl. The arity check
            // lives at the user-written syntax sites (here, plus the
            // impl-level and method-level where-clause loops in
            // `register_trait_impl`) because bare `&[]` is legitimate
            // for supertrait sub-obligations inside
            // `verify_trait_obligation` itself.
            self.check_where_bound_arity(trait_name, trait_args.len(), f.span);
        }

        let param_types = sig.params.clone();
        let ret_type = sig.ret.clone();
        for r in &sig.rigid {
            self.rigid_of.insert(r.var, Type::Rigid(*r));
        }
        let prev_names = std::mem::replace(&mut self.sig_names, sig.names.clone());

        // The declared bounds are in scope in the body: a rigid variable
        // has the methods of its bounds and of their supertraits.
        for Pred::Trait { tr, args, subject } in &sig.bounds {
            if let Type::Var(var) = subject {
                self.declare_bound(*var, *tr, args.clone());
            }
        }

        // Bind parameters
        // Soundness: reject duplicate binding names across the whole fn
        // param list before we start defining them in the env. Without
        // this, `fn f(a: Int, a: Int)` typechecks and the second param
        // silently shadows the first. See `check_fn_params_duplicate_bindings`.
        self.check_fn_params_duplicate_bindings(&f.params);
        env.push();
        for (i, param) in f.params.iter_mut().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.check_pattern(
                    &mut param.pattern,
                    ty,
                    env,
                    f.span,
                    PatternMode::Binding(BindingSite::FnParam),
                );
            }
        }

        // Set the expected return type for return and ? validation
        let prev_return_type = self.current_return_type.take();
        self.current_return_type = Some(ret_type.clone());
        // Round 93: fresh `?`-site tracking per fn body, so a failed
        // body/return unify can point back at the `?` that demanded a
        // Result/Option return.
        let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

        // Infer the body and unify with declared return type
        let body_type = self.infer_expr(&mut f.body, env);
        env.pop();
        let ret_unify_err_count = self.errors.len();
        self.unify(&body_type, &ret_type, f.body.span);
        self.retarget_ok_wrap_fixes(ret_unify_err_count, &f.body);
        self.note_qmark_requirement_on_ret_mismatch(ret_unify_err_count, &ret_type);

        self.current_return_type = prev_return_type;
        self.current_qmark_spans = prev_qmark_spans;
        self.sig_names = prev_names;
    }

    /// The Ok-wrap fixes of the diagnostics from `from` on that are about
    /// the whole `body` of a function: a block is wrapped at its tail
    /// expression, not around its braces, and a block without one gets
    /// no fix.
    fn retarget_ok_wrap_fixes(&mut self, from: usize, body: &Expr) {
        let tail = match &body.kind {
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(e)) => Some(e.span),
                _ => None,
            },
            _ => return,
        };
        for d in self.errors.iter_mut().skip(from) {
            if d.span != body.span {
                continue;
            }
            match tail {
                Some(tail) => {
                    for fix in &mut d.fixes {
                        fix.edits = vec![
                            (Span::point(tail.file, tail.start), "Ok(".to_string()),
                            (Span::point(tail.file, tail.end), ")".to_string()),
                        ];
                    }
                }
                None => d.fixes.clear(),
            }
        }
    }

    /// GAP (round 93): when the body/return-type unify fails AND the
    /// return type was driven to Result/Option by a `?` in the body,
    /// append a note naming the `?` site. Without this, `?` inside an
    /// unannotated (effectively unit-returning) fn surfaces as a bare
    /// "type mismatch: expected Result(_, String), got ()" with the
    /// caret on the fn header — no pointer to the `?` that created the
    /// requirement. Companion to `chain_hint` (mod.rs), which covers
    /// the inverse direction (got Result/Option, expected plain): both
    /// append a continuation line to the raw unify mismatch rather
    /// than emitting a separate diagnostic.
    fn note_qmark_requirement_on_ret_mismatch(&mut self, err_count_before: usize, ret_ty: &Type) {
        if self.errors.len() == err_count_before {
            return;
        }
        let Some(qspan) = self.current_qmark_spans.first().copied() else {
            return;
        };
        // Only when the return type is actually Result/Option-shaped —
        // i.e. the shape `?` itself unifies into the return type. If the
        // user annotated something else, the `?` site already got its own
        // error and this note would mislead.
        let ret_resolved = self.apply(ret_ty);
        let is_qmark_shape = matches!(
            &ret_resolved,
            Type::Generic(n, _) if n.is_builtin("Result") || n.is_builtin("Option")
        );
        if !is_qmark_shape {
            return;
        }
        if let Some(err) = self.errors.last_mut() {
            err.labels.push((
                qspan,
                format!("this `?` requires this function to return {ret_resolved}"),
            ));
            // The `?` made the return type a `Result`; wrapping the body
            // in `Ok(...)` would not fix the function.
            err.fixes.clear();
        }
    }

    // ── Named records and variants ──────────────────────────────────
    //
    // What a record literal, record pattern or variant pattern names
    // (`util.Pt { x: 1 }`, `shapes.Circle(r)`), the resolver decided; the
    // helpers below turn its resolution into the checker's tables.

    /// Instantiate a record's declared field types, substituting fresh
    /// type variables for the record's type parameters (if any) so
    /// distinct uses don't share template variables. Shared by the
    /// record-literal and record-pattern paths.
    pub(super) fn instantiate_record_fields(
        &mut self,
        rec_info: &RecordInfo,
        param_var_ids: Option<&[TyVar]>,
    ) -> (Vec<(Symbol, Type)>, Vec<Type>) {
        if let Some(param_var_ids) = param_var_ids {
            let args: Vec<Type> = param_var_ids.iter().map(|_| self.fresh_var()).collect();
            let mapping: HashMap<TyVar, Type> = param_var_ids
                .iter()
                .copied()
                .zip(args.iter().cloned())
                .collect();
            let fields = rec_info
                .fields
                .iter()
                .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                .collect();
            (fields, args)
        } else {
            (rec_info.fields.clone(), Vec::new())
        }
    }

    /// Whether a name written as a constructor names a record type.
    pub(super) fn names_record(&self, res: Option<crate::defs::Res>, name: Symbol) -> bool {
        self.res_type(res)
            .or_else(|| self.named_type(None, name))
            .is_some_and(|ty| self.tables.records.contains_key(&ty))
    }

    /// The record type a record pattern or literal names, with its type
    /// parameters' variables: the one the resolver resolved it to.
    /// Reports a name that names no record type, unless the resolver
    /// reported it already.
    pub(super) fn named_record(
        &mut self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
        in_pattern: bool,
    ) -> Option<(TypeRef, RecordInfo, Option<Vec<TyVar>>)> {
        if res == Some(crate::defs::Res::Error) || self.names_rejected(res, name) {
            return None;
        }
        let ty = self.named_type(res, name);
        let name = ty.map_or(name, |ty| ty.name);
        match ty.and_then(|ty| Some((ty, self.tables.records.get(&ty).cloned()?))) {
            Some((ty, info)) => {
                let ids = self.tables.record_param_var_ids.get(&ty).cloned();
                Some((ty, info, ids))
            }
            None => {
                let message = if in_pattern {
                    format!("undefined record type '{name}' in pattern")
                } else {
                    format!("undefined type '{name}'")
                };
                self.error(Code::UndefinedType, message, span);
                None
            }
        }
    }

    /// Whether `callee` is a member of a module, `m.f`, as the resolver
    /// resolved it: the Call arm then uses the qualified `m.f` scheme
    /// directly, so that its where-clause constraints flow through
    /// (`instantiate_with_constraints` carries them; the FieldAccess arm's
    /// `instantiate` would drop them).
    fn callee_module_is_in_scope(&self, callee: &Expr, _env: &TypeEnv) -> bool {
        let ExprKind::FieldAccess(obj, _, _) = &callee.kind else {
            return false;
        };
        matches!(obj.res, Some(crate::defs::Res::Module(_)))
            && !matches!(callee.res, Some(crate::defs::Res::Error))
    }

    /// The name a callee is written with, for a message about a call of
    /// it: `f`, or `m.f` for a member of a module.
    fn callee_label(callee: &Expr) -> Option<Symbol> {
        match &callee.kind {
            ExprKind::Ident(name) => Some(*name),
            ExprKind::FieldAccess(obj, field, _)
                if matches!(obj.res, Some(crate::defs::Res::Module(_))) =>
            {
                match &obj.kind {
                    ExprKind::Ident(module) => Some(intern(&format!("{module}.{field}"))),
                    _ => Some(*field),
                }
            }
            _ => None,
        }
    }

    /// Whether the function that `callee` names declares its last
    /// parameter optional (see `Scheme::optional_last_param`).
    ///
    /// Only a bare function name or a `module.function` path names a
    /// signature. Any other callee expression is a function value, and a
    /// function value is called with its full arity. A `module.function`
    /// path counts only when it really is a module-qualified name, by
    /// the same test the Call arm uses to pick the qualified scheme.
    fn callee_declares_optional_last_param(&self, callee: &Expr, env: &TypeEnv) -> bool {
        self.callee_scheme(callee, env)
            .is_some_and(|scheme| scheme.optional_last_param)
    }

    /// The scheme of the function a callee names: a bare name (a local,
    /// a function of the module or one it imports, a builtin) or a
    /// `module.function` path. `None` for any other callee, a function
    /// value.
    fn callee_scheme(&self, callee: &Expr, env: &TypeEnv) -> Option<Scheme> {
        match &callee.kind {
            ExprKind::Ident(name) => {
                if callee.res == Some(crate::defs::Res::Error) {
                    return None;
                }
                self.def_scheme(callee.res, env)
                    .or_else(|| env.lookup(*name).cloned())
            }
            ExprKind::FieldAccess(..) if self.callee_module_is_in_scope(callee, env) => {
                self.def_scheme(callee.res, env)
            }
            _ => None,
        }
    }

    /// The type a `let`'s annotation writes, and the type variables it
    /// introduces. A variable of the enclosing signature is that
    /// variable; any other is the `let`'s own, and rigid: the value must
    /// have the annotated type whatever the variable stands for, and a
    /// `let` that generalises is general in it.
    pub(super) fn resolve_let_annotation(&mut self, te: &TypeExpr) -> (Type, Vec<RigidId>) {
        // B2: the arity-error span hint is the annotation's own span, so
        // diagnostics from `resolve_type_expr` point at the user-written
        // type.
        let prev_type_span = self.current_type_anno_span.replace(te.span);
        let mut names = self.sig_names.clone();
        let declared = self.resolve_type_expr(te, &mut names);
        self.current_type_anno_span = prev_type_span;
        let mut own: Vec<RigidId> = names
            .iter()
            .filter(|(name, _)| !self.sig_names.contains_key(name))
            .filter_map(|(name, ty)| match ty {
                Type::Var(var) => Some(RigidId {
                    var: *var,
                    name: match resolve(*name).strip_prefix("__row__") {
                        Some(row) => intern(row),
                        None => *name,
                    },
                }),
                _ => None,
            })
            .collect();
        own.sort_by_key(|r| r.var);
        self.let_vars.extend(own.iter().map(|r| r.var));
        (rigidify(&declared, &own), own)
    }

    /// The scheme of a `let` that generalises: `generalize`, and general
    /// in the type variables its annotation introduced (`own`).
    fn generalize_let(&mut self, ty: &Type, own: &[RigidId]) -> Scheme {
        let mut scheme = self.generalize_local(ty);
        if !own.is_empty() {
            let (ty, vars) = release_rigid(&scheme.ty, own);
            scheme.ty = ty;
            scheme.vars.extend(vars);
        }
        scheme
    }

    /// Whether a name in the value of the top-level `let` being checked
    /// names a top-level `let` of the module declared after it.
    fn reads_later_let(&self, res: Option<crate::defs::Res>, name: Symbol) -> bool {
        let Some(current) = self.checking_let else {
            return false;
        };
        self.res_def(res).is_some_and(|def| {
            def.module == self.module
                && def.kind == crate::defs::DefKind::Let
                && self.let_index.get(&name).is_some_and(|i| *i > current)
        })
    }

    /// Check a call: `callee(args)`, `a |> callee(rest)` (the piped
    /// value is the first argument) or `a |> callee`. The one call rule:
    /// the callee is checked first, so its use owes its predicates
    /// (`instantiate`); each argument is then checked and unified with
    /// its parameter, left to right, a closure literal against the
    /// function type its parameter expects; last, the predicates the
    /// arguments have decided are checked.
    fn check_call(
        &mut self,
        callee: &mut Expr,
        mut args: Vec<&mut Expr>,
        span: Span,
        form: CallForm,
        env: &mut TypeEnv,
    ) -> Type {
        let callee_name = match &callee.kind {
            ExprKind::Ident(name) => Some(*name),
            _ => None,
        };
        // Option B (parser-recovery cascade fix): the signature of a
        // parser-recovery stub cannot be trusted; the real error is the
        // parse failure that produced it. The arguments are still
        // checked for their own errors.
        if callee_name.is_some_and(|name| self.recovery_stub_names.contains(&name)) {
            for arg in args {
                let _ = self.infer_expr(arg, env);
            }
            return self.fresh_var();
        }
        // Whether the named callee's signature lets the call leave out
        // the last argument.
        let optional_last_param = self.callee_declares_optional_last_param(callee, env);

        let owed_before = self.wanted.len();
        // Reset the method-dispatch flag so a stale value from an
        // earlier field access does not leak into this call, and read it
        // before the arguments are checked (they may overwrite it).
        self.last_field_access_was_method = false;
        self.named_use = Self::callee_label(callee).map(|label| Origin {
            span,
            callee: Some(label),
            op: None,
        });
        self.callee_position = true;
        self.unknown_receiver = None;
        let callee_ty = self.infer_expr(callee, env);
        self.callee_position = false;
        self.named_use = None;
        let callee_ty = self.apply(&callee_ty);
        let is_method_call = self.last_field_access_was_method;
        self.last_field_access_was_method = false;
        // `x.m(args)` where the type of `x` is unknown: the call waits
        // for it.
        if let Some((recv, name)) = self.unknown_receiver.take() {
            let args: Vec<Type> = args.iter_mut().map(|a| self.infer_expr(a, env)).collect();
            let result = self.fresh_var();
            self.want_goal(
                Goal::Select {
                    recv,
                    name,
                    args,
                    result: result.clone(),
                },
                Origin {
                    span: callee.span,
                    callee: Some(name),
                    op: None,
                },
            );
            return result;
        }

        let result_ty = match &callee_ty {
            Type::Fun(params, ret) => {
                // A method call supplies `self` implicitly
                // (`dispatch_method_entry` has unified it with the
                // receiver), so the arguments line up with `params[1..]`.
                let implicit_self = usize::from(is_method_call);
                // What the callee's use owes for a type variable of a
                // parameter is owed for that argument: a bound that
                // fails is reported at it.
                for k in owed_before..self.wanted.len() {
                    let Goal::Pred(Pred::Trait {
                        subject: Type::Var(v),
                        ..
                    }) = &self.wanted[k].goal
                    else {
                        continue;
                    };
                    let at = params
                        .iter()
                        .skip(implicit_self)
                        .position(|param| free_vars_in(param).contains(v))
                        .and_then(|i| args.get(i));
                    if let (Some(arg), CallForm::Call) = (at, form) {
                        self.wanted[k].origin.span = arg.span;
                    }
                }
                let mut mismatched = false;
                for (i, arg) in args.iter_mut().enumerate() {
                    let param = params.get(i + implicit_self);
                    if let (ExprKind::Lambda { .. }, Some(param)) = (&arg.kind, param)
                        && let Type::Fun(expected, _) = self.apply(param)
                    {
                        self.expected_closure = Some(expected);
                    }
                    let arg_ty = self.infer_expr(arg, env);
                    self.expected_closure = None;
                    if let Some(param) = param {
                        let at = match form {
                            CallForm::Call => arg.span,
                            CallForm::BarePipe => span,
                        };
                        let reported = self.errors.len();
                        self.unify(&arg_ty, param, at);
                        mismatched |= self.errors.len() > reported;
                    }
                }
                // An argument of the wrong type is the one thing wrong
                // with the call: what the callee owes for it is not
                // asked as well.
                if mismatched {
                    for wanted in &mut self.wanted[owed_before..] {
                        wanted.solved = true;
                    }
                }
                // A method has no optional parameter.
                let optional_last_param = optional_last_param && !is_method_call;
                if !call_arity_matches(
                    params.len(),
                    optional_last_param,
                    args.len() + implicit_self,
                ) {
                    let message = match form {
                        CallForm::Call => {
                            // A method's receiver is not one of the
                            // arguments the call writes.
                            let what = match (&callee.kind, callee_name) {
                                (ExprKind::FieldAccess(_, method, _), _) if is_method_call => {
                                    format!("method `{method}`")
                                }
                                (_, Some(name)) => format!("`{name}`"),
                                _ => "function".to_string(),
                            };
                            format!(
                                "{what} expects {}, got {}",
                                accepted_arity_text(
                                    params.len() - implicit_self,
                                    optional_last_param
                                ),
                                args.len()
                            )
                        }
                        // B6: `a |> f` supplies one argument; piping into
                        // a function that needs more without an explicit
                        // call forgets the remaining ones.
                        CallForm::BarePipe => format!(
                            "cannot pipe into function taking {} {}; wrap in a call or use partial application",
                            params.len() - implicit_self,
                            plural(params.len() - implicit_self, "argument", "arguments")
                        ),
                    };
                    self.error(Code::ArityMismatch, message, span);
                }
                (**ret).clone()
            }
            // The callee is of a type still unknown: a function of these
            // arguments.
            Type::Var(_) => {
                let arg_types: Vec<Type> =
                    args.iter_mut().map(|a| self.infer_expr(a, env)).collect();
                let ret = self.fresh_var();
                let fn_ty = Type::Fun(arg_types, Box::new(ret.clone()));
                self.unify(&callee_ty, &fn_ty, span);
                ret
            }
            other => {
                for arg in args.iter_mut() {
                    let _ = self.infer_expr(arg, env);
                }
                match (other, form) {
                    (Type::Error, _) => Type::Error,
                    (Type::Never, _) => Type::Never,
                    (_, CallForm::Call) => {
                        self.error(
                            Code::TypeMismatch,
                            format!("`{other}` is not callable"),
                            span,
                        );
                        self.fresh_var()
                    }
                    (_, CallForm::BarePipe) => {
                        self.error(
                            Code::TypeMismatch,
                            "pipe operator requires a function on the right-hand side".to_string(),
                            callee.span,
                        );
                        self.fresh_var()
                    }
                }
            }
        };

        // What the callee's use owes, where the arguments have decided
        // the subject.
        self.solve_wanted(owed_before);
        result_ty
    }

    // ── Expression type inference ───────────────────────────────────

    pub(super) fn infer_expr(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        if !matches!(expr.kind, ExprKind::FieldAccess(..)) {
            return self.infer_expr_kind(expr, env);
        }
        // A method call names its method's trait: the access records it
        // (`Expr::res`), and the compiler keys the call by it.
        let outer = self.method_trait.take();
        // A trait the access names already (a derived impl's body) is
        // taken off it while it is inferred, and written back below.
        let forced = match expr.res {
            Some(crate::defs::Res::Def(id)) => self.trait_key(id),
            _ => None,
        };
        if forced.is_some() {
            expr.res = None;
        }
        let outer_forced = std::mem::replace(&mut self.forced_trait, forced);
        let called = self.callee_position;
        let ty = self.infer_expr_kind(expr, env);
        self.forced_trait = outer_forced;
        // A method is called, not taken: `x.m` that is not a callee is a
        // field.
        let ty = if self.last_field_access_was_method && !called {
            self.last_field_access_was_method = false;
            self.method_trait = None;
            let ExprKind::FieldAccess(_, method, _) = &expr.kind else {
                unreachable!()
            };
            self.errors.push(
                Diagnostic::error(
                    Code::InvalidMethodCall,
                    expr.span,
                    format!("method '{method}' is not a value: a method is called"),
                )
                .with_help(format!(
                    "call it, `x.{method}(..)`, or pass a closure that does, `{{ x -> x.{method}(..) }}`"
                )),
            );
            expr.ty = Some(Type::Error);
            Type::Error
        } else {
            ty
        };
        if let Some(t) = self.method_trait.take() {
            expr.res = Some(crate::defs::Res::Def(t.id.0));
        }
        self.method_trait = outer;
        ty
    }

    fn infer_expr_kind(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let span = expr.span;
        self.at = span;
        let called = std::mem::take(&mut self.callee_position);
        let ty = match &mut expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::StringLit(..) => Type::String,
            ExprKind::Unit => Type::Unit,

            ExprKind::StringInterp(parts) => {
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
                            },
                            Origin {
                                span: expr_span,
                                callee: None,
                                op: None,
                            },
                        );
                    }
                }
                Type::String
            }

            ExprKind::List(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::List(Box::new(tv))
                } else {
                    // Infer each element first (without unifying), so we can
                    // produce a single targeted "list elements must have the
                    // same type" error pointing at the first mismatching
                    // element instead of the old "expected X, got Y" which
                    // read as if the user had declared the first type.
                    let mut elem_infos: Vec<(Type, Span, bool)> = Vec::with_capacity(elems.len());
                    for elem in elems.iter_mut() {
                        match elem {
                            ListElem::Single(e) => {
                                let t = self.infer_expr(e, env);
                                elem_infos.push((t, e.span, false));
                            }
                            ListElem::Spread(e) => {
                                let t = self.infer_expr(e, env);
                                elem_infos.push((t, e.span, true));
                            }
                        }
                    }

                    // Establish the "first element type" once, up front.
                    let first_ty = {
                        let (t, _, is_spread) = &elem_infos[0];
                        if *is_spread {
                            // Spread contributes a List(inner); extract inner
                            // for the "first element" description.
                            let applied = self.apply(t);
                            match applied {
                                Type::List(inner) => *inner,
                                _ => t.clone(),
                            }
                        } else {
                            t.clone()
                        }
                    };

                    let elem_type = self.fresh_var();
                    self.unify(&elem_type, &first_ty, elem_infos[0].1);

                    for (idx, (t, espan, is_spread)) in elem_infos.iter().enumerate() {
                        let unified = if *is_spread {
                            let expected = Type::List(Box::new(elem_type.clone()));
                            self.unify_types(&expected, t)
                        } else {
                            self.unify_types(&elem_type, t)
                        };
                        if unified.is_err() {
                            // A list-level message, clearer than the
                            // mismatch of the two types.
                            let elem_ty = if *is_spread {
                                let applied = self.apply(t);
                                match applied {
                                    Type::List(inner) => *inner,
                                    other => other,
                                }
                            } else {
                                self.apply(t)
                            };
                            let first_resolved = self.apply(&first_ty);
                            let (first_shown, elem_shown) =
                                self.show_apart(&first_resolved, &elem_ty);
                            self.error(Code::TypeMismatch,
                                format!(
                                    "list elements must have the same type: first element is {}, but element {} is {}",
                                    first_shown,
                                    idx + 1,
                                    elem_shown
                                ),
                                *espan,
                            );
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
                    // GAP (round 93): the unify calls here used to pass the
                    // established type as t1 and the deviant entry as t2,
                    // reversing the documented t1=got/t2=expected convention
                    // (`#{ "a": 1, 2: 3 }` reported "expected Int, got
                    // String"). Fixed direction, and — mirroring the list-
                    // literal arm above — replace the raw unify mismatch
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
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
                } else {
                    // GAP (round 93): same direction fix + curated message
                    // as the map arm above — `#[1, 2, "x"]` used to report
                    // "expected String, got Int". Mirror the list-literal
                    // wording for set elements.
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
                    Type::Set(Box::new(elem_type))
                }
            }

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems.iter_mut().map(|e| self.infer_expr(e, env)).collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(name) => {
                let name = *name;
                if expr.res == Some(crate::defs::Res::Error) {
                    // The resolver reported the name, or it comes from a
                    // module that failed to load.
                    Type::Error
                } else if self.reads_later_let(expr.res, name) {
                    // A top-level `let` runs before the ones declared
                    // after it: the value of a later one is not there
                    // yet.
                    self.errors.push(
                        Diagnostic::error(
                            Code::UndefinedVariable,
                            span,
                            format!("undefined variable '{name}'"),
                        )
                        .with_help(format!(
                            "`{name}` is a top-level `let` declared after this one, and \
                             top-level `let`s run in the order they are written"
                        )),
                    );
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
                    });
                    self.instantiate(&scheme)
                } else if name == intern("self") {
                    // `self` is resolved at runtime — allow without error
                    self.fresh_var()
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
                    let msg = format_undefined_variable_message(name, env, "");
                    self.error_help(Code::UndefinedVariable, msg, span);
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field, _) => {
                self.last_field_access_was_method = false;
                let field = *field;
                // What the resolver said the access names: nothing (it
                // reported why, or the module failed to load), a variant
                // (`m.Red`, `Shape.Red`, `m.Shape.Red`), whose constructor
                // is bound as `Enum.Variant`, or a member of a module
                // (`m.f`, `m.Pt`), bound as `m.f`.
                if expr.res == Some(crate::defs::Res::Error) {
                    expr.ty = Some(Type::Error);
                    return Type::Error;
                }
                let names_member = self.res_variant_enum(expr.res).is_some()
                    || matches!(obj.res, Some(crate::defs::Res::Module(_)));
                if names_member {
                    let ty = match self.def_scheme(expr.res, env) {
                        Some(scheme) => {
                            let ty = self.instantiate(&scheme);
                            self.apply(&ty)
                        }
                        // A member the resolver found that has no
                        // scheme: a type alias or a trait used as a
                        // value.
                        None => {
                            let module = match &obj.kind {
                                ExprKind::Ident(m) => format!("{m}."),
                                _ => String::new(),
                            };
                            self.error(
                                Code::UndefinedVariable,
                                format!("'{module}{field}' is not a value"),
                                span,
                            );
                            Type::Error
                        }
                    };
                    expr.ty = Some(ty.clone());
                    return ty;
                }
                // `Type.method`: a method of a type, called through it
                // (`Shape.describe`, `m.Shape.describe`, `Int.display`).
                let type_ref = match (&obj.kind, obj.res) {
                    (ExprKind::Ident(name), None) => self.named_type(None, *name),
                    (ExprKind::Ident(_) | ExprKind::FieldAccess(..), res) => self.res_type(res),
                    _ => None,
                };
                if let Some(ty) = type_ref {
                    let key = (canonical_head(&self.tables.resolver, ty), field);
                    if let Some(entry) = self.tables.method_table.get(&key).cloned() {
                        if let Some(trait_name) = entry.trait_name
                            && self.private_owner(trait_name).is_some()
                        {
                            self.private_method(trait_name, field, span);
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if self.ambiguous_method_call(key.0, field, span) {
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        self.method_trait = self.entry_trait(&entry, field);
                        let scheme = self.method_scheme(&entry);
                        let ty = self.instantiate_method(&scheme, field, span);
                        let ty = self.apply(&ty);
                        expr.ty = Some(ty.clone());
                        return ty;
                    }
                    // A type with no such method, whose name is no value
                    // either (`Option.compare`, `time.Weekday.nope`): the
                    // method is what is missing.
                    if matches!(obj.kind, ExprKind::FieldAccess(..))
                        || self.def_scheme(obj.res, env).is_none()
                    {
                        self.no_type_method(&resolve(ty.name), field, span);
                        expr.ty = Some(Type::Error);
                        return Type::Error;
                    }
                }

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                self.last_field_access_was_method = false;
                let obj_ty = self.apply(&obj_ty);
                // Phase B: canonicalise before dispatch so a Range
                // receiver (from `1..n`) lands in the List arm. The
                // dedicated `Type::List(_) | Type::Range(_)` arm
                // below is therefore reduced to `Type::List(_)`; no
                // separate Range redirect to the List method table
                // is needed because the Range form has been collapsed
                // away upstream of this match.
                let obj_ty = crate::types::canonical::canonicalize(&self.tables.resolver, &obj_ty);

                // Field / method access
                //
                // `TypeOf(inner)` values (descriptors produced by `type a`
                // parameters or bare type names like `Int`) dispatch to
                // trait methods on `inner`'s type. The descriptor is a
                // type carrier, not a value argument — the downstream
                // Call arm therefore sees `is_method_call = false`, so
                // arg unification runs with no offset and the fn body's
                // parameter slots line up one-for-one with the user's
                // explicit arguments.
                if let Type::Generic(gname, gargs) = &obj_ty
                    && gname.is_builtin(crate::defs::TYPE_OF)
                    && gargs.len() == 1
                {
                    let resolved = self.resolve_type_descriptor_method(&gargs[0], field, span);
                    if let Some(ty) = resolved {
                        self.last_field_access_was_method = false;
                        expr.ty = Some(ty.clone());
                        return ty;
                    }
                    // A concrete type with no such method (a type
                    // variable's case is reported above).
                    let inner = self.apply(&gargs[0]);
                    if !matches!(inner, Type::Var(_) | Type::Rigid(_) | Type::Error) {
                        self.no_type_method(&format!("{inner}"), field, span);
                    }
                    return Type::Error;
                }
                match &obj_ty {
                    Type::AnonRecord { fields, tail } => {
                        if let Some(ft) = fields.get(&field) {
                            ft.clone()
                        } else if let RowTail::Var(_) = tail {
                            // Open row: extend the row variable with this
                            // new field. Bind the existing row var to
                            // `{field: fresh, ...new_row}` and surface the
                            // fresh field type.
                            use std::collections::BTreeMap;
                            let result_ty = self.fresh_var();
                            let new_row = self.fresh_tyvar_id();
                            let mut new_fields = BTreeMap::new();
                            new_fields.insert(field, result_ty.clone());
                            let extended = Type::AnonRecord {
                                fields: new_fields,
                                tail: RowTail::Var(new_row),
                            };
                            self.unify(&obj_ty, &extended, span);
                            result_ty
                        } else {
                            // ERR-GAP (round 81 F2): mirror the sibling
                            // Record / Generic field-access sites and
                            // append a did-you-mean hint.
                            let candidates: Vec<(Symbol, Type)> =
                                fields.iter().map(|(k, v)| (*k, v.clone())).collect();
                            let base = format!("anon record has no field '{field}'");
                            self.error_help(
                                Code::NoSuchField,
                                format_record_field_suggestion(base, field, &candidates),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, type_args) => {
                        // Check record field definitions, substituting type parameters
                        if let Some(rec_info) = self.tables.records.get(type_name).cloned()
                            && let Some((_, ft)) = rec_info.fields.iter().find(|(n, _)| *n == field)
                        {
                            // Substitute the record's type parameters with concrete type args.
                            // When `type_args` is empty but the record is actually
                            // parameterized (which can now only happen if unification
                            // reached here with mismatched arity), instantiate fresh
                            // type vars for each param — never return the shared
                            // template TyVar, which would get mutated across uses
                            // (T1 audit fix; mirrors the check_pattern path).
                            let resolved = if let Some(param_var_ids) =
                                self.tables.record_param_var_ids.get(type_name).cloned()
                            {
                                let mapping: HashMap<TyVar, Type> =
                                    if type_args.len() == param_var_ids.len() {
                                        param_var_ids
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(&v, t)| (v, t.clone()))
                                            .collect()
                                    } else {
                                        // Arity mismatch (already reported elsewhere).
                                        // Fall back to fresh vars to avoid leaking
                                        // the shared template TyVars.
                                        param_var_ids
                                            .iter()
                                            .map(|&v| (v, self.fresh_var()))
                                            .collect()
                                    };
                                let substituted = substitute_vars(ft, &mapping);
                                self.apply(&substituted)
                            } else {
                                self.apply(ft)
                            };
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Check method table (trait methods)
                        if let Some(entry) =
                            self.tables.method_table.get(&(*type_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
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
                        // A method of a structural trait the type has
                        // no entry for (`Some(1).compare(Some(2))`).
                        if let Some(method_ty) = self.structural_method(&obj_ty, field, span) {
                            expr.ty = Some(method_ty.clone());
                            return method_ty;
                        }
                        // GAP (round 35 F7): thread did-you-mean suggestion
                        // through the Generic/named-record field-access
                        // path so `u.nam` on `type User { name, age }`
                        // prints `did you mean 'name'?`.
                        let shown = self.show_type(&Type::Generic(*type_name, vec![]));
                        let base = format!("unknown field or method '{field}' on type {shown}");
                        let msg = if let Some(rec_info) = self.tables.records.get(type_name) {
                            format_record_field_suggestion(base, field, &rec_info.fields)
                        } else {
                            (base, None)
                        };
                        self.error_help(Code::UnknownField, msg, span);
                        Type::Error
                    }
                    // Primitive types — check method table for trait methods.
                    // Channel and Fn are not
                    // auto-derived but user-defined trait impls register entries
                    // under the canonical names "Channel" / "Fn" via
                    // `type_name_for_impl` (see `src/typechecker/mod.rs:2081`,
                    // `src/typechecker/mod.rs:2092`), so dispatch must route those
                    // receivers through the same `method_table` lookup. The
                    // `"Fn"` key matches `canonical_name(Type::Fun)`,
                    // `head_of_canon`, and `dispatch_type_for_value`
                    // — round 71 follow-up unified all four sites on `"Fn"`.
                    Type::Int
                    | Type::Float
                    | Type::Bool
                    | Type::String
                    | Type::Unit
                    | Type::Channel(_)
                    | Type::Fun(_, _) => {
                        // `Unit` is the key of `()` (it matches
                        // canonical_name(Type::Unit) and
                        // dispatch_type_for_value(Value::Unit)), `Fn` of a
                        // function.
                        let type_name = head_of(&obj_ty).expect("a primitive head has a type");
                        if let Some(entry) =
                            self.tables.method_table.get(&(type_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // GAP (round 23 #3): append "did you mean ...?"
                        // when a near edit-distance method is registered
                        // on this type. Keep the existing "on type <Name>"
                        // header so prior-lock tests that match only the
                        // header prefix still pass; the hint is appended
                        // on its own `help:` line.
                        // A method of an impl the resolver rejected: reported there.
                        if self.unresolved_impl_methods.contains(&field) {
                            return Type::Error;
                        }
                        let display = format!("type {type_name}");
                        self.error_help(
                            Code::UnknownMethod,
                            format_unknown_method_message(
                                field,
                                &display,
                                &self.tables.method_table,
                                type_name,
                            ),
                            span,
                        );
                        Type::Error
                    }
                    // Collection types. Phase B: Range receivers were
                    // canonicalised to List upstream of this match, so
                    // the dispatch-time redirect (formerly
                    // `Type::List(_) | Type::Range(_)`) is reduced to a
                    // single List arm. Range no longer reaches here.
                    //
                    // Round 77 BLOAT D3: the four container arms (List,
                    // Tuple, Map, Set) are line-for-line identical
                    // except for the literal type-name string. They
                    // collapse onto a single arm that asks
                    // `type_name_for_impl` for the canonical name.
                    // Adding a fifth container head only requires
                    // extending the match pattern below — not cloning
                    // a fourth body. The unknown-method error wording
                    // matches the pre-fix form (`unknown method 'X' on
                    // List` — without the `type` prefix used by the
                    // primitive arm above), preserving observable
                    // behavior across all four heads.
                    t @ (Type::List(_) | Type::Tuple(_) | Type::Map(_, _) | Type::Set(_)) => {
                        let type_name = self
                            .type_name_for_impl(t)
                            .expect("container head has canonical name");
                        if let Some(entry) =
                            self.tables.method_table.get(&(type_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // GAP (round 92): `t.0` parses as a FieldAccess
                        // with an all-numeric "method" name (the parser
                        // keeps a dedicated tuple-index arm), but tuple
                        // indexing is intentionally unsupported (round 69
                        // decision — docs-only). The generic "unknown
                        // method '0' on Tuple" wording misled users into
                        // thinking they typo'd a method name. Emit a
                        // targeted diagnostic pointing at destructuring
                        // instead. Numeric "methods" can only arise from
                        // tuple-index syntax, so this never shadows a real
                        // method lookup.
                        if matches!(t, Type::Tuple(_)) {
                            let field_str = resolve(field);
                            if !field_str.is_empty()
                                && field_str.bytes().all(|b| b.is_ascii_digit())
                            {
                                self.error(
                                    Code::UnsupportedOperation,
                                    format!(
                                        "tuple indexing ('t.{field_str}') is not supported; \
                                         destructure instead: 'let (a, b) = t'"
                                    ),
                                    span,
                                );
                                return Type::Error;
                            }
                        }
                        // A method of an impl the resolver rejected: reported there.
                        if self.unresolved_impl_methods.contains(&field) {
                            return Type::Error;
                        }
                        let display = resolve(type_name.name).to_string();
                        self.error_help(
                            Code::UnknownMethod,
                            format_unknown_method_message(
                                field,
                                &display,
                                &self.tables.method_table,
                                type_name,
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Rigid(r) => {
                        // An annotation variable has the methods of its
                        // bounds, and nothing else: no field, no method
                        // of a trait the declaration does not promise.
                        let trait_names: Vec<TraitKey> = self
                            .bounds
                            .get(&r.var)
                            .map(|bounds| bounds.iter().map(|(t, _)| *t).collect())
                            .unwrap_or_default();
                        let matches = self.bound_methods(*r, field);
                        if matches.len() > 1 {
                            let traits: Vec<TraitKey> = matches.iter().map(|(t, _)| *t).collect();
                            self.ambiguous_method(
                                field,
                                &format!("a value of type `{}`", r.name),
                                &traits,
                                span,
                            );
                            Type::Error
                        } else if let Some((trait_name, scheme)) = matches.first() {
                            self.last_field_access_was_method = true;
                            self.method_trait = Some(*trait_name);
                            let instantiated = self.instantiate_method(scheme, field, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        } else if self.unknown_bounds.contains(&r.var) {
                            // One of its bounds names an unknown trait
                            // (reported): the method may be that trait's.
                            Type::Error
                        } else if trait_names.is_empty() {
                            self.errors.push(
                                Diagnostic::error(
                                    Code::UnknownMethod,
                                    span,
                                    format!(
                                        "no field or method '{field}' on a value of type `{}`: \
                                         the type variable has no trait bound",
                                        r.name
                                    ),
                                )
                                .with_help(format!(
                                    "a value of type `{0}` has only the methods its bounds \
                                     promise: add `where {0}: SomeTrait`",
                                    r.name
                                )),
                            );
                            Type::Error
                        } else {
                            // Method not found on any constrained trait — error
                            let traits_str = trait_names
                                .iter()
                                .map(|s| format!("{s}"))
                                .collect::<Vec<_>>()
                                .join(" + ");
                            self.error(
                                Code::UnknownMethod,
                                format!(
                                    "no method '{field}' found in trait constraints ({traits_str})"
                                ),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Var(_) => {
                        // A method only another module's private trait
                        // provides cannot be called here, whatever the
                        // receiver turns out to be.
                        if called && let Some(trait_name) = self.only_private_provider(field) {
                            self.private_method(trait_name, field, span);
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if called {
                            // `x.m(..)`: what it calls is decided with
                            // the type of `x` (`check_call`).
                            self.unknown_receiver = Some((obj_ty.clone(), field));
                            self.fresh_var()
                        } else {
                            // `x.f`: a record with the field `f`,
                            // whatever methods traits declare.
                            let result_ty = self.fresh_var();
                            let row_ty = Type::AnonRecord {
                                fields: std::collections::BTreeMap::from([(
                                    field,
                                    result_ty.clone(),
                                )]),
                                tail: RowTail::Var(self.fresh_tyvar_id()),
                            };
                            self.unify(&obj_ty, &row_ty, span);
                            result_ty
                        }
                    }
                    Type::Error => {
                        // Prior error — propagate to prevent cascading false positives
                        Type::Error
                    }
                    _ => {
                        self.error(
                            Code::UnknownField,
                            format!(
                                "unknown field or method '{field}' on type {}",
                                self.show_type(&obj_ty)
                            ),
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
                    // An operator is its trait: the operands have one
                    // type, which owes it (`Number`, `Equal`, `Compare`).
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod | BinOp::Div => {
                        let op_str = match op {
                            BinOp::Add => "'+'",
                            BinOp::Sub => "'-'",
                            BinOp::Mul => "'*'",
                            BinOp::Mod => "'%'",
                            _ => "'/'",
                        };
                        // (`Type::Error` on a reported operand keeps an
                        // ascribed `let` from reporting the result again.)
                        match self
                            .check_operator("Number", op_str, &lt, &rt, lhs_span, rhs_span, span)
                        {
                            true => lt,
                            false => Type::Error,
                        }
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                        let (tr, op_str) = match op {
                            BinOp::Eq => ("Equal", "'=='"),
                            BinOp::Neq => ("Equal", "'!='"),
                            BinOp::Lt => ("Compare", "'<'"),
                            BinOp::Gt => ("Compare", "'>'"),
                            BinOp::Leq => ("Compare", "'<='"),
                            _ => ("Compare", "'>='"),
                        };
                        self.check_operator(tr, op_str, &lt, &rt, lhs_span, rhs_span, span);
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
                    UnaryOp::Neg => {
                        let reported = self.errors.len();
                        self.want_operand("Number", &t, "unary '-'", operand_span);
                        match self.errors.len() == reported {
                            true => t,
                            false => Type::Error,
                        }
                    }
                    UnaryOp::Not => {
                        self.unify(&t, &Type::Bool, operand_span);
                        Type::Bool
                    }
                }
            }

            // `a |> f(b)` is the call `f(a, b)`, and `a |> f` the call
            // `f(a)`.
            ExprKind::Pipe(lhs, rhs) => match &mut rhs.kind {
                ExprKind::Call(callee, call_args) => {
                    let mut args: Vec<&mut Expr> = vec![&mut **lhs];
                    args.extend(call_args.iter_mut());
                    self.check_call(callee, args, span, CallForm::Call, env)
                }
                _ => self.check_call(rhs, vec![&mut **lhs], span, CallForm::BarePipe, env),
            },

            ExprKind::Range(start, end) => {
                let start_span = start.span;
                let end_span = end.span;
                let st = self.infer_expr(start, env);
                let et = self.infer_expr(end, env);
                self.unify(&st, &Type::Int, start_span);
                self.unify(&et, &Type::Int, end_span);
                // Range(T) is a nominal alias over List(T): unifies
                // bidirectionally with List(T) (see unify in
                // src/typechecker/mod.rs), runtime rep is still Vec<Value>.
                // The nominal distinction lets `let r: Range(Int) = 1..10`
                // typecheck cleanly without silently widening annotations.
                Type::Range(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // Round 93: remember the `?` site so a later body/return
                // unify failure can point back here (see
                // `note_qmark_requirement_on_ret_mismatch`).
                self.current_qmark_spans.push(span);

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                match &inner_ty {
                    Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let err_ty = args[1].clone();
                            let fresh_ok = self.fresh_var();
                            let expected_result = Type::builtin("Result", vec![fresh_ok, err_ty]);
                            self.unify(&expected_ret, &expected_result, span);
                        } else {
                            self.error(Code::InvalidQuestion,
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let fresh_inner = self.fresh_var();
                            let expected_option = Type::option(fresh_inner);
                            self.unify(&expected_ret, &expected_option, span);
                        } else {
                            self.error(Code::InvalidQuestion,
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    // An operand already reported (or from a module that
                    // failed to load): nothing more to say.
                    Type::Error => Type::Error,
                    Type::Var(_) => {
                        // The operand's type is unknown: the `?` waits
                        // for it (`Goal::Try`).
                        let result_ty = self.fresh_var();
                        self.want_goal(
                            Goal::Try {
                                operand: inner_ty.clone(),
                                ok: result_ty.clone(),
                                ret: self.current_return_type.clone(),
                            },
                            Origin {
                                span,
                                callee: None,
                                op: None,
                            },
                        );
                        result_ty
                    }
                    _ => {
                        self.error(
                            Code::InvalidQuestion,
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
                // B2: annotation arity errors should carry the annotation's
                // own span, not a zero-span sentinel.
                let prev_type_span = self.current_type_anno_span.replace(type_expr.span);
                let declared = self.resolve_type_expr(type_expr, &mut self.sig_names.clone());
                self.current_type_anno_span = prev_type_span;
                self.unify(&inner_ty, &declared, span);
                declared
            }

            ExprKind::Call(callee, args) => {
                self.check_call(callee, args.iter_mut().collect(), span, CallForm::Call, env)
            }

            ExprKind::Lambda { params, body } => {
                env.push();
                // Soundness: lambda param lists are a single conjunctive
                // scope too — `|a, a| ...` must be rejected the same way
                // `fn f(a, a)` is.
                self.check_fn_params_duplicate_bindings(params);
                // The parameter types the place the closure stands in
                // expects (an argument of a call whose callee is known):
                // an unannotated parameter has that type before the body
                // is checked.
                let expected = self
                    .expected_closure
                    .take()
                    .filter(|expected| expected.len() == params.len());
                let param_types: Vec<Type> = params
                    .iter_mut()
                    .enumerate()
                    .map(|(i, p)| {
                        let ty = if let Some(te) = &p.ty {
                            // B2: annotation arity errors carry the
                            // annotation's own span.
                            let prev_type_span = self.current_type_anno_span.replace(te.span);
                            let resolved = self.resolve_type_expr(te, &mut self.sig_names.clone());
                            self.current_type_anno_span = prev_type_span;
                            resolved
                        } else if let Some(expected) = &expected {
                            expected[i].clone()
                        } else {
                            self.fresh_var()
                        };
                        self.check_pattern(
                            &mut p.pattern,
                            &ty,
                            env,
                            span,
                            PatternMode::Binding(BindingSite::ClosureParam),
                        );
                        ty
                    })
                    .collect();

                // BROKEN (round 93): a lambda is its own `?`/`return`
                // boundary — the VM's Op::QuestionMark pops exactly ONE
                // frame (the lambda's), so `?` inside a lambda must
                // validate against the LAMBDA's return type, not the
                // enclosing named fn's. Establish a fresh return-type
                // context for the body, mirroring check_body.
                // Without this, `{ x -> x? + 1 }` was checked against the
                // outer fn's return type and a Variant escaped into a
                // List(Int) at runtime.
                let lambda_ret = self.fresh_var();
                let prev_return_type = self.current_return_type.replace(lambda_ret.clone());
                let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

                let body_type = self.infer_expr(body, env);
                env.pop();
                let ret_unify_err_count = self.errors.len();
                self.unify(&body_type, &lambda_ret, body.span);
                self.retarget_ok_wrap_fixes(ret_unify_err_count, body);
                self.note_qmark_requirement_on_ret_mismatch(ret_unify_err_count, &lambda_ret);

                self.current_return_type = prev_return_type;
                self.current_qmark_spans = prev_qmark_spans;

                // `lambda_ret` rather than `body_type`: identical when the
                // unify above succeeded, and on failure it carries any
                // `?`-imposed Result/Option constraint, which is the type
                // the VM actually returns.
                Type::Fun(param_types, Box::new(lambda_ret))
            }

            ExprKind::RecordCreate { name, fields, .. } => {
                let name = *name;
                // GAP (round 35 F4): duplicate fields in a record literal
                // (e.g. `User { name: "a", name: "b" }`) used to slip past
                // the typechecker because downstream processing went through
                // a HashSet that silently deduped them. Mirror the record
                // type declaration's duplicate-field check: walk once and
                // emit a diagnostic per duplicate.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
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
                if let Some((rec_ty, rec_info, param_ids)) = looked {
                    // For parameterized record types, create fresh type variables
                    // for each type parameter and substitute them into field types.
                    // This prevents different instantiations from sharing the same
                    // template variables (e.g., Box { value: 42 } and Box { value: "hi" }).
                    let (instantiated_fields, type_args) =
                        self.instantiate_record_fields(&rec_info, param_ids.as_deref());

                    let field_types: Vec<(Symbol, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (*n, ty)
                        })
                        .collect();

                    // Unify with declared field types (using instantiated copies)
                    for (field_name, inferred_ty) in &field_types {
                        if let Some((_, declared_ty)) =
                            instantiated_fields.iter().find(|(n, _)| n == field_name)
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
                            // GAP (round 26 L5): append a did-you-mean
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
                    // G2: Unknown record type (reported by `named_record`
                    // or the resolver) — this used to silently synthesize
                    // an anonymous record. We still walk the field
                    // expressions so nested errors are reported, but return
                    // Type::Error to prevent downstream cascades.
                    for (_, e) in fields.iter_mut() {
                        let _ = self.infer_expr(e, env);
                    }
                    Type::Error
                }
            }

            ExprKind::RecordUpdate { expr: base, fields } => {
                let base_span = base.span;
                let base_ty = self.infer_expr(base, env);
                let resolved = self.apply(&base_ty);
                // GAP (round 35 F4): duplicate fields in a record-update
                // expression (e.g. `r.{ age: 1, age: 2 }`) used to be
                // silently deduped downstream. Emit a diagnostic per
                // duplicate so the typo is surfaced at compile time.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
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
                // Three cases:
                //  1. Concrete `Type::Record(name, fields)` — validate directly.
                //  2. `Type::Generic(name, args)` resolving to a declared
                //     record (happens when the base is a param annotated
                //     with a user-defined record type). BROKEN-1.
                //  3. Anything else — compile-time reject. BROKEN-2.
                let mut handled = false;
                // Anon record (row-poly) update: validate each field
                // against the row's known fields when present; for open
                // rows, missing fields are added to the row variable.
                if let Type::AnonRecord {
                    fields: af,
                    tail: at,
                } = &resolved
                {
                    let af = af.clone();
                    let at = at.clone();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = af.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else if let RowTail::Var(_) = at {
                            // Open row: extend by unifying with a
                            // record carrying this new field.
                            use std::collections::BTreeMap;
                            let new_row = self.fresh_tyvar_id();
                            let mut new_fields = BTreeMap::new();
                            new_fields.insert(*field_name, ft.clone());
                            let extended = Type::AnonRecord {
                                fields: new_fields,
                                tail: RowTail::Var(new_row),
                            };
                            self.unify(&resolved, &extended, span);
                        } else {
                            // ERR-GAP (round 81 F3): mirror the sibling
                            // Record / Generic record-update sites and
                            // append a did-you-mean hint when the typo
                            // is close to one of the closed row's fields.
                            let candidates: Vec<(Symbol, Type)> =
                                af.iter().map(|(k, v)| (*k, v.clone())).collect();
                            let base =
                                format!("unknown field '{field_name}' in closed anon record");
                            self.error_help(
                                Code::UnknownField,
                                format_record_field_suggestion(base, *field_name, &candidates),
                                span,
                            );
                        }
                    }
                    handled = true;
                }
                if let Type::Generic(type_name, type_args) = &resolved
                    && let Some(rec_info) = self.tables.records.get(type_name).cloned()
                {
                    let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                        self.tables.record_param_var_ids.get(type_name).cloned()
                    {
                        let mapping: HashMap<TyVar, Type> =
                            if type_args.len() == param_var_ids.len() {
                                param_var_ids
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(&v, t)| (v, t.clone()))
                                    .collect()
                            } else {
                                param_var_ids
                                    .iter()
                                    .map(|&v| (v, self.fresh_var()))
                                    .collect()
                            };
                        rec_info
                            .fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect()
                    } else {
                        rec_info.fields.clone()
                    };
                    let declared: std::collections::HashMap<Symbol, Type> = instantiated_fields
                        .iter()
                        .map(|(n, t)| (*n, t.clone()))
                        .collect();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = declared.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else {
                            // GAP (round 26 L5): did-you-mean on the
                            // generic-record update path.
                            let base = format!("unknown field '{field_name}' in {type_name}");
                            self.error_help(
                                Code::UnknownField,
                                format_record_field_suggestion(
                                    base,
                                    *field_name,
                                    &instantiated_fields,
                                ),
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
                                },
                            );
                        }
                    }
                    if !matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                        self.error(Code::TypeMismatch,
                            format!(
                                "record update requires a record base, but '{resolved}' is not a record type"
                            ),
                            base_span,
                        );
                    }
                }
                base_ty
            }

            ExprKind::AnonRecord { spread, fields } => {
                use std::collections::BTreeMap;
                // Phase F: Extend operator. With a spread, infer
                // base's type, then merge the listed `fields` on top.
                // Reject duplicate field names in the literal portion.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
                    for (fn_name, _) in fields.iter() {
                        if !seen.insert(*fn_name) {
                            self.error(
                                Code::DuplicateRecordField,
                                format!("duplicate field '{}' in anon record literal", fn_name),
                                span,
                            );
                        }
                    }
                }
                // Type each new field expression.
                let new_field_tys: Vec<(Symbol, Type)> = fields
                    .iter_mut()
                    .map(|(n, e)| (*n, self.infer_expr(e, env)))
                    .collect();
                let mut field_map: BTreeMap<Symbol, Type> = BTreeMap::new();
                for (n, t) in &new_field_tys {
                    field_map.insert(*n, t.clone());
                }
                if let Some(base_expr) = spread {
                    let base_ty = self.infer_expr(base_expr, env);
                    let base_ty = self.apply(&base_ty);
                    let base_canon =
                        crate::types::canonical::canonicalize(&self.tables.resolver, &base_ty);
                    // The base's known fields and its row.
                    let (base_fields, base_tail): (BTreeMap<Symbol, Type>, RowTail) =
                        match &base_canon {
                            Type::AnonRecord { fields, tail } => (fields.clone(), tail.clone()),
                            // A base of unknown type is a record with a
                            // row of its own.
                            Type::Var(_) => {
                                let row = self.fresh_tyvar_id();
                                let open = Type::AnonRecord {
                                    fields: BTreeMap::new(),
                                    tail: RowTail::Var(row),
                                };
                                self.unify(&base_ty, &open, base_expr.span);
                                (BTreeMap::new(), RowTail::Var(row))
                            }
                            // A spread over a value of a declared record
                            // type is the conversion to an anonymous
                            // record, written out: the result has the
                            // record's fields, and each field written
                            // after the spread is added or replaces the
                            // record's by name.
                            Type::Generic(name, args) if self.tables.records.contains_key(name) => {
                                let mut merged: BTreeMap<Symbol, Type> = self
                                    .instantiate_record_fields_with_args(*name, args)
                                    .into_iter()
                                    .collect();
                                for (n, t) in &new_field_tys {
                                    merged.insert(*n, t.clone());
                                }
                                let ty = Type::AnonRecord {
                                    fields: merged,
                                    tail: RowTail::Closed,
                                };
                                expr.ty = Some(ty.clone());
                                return ty;
                            }
                            _ => {
                                if !matches!(base_canon, Type::Error | Type::Never) {
                                    self.error(Code::TypeMismatch,
                                        format!(
                                            "spread requires a record base, but '{base_canon}' is not a record type"
                                        ),
                                        base_expr.span,
                                    );
                                }
                                expr.ty = Some(Type::Error);
                                return Type::Error;
                            }
                        };
                    // A record is extended, never overwritten: a field
                    // the base is known to have is an error here, and
                    // one its row may turn out to have is checked when
                    // the row is known (`Goal::Lacks`).
                    for (n, _) in &new_field_tys {
                        if base_fields.contains_key(n) {
                            self.error(Code::DuplicateRecordField,
                                format!(
                                    "cannot extend record with existing field '{n}'; v1 row polymorphism does not support override"
                                ),
                                span,
                            );
                        } else if let RowTail::Var(row) = &base_tail {
                            self.want_goal(
                                Goal::Lacks {
                                    row: Type::Var(*row),
                                    field: *n,
                                },
                                Origin {
                                    span,
                                    callee: None,
                                    op: None,
                                },
                            );
                        }
                    }
                    let mut merged: BTreeMap<Symbol, Type> = base_fields.clone();
                    for (n, t) in &new_field_tys {
                        merged.insert(*n, t.clone());
                    }
                    Type::AnonRecord {
                        fields: merged,
                        tail: base_tail,
                    }
                } else {
                    // Closed anon record literal.
                    Type::AnonRecord {
                        fields: field_map,
                        tail: RowTail::Closed,
                    }
                }
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

                        // GAP (round 93): every arm checks its pattern
                        // against the SAME scrutinee, so a wrong-scrutinee
                        // match (`match 42 { Ok(v) -> ..., Err(e) -> ... }`)
                        // used to print the identical "expected Result(_, _),
                        // got Int" once per arm, plus a non-exhaustive
                        // cascade. Dedup identical (message, span, severity)
                        // diagnostics across the arm loop — genuinely
                        // distinct per-arm errors still each report.
                        let mut seen_arm_diags: std::collections::HashSet<(
                            std::string::String,
                            crate::source::FileId,
                            u32,
                            bool,
                        )> = std::collections::HashSet::new();
                        let mut any_pattern_mismatch = false;
                        // What each arm's pattern tests, for the
                        // exhaustiveness check.
                        let mut arm_pats = Vec::with_capacity(arms.len());
                        // A match diverges when it has arms and every
                        // arm diverges. `unify` leaves `result_ty`
                        // unbound against `Never`, so the arms are
                        // tracked here to type such a match `Never`.
                        let mut every_arm_diverges = !arms.is_empty();
                        for arm in arms.iter_mut() {
                            env.push();
                            // Soundness: `match e { (x, x) -> x }` used to
                            // typecheck silently, binding the second `x` on
                            // top of the first. Reject duplicate binders
                            // in the arm pattern before check_pattern walks
                            // it and defines them in the arm's frame.
                            self.check_pattern_duplicate_bindings(&arm.pattern);
                            // A name in the pattern the resolver reported:
                            // what the arm covers is not known.
                            if names_unresolved(&arm.pattern) {
                                any_pattern_mismatch = true;
                            }
                            let pat_err_count = self.errors.len();
                            arm_pats.push(self.check_pattern(
                                &mut arm.pattern,
                                &scrutinee_ty,
                                env,
                                scrutinee_span,
                                PatternMode::Arm,
                            ));
                            if self.errors.len() > pat_err_count {
                                let new_diags: Vec<Diagnostic> =
                                    self.errors.drain(pat_err_count..).collect();
                                for d in new_diags {
                                    if matches!(d.severity, Severity::Error) {
                                        any_pattern_mismatch = true;
                                    }
                                    let key = (
                                        d.message.clone(),
                                        d.span.file,
                                        d.span.start,
                                        matches!(d.severity, Severity::Error),
                                    );
                                    if seen_arm_diags.insert(key) {
                                        // Re-inserting a pre-built Diagnostic drained
                                        // above for dedup — not a new emission site,
                                        // so `self.error(...)` does not apply. This
                                        // is the sanctioned exception counted by the
                                        // round-83 STYLE lock (threshold 1).
                                        self.errors.push(d);
                                    }
                                }
                            }

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, env);
                            env.pop();
                            every_arm_diverges &= matches!(arm_ty, Type::Never);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        // Skipped when the scrutinee type already failed to
                        // match the arms — exhaustiveness over a broken match
                        // is a cascade, not new information (round 93).
                        // So is a scrutinee of a type already reported.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        if !any_pattern_mismatch && resolved_scrutinee_ty != Type::Error {
                            self.check_exhaustiveness(
                                arms,
                                &arm_pats,
                                &resolved_scrutinee_ty,
                                scrutinee_span,
                            );
                        }

                        if every_arm_diverges {
                            Type::Never
                        } else {
                            result_ty
                        }
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean
                        // condition. When no condition holds there is no
                        // value, so the last arm must be the `_` default,
                        // and there is exactly one: arms after a `_` never
                        // run.
                        let defaults: Vec<usize> = arms
                            .iter()
                            .enumerate()
                            .filter(|(_, arm)| arm.guard.is_none())
                            .map(|(i, _)| i)
                            .collect();
                        match defaults.first() {
                            None => self.error(
                                Code::NonExhaustive,
                                "a `match` without a scrutinee must end with a `_ -> ...` arm \
                                 for when no condition is true"
                                    .to_string(),
                                span,
                            ),
                            Some(&first) => {
                                if arms[first + 1..].iter().any(|arm| arm.guard.is_some()) {
                                    self.error(
                                        Code::UnreachablePattern,
                                        "the `_` arm must be last: it always matches, so the \
                                         arms after it never run"
                                            .to_string(),
                                        arms[first].pattern.span,
                                    );
                                }
                                for &i in &defaults[1..] {
                                    self.error(
                                        Code::UnreachablePattern,
                                        "unreachable `_` arm: an earlier `_` arm always matches"
                                            .to_string(),
                                        arms[i].pattern.span,
                                    );
                                }
                            }
                        }
                        let result_ty = self.fresh_var();
                        // Same rule as the scrutinee form: arms that all
                        // diverge make the match diverge.
                        let mut every_arm_diverges = !arms.is_empty();

                        for arm in arms.iter_mut() {
                            env.push();

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, env);
                            env.pop();
                            every_arm_diverges &= matches!(arm_ty, Type::Never);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        if every_arm_diverges {
                            Type::Never
                        } else {
                            result_ty
                        }
                    }
                }
            }

            ExprKind::Return(maybe_expr) => {
                let ret_val_ty = if let Some(e) = maybe_expr {
                    self.infer_expr(e, env)
                } else {
                    Type::Unit
                };
                if let Some(expected_ret) = self.current_return_type.clone() {
                    self.unify(&ret_val_ty, &expected_ret, span);
                }
                Type::Never
            }

            ExprKind::Block(stmts) => {
                let mut last_ty = Type::Unit;
                env.push();
                for stmt in stmts {
                    last_ty = self.infer_stmt(stmt, env);
                }
                env.pop();

                last_ty
            }

            ExprKind::Loop { bindings, body } => {
                // Each initialiser is checked outside the loop's frame: it
                // does not see the bindings.
                let mut binding_types = Vec::new();
                for (_, _, value) in bindings.iter_mut() {
                    binding_types.push(self.infer_expr(value, env));
                }
                env.push();
                for ((name, _, _), ty) in bindings.iter().zip(&binding_types) {
                    env.define(*name, Scheme::mono(ty.clone()));
                }
                self.check_recur_tail_positions(body, RecurPos::Tail);
                let prev_loop = self.loop_binding_types.take();
                self.loop_binding_types = Some(binding_types);
                let result = self.infer_expr(body, env);
                env.pop();
                self.loop_binding_types = prev_loop;
                result
            }

            ExprKind::Recur(args) => {
                let recur_count = args.len();
                let arg_types: Vec<Type> = args
                    .iter_mut()
                    .map(|arg| self.infer_expr(arg, env))
                    .collect();
                if let Some(binding_types) = self.loop_binding_types.clone() {
                    if recur_count != binding_types.len() {
                        let bindings_n = binding_types.len();
                        self.error(
                            Code::ArityMismatch,
                            format!(
                                "loop has {} {}, but `loop(...)` supplies {} {}",
                                bindings_n,
                                plural(bindings_n, "binding", "bindings"),
                                recur_count,
                                plural(recur_count, "argument", "arguments")
                            ),
                            span,
                        );
                    }
                    // Unify each recur arg with its corresponding loop binding type
                    for (i, arg_ty) in arg_types.iter().enumerate() {
                        if let Some(binding_ty) = binding_types.get(i) {
                            self.unify(arg_ty, binding_ty, span);
                        }
                    }
                } else {
                    self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` can only appear inside a `loop` body — it restarts \
                         the enclosing loop with new binding values"
                            .to_string(),
                        span,
                    );
                }
                // `loop(...)` jumps back to the top of the loop and never
                // produces a value here.
                Type::Never
            }
        };
        let resolved = self.apply(&ty);
        expr.ty = Some(resolved.clone());
        resolved
    }

    /// Reject a `loop(...)` call that is not in tail position of its
    /// loop. `loop(...)` restarts the loop, so nothing may use its result:
    /// it must be the last expression of the loop body, or of a block,
    /// match arm or `when ... else` body that is itself in tail position.
    /// `pos` says where `expr` stands. A nested `loop` starts its own
    /// tail context, checked when that loop is inferred, so only its
    /// binding initialisers are walked here.
    fn check_recur_tail_positions(&mut self, expr: &Expr, pos: RecurPos) {
        let inner = pos.inner();
        match &expr.kind {
            ExprKind::Recur(args) => {
                match pos {
                    RecurPos::Tail => {}
                    RecurPos::NotTail => self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` must be in tail position: it restarts the loop, so \
                         nothing can use its result — make it the last expression of \
                         the loop body, or of a block, match arm or `when ... else` body \
                         in tail position"
                            .to_string(),
                        expr.span,
                    ),
                    RecurPos::InClosure => self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` inside a closure is not the loop's tail: the closure \
                         runs when it is called, not as the loop body, so it cannot \
                         restart the loop — return a value from the closure and call \
                         `loop(...)` in the loop body"
                            .to_string(),
                        expr.span,
                    ),
                }
                for arg in args {
                    self.check_recur_tail_positions(arg, inner);
                }
            }
            ExprKind::Loop { bindings, .. } => {
                for (_, _, value) in bindings {
                    self.check_recur_tail_positions(value, inner);
                }
            }
            ExprKind::Block(stmts) => {
                let last = stmts.len().saturating_sub(1);
                for (i, stmt) in stmts.iter().enumerate() {
                    match stmt {
                        Stmt::Let { value, .. } => self.check_recur_tail_positions(value, inner),
                        Stmt::When {
                            expr: value,
                            else_body,
                            ..
                        } => {
                            self.check_recur_tail_positions(value, inner);
                            self.check_recur_tail_positions(else_body, pos);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            self.check_recur_tail_positions(condition, inner);
                            self.check_recur_tail_positions(else_body, pos);
                        }
                        Stmt::Expr(e) => {
                            self.check_recur_tail_positions(e, if i == last { pos } else { inner })
                        }
                    }
                }
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(scrutinee) = scrutinee {
                    self.check_recur_tail_positions(scrutinee, inner);
                }
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.check_recur_tail_positions(guard, inner);
                    }
                    self.check_recur_tail_positions(&arm.body, pos);
                }
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::StringLit(..)
            | ExprKind::Ident(_)
            | ExprKind::Unit
            | ExprKind::Return(None) => {}
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.check_recur_tail_positions(e, inner);
                    }
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            self.check_recur_tail_positions(e, inner)
                        }
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.check_recur_tail_positions(k, inner);
                    self.check_recur_tail_positions(v, inner);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
                for e in elems {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::FieldAccess(e, _, _)
            | ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => self.check_recur_tail_positions(e, inner),
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                self.check_recur_tail_positions(l, inner);
                self.check_recur_tail_positions(r, inner);
            }
            ExprKind::Call(callee, args) => {
                self.check_recur_tail_positions(callee, inner);
                for arg in args {
                    self.check_recur_tail_positions(arg, inner);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.check_recur_tail_positions(body, RecurPos::InClosure)
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::RecordUpdate { expr: base, fields } => {
                self.check_recur_tail_positions(base, inner);
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(base) = spread {
                    self.check_recur_tail_positions(base, inner);
                }
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
        }
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                // A `let` generalises only a syntactic value: its value is
                // then checked one level deeper, and the variables left
                // at that level are the ones to quantify.
                let is_value = self.is_syntactic_value(value);
                if is_value {
                    self.enter_level();
                }
                let mut val_ty = self.infer_expr(value, env);

                // The type variables the annotation introduces.
                let mut own: Vec<RigidId> = Vec::new();
                if let Some(te) = &ty {
                    let (declared, introduced) = self.resolve_let_annotation(te);
                    own = introduced;
                    self.unify(&val_ty, &declared, value_span);
                    // A value of unknown type (from a module that failed
                    // to load) takes the declared type: `let y: Int = x`
                    // makes `y` an Int.
                    if matches!(self.apply(&val_ty), Type::Error) {
                        val_ty = declared;
                    }
                }

                // A call may return a type with shared mutable state (a
                // channel), which must stay monomorphic so that the
                // element type is shared across all uses.
                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        let scheme = if is_value {
                            self.exit_level();
                            self.generalize_let(&val_ty, &own)
                        } else {
                            Scheme::mono(self.apply(&val_ty))
                        };
                        env.define(*name, scheme);
                    }
                    _ => {
                        // Soundness: reject duplicate binding names within
                        // the let pattern. `let (a, a) = (1, 2)` used to
                        // silently shadow the first `a`.
                        self.check_pattern_duplicate_bindings(pattern);
                        // A `let` has no failure branch: the pattern
                        // must match every value of the bound type.
                        self.check_pattern(
                            pattern,
                            &val_ty,
                            env,
                            value_span,
                            PatternMode::Binding(BindingSite::Let),
                        );
                        // Each name the pattern binds to a part of a value
                        // is general as the part is.
                        if is_value {
                            self.exit_level();
                            for name in collect_pattern_vars(pattern) {
                                if let Some(bound) = env.lookup(name).cloned() {
                                    let scheme = self.generalize_let(&bound.ty, &own);
                                    env.define(name, scheme);
                                }
                            }
                        }
                    }
                }

                Type::Unit
            }

            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                let expr_span = expr.span;
                let expr_ty = self.infer_expr(expr, env);

                // Type check the else body — it must diverge (return / panic)
                let else_ty = self.infer_expr(else_body, env);
                let resolved_else = self.apply(&else_ty);
                if !matches!(resolved_else, Type::Never | Type::Error) {
                    self.error(
                        Code::InvalidControlFlow,
                        "'when let' else body must diverge — end it with 'return', 'panic' or, \
                         inside a loop, 'loop(...)'"
                            .to_string(),
                        else_body.span,
                    );
                }

                // Bind the pattern in the current scope (type narrowing).
                //
                // Soundness: reject duplicate binders before defining so
                // `when let (a, a) = expr` doesn't silently shadow.
                self.check_pattern_duplicate_bindings(pattern);
                self.check_pattern(pattern, &expr_ty, env, expr_span, PatternMode::Arm);

                Type::Unit
            }

            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                let cond_ty = self.infer_expr(condition, env);
                self.unify(&cond_ty, &Type::Bool, condition.span);

                // Type check the else body — it must diverge (return / panic)
                let else_ty = self.infer_expr(else_body, env);
                let resolved_else = self.apply(&else_ty);
                if !matches!(resolved_else, Type::Never | Type::Error) {
                    self.error(
                        Code::InvalidControlFlow,
                        "'when' else body must diverge — end it with 'return', 'panic' or, \
                         inside a loop, 'loop(...)'"
                            .to_string(),
                        else_body.span,
                    );
                }

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    /// LATENT (round 88): walk a supertrait-arg `TypeExpr` and emit a
    /// diagnostic at every `Named(sym)` that refers to a *parametric*
    /// user-defined type (enum, record, or alias with arity > 0) used
    /// without its arguments — e.g. `trait Sub: Super(Box)` where
    /// `type Box(a) { ... }`. `resolve_supertrait_arg` falls through
    /// such names to `Type::Generic(sym, [])`, which then never matches
    /// any downstream impl's actual args, so the obligation silently
    /// goes unsatisfied. With this check the user sees a clean arity
    /// diagnostic at the supertrait declaration's span instead.
    ///
    /// Names that map to a trait param (`info.params`), builtin
    /// primitives, or any name we don't recognise are left to the
    /// existing handling — the latter are diagnosed elsewhere as
    /// "unknown type" at the type-expr resolve site.
    ///
    /// Skips emission when the same message+span is already in
    /// `self.errors` so multiple constrained fn-body passes (one per
    /// fn whose param is bound on the trait) don't pile on duplicates.
    pub(super) fn check_supertrait_arg_parametric_arity(
        &mut self,
        te: &TypeExpr,
        trait_info: &TraitInfo,
    ) {
        match &te.kind {
            TypeExprKind::Named { name: sym, .. } => {
                // Trait params are substituted, not type references.
                if trait_info.params.iter().any(|p| p == sym) {
                    return;
                }
                // Unknown name — leave to other diagnostics.
                let Some(ty) = self.named_type(te.res, *sym) else {
                    return;
                };
                let arity = if let Some(info) = self.tables.enums.get(&ty) {
                    info.params.len()
                } else if let Some(ids) = self.tables.record_param_var_ids.get(&ty) {
                    ids.len()
                } else if let Some(info) = self.tables.resolver.lookup_alias(ty) {
                    info.params.len()
                } else {
                    // Builtins like Int/Float/String are 0-arity by design.
                    return;
                };
                if arity == 0 {
                    return;
                }
                let msg = format!(
                    "type '{}' expects {} {}, got 0 in supertrait bound",
                    resolve(*sym),
                    arity,
                    plural(arity, "type argument", "type arguments"),
                );
                let already = self
                    .errors
                    .iter()
                    .any(|e| e.message == msg && e.span == te.span);
                if !already {
                    self.error(Code::ArityMismatch, msg, te.span);
                }
            }
            TypeExprKind::Generic { args, .. } => {
                for a in args {
                    self.check_supertrait_arg_parametric_arity(a, trait_info);
                }
            }
            TypeExprKind::Tuple(elems) => {
                for e in elems {
                    self.check_supertrait_arg_parametric_arity(e, trait_info);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for p in params {
                    self.check_supertrait_arg_parametric_arity(p, trait_info);
                }
                self.check_supertrait_arg_parametric_arity(ret, trait_info);
            }
            TypeExprKind::SelfType
            | TypeExprKind::AssocProj { .. }
            | TypeExprKind::AnonRecord { .. } => {
                // These either aren't meaningful as supertrait args
                // (handled elsewhere) or carry no bare-name parametric
                // reference at the top level. Nothing to validate here.
            }
        }
    }

    /// Check `l op r`: the operands have one type, and it has the
    /// operator's trait `tr`. `false` if an error is reported for the
    /// operands' types.
    #[allow(clippy::too_many_arguments)]
    fn check_operator(
        &mut self,
        tr: &'static str,
        op: &'static str,
        lt: &Type,
        rt: &Type,
        lhs_span: Span,
        rhs_span: Span,
        span: Span,
    ) -> bool {
        // The left operand is read first and says what the right one is
        // expected to be.
        if let Err(mismatch) = self.unify_types(rt, lt) {
            // When exactly one operand is of a type the operator is not
            // for, that operand is what is wrong, whichever side it is
            // on (`true + 1`): the operator's message, at it.
            let resolved_l = self.apply(lt);
            let resolved_r = self.apply(rt);
            let l_bad = !self.operand_fits(tr, &resolved_l);
            let r_bad = !self.operand_fits(tr, &resolved_r);
            if l_bad != r_bad {
                let (offender, other, offender_span) = if l_bad {
                    (&resolved_l, &resolved_r, lhs_span)
                } else {
                    (&resolved_r, &resolved_l, rhs_span)
                };
                let mut d = Diagnostic::error(
                    Code::UnsupportedOperation,
                    offender_span,
                    operator_message(tr, op, offender),
                );
                d.help.extend(Self::chain_hint(offender, other));
                self.errors.push(d);
            } else {
                self.report_mismatch(mismatch, rhs_span);
            }
            return false;
        }
        let errors = self.errors.len();
        self.want_operand(tr, lt, op, span);
        self.errors.len() == errors
    }

    /// Whether a value of the type `ty` can be an operand of an
    /// operator of the builtin trait `tr`, as far as the type itself
    /// says (its parts are the judgement's to check).
    fn operand_fits(&self, tr: &str, ty: &Type) -> bool {
        let tr = TraitKey::builtin(tr);
        let Some(head) = self.type_name_for_impl(ty) else {
            return true;
        };
        if self.by_structure(tr, head) {
            let mut walk = super::solve::Walk::default();
            return self
                .structure_gap(tr, ty, &mut walk, 0)
                .is_none_or(|gap| !gap.is_whole());
        }
        matches!(ty, Type::Error | Type::Never | Type::AssocProj { .. })
            || self.tables.trait_impl_set.contains(&(tr, head))
    }
}

/// What is said of an operand of the type `ty` that the operator `op`,
/// of the builtin trait `tr`, is not for.
pub(super) fn operator_message(tr: &str, op: &str, ty: &Type) -> String {
    match tr {
        "Number" => arith_operand_message(op, ty),
        "Equal" => format!("operator {op} requires a comparable type, got '{ty}'"),
        _ => format!(
            "operator {op} requires Int, Float, String, Bool, List, Tuple, Record, or Variant, got '{ty}'"
        ),
    }
}

/// The type a type expression written in a trait declaration names: its
/// resolution (a type's id is its definition's), else the builtin type
/// of that name.
fn written_type(te: &TypeExpr, name: Symbol) -> Option<TypeRef> {
    match te.res {
        Some(crate::defs::Res::Def(id)) => Some(TypeRef {
            id: crate::defs::TypeId(id),
            name,
        }),
        Some(_) => None,
        None => {
            let name_str = resolve(name);
            let name_str = if name_str == "()" {
                "Unit"
            } else {
                name_str.as_str()
            };
            crate::defs::builtin_type_id(name_str).map(|id| TypeRef { id, name })
        }
    }
}

/// Resolve a supertrait reference's TypeExpr argument against the
/// enclosing trait's params. `Named("a")` where `"a"` is in
/// `trait_info.params` maps to the corresponding entry in `base_args`
/// (the enclosing trait's supplied args at the call site). Nested forms
/// (`Generic`, `Tuple`) recurse. Unmapped names and concrete primitives
/// fall through to their `Type::…` counterparts.
///
/// INVARIANT (round 101): the shapes produced here must be
/// CANONICAL — identical to what `resolve_type_expr` builds for the
/// same surface syntax on the impl side — because
/// `trait_arg_compatible_canon` compares them structurally and
/// `canonicalize()` does not collapse `Generic("List", …)` onto
/// `Type::List` (etc.). Non-canonical output makes every builtin
/// container / Unit supertrait arg spuriously incompatible
/// with its own impl.
pub(super) fn resolve_supertrait_arg(
    te: &TypeExpr,
    trait_info: &TraitInfo,
    base_args: &[Type],
) -> Type {
    match &te.kind {
        TypeExprKind::Named { name: sym, .. } => {
            if let Some(idx) = trait_info.params.iter().position(|p| p == sym)
                && let Some(ty) = base_args.get(idx)
            {
                return ty.clone();
            }
            // Bare type name that isn't a trait param — interpret as a
            // concrete type reference (Int, String, or user type).
            // Round 101: mirror `resolve_type_expr` (mod.rs Named arm)
            // exactly for primitives — Unit/() previously
            // fell through to `Type::Generic`, which never compares
            // equal to the canonical `Type::Unit` the
            // impl side produces, spuriously failing
            // `trait_arg_compatible_canon` with identical Display
            // strings on both sides of the error.
            let Some(ty) = written_type(te, *sym) else {
                return Type::Error;
            };
            match builtin_type_name(ty) {
                Some("Int") => Type::Int,
                Some("Float") => Type::Float,
                Some("Bool") => Type::Bool,
                Some("String") => Type::String,
                Some("Unit") => Type::Unit,
                _ => Type::Generic(ty, Vec::new()),
            }
        }
        TypeExprKind::Generic {
            name: sym, args, ..
        } => {
            let mut resolved: Vec<Type> = args
                .iter()
                .map(|a| resolve_supertrait_arg(a, trait_info, base_args))
                .collect();
            // Round 101: builtin container heads must produce the same
            // canonical `Type::…` shapes `resolve_type_expr` builds for
            // the impl side (mod.rs Generic arm). `canonicalize()` does
            // NOT collapse `Generic("List",[T])` onto `Type::List(T)`,
            // so leaving these as `Type::Generic` made supertrait args
            // like `List(b)` incomparable with the registered impl's
            // `Type::List(Int)`. Wrong-arity forms fall through to
            // `Type::Generic` (they can't match any canonical impl
            // shape, and resolve_type_expr diagnoses the arity at the
            // declaration site).
            let Some(ty) = written_type(te, *sym) else {
                return Type::Error;
            };
            match builtin_type_name(ty).unwrap_or_default() {
                "List" if resolved.len() == 1 => Type::List(Box::new(resolved.pop().unwrap())),
                "Range" if resolved.len() == 1 => Type::Range(Box::new(resolved.pop().unwrap())),
                "Set" if resolved.len() == 1 => Type::Set(Box::new(resolved.pop().unwrap())),
                "Channel" if resolved.len() == 1 => {
                    Type::Channel(Box::new(resolved.pop().unwrap()))
                }
                "Map" if resolved.len() == 2 => {
                    let v = resolved.pop().unwrap();
                    let k = resolved.pop().unwrap();
                    Type::Map(Box::new(k), Box::new(v))
                }
                _ => Type::Generic(ty, resolved),
            }
        }
        TypeExprKind::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| resolve_supertrait_arg(e, trait_info, base_args))
                .collect(),
        ),
        TypeExprKind::Function(params, ret) => Type::Fun(
            params
                .iter()
                .map(|p| resolve_supertrait_arg(p, trait_info, base_args))
                .collect(),
            Box::new(resolve_supertrait_arg(ret, trait_info, base_args)),
        ),
        TypeExprKind::SelfType => Type::Error, // Self isn't meaningful in a supertrait arg
        TypeExprKind::AssocProj { .. } => {
            // Associated-type projections aren't meaningful as
            // supertrait args either: a supertrait arg is a fixed
            // type expression substituted from the enclosing trait's
            // params. Returning `Type::Error` matches the SelfType
            // arm and lets the caller continue without a cascade.
            Type::Error
        }
        TypeExprKind::AnonRecord { .. } => {
            // Anon records aren't meaningful as supertrait args either —
            // supertrait params are referred to by name.
            Type::Error
        }
    }
}

/// The operand-domain diagnostic for an arithmetic operator (`op_str` is
/// the quoted operator, e.g. `'+'`). A String operand of `+` also names
/// the way to build strings: interpolation.
pub(super) fn arith_operand_message(op_str: &str, ty: &Type) -> String {
    let msg = match op_str.starts_with("unary") {
        true => format!("{op_str} requires Int or Float, got '{ty}'"),
        false => format!("operator {op_str} requires Int or Float, got '{ty}'"),
    };
    if op_str == "'+'" && matches!(ty, Type::String) {
        format!("{msg}; build strings with interpolation, e.g. \"{{a}}{{b}}\"")
    } else {
        msg
    }
}

impl TypeChecker {
    /// Whether an expression is a syntactic value, for the value
    /// restriction on let-generalization: a literal, a name, a closure,
    /// or a tuple, list, record or constructor application of syntactic
    /// values. Those are safe to generalize; any other call is not,
    /// because it may produce a type with shared mutable state (a
    /// channel) that must remain monomorphic.
    pub(super) fn is_syntactic_value(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::StringLit(..)
            | ExprKind::Unit
            | ExprKind::Ident(_)
            | ExprKind::Lambda { .. } => true,
            // A name through its module or its enum: `m.f`, `Color.Red`.
            ExprKind::FieldAccess(..) => matches!(expr.res, Some(crate::defs::Res::Def(_))),
            ExprKind::Tuple(elems) => elems.iter().all(|e| self.is_syntactic_value(e)),
            ExprKind::List(elems) => elems.iter().all(|e| match e {
                ListElem::Single(expr) => self.is_syntactic_value(expr),
                ListElem::Spread(_) => false,
            }),
            ExprKind::RecordCreate { fields, .. } => {
                fields.iter().all(|(_, e)| self.is_syntactic_value(e))
            }
            ExprKind::AnonRecord {
                spread: None,
                fields,
            } => fields.iter().all(|(_, e)| self.is_syntactic_value(e)),
            // A variant applied to values: `Some(1)`, `Err("x")`. (The
            // builtin environment, which the builtin definitions are
            // made from, cannot ask what a name resolves to.)
            ExprKind::Call(callee, args) => {
                self.defs.is_some()
                    && self.res_variant_enum(callee.res).is_some()
                    && args.iter().all(|e| self.is_syntactic_value(e))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
