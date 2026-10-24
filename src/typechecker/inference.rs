//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! and check_body.

use super::infer::pattern::*;
use super::names::is_placeholder;
use super::suggest::suggest_similar;
use super::*;
use crate::parser::{NO_IF, NO_IF_HELP};

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

/// Pick the singular or plural form of a word
/// based on `n`. Used to render arity/field/binding counts in
/// typechecker diagnostics without an awkward "1 argument(s)".
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

/// The call to write for a method without `self` where only a type
/// variable is at hand: on a `type` parameter of that variable.
/// How a program does what the method `method` of the builtin trait
/// `tr` would, where that method is not called (`display`, `equal`):
/// the help of an error at a call of it. `None` for any other method.
pub(super) fn uncallable_builtin(tr: TraitKey, method: Symbol, x: &str) -> Option<String> {
    let builtin = crate::defs::builtin_trait_id(&resolve(tr.name)) == Some(tr.id);
    match (builtin, resolve(tr.name).as_str(), resolve(method).as_str()) {
        (true, "Display", "display") => Some(format!(
            "show a value with interpolation, `\"{{{x}}}\"`, or with `string.from({x})`"
        )),
        (true, "Equal", "equal") => Some(format!("compare with `==`: `{x} == y`")),
        _ => None,
    }
}

/// The call of the method `method`, of the type `method_ty`, on the type
/// `on`, as a message writes it: `` `Int.empty()` ``, or with `..` for
/// the method's parameters, `` `Int.make(..)` ``.
pub(super) fn on_type_call(on: &str, method: Symbol, method_ty: &Type) -> String {
    let args = match method_ty {
        Type::Fun(params, _) if !params.is_empty() => "..",
        _ => "",
    };
    format!("`{on}.{method}({args})`")
}

/// Render a set of symbols for a user-facing
/// diagnostic. `BTreeSet<Symbol>` formatted with `{:?}` would show the
/// interner's Debug form (`Symbol(6: "x")`). This helper resolves each symbol to its
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
/// See `src/typechecker/suggest.rs`.
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
    if name_str == "if" {
        return (NO_IF.to_string(), Some(NO_IF_HELP.to_string()));
    }
    let foreign_keyword_hint = match name_str.as_str() {
        "break" | "continue" => {
            Some("silt has no 'break'/'continue' — return early or restructure the recursion")
        }
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

/// Append a "did you mean `<cand>`?" hint when a
/// record-field diagnostic mentions a name that's close in edit
/// distance to one of the record's declared fields. Used by every
/// "record 'X' has no field 'Y'" / "unknown field 'Y' in X" site so
/// `u.nam` on `type User { name, age }` gets `did you mean \`name\`?`.
/// Delegates to `suggest::suggest_similar` for the threshold policy
/// (single-edit only for names up to 5 chars; scaled for longer names).
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

impl TypeChecker {
    /// Put `var: trait_name` (at the trait arguments `args`) in scope,
    /// and with it each supertrait, at the arguments the trait's
    /// declaration gives it: `where a: Ordered` with
    /// `trait Ordered: Equal` makes both traits' methods callable on `a`,
    /// and `v: Sub(Int)` with `trait Sub(a): Super(a)` says
    /// `v: Super(Int)`.
    /// [`Self::declare_bound`] of a bound a `where` clause writes, which
    /// is remembered as written.
    pub(super) fn declare_written_bound(
        &mut self,
        var: TyVar,
        trait_name: TraitKey,
        args: Vec<Type>,
    ) {
        let written = self.written_bounds.entry(var).or_default();
        if !written.iter().any(|(t, a)| *t == trait_name && *a == args) {
            written.push((trait_name, args.clone()));
        }
        self.declare_bound(var, trait_name, args);
    }

    pub(super) fn declare_bound(&mut self, var: TyVar, trait_name: TraitKey, args: Vec<Type>) {
        // (Two bounds of one trait at different arguments are two
        // bounds: `where a: Conv(Int) + Conv(String)`.)
        let in_scope = self.bounds.entry(var).or_default();
        if in_scope.iter().any(|(t, a)| *t == trait_name && *a == args) {
            return;
        }
        in_scope.push((trait_name, args.clone()));
        let Some(info) = self.tables.traits.get(&trait_name).cloned() else {
            return;
        };
        for (i, super_name) in info.supertraits.iter().enumerate() {
            let super_args: Vec<Type> = match info.supertrait_args.get(i) {
                Some(exprs) if !exprs.is_empty() => {
                    // A bare parametric type name
                    // (`Box` where `type Box(a) { ... }`) among the
                    // supertrait's arguments would silently produce a
                    // 0-arity type that matches no impl: it is an error.
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
    /// structural trait (`compare`, `equal`, `hash`, `display`,
    /// `encode` of `json.Encode`, `decode` of `json.Decode`) and no
    /// impl of the trait is written for the receiver's type: the type
    /// has the method when it has the trait by its structure, which the
    /// access owes. The other operand of `compare` and `equal` is of
    /// the receiver's type. `decode` takes no `self`: it is the method
    /// of a call through the type only (`on_type`), of the type its
    /// trait declares at the type (`Fn(json.Value) -> Result(T,
    /// json.JsonError)`).
    pub(super) fn structural_method(
        &mut self,
        recv: &Type,
        method: Symbol,
        span: Span,
    ) -> Option<Type> {
        // (The structural methods that take a value of the type are
        // called through their traits; through the type, `decode`.)
        if resolve(method) != "decode" {
            return None;
        }
        let tr = TraitKey::builtin("Decode");
        let head = self.type_name_for_impl(&self.apply(recv))?;
        // (A trait of a builtin module, where the module checked does
        // not import it, gives no method: `T.decode` needs `import
        // json`.)
        if !self.by_structure(tr, head) || self.unreached(tr).is_some() {
            return None;
        }
        let ty = {
            let info = self.tables.traits.get(&tr)?;
            let (_, declared) = info.methods.iter().find(|(name, _)| *name == method)?;
            let mapping = HashMap::from([(info.self_var, recv.clone())]);
            substitute_vars(declared, &mapping)
        };
        // (A refusal of `T.decode` names it so.)
        let callee = intern(&format!("{head}.{method}"));
        let ev = self.want(
            Pred::Trait {
                tr,
                args: Vec::new(),
                subject: recv.clone(),
                asker: None,
            },
            Origin {
                span,
                callee: Some(callee),
                op: None,
            },
        );
        if let Some(ev) = ev {
            self.ev.calls.insert(span, (ev, Vec::new()));
        }
        self.method_trait = Some(tr);
        Some(ty)
    }

    /// The map or set literal at `span` hashes values of the type
    /// `key`.
    fn want_hash(&mut self, key: &Type, span: Span) {
        self.want(
            Pred::Trait {
                tr: TraitKey::builtin("Hash"),
                args: Vec::new(),
                subject: key.clone(),
                asker: None,
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
                asker: None,
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
    /// it does. Where it declares one bound for the trait, its arguments
    /// are the ones needed; of several (`a: Conv(Int) + Conv(String)`)
    /// the one at the arguments needed, if one is.
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
        let of_trait: Vec<Vec<Type>> = (self.bounds.get(&r.var).into_iter().flatten())
            .filter(|(t, _)| *t == trait_name)
            .map(|(_, declared)| declared.clone())
            .collect();
        if of_trait.len() > 1 {
            self.several_bounds(r, trait_name, args, origin, false);
            return;
        }
        if let Some(declared) = of_trait.into_iter().next() {
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
            (Some(pinned), Some(super::solve::PIN)) => format!(", which the pin '^{pinned}' needs"),
            (Some(callee), _) => format!(", which '{callee}' needs"),
            (None, Some(op)) => format!(", which {op} needs"),
            (None, None) => String::new(),
        };
        // The `self` of an impl for every function or every tuple has
        // no name a `where` clause could bound.
        if let Some((what, _)) = self.shape_vars.get(&r.var) {
            self.error(
                Code::MissingConstraint,
                format!(
                    "in an impl for `{}`, `self` is some {what}: it is not known to \
                     implement trait '{bound}'{needs}",
                    r.name
                ),
                origin.span,
            );
            return;
        }
        let diagnostic = Diagnostic::error(
            Code::MissingConstraint,
            origin.span,
            format!(
                "type variable `{}` is not known to implement trait '{bound}'{needs}",
                r.name
            ),
        )
        .with_help(match self.default_body_of {
            // A default body may also have the trait from a supertrait.
            Some(own) if resolve(r.name) == "Self" => format!(
                "add `where Self: {bound}`, or make `{bound}` a supertrait: `trait {}: {bound}`",
                self.show_trait(own)
            ),
            // A variable with bounds of its own takes one more with `+`.
            _ => match (self.written_bounds.get(&r.var)).filter(|bounds| !bounds.is_empty()) {
                Some(bounds) => {
                    // The bounds the clause writes, every one of them, and
                    // not the supertraits they bring.
                    let mut written: Vec<String> = (bounds.iter())
                        .map(|(t, args)| self.show_bound(*t, args))
                        .collect();
                    written.push(bound.clone());
                    format!(
                        "add `+ {bound}` to the bounds of `{}`: `where {}: {}`",
                        r.name,
                        r.name,
                        written.join(" + ")
                    )
                }
                None => format!("add `where {}: {bound}`", r.name),
            },
        });
        self.errors.push(diagnostic);
    }

    /// `require_declared_bound` where `r` has several bounds of
    /// `trait_name` (`a: Conv(Int) + Conv(String)`): the use needs the
    /// one at its arguments. Arguments still unknown wait for the end of
    /// the function's body (`settle`: `settle_open_bound_uses`), which
    /// may decide them; unknown then, the use does not say which bound.
    fn several_bounds(
        &mut self,
        r: RigidId,
        trait_name: TraitKey,
        args: &[Type],
        origin: Origin,
        settle: bool,
    ) {
        if args.is_empty() {
            return;
        }
        let of_trait: Vec<Vec<Type>> = (self.bounds.get(&r.var).into_iter().flatten())
            .filter(|(t, _)| *t == trait_name)
            .map(|(_, declared)| {
                (declared
                    .iter()
                    .map(|t| self.apply(&substitute_vars(t, &self.rigid_of))))
                .collect()
            })
            .collect();
        let needed: Vec<Type> = args.iter().map(|t| self.apply(t)).collect();
        if of_trait.contains(&needed) {
            return;
        }
        let open = needed.iter().any(|t| !free_vars_in(t).is_empty());
        if open && !settle {
            self.open_bound_uses.push((r, trait_name, needed, origin));
            return;
        }
        let declared: Vec<String> = (of_trait.iter())
            .map(|declared| format!("'{}'", self.show_bound(trait_name, declared)))
            .collect();
        let message = match open {
            true => format!(
                "type variable `{}` is declared to implement {}, and which of them this use \
                 needs is not known",
                r.name,
                declared.join(" and ")
            ),
            false => format!(
                "type variable `{}` is declared to implement {}, not '{}'",
                r.name,
                declared.join(" and "),
                self.show_bound(trait_name, &needed)
            ),
        };
        self.error(Code::MissingConstraint, message, origin.span);
    }

    /// Check the uses that wait for their arguments to choose among
    /// several bounds of one trait (`several_bounds`): at the end of a
    /// function's body, which decides what it can.
    pub(super) fn settle_open_bound_uses(&mut self) {
        for (r, trait_name, args, origin) in std::mem::take(&mut self.open_bound_uses) {
            self.several_bounds(r, trait_name, &args, origin, true);
        }
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
            // is new at each call, which owes the method's own bounds,
            // and what the method of the type `r` stands for asks.
            let ty = substitute_vars(method_ty, &mapping);
            let preds = info
                .method_bounds
                .get(&field)
                .into_iter()
                .flatten()
                .map(|pred| pred.substitute(&mapping))
                .chain([Pred::Via {
                    tr: trait_name,
                    method: field,
                    receiver: Type::Rigid(r),
                    method_ty: ty.clone(),
                    passer: None,
                    at: None,
                }])
                .collect();
            matches.push((
                trait_name,
                Scheme {
                    vars: free_vars_in(&ty),
                    preds,
                    ty,
                },
            ));
        }
        matches
    }

    /// The evidence of `Type.method` at `span`, the method of an impl
    /// reached through its type `head`, just instantiated: as a call of
    /// the impl's method passes it on (`method_site`).
    fn type_method_site(&mut self, head: TypeRef, entry: &MethodEntry, method: Symbol, span: Span) {
        let owed = self.ev.use_ev.take().unwrap_or_default();
        if let Some(tr) = self.entry_trait(entry, method) {
            let site = self.method_site(tr, head, method, owed);
            self.ev.calls.insert(span, site);
        }
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

    /// Resolve a method that makes a value on a type descriptor
    /// (`TypeOf(inner)`, `a.empty()`). The descriptor is a type carrier —
    /// lookup uses `inner`'s effective type name (for concrete inners) or
    /// the active trait constraints (for a type variable inner). Returns
    /// the method's function type with all `Self` references substituted
    /// to `inner`; a method that takes a value of the type is reported
    /// (it is called through its trait).
    ///
    /// The descriptor occupies no parameter of the method:
    /// `receiver_taken` stays false, so the call unifies its arguments
    /// with all of the method's parameters.
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
                let Some(mut trait_names) = self
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
                distinct(&mut trait_names);
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
                    self.ambiguous_bound_method(
                        field,
                        &format!("`type {inner}`"),
                        *r,
                        &traits,
                        span,
                    );
                    return None;
                }
                if self.self_param(matches[0].0, field).is_some() {
                    self.through_its_trait(&inner.to_string(), &[matches[0].0], field, span);
                    return Some(Type::Error);
                }
                self.method_trait = Some(matches[0].0);
                // The receiver has the trait by the bound.
                let recv = self.ev_new(evidence::EvNode::Bound {
                    var: r.var,
                    tr: matches[0].0,
                    args: Vec::new(),
                });
                let instantiated = self.instantiate_method(&matches[0].1, field, span);
                let own = self.ev.use_ev.take().unwrap_or_default();
                self.ev.calls.insert(span, (recv, own));
                Some(self.apply(&instantiated))
            }
            _ => {
                // Concrete inner — look up via the method table, keyed on
                // the effective type name (same path as
                // `type_name_for_impl`).
                let name = self.type_name_for_impl(&inner)?;
                let entry = self.method_entry(name, field)?;
                if let Some(tr) = self.entry_trait(&entry, field)
                    && self.self_param(tr, field).is_some()
                {
                    let shown = self.show_type(&inner);
                    self.through_its_trait(&shown, &[tr], field, span);
                    return Some(Type::Error);
                }
                if let Some(trait_name) = entry.trait_name
                    && self.private_owner(trait_name).is_some()
                {
                    self.private_method(trait_name, field, span);
                    return Some(Type::Error);
                }
                if self.ambiguous_method_call(name, field, span) {
                    return Some(Type::Error);
                }
                if self.of_every_shape(name, &entry, field, span) {
                    return Some(Type::Error);
                }
                if entry.structural {
                    return Some(self.structural_type_method(name, &inner, field, span));
                }
                self.method_trait = self.entry_trait(&entry, field);
                let scheme = self.method_scheme(&entry);
                let instantiated = self.instantiate_method(&scheme, field, span);
                self.type_method_site(name, &entry, field, span);
                Some(self.apply(&instantiated))
            }
        }
    }

    /// Where `expr` names a variant that has the name of a type (its own,
    /// `Box` of `type Box(a) { Box(a), Empty }`, or one in sight), let it
    /// name the type.
    fn type_named_like_its_variant(&self, expr: &mut Expr) {
        if !matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return;
        }
        let (Some(crate::defs::Res::Def(id)), Some(defs)) = (expr.res, self.defs.as_ref()) else {
            return;
        };
        let Some(ty) = defs.variant_type(id) else {
            return;
        };
        let name = defs.get(id).name;
        if let ExprKind::Ident(written) = expr.kind
            && let Some((t, _)) =
                (self.spelled.iter()).find(|(t, n)| **n == written && defs.get(**t).is_type())
        {
            expr.res = Some(crate::defs::Res::Def(*t));
        } else if ty.name == name
            && let crate::defs::DefKind::Type(t) = ty.kind
        {
            expr.res = Some(crate::defs::Res::Def(t.0));
        }
    }

    /// The trait an expression names (`Greet`, `m.Greet`, `Compare`), as
    /// the resolver resolved it; `None` for anything else.
    pub(super) fn trait_named(&self, expr: &Expr) -> Option<TraitKey> {
        if !matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return None;
        }
        match expr.res {
            Some(crate::defs::Res::Def(id)) => self.trait_key(id),
            _ => None,
        }
    }

    /// The type of `Tr.field`, a method of the trait `tr` called through
    /// it: the method's type with a type of its own for `Self`, whose
    /// impl the call reaches (the type of the arguments it is given).
    /// The use owes `Self: Tr`, the method's own bounds, and what the
    /// method of that impl asks (`Pred::Via`), as a call on a value of
    /// that type would. A method that takes no value of type `Self`
    /// cannot say which impl it is of: it is called on a type.
    fn trait_method(
        &mut self,
        tr: TraitKey,
        field: Symbol,
        receiver: Option<(Type, Span)>,
        span: Span,
    ) -> Type {
        let Some(info) = self.tables.traits.get(&tr).cloned() else {
            return Type::Error;
        };
        let Some((_, method_ty)) = info.methods.iter().find(|(n, _)| *n == field) else {
            let message = format!("trait '{}' has no method '{field}'", self.show_trait(tr));
            // A method of a supertrait is called through that trait.
            let sup = (self.supertraits_of(tr).into_iter()).find(|s| {
                (self.tables.traits.get(s))
                    .is_some_and(|i| i.methods.iter().any(|(n, _)| *n == field))
            });
            let help = match sup {
                Some(s) => uncallable_builtin(s, field, "x").or_else(|| {
                    let shown = self.spell_trait(s);
                    let rest = if self.method_arity(s, field) > 1 { ", .." } else { "" };
                    Some(format!(
                        "`{field}` is a method of supertrait `{shown}`: call it as `{shown}.{field}(x{rest})`"
                    ))
                }),
                None => {
                    let names: Vec<String> =
                        info.methods.iter().map(|(n, _)| resolve(*n)).collect();
                    super::suggest::suggest_similar(&resolve(field), &names)
                        .map(|name| format!("did you mean '{name}'?"))
                }
            };
            self.error_help(Code::UnknownMethod, (message, help), span);
            return Type::Error;
        };
        if let Some(how) = uncallable_builtin(tr, field, "x") {
            self.error_help(
                Code::InvalidMethodCall,
                (
                    format!("trait '{}' has no callable methods", self.show_trait(tr)),
                    Some(how),
                ),
                span,
            );
            return Type::Error;
        }
        if self.private_owner(tr).is_some() {
            self.private_method(tr, field, span);
            return Type::Error;
        }
        let takes_self = matches!(method_ty, Type::Fun(params, _)
            if params.iter().any(|p| matches!(p, Type::Var(v) if *v == info.self_var)));
        if !takes_self {
            self.errors.push(
                Diagnostic::error(
                    Code::InvalidMethodCall,
                    span,
                    format!(
                        "method '{field}' of trait '{}' takes no value of the type, so it is \
                         called on a type",
                        self.show_trait(tr)
                    ),
                )
                .with_help(format!(
                    "call it on a type, {}, or on a `type a` parameter, {}",
                    on_type_call("T", field, method_ty),
                    on_type_call("a", field, method_ty)
                )),
            );
            return Type::Error;
        }
        // Called with its first argument a value of a type that has an
        // impl written: the method of that impl, as a function of the
        // impl's module calls it, owing what the impl's header and the
        // method's body ask (`dispatch_method_entry`).
        if let Some((recv, _)) = &receiver {
            let recv = self.apply(recv);
            let entry = match crate::types::canonical::canonicalize(&self.tables.resolver, &recv) {
                Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => None,
                canon => (self.type_name_for_impl(&canon)).and_then(|head| {
                    let entry = self.tables.impl_methods.get(head, field, tr)?;
                    (!entry.structural).then(|| (head, entry.clone()))
                }),
            };
            if let Some((head, entry)) = entry {
                self.method_trait = Some(tr);
                self.trait_call_self.insert(span, recv.clone());
                self.receiver_taken = true;
                let owed_before = self.wanted.len();
                let scheme = self.method_scheme(&entry);
                let ty = self.instantiate_method(&scheme, field, span);
                // The impl's header's predicates come first among the
                // method's.
                let owed = self.ev.use_ev.take().unwrap_or_default();
                let site = self.method_site(tr, head, field, owed);
                self.ev.calls.insert(span, site);
                if let Type::Fun(params, _) = &ty
                    && let Some(first) = params.first()
                {
                    self.unify(&recv, first, span);
                }
                self.solve_wanted(owed_before);
                return self.apply(&ty);
            }
        }
        let self_ty = self.fresh_var();
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        mapping.insert(info.self_var, self_ty.clone());
        let params: Vec<Type> = info
            .param_var_ids
            .iter()
            .map(|v| {
                let fresh = self.fresh_var();
                mapping.insert(*v, fresh.clone());
                fresh
            })
            .collect();
        let ty = substitute_vars(method_ty, &mapping);
        let preds: Vec<Pred> = info
            .method_bounds
            .get(&field)
            .into_iter()
            .flatten()
            .map(|pred| pred.substitute(&mapping))
            .chain([Pred::Via {
                tr,
                method: field,
                receiver: self_ty.clone(),
                method_ty: ty.clone(),
                passer: None,
                at: None,
            }])
            .collect();
        let own: Vec<TyVar> = free_vars_in(method_ty)
            .into_iter()
            .filter(|v| !mapping.contains_key(v))
            .collect();
        let scheme = Scheme {
            vars: own,
            preds,
            ty,
        };
        if let Some((recv, _)) = &receiver {
            self.unify(recv, &self_ty, span);
        }
        let recv_ev = self.want(
            Pred::Trait {
                tr,
                args: params,
                subject: self_ty.clone(),
                asker: None,
            },
            Origin {
                span: receiver.as_ref().map_or(span, |(_, at)| *at),
                callee: Some(field),
                op: None,
            },
        );
        self.method_trait = Some(tr);
        self.trait_call_self.insert(span, self_ty);
        self.receiver_taken = receiver.is_some();
        let method_ty = self.instantiate_method(&scheme, field, span);
        let own = self.ev.use_ev.take().unwrap_or_default();
        if let Some(recv_ev) = recv_ev {
            self.ev.calls.insert(span, (recv_ev, own));
        }
        self.apply(&method_ty)
    }

    /// Report `recv.field` where a value of the type `ty` has no field
    /// `field` (`fields`: the fields it has): when a trait in sight has a
    /// method of that name, or a builtin module of the type a function,
    /// the help says how to call it; else a field of a similar name.
    pub(super) fn no_field(
        &mut self,
        ty: &Type,
        field: Symbol,
        recv: Option<&Expr>,
        fields: &[(Symbol, Type)],
        span: Span,
    ) {
        let shown = self.show_type(ty);
        let base = format!("type '{shown}' has no field '{field}'");
        let placeholder = match ty {
            Type::List(_) => "xs",
            _ => "x",
        };
        let written = match recv.map(|recv| &recv.kind) {
            Some(ExprKind::Ident(name)) => resolve(*name),
            _ => placeholder.to_string(),
        };
        let help = match self.method_help(ty, field, &written) {
            Some(help) => Some(help),
            None => format_record_field_suggestion(base.clone(), field, fields).1,
        };
        self.error_help(Code::UnknownField, (base, help), span);
    }

    /// How to call the method or function `field` that a dot after a
    /// value of the type `ty`, written `written`, names, if a trait in
    /// sight or a builtin module of the type has one of that name: the
    /// help of `no_field`.
    pub(super) fn method_help(&self, ty: &Type, field: Symbol, written: &str) -> Option<String> {
        let mut traits: Vec<TraitKey> = (self.tables.traits.iter())
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == field))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t) && self.private_owner(*t).is_none())
            .collect();
        traits.sort_by_key(|t| self.show_trait(*t));
        if let Some(tr) = traits.first().copied() {
            if let Some(how) = uncallable_builtin(tr, field, written) {
                return Some(how);
            }
            let params = self.method_arity(tr, field);
            if self.self_param(tr, field).is_some() {
                // (Every trait in sight that gives it, by how this module
                // writes it.)
                let taking: Vec<String> = (traits.iter())
                    .filter(|t| uncallable_builtin(**t, field, written).is_none())
                    .filter(|t| self.self_param(**t, field).is_some())
                    .map(|t| self.spell_trait(*t))
                    .collect();
                let rest = if params > 1 { ", .." } else { "" };
                let calls: Vec<String> = (taking.iter())
                    .map(|shown| format!("`{shown}.{field}({written}{rest})`"))
                    .collect();
                let of: Vec<String> = taking
                    .iter()
                    .map(|shown| format!("trait `{shown}`"))
                    .collect();
                return Some(format!(
                    "`{field}` is a method of {}: call it as {}",
                    of.join(" and of "),
                    calls.join(" or ")
                ));
            }
            let shown = self.spell_trait(tr);
            // (A type variable is called on as the `type a` parameter
            // that would name it.)
            let (on, parameter) = match ty {
                Type::Rigid(r) => (
                    r.name.to_string(),
                    format!(", where `{0}` is a `type {0}` parameter", r.name),
                ),
                _ => (
                    self.type_name_for_impl(ty)
                        .map_or_else(|| "T".to_string(), |t| self.spell_type(t)),
                    String::new(),
                ),
            };
            let args = if params > 0 { ".." } else { "" };
            return Some(format!(
                "`{field}` is a method of trait `{shown}` that makes a value: call it on the type, \
                 `{on}.{field}({args})`{parameter}"
            ));
        }
        let module = match ty {
            Type::List(_) => "list",
            Type::String => "string",
            Type::Map(..) => "map",
            Type::Set(_) => "set",
            Type::Int => "int",
            Type::Float => "float",
            Type::Channel(_) => "channel",
            Type::Generic(name, _) if name.is_builtin("Option") => "option",
            Type::Generic(name, _) if name.is_builtin("Result") => "result",
            Type::Generic(name, _) if name.is_builtin("Bytes") => "bytes",
            _ => return None,
        };
        let row = crate::builtins::registry::registry()
            .row(module, &resolve(field))
            .filter(|row| row.enabled)?;
        let rest = if row.params.len() > 1 { ", .." } else { "" };
        // (As this module writes the module: `l.length` after `import
        // list as l`.)
        let written_module = (crate::session::ModuleId::builtin(module))
            .and_then(|id| self.module_spelled.get(&id))
            .map_or_else(|| module.to_string(), |alias| resolve(*alias));
        Some(format!(
            "`{field}` is a function of module `{module}`: call it as \
             `{written_module}.{field}({written}{rest})`"
        ))
    }

    /// The trait `tr` as this module writes it: by the name it binds it
    /// to (its own, an imported item), else through the module it
    /// imports it by (`sh.Area` after `import shapes as sh`), else with
    /// the module that declares it.
    pub(super) fn spell_trait(&self, tr: TraitKey) -> String {
        if let Some((name, None)) = crate::defs::builtin_trait_of_id(tr.id.0) {
            return name.to_string();
        }
        self.spell_def(tr.id.0)
            .unwrap_or_else(|| self.show_trait(tr))
    }

    /// The type `ty` as this module writes it (see `spell_trait`).
    pub(super) fn spell_type(&self, ty: TypeRef) -> String {
        self.spell_def(ty.id.0)
            .unwrap_or_else(|| self.show_type(&Type::Generic(ty, Vec::new())))
    }

    /// The definition `id` as this module writes it: by the name it
    /// binds it to, or through the module it imports it by.
    fn spell_def(&self, id: crate::defs::DefId) -> Option<String> {
        if let Some(name) = self.spelled.get(&id) {
            return Some(resolve(*name));
        }
        let def = self.def(id)?;
        if def.module == self.module {
            return Some(resolve(def.name));
        }
        let alias = self.module_spelled.get(&def.module)?;
        Some(format!("{alias}.{}", def.name))
    }

    /// Report `T.field`, through the type `on`, of the method `field` of
    /// the trait `tr`, which takes a value of the type: it is called
    /// through its trait.
    fn through_its_trait(&mut self, on: &str, traits: &[TraitKey], field: Symbol, span: Span) {
        let callable: Vec<TraitKey> = (traits.iter().copied())
            .filter(|t| uncallable_builtin(*t, field, "x").is_none())
            .collect();
        let help = match (callable.as_slice(), traits.first()) {
            ([], Some(first)) => uncallable_builtin(*first, field, "x").unwrap_or_default(),
            _ => {
                let of: Vec<String> = (callable.iter())
                    .map(|t| format!("trait `{}`", self.spell_trait(*t)))
                    .collect();
                let calls: Vec<String> = (callable.iter())
                    .map(|t| {
                        let more = self.method_arity(*t, field) > 1;
                        let rest = if more { ", .." } else { "" };
                        format!("`{}.{field}(x{rest})`", self.spell_trait(*t))
                    })
                    .collect();
                format!(
                    "`{field}` is a method of {}: call it as {}",
                    of.join(" and of "),
                    calls.join(" or ")
                )
            }
        };
        self.error_help(
            Code::UnknownMethod,
            (format!("type '{on}' has no method '{field}'"), Some(help)),
            span,
        );
    }

    /// The number of parameters of the method `field` of the trait `tr`.
    fn method_arity(&self, tr: TraitKey, field: Symbol) -> usize {
        (self.tables.traits.get(&tr))
            .and_then(|info| info.methods.iter().find(|(n, _)| *n == field))
            .and_then(|(_, ty)| match ty {
                Type::Fun(params, _) => Some(params.len()),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// The index of the first parameter of type `Self` of the method
    /// `field` of the trait `tr`: the argument a call through the trait
    /// picks the impl by. `None` when the method has none, or the trait
    /// no such method.
    pub(super) fn self_param(&self, tr: TraitKey, field: Symbol) -> Option<usize> {
        let info = self.tables.traits.get(&tr)?;
        let (_, ty) = info.methods.iter().find(|(n, _)| *n == field)?;
        match ty {
            Type::Fun(params, _) => params
                .iter()
                .position(|p| matches!(p, Type::Var(v) if *v == info.self_var)),
            _ => None,
        }
    }

    /// `T.decode` where the type `ty`, of the instance `inner`, has
    /// `json.Decode` by its structure: the method `structural_method`
    /// gives `inner`.
    fn structural_type_method(
        &mut self,
        ty: TypeRef,
        inner: &Type,
        field: Symbol,
        span: Span,
    ) -> Type {
        let method = self.structural_method(inner, field, span);
        match method {
            Some(method) => self.apply(&method),
            // The type has the method by its structure, so the
            // receiver's head is the type's: `structural_method` gives
            // it a type.
            None => {
                debug_assert!(false, "structural method {ty}.{field} without a type");
                Type::Error
            }
        }
    }

    /// The value an alias names written as a value (`json.parse(s, PA)`
    /// for `type PA = P`): the descriptor of the type it stands for, of
    /// the type `TypeOf(target)`, when that type is a value (a primitive,
    /// container, record or enum type). An alias is only another name.
    pub(super) fn alias_value(
        &mut self,
        res: Option<crate::defs::Res>,
        env: &TypeEnv,
    ) -> Option<Type> {
        let def = self.res_def(res)?;
        if def.kind != crate::defs::DefKind::TypeAlias {
            return None;
        }
        let alias = self.res_type(res)?;
        let instance = self.type_instance(alias)?;
        let target = crate::types::canonical::canonicalize(&self.tables.resolver, &instance);
        let head = crate::types::canonical::head_of_canon(&target)?;
        let scheme = self.def_scheme(Some(crate::defs::Res::Def(head.id.0)), env)?;
        match self.instantiate(&scheme) {
            Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF) => {
                Some(Type::type_of(target))
            }
            _ => None,
        }
    }

    /// `Fn.field` or `Tuple.field` where `entry` is the method of an impl
    /// for every function (`trait T for Fn`) or every tuple: reported.
    /// That impl is found by the receiver; as far as the method's
    /// signature says its `self` is of any type, so written through the
    /// type it would be a function of anything.
    fn of_every_shape(
        &mut self,
        ty: TypeRef,
        entry: &MethodEntry,
        field: Symbol,
        span: Span,
    ) -> bool {
        let every = (ty.is_builtin("Fn") || ty.is_builtin("Tuple"))
            && matches!(&entry.method_type, Type::Fun(params, _)
                if matches!(params.first(), Some(Type::Var(_))));
        if every {
            self.of_every_shape_error(ty, field, span);
        }
        every
    }

    /// Report `Fn.field` or `Tuple.field`: the method is found by the
    /// value it is called on.
    fn of_every_shape_error(&mut self, ty: TypeRef, field: Symbol, span: Span) {
        self.error(
            Code::InvalidMethodCall,
            format!(
                "method `{field}` of the impl for every `{0}` is called on a value: \
                 write `x.{field}(..)`, not `{0}.{field}`",
                ty.name
            ),
            span,
        );
    }

    /// Report `T.field` where the type `T` has no method `field`. A builtin
    /// module named like the type with a function of that name (`int.parse`
    /// for `Int.parse`) is suggested.
    fn no_type_method(&mut self, type_name: &str, field: Symbol, span: Span, ty: Option<TypeRef>) {
        let module = type_name.to_lowercase();
        let shown = match ty {
            Some(ty) => self.show_type(&Type::Generic(ty, Vec::new())),
            None => type_name.to_string(),
        };
        let mut d = Diagnostic::error(
            Code::UnresolvedName,
            span,
            format!("type '{shown}' has no method '{field}'"),
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
            // A where-clause bound
            // must supply exactly the trait's declared number of type
            // arguments. Zero args on a parameterized trait
            // (`trait Cast(to)` + `where a: Cast`) would leave the implied
            // `to` unresolved; a nonzero length MISMATCH
            // (`where a: Cast(Int, String)`) is worse — the
            // parameterized-trait verification in
            // `verify_trait_obligation` only runs its
            // positional arg-compatibility zip when the bound's and
            // the impl's arg lists have equal length, so a mismatched
            // bound would degrade to a bare "implements Cast"
            // check and match ANY `Cast(*)` impl. The arity check
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
        for pred in &sig.bounds {
            if let Pred::Trait {
                tr,
                args,
                subject: Type::Var(var),
                ..
            } = pred
            {
                self.declare_written_bound(*var, *tr, args.clone());
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
        let never = matches!(ret_type, Type::Never);
        let prev_never_fn = std::mem::replace(&mut self.never_fn, never.then_some(f.name));
        // Fresh `?`-site tracking per fn body, so a failed
        // body/return unify can point back at the `?` that demanded a
        // Result/Option return.
        let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

        // Infer the body and unify with declared return type
        let owner = self.frame_owner.take();
        self.fn_frames.push(unused::FnFrame {
            owner,
            params: param_types,
        });
        let body_type = self.infer_expr(&mut f.body, env);
        self.fn_frames.pop();
        env.pop();
        // A function that returns `Never` never returns: its body does
        // not end (`Never` would unify with what it ends with).
        if never && !matches!(self.apply(&body_type), Type::Never | Type::Error) {
            self.error(
                Code::InvalidControlFlow,
                format!(
                    "'{}' is declared to return `Never`, but its body can return — end it with \
                     'panic', a call of a function that returns `Never` or a `loop` that never ends",
                    f.name
                ),
                f.body.span,
            );
        }
        self.unify_body_result(&f.body, &body_type, &ret_type);
        self.settle_open_bound_uses();

        self.never_fn = prev_never_fn;
        self.current_return_type = prev_return_type;
        self.current_qmark_spans = prev_qmark_spans;
        self.sig_names = prev_names;
    }

    /// Unify the type `body_type` of a function's or closure's `body`
    /// with its result `ret`, at the value the body gives: a block's tail
    /// expression, where a mismatch is underlined and the Ok-wrap fix
    /// wraps. A block without one is underlined whole and gets no fix.
    fn unify_body_result(&mut self, body: &Expr, body_type: &Type, ret: &Type) {
        let from = self.errors.len();
        let tail = match &body.kind {
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(e)) => Some(e.span),
                _ => None,
            },
            _ => Some(body.span),
        };
        self.unify(body_type, ret, tail.unwrap_or(body.span));
        if tail.is_none() {
            for d in self.errors.iter_mut().skip(from) {
                if d.span == body.span {
                    d.fixes.clear();
                }
            }
        }
        self.note_qmark_requirement_on_ret_mismatch(from, body_type, ret);
    }

    /// When the body/return-type unify fails AND the
    /// return type was driven to Result/Option by a `?` in the body,
    /// append a note naming the `?` site: else a `?` inside an
    /// unannotated (effectively unit-returning) fn surfaces as a bare
    /// "type mismatch: expected Result(_, String), got ()" with the
    /// caret on the fn header — no pointer to the `?` that created the
    /// requirement. Companion to `chain_hint` (mod.rs), which covers
    /// the inverse direction (got Result/Option, expected plain): both
    /// append a continuation line to the raw unify mismatch rather
    /// than emitting a separate diagnostic.
    ///
    /// Where the `?` itself decided the return type (an unannotated
    /// function, a closure) and the body gives no Result or Option, the
    /// function returns neither: that is the `?`'s error, the one text
    /// of a `?` in such a function (`try_operand`).
    fn note_qmark_requirement_on_ret_mismatch(
        &mut self,
        err_count_before: usize,
        body_ty: &Type,
        ret_ty: &Type,
    ) {
        if self.errors.len() == err_count_before {
            return;
        }
        let Some(qspan) = self.current_qmark_spans.first().copied() else {
            return;
        };
        let body_resolved = self.apply(body_ty);
        let wrapper = |t: &Type| matches!(t, Type::Generic(n, _) if n.is_builtin("Result") || n.is_builtin("Option"));
        if let Some(operand) = self.qmark_decided.get(&qspan).cloned()
            && !wrapper(&body_resolved)
            && !matches!(body_resolved, Type::Var(_) | Type::Error | Type::Never)
        {
            let note = format!(
                "the ?-ed expression is {}, but the enclosing function returns {}",
                self.show_type(&self.apply(&operand)),
                self.show_type(&body_resolved)
            );
            self.errors.truncate(err_count_before);
            self.errors.push(
                Diagnostic::error(
                    Code::InvalidQuestion,
                    qspan,
                    "? operator can only be used inside a function that returns Result or Option",
                )
                .with_note(note),
            );
            return;
        }
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
    /// parameters' variables: the one the resolver resolved it to. An
    /// alias of a record type names that type, with the arguments the
    /// alias gives it (the last part). Reports a name that names no
    /// record type, unless the resolver reported it already.
    #[allow(clippy::type_complexity)]
    pub(super) fn named_record(
        &mut self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
        in_pattern: bool,
    ) -> Option<(TypeRef, RecordInfo, Option<Vec<TyVar>>, Option<Vec<Type>>)> {
        if res == Some(crate::defs::Res::Error) || self.names_rejected(res, name) {
            return None;
        }
        let mut ty = self.named_type(res, name);
        let name = ty.map_or(name, |ty| ty.name);
        let mut fixed = None;
        if let Some(alias) = ty.filter(|ty| self.tables.type_aliases.contains(ty)) {
            let target = self.type_instance(alias).map(|instance| {
                crate::types::canonical::canonicalize(&self.tables.resolver, &instance)
            });
            match target {
                Some(Type::Generic(rec, args)) if self.tables.records.contains_key(&rec) => {
                    ty = Some(rec);
                    fixed = Some(args);
                }
                // An alias of an anonymous record type: such a record is
                // written without a name.
                Some(anon @ Type::AnonRecord { .. }) => {
                    let Type::AnonRecord { fields, .. } = &anon else {
                        unreachable!()
                    };
                    // The record as a literal or a pattern writes it.
                    let written: Vec<String> = fields
                        .keys()
                        .map(|field| match in_pattern {
                            true => format!("{field}"),
                            false => format!("{field}: ..."),
                        })
                        .collect();
                    let shown = self.show_type(&anon);
                    let mut message =
                        format!("`{name}` is another name for the anonymous record type `{shown}`");
                    // (`{}` is the empty block: no literal writes the
                    // record without fields.)
                    if !written.is_empty() {
                        message.push_str(&format!(
                            ": write the record without the name, `{{ {} }}`",
                            written.join(", ")
                        ));
                    }
                    self.error(Code::UndefinedConstructor, message, span);
                    return None;
                }
                _ => {}
            }
        }
        match ty.and_then(|ty| Some((ty, self.tables.records.get(&ty).cloned()?))) {
            Some((ty, info)) => {
                let ids = self.tables.record_param_var_ids.get(&ty).cloned();
                Some((ty, info, ids, fixed))
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

    /// The name of the callee, as a message writes it, if it is a
    /// function of the program (or a local that holds one), not a
    /// builtin.
    fn user_callee(&self, callee: &Expr) -> Option<Symbol> {
        if let Some(crate::defs::Res::Def(id)) = callee.res
            && (self.defs.as_ref()).is_some_and(|defs| defs.get(id).module.is_builtin())
        {
            return None;
        }
        Self::callee_label(callee)
    }

    /// The alias `arg` names, if it is a type written as a value by an
    /// alias's name (`IntBox`, `m.IntBox`).
    fn alias_named(&self, arg: &Expr) -> Option<Symbol> {
        if !matches!(arg.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return None;
        }
        let Some(crate::defs::Res::Def(id)) = arg.res else {
            return None;
        };
        let def = self.defs.as_ref()?.get(id);
        (def.kind == crate::defs::DefKind::TypeAlias).then_some(def.name)
    }

    /// Whether `param`, a parameter of the callee of the call being
    /// checked, is a `type a` parameter whose type a decoder decodes:
    /// the callee owes, from the wanted predicates from `owed` on, a
    /// decoder's check of `a` or `json.Decode` of it.
    fn asks_decoding(&self, param: &Type, owed: usize) -> bool {
        let Type::Generic(head, args) = self.apply(param) else {
            return false;
        };
        let [Type::Var(v)] = args.as_slice() else {
            return false;
        };
        if !head.is_builtin(crate::defs::TYPE_OF) {
            return false;
        }
        self.wanted[owed..].iter().any(|wanted| match &wanted.goal {
            Goal::Pred(Pred::Builtin {
                check: BuiltinCheck::Decodes { .. },
                subject,
                ..
            }) => *subject == Type::Var(*v),
            Goal::Pred(Pred::Trait { tr, subject, .. }) => {
                Self::is_decode(*tr) && *subject == Type::Var(*v)
            }
            _ => false,
        })
    }

    /// The type `arg` writes as a value if it writes a type of types:
    /// a type applied to types (`List(Int)`, `Map(String, Int)`,
    /// `Box(Int)`) or a tuple of types (`(Int, String)`), at any depth.
    /// `None` for any other expression, a type alone among them (its
    /// value has its type already), and one whose arguments do not fit
    /// the type.
    fn spelled_type(&mut self, arg: &mut Expr, env: &mut TypeEnv) -> Option<Type> {
        if !matches!(arg.kind, ExprKind::Call(..) | ExprKind::Tuple(_)) || !self.writes_types(arg) {
            return None;
        }
        self.spell(arg, env)
    }

    /// Whether `e` is made of names of types only, applied to each
    /// other or in tuples: what `spell` reads.
    fn writes_types(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(_) | ExprKind::FieldAccess(..) => {
                let Some(crate::defs::Res::Def(id)) = e.res else {
                    return false;
                };
                (self.defs.as_ref()).is_some_and(|defs| {
                    matches!(
                        defs.get(id).kind,
                        crate::defs::DefKind::Type(_) | crate::defs::DefKind::TypeAlias
                    )
                })
            }
            ExprKind::Call(callee, args) => {
                self.writes_types(callee) && args.iter().all(|a| self.writes_types(a))
            }
            ExprKind::Tuple(elems) => elems.iter().all(|e| self.writes_types(e)),
            _ => false,
        }
    }

    /// The type `e`, which `writes_types`, writes.
    fn spell(&mut self, e: &mut Expr, env: &mut TypeEnv) -> Option<Type> {
        match &mut e.kind {
            ExprKind::Call(callee, args) => {
                let head = self.spell(callee, env)?;
                let args: Vec<Type> = (args.iter_mut())
                    .map(|a| self.spell(a, env))
                    .collect::<Option<_>>()?;
                // (A type whose parameters the value leaves open.)
                let open = |t: &Type| matches!(t, Type::Var(_));
                match (head, args.as_slice()) {
                    (Type::List(p), [t]) if open(&p) => Some(Type::List(Box::new(t.clone()))),
                    (Type::Set(p), [t]) if open(&p) => Some(Type::Set(Box::new(t.clone()))),
                    (Type::Channel(p), [t]) if open(&p) => Some(Type::Channel(Box::new(t.clone()))),
                    (Type::Map(pk, pv), [k, v]) if open(&pk) && open(&pv) => {
                        Some(Type::Map(Box::new(k.clone()), Box::new(v.clone())))
                    }
                    (Type::Generic(r, params), _)
                        if !params.is_empty()
                            && params.len() == args.len()
                            && params.iter().all(open) =>
                    {
                        Some(Type::Generic(r, args))
                    }
                    _ => None,
                }
            }
            ExprKind::Tuple(elems) => (elems.iter_mut())
                .map(|e| self.spell(e, env))
                .collect::<Option<_>>()
                .map(Type::Tuple),
            _ => match self.infer_expr(e, env) {
                Type::Generic(head, args) if head.is_builtin(crate::defs::TYPE_OF) => {
                    args.first().map(|t| self.apply(t))
                }
                _ => None,
            },
        }
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
        let owed_before = self.wanted.len();
        let given_before = self.given_waiting.len();
        // Reset the method-dispatch flag so a stale value from an
        // earlier field access does not leak into this call, and read it
        // before the arguments are checked (they may overwrite it).
        self.receiver_taken = false;
        self.named_use = Self::callee_label(callee).map(|label| Origin {
            span,
            callee: Some(label),
            op: None,
        });
        self.callee_position = true;
        self.unknown_receiver = None;
        // `Tr.m(x, ..)`: the argument of type `Self` is checked with the
        // callee, as a receiver: the impl is the one for its type.
        let through_trait = match &callee.kind {
            ExprKind::FieldAccess(obj, field, _) if !args.is_empty() => self
                .trait_named(obj)
                .is_some_and(|tr| self.self_param(tr, *field) == Some(0)),
            _ => false,
        };
        if through_trait {
            let receiver = args.remove(0);
            self.callee_position = false;
            let recv_ty = self.infer_expr(receiver, env);
            self.callee_position = true;
            self.trait_receiver = Some((recv_ty, receiver.span));
        }
        let callee_ty = self.infer_expr(callee, env);
        self.trait_receiver = None;
        self.callee_position = false;
        self.named_use = None;
        let callee_ty = self.apply(&callee_ty);
        let receiver_taken_by_callee = self.receiver_taken;
        self.receiver_taken = false;
        // `x.f(args)` where the type of `x` is unknown: the call waits
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
                let implicit_self = usize::from(receiver_taken_by_callee);
                // (A method has its `self`: `takes_no_self` refused the
                // access otherwise.)
                let written_params = match through_trait {
                    true => params.len(),
                    false => params.len().saturating_sub(implicit_self),
                };
                // What the callee's use owes for a type variable of a
                // parameter is owed for that argument: a bound that
                // fails is reported at it.
                for k in owed_before..self.wanted.len() {
                    // (A trait method's call is at the type it is given,
                    // not at its receiver: where it was given, if that is
                    // known.)
                    if let Goal::Pred(Pred::Via { at: Some(at), .. }) = &self.wanted[k].goal {
                        self.wanted[k].origin.span = *at;
                        continue;
                    }
                    let type_given = match &self.wanted[k].goal {
                        Goal::Pred(Pred::Via { method_ty, .. }) => {
                            type_of_var(&self.apply(method_ty))
                        }
                        _ => None,
                    };
                    let Type::Var(v) = (match (&self.wanted[k].goal, &type_given) {
                        (_, Some(given)) => given,
                        (Goal::Pred(pred), None) => pred.subject(),
                        (Goal::Listed { row, .. }, None) => row,
                        _ => continue,
                    }) else {
                        continue;
                    };
                    let at = params
                        .iter()
                        .skip(implicit_self)
                        .position(|param| free_vars_in(param).contains(v))
                        .and_then(|i| args.get(i));
                    if let (Some(arg), CallForm::Call) = (at, form) {
                        self.wanted[k].origin.span = arg.span;
                        // A type argument the call names by an alias:
                        // what a decoder's refusal calls it.
                        let alias = self.alias_named(arg);
                        match &mut self.wanted[k].goal {
                            Goal::Pred(Pred::Builtin {
                                check: BuiltinCheck::Decodes { written, .. },
                                ..
                            }) => *written = alias,
                            Goal::Pred(Pred::Trait { tr, .. }) if Self::is_decode(*tr) => {
                                if let Some(alias) = alias {
                                    self.decode_aliases.insert(arg.span, alias);
                                }
                            }
                            _ => {}
                        }
                        // A trait method's call: what the method of the
                        // receiver's impl gives a decoder is decided with
                        // the module's bodies, each type at the type
                        // argument of the call that gives it, by the alias
                        // that names it (as through a function: of two
                        // that give one type, the first).
                        if let Goal::Pred(Pred::Via { .. }) = &self.wanted[k].goal {
                            let origin = self.wanted[k].origin;
                            self.wanted[k].places = (params.iter().skip(implicit_self))
                                .zip(args.iter())
                                .filter_map(|(param, arg)| {
                                    let given = match self.apply(param) {
                                        Type::Generic(name, given)
                                            if name.is_builtin(crate::defs::TYPE_OF) =>
                                        {
                                            given.first().cloned()?
                                        }
                                        _ => return None,
                                    };
                                    let origin = Origin {
                                        span: arg.span,
                                        ..origin
                                    };
                                    Some((given, origin, self.alias_named(arg)))
                                })
                                .collect();
                        }
                    }
                }
                // The same for a type given to an annotation variable of
                // the callee's signature.
                for k in given_before..self.given_waiting.len() {
                    let Type::Var(v) = self.given_waiting[k].subject else {
                        continue;
                    };
                    let at = params
                        .iter()
                        .skip(implicit_self)
                        .position(|param| free_vars_in(param).contains(&v))
                        .and_then(|i| args.get(i));
                    if let (Some(arg), CallForm::Call) = (at, form) {
                        let alias = self.alias_named(arg);
                        let use_ = &mut self.given_waiting[k];
                        use_.origin.span = arg.span;
                        use_.written = alias;
                    }
                }
                let mut mismatched = false;
                // Each argument's place, the alias it names a type by,
                // and its type.
                let mut given: Vec<(Span, Option<Symbol>, Type)> = Vec::new();
                // The calls of trait methods held in a closure (returned
                // by a function, kept in a record) whose type argument is
                // still unknown: this call may be the one that gives it.
                let open_vias = self.open_vias();
                // (The uses the callee owes are at this call's arguments.)
                let callee_owes = given_before..self.given_waiting.len();
                for (i, arg) in args.iter_mut().enumerate() {
                    let param = params.get(i + implicit_self);
                    if let (ExprKind::Lambda { .. }, Some(param)) = (&arg.kind, param)
                        && let Type::Fun(expected, _) = self.apply(param)
                    {
                        self.expected_closure = Some(expected);
                    }
                    // A `type a` argument that names both a type and its
                    // variant (`type Box(a) { Box(a), Empty }`) is the
                    // type.
                    if let Some(param) = param
                        && matches!(self.apply(param),
                            Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF))
                    {
                        self.type_named_like_its_variant(arg);
                    }
                    // A decoder's type argument written as a type of
                    // types (`List(Int)`, `(Int, String)`) is that type,
                    // which the decoder's own check refuses.
                    let spelled = match param {
                        Some(param) if self.asks_decoding(param, owed_before) => {
                            self.spelled_type(arg, env)
                        }
                        _ => None,
                    };
                    let arg_ty = match spelled {
                        Some(ty) => Type::type_of(ty),
                        None => self.infer_expr(arg, env),
                    };
                    self.expected_closure = None;
                    given.push((arg.span, self.alias_named(arg), arg_ty.clone()));
                    if let Some(param) = param {
                        let at = match form {
                            CallForm::Call => arg.span,
                            CallForm::BarePipe => span,
                        };
                        // A parameter the callee calls as a statement.
                        let statement = match receiver_taken_by_callee {
                            true => None,
                            false => self.statement_unit(callee, i),
                        };
                        let passed_on = statement.map(|_| self.frames_sharing(&arg_ty));
                        let reported = self.errors.len();
                        self.unify(&arg_ty, param, at);
                        mismatched |= self.errors.len() > reported;
                        if let (Some(statement), Some(passed_on)) = (statement, passed_on) {
                            self.name_statement_unit(statement, reported, passed_on);
                        }
                    }
                }
                // An argument of the wrong type is the one thing wrong
                // with the call: what the callee owes for it is not
                // asked as well.
                if mismatched {
                    for wanted in &mut self.wanted[owed_before..] {
                        wanted.solved = true;
                    }
                    self.given_waiting.truncate(given_before);
                    self.unplaced.retain(|&k| k < given_before);
                } else if let CallForm::Call = form {
                    let passer = self.user_callee(callee);
                    self.point_at_type_arguments(&given, passer, callee_owes);
                    self.point_vias_at_type_arguments(&given, &open_vias);
                }
                // A type this call gives its callee is placed here: no later
                // call is its place.
                for k in given_before..self.given_waiting.len() {
                    if !matches!(self.apply(&self.given_waiting[k].subject), Type::Var(_)) {
                        self.given_waiting[k].placed = true;
                    }
                }
                // A call supplies exactly as many arguments as the callee
                // has parameters, the piped value and the receiver of a
                // method call among them.
                if params.len() != args.len() + implicit_self {
                    let (message, help) = match form {
                        CallForm::Call => {
                            // A method's receiver is not one of the
                            // arguments the call writes.
                            // (A call through a trait writes its
                            // receiver: it counts.)
                            let (what, written, got) = match (&callee.kind, callee_name) {
                                (ExprKind::FieldAccess(obj, method, _), _) if through_trait => {
                                    let shown = match self.trait_named(obj) {
                                        Some(tr) => format!("`{}.{method}`", self.show_trait(tr)),
                                        None => format!("method `{method}`"),
                                    };
                                    (shown, written_params, args.len() + 1)
                                }
                                (ExprKind::FieldAccess(obj, method, _), _)
                                    if let Some(tr) = self.trait_named(obj) =>
                                {
                                    let shown = format!("`{}.{method}`", self.show_trait(tr));
                                    (shown, written_params, args.len())
                                }
                                (_, Some(name)) => {
                                    (format!("`{name}`"), written_params, args.len())
                                }
                                _ => ("function".to_string(), written_params, args.len()),
                            };
                            let message = format!(
                                "{what} expects {} {}, got {}",
                                written,
                                plural(written, "argument", "arguments"),
                                got
                            );
                            (message, None)
                        }
                        // B6: `a |> f` supplies one argument; piping into
                        // a function that needs more without an explicit
                        // call forgets the remaining ones. The others are
                        // written in a call, which a name can take.
                        CallForm::BarePipe => {
                            let message = format!(
                                "cannot pipe into function taking {} {}",
                                written_params,
                                plural(written_params, "argument", "arguments")
                            );
                            let name = match &callee.kind {
                                ExprKind::Ident(name) => Some(name.to_string()),
                                ExprKind::FieldAccess(module, name, _) => match &module.kind {
                                    ExprKind::Ident(module) => Some(format!("{module}.{name}")),
                                    _ => None,
                                },
                                _ => None,
                            };
                            let help = match name {
                                _ if written_params == 0 => None,
                                Some(name) => Some(format!(
                                    "`|>` passes only the first argument; write the others \
                                     in a call: `|> {name}(...)`"
                                )),
                                None => Some("`|>` passes only the first argument".to_string()),
                            };
                            (message, help)
                        }
                    };
                    self.error_help(Code::ArityMismatch, (message, help), span);
                    // A parameter no argument is given is of no type a
                    // later message could ask about (`Greet.greet()`).
                    for param in params.iter().skip(args.len() + implicit_self) {
                        if let Type::Var(_) = self.apply(param) {
                            self.unify(param, &Type::Error, span);
                        }
                    }
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
                let arg_types: Vec<Type> =
                    args.iter_mut().map(|a| self.infer_expr(a, env)).collect();
                match (other, form) {
                    // What an argument of a callee already reported
                    // leaves unknown is asked about no more
                    // (`list.map(xs, Greet.greet)` before `import list`).
                    (Type::Error, _) => {
                        for ty in &arg_types {
                            for v in free_vars_in(&self.apply(ty)) {
                                self.unify(&Type::Var(v), &Type::Error, span);
                            }
                        }
                        Type::Error
                    }
                    (Type::Never, _) => Type::Never,
                    (_, CallForm::Call) => {
                        let mut d = Diagnostic::error(
                            Code::TypeMismatch,
                            span,
                            format!("`{}` is not callable", self.show_type(other)),
                        );
                        // The `self` of an impl for every function.
                        if let Type::Rigid(r) = other
                            && let Some((what, parts)) = self.shape_vars.get(&r.var)
                        {
                            d = d.with_note(format!(
                                "in an impl for `{}`, `self` is some {what}: {parts} are not \
                                 known",
                                r.name
                            ));
                        }
                        self.errors.push(d);
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
        if !matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return self.infer_expr_kind(expr, env);
        }
        // A use of a definition with bounds: the evidence of each.
        self.ev.use_ev = None;
        let ty = self.infer_name(expr, env);
        if let Some(owed) = self.ev.use_ev.take() {
            self.ev.refs.insert(expr.span, owed);
        }
        ty
    }

    /// `infer_expr` for a name or a field access.
    fn infer_name(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        if !matches!(expr.kind, ExprKind::FieldAccess(..)) {
            return self.infer_expr_kind(expr, env);
        }
        // A method call names its method's trait: the access records it
        // (`Expr::res`), and the compiler keys the call by it.
        let outer = self.method_trait.take();
        // (An access inferred before has the trait it was given then:
        // it is decided anew.)
        if let Some(crate::defs::Res::Def(id)) = expr.res
            && self.trait_key(id).is_some()
        {
            expr.res = None;
        }
        let ty = self.infer_expr_kind(expr, env);
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
                                asker: None,
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
                                let list = Type::List(Box::new(inner.clone()));
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
                                let (first_shown, elem_shown) =
                                    self.show_apart(&self.apply(&first_ty), &self.apply(t));
                                self.error(
                                    Code::TypeMismatch,
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
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
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
                    // A type of a builtin module imported by its name
                    // (`import time.{ DateTime }`): it is no value.
                    if let Some((module, t)) = self
                        .res_type(expr.res)
                        .and_then(|t| self.other_module(t).map(|m| (m, t)))
                        .filter(|(m, t)| {
                            super::builtin_env::is_module_type_no_value(
                                &resolve(*m),
                                &resolve(t.name),
                            )
                        })
                    {
                        help = Some(super::builtin_env::bare_module_type_is_no_value_help(
                            &resolve(name),
                            &format!("{module}.{}", t.name),
                        ));
                    }
                    self.error_help(Code::UndefinedVariable, (message, help), span);
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field, _) => {
                self.receiver_taken = false;
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
                            // (A type of a builtin module: as a program
                            // writes it, whatever the module's name here.)
                            let help = self
                                .res_type(expr.res)
                                .and_then(|t| self.other_module(t).map(|m| (m, t)))
                                .filter(|(m, t)| {
                                    super::builtin_env::is_module_type_no_value(
                                        &resolve(*m),
                                        &resolve(t.name),
                                    )
                                })
                                .map(|(m, t)| {
                                    super::builtin_env::module_type_is_no_value_help(&format!(
                                        "{m}.{}",
                                        t.name
                                    ))
                                });
                            match help {
                                Some(help) => self.error_help(
                                    Code::UndefinedVariable,
                                    (message, Some(help)),
                                    span,
                                ),
                                None => self.error(Code::UndefinedVariable, message, span),
                            }
                            Type::Error
                        }
                    };
                    expr.ty = Some(ty.clone());
                    return ty;
                }
                // `Trait.method`: a method of a trait, called through it
                // (`Greet.greet(x)`, `m.Greet.greet`, `Compare.compare`).
                if let Some(tr) = self.trait_named(obj) {
                    let receiver = self.trait_receiver.take();
                    let ty = self.trait_method(tr, field, receiver, span);
                    expr.ty = Some(ty.clone());
                    return ty;
                }
                // `Type.method`: a method that makes a value, called
                // through its type (`Int.empty`, `m.Shape.default`).
                let type_ref = match (&obj.kind, obj.res) {
                    (ExprKind::Ident(name), None) => self.named_type(None, *name),
                    (ExprKind::Ident(_) | ExprKind::FieldAccess(..), res) => self.res_type(res),
                    _ => None,
                };
                if let Some(ty) = type_ref {
                    let key = (canonical_head(&self.tables.resolver, ty), field);
                    if let Some(entry) = self.method_entry(key.0, key.1) {
                        if let Some(trait_name) = entry.trait_name
                            && self.private_owner(trait_name).is_some()
                        {
                            self.private_method(trait_name, field, span);
                            expr.ty = Some(Type::Error);
                            return Type::Error;
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
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if self.ambiguous_method_call(key.0, field, span) {
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if self.of_every_shape(key.0, &entry, field, span) {
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if entry.structural {
                            // Written through an alias, the instance is the
                            // alias's (`Ints` for `type Ints = List(Int)` is
                            // a `List(Int)`), with its arguments.
                            let instance = self.type_instance(ty).map(|instance| {
                                crate::types::canonical::canonicalize(
                                    &self.tables.resolver,
                                    &instance,
                                )
                            });
                            let ty = match instance {
                                Some(inner) => {
                                    self.structural_type_method(key.0, &inner, field, span)
                                }
                                // A type of every shape (`Tuple`): its
                                // values have the method, not the type.
                                None => {
                                    self.of_every_shape_error(key.0, field, span);
                                    Type::Error
                                }
                            };
                            expr.ty = Some(ty.clone());
                            return ty;
                        }
                        self.method_trait = self.entry_trait(&entry, field);
                        let scheme = self.method_scheme(&entry);
                        let ty = self.instantiate_method(&scheme, field, span);
                        self.type_method_site(key.0, &entry, field, span);
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
                        self.no_type_method(&resolve(ty.name), field, span, Some(key.0));
                        expr.ty = Some(Type::Error);
                        return Type::Error;
                    }
                }

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                self.receiver_taken = false;
                let obj_ty = self.apply(&obj_ty);
                // Canonicalise before dispatch so a receiver whose type
                // is written through an alias lands in the arm of the
                // type it stands for.
                let obj_ty = crate::types::canonical::canonicalize(&self.tables.resolver, &obj_ty);

                // A dot after a value is a field: a record's (its type's
                // or an anonymous one's), or, on a value whose type is
                // still unknown, the field of a record that has one.
                //
                // `TypeOf(inner)` values (descriptors produced by `type a`
                // parameters or bare type names like `Int`) call the
                // methods of `inner` that make a value of it.
                if let Type::Generic(gname, gargs) = &obj_ty
                    && gname.is_builtin(crate::defs::TYPE_OF)
                    && gargs.len() == 1
                {
                    let resolved = self.resolve_type_descriptor_method(&gargs[0], field, span);
                    if let Some(ty) = resolved {
                        expr.ty = Some(ty.clone());
                        return ty;
                    }
                    // A concrete type with no such method (a type
                    // variable's case is reported above).
                    let inner = self.apply(&gargs[0]);
                    if !matches!(inner, Type::Var(_) | Type::Rigid(_) | Type::Error) {
                        self.no_type_method(&self.show_type(&inner), field, span, None);
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
                            let candidates: Vec<(Symbol, Type)> =
                                fields.iter().map(|(k, v)| (*k, v.clone())).collect();
                            self.no_field(&obj_ty, field, Some(obj), &candidates, span);
                            Type::Error
                        }
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
                            expr.ty = Some(resolved.clone());
                            return resolved;
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
                        self.error(
                            Code::UnsupportedOperation,
                            format!(
                                "tuple indexing ('t.{field}') is not supported; \
                                 destructure instead: 'let (a, b) = t'"
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Var(_) => {
                        if called {
                            // `x.f(..)`: what it calls is decided with
                            // the type of `x` (`check_call`).
                            self.unknown_receiver = Some((obj_ty.clone(), field));
                            self.fresh_var()
                        } else {
                            // `x.f`: a record with the field `f`.
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
                    Type::Error | Type::Never => {
                        // Prior error — propagate to prevent cascading false positives
                        Type::Error
                    }
                    // Any other type has no fields.
                    _ => {
                        self.no_field(&obj_ty, field, Some(obj), &[], span);
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
                // A `_` among the arguments was meant to say where the
                // piped value goes, which the resolver reports; the call
                // with the value first is not what was meant, so it is
                // not checked.
                ExprKind::Call(callee, call_args) if call_args.iter().any(is_placeholder) => {
                    self.infer_expr(lhs, env);
                    self.infer_expr(callee, env);
                    for arg in call_args {
                        self.infer_expr(arg, env);
                    }
                    self.fresh_var()
                }
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
                Type::List(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // Remember the `?` site so a later body/return
                // unify failure can point back here (see
                // `note_qmark_requirement_on_ret_mismatch`).
                self.current_qmark_spans.push(span);

                // A `?` returns from the function as `return` does: not
                // from one that never returns. (A closure's `?` ends the
                // closure: its body has a return type of its own.)
                if let (Some(Type::Never), Some(name)) = (&self.current_return_type, self.never_fn)
                {
                    self.error(
                        Code::InvalidControlFlow,
                        format!(
                            "'?' in '{name}', which is declared to return `Never`: `?` returns \
                             the `Err` or `None` it meets, and such a function never returns"
                        ),
                        span,
                    );
                }

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                // (The one check, also of an operand known only later:
                // `try_operand`.)
                match &inner_ty {
                    Type::Generic(name, args)
                        if (name.is_builtin("Result") && args.len() == 2)
                            || (name.is_builtin("Option") && args.len() == 1) =>
                    {
                        let ok = args[0].clone();
                        let ret = self.current_return_type.clone();
                        self.try_operand(&inner_ty, &ok, ret, span);
                        ok
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
                                "'?' operator requires Result or Option type, got '{}'",
                                self.show_type(&inner_ty)
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
                let owner = self.frame_owner.take();
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

                // A lambda is its own `?`/`return`
                // boundary — the VM's Op::QuestionMark pops exactly ONE
                // frame (the lambda's), so `?` inside a lambda must
                // validate against the LAMBDA's return type, not the
                // enclosing named fn's. Establish a fresh return-type
                // context for the body, mirroring check_body: checked
                // against the outer fn's return type, `{ x -> x? + 1 }`
                // would let a Variant escape into a List(Int) at runtime.
                let lambda_ret = self.fresh_var();
                let prev_return_type = self.current_return_type.replace(lambda_ret.clone());
                let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

                self.fn_frames.push(unused::FnFrame {
                    owner,
                    params: param_types.clone(),
                });
                let body_type = self.infer_expr(body, env);
                self.fn_frames.pop();
                env.pop();
                self.unify_body_result(body, &body_type, &lambda_ret);

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
                // A field written twice in a record literal
                // (e.g. `User { name: "a", name: "b" }`) is an error:
                // downstream processing goes through a HashSet that would
                // silently dedupe them. Mirror the record type
                // declaration's duplicate-field check: walk once and
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
                    for ((field_name, inferred_ty), (_, value)) in field_types.iter().zip(&*fields)
                    {
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
                // A field written twice in a record-update
                // expression (e.g. `r.{ age: 1, age: 2 }`) would be
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
                //  1. An anonymous record type — validate against its
                //     fields and row.
                //  2. `Type::Generic(name, args)`, a declared record —
                //     validate against the declared fields.
                //  3. Anything else — compile-time reject.
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
                            self.unify_field(*field_name, &ft, declared_ty, field_expr.span);
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
                            // Mirror the sibling
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
                                "record update requires a record base, but '{}' is not a record type",
                                self.show_type(&resolved)
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
                    // A spread makes an anonymous record of the base's
                    // fields and the written ones, once the base's type
                    // is decided (`Goal::Rebuild`).
                    let base = self.infer_expr(base_expr, env);
                    let result = self.fresh_var();
                    self.want_goal(
                        Goal::Rebuild {
                            base,
                            without: Vec::new(),
                            with: new_field_tys,
                            result: result.clone(),
                            rest: false,
                        },
                        Origin {
                            span,
                            callee: None,
                            op: None,
                        },
                    );
                    self.apply(&result)
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

                        // Every arm checks its pattern
                        // against the SAME scrutinee, so a wrong-scrutinee
                        // match (`match 42` with an `Ok(v)` and an `Err(e)` arm)
                        // would print the identical "expected Result(_, _),
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
                                        // so `self.error(...)` does not apply.
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
                            self.unify(&arm_ty, &result_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        // Skipped when the scrutinee type already failed to
                        // match the arms — exhaustiveness over a broken match
                        // is a cascade, not new information.
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
                            self.unify(&arm_ty, &result_ty, body_span);
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
                match (&self.current_return_type, self.never_fn) {
                    // (A closure's `return` ends the closure: its body
                    // has a return type of its own.)
                    (Some(Type::Never), Some(name)) => self.error(
                        Code::InvalidControlFlow,
                        format!(
                            "'return' in '{name}', which is declared to return `Never`: such a \
                             function never returns"
                        ),
                        span,
                    ),
                    (Some(expected_ret), _) => {
                        let expected_ret = expected_ret.clone();
                        self.unify(&ret_val_ty, &expected_ret, span);
                    }
                    (None, _) => {}
                }
                Type::Never
            }

            ExprKind::Block(stmts) => {
                let mut last_ty = Type::Unit;
                env.push();
                let last = stmts.len().saturating_sub(1);
                for (i, stmt) in stmts.iter_mut().enumerate() {
                    last_ty = self.infer_stmt(stmt, env);
                    if i != last {
                        self.note_statement(stmt, &last_ty);
                    }
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
                // In `let f = { … }` the closure is `f` inside itself: of
                // one type there, the closure's, which is generalised
                // only after it is checked.
                let mut val_ty = if let (PatternKind::Ident(name), ExprKind::Lambda { .. }) =
                    (&pattern.kind, &value.kind)
                {
                    self.frame_owner = Some(unused::FrameOwner::Local(*name));
                    let itself = self.fresh_var();
                    env.push();
                    env.define(*name, Scheme::mono(itself.clone()));
                    let val_ty = self.infer_expr(value, env);
                    env.pop();
                    self.unify(&itself, &val_ty, value_span);
                    val_ty
                } else {
                    self.infer_expr(value, env)
                };

                // The type variables the annotation introduces.
                let mut own: Vec<RigidId> = Vec::new();
                if let Some(te) = &ty {
                    let (declared, introduced) = self.resolve_let_annotation(te);
                    own = introduced;
                    let reported = self.errors.len();
                    self.unify(&val_ty, &declared, value_span);
                    // A value of unknown type (from a module that failed
                    // to load) takes the declared type: `let y: Int = x`
                    // makes `y` an Int. So does a value that is not of
                    // the declared type: that is reported here, and what
                    // follows reads the name as it is declared.
                    if self.errors.len() > reported || matches!(self.apply(&val_ty), Type::Error) {
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
                            self.passing_on =
                                matches!(value.kind, ExprKind::Lambda { .. }).then_some(*name);
                            let scheme = self.generalize_let(&val_ty, &own);
                            self.passing_on = None;
                            let bounds = self.trait_bounds(&scheme.preds);
                            if !bounds.is_empty() {
                                self.ev.lets.insert(value_span, bounds);
                            }
                            scheme
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
                        "'when let' else body must diverge — end it with 'return', 'panic', a call \
                         of a function that returns `Never` or, inside a loop, 'loop(...)'"
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
                        "'when' else body must diverge — end it with 'return', 'panic', a call of \
                         a function that returns `Never` or, inside a loop, 'loop(...)'"
                            .to_string(),
                        else_body.span,
                    );
                }

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    /// Walk a supertrait-arg `TypeExpr` and emit a
    /// diagnostic at every `Named(sym)` that refers to a *parametric*
    /// user-defined type (enum, record, or alias with arity > 0) used
    /// without its arguments — e.g. `trait Sub: Super(Box)` where
    /// `type Box(a) { ... }`. `resolve_supertrait_arg` falls through
    /// such names to `Type::Generic(sym, [])`, which then never matches
    /// any downstream impl's actual args, so the obligation would
    /// silently go unsatisfied. With this check the user sees a clean arity
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
                    operator_message(tr, op, offender, &self.show_type(offender)),
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

/// What is said of an operand of the type `ty`, which a message writes
/// as `shown`, that the operator `op`, of the builtin trait `tr`, is
/// not for.
pub(super) fn operator_message(tr: &str, op: &str, ty: &Type, shown: &str) -> String {
    match tr {
        "Number" => arith_operand_message(op, ty, shown),
        "Equal" => format!("operator {op} requires a comparable type, got '{shown}'"),
        _ => format!(
            "operator {op} requires Int, Float, String, Bool, List, Tuple, Record, or Variant, got '{shown}'"
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
/// INVARIANT: the shapes produced here must be
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
            // Mirror `resolve_type_expr` (mod.rs Named arm)
            // exactly for primitives: a Unit/() falling
            // through to `Type::Generic` would never compare
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
            // Builtin container heads must produce the same
            // canonical `Type::…` shapes `resolve_type_expr` builds for
            // the impl side (mod.rs Generic arm). `canonicalize()` does
            // NOT collapse `Generic("List",[T])` onto `Type::List(T)`,
            // so leaving these as `Type::Generic` would make supertrait args
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
pub(super) fn arith_operand_message(op_str: &str, ty: &Type, shown: &str) -> String {
    let msg = match op_str.starts_with("unary") {
        true => format!("{op_str} requires Int or Float, got '{shown}'"),
        false => format!("operator {op_str} requires Int or Float, got '{shown}'"),
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
            // A name through its module or its enum: `m.f`, `Color.Red`;
            // a method through its trait or its type: `Greet.greet`,
            // `Box.default`.
            ExprKind::FieldAccess(obj, ..) => {
                matches!(expr.res, Some(crate::defs::Res::Def(_)))
                    || self.trait_named(obj).is_some()
                    || self.res_type(obj.res).is_some()
            }
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

/// The first type variable `ty` gives as a type argument (`TypeOf(a)`),
/// as a type.
pub(super) fn type_of_var(ty: &Type) -> Option<Type> {
    match ty {
        Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => args
            .first()
            .filter(|arg| matches!(arg, Type::Var(_)))
            .cloned(),
        Type::Fun(params, ret) => params.iter().chain([&**ret]).find_map(type_of_var),
        _ => None,
    }
}

/// `traits` with each trait once, in the order of their first places
/// (two bounds of one trait at different arguments name it twice).
pub(super) fn distinct(traits: &mut Vec<TraitKey>) {
    let mut seen: Vec<TraitKey> = Vec::new();
    traits.retain(|t| {
        let first = !seen.contains(t);
        if first {
            seen.push(*t);
        }
        first
    });
}
