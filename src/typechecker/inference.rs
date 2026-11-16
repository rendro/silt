//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! and check_body.

use std::ops::ControlFlow;

use super::infer::pattern::*;
use super::names::is_placeholder;
use super::suggest::suggest_similar;
use super::*;
use crate::parser::{NO_BREAK, NO_IF, NO_IF_HELP, NO_WHILE_FOR};

// The arms of `infer_expr_kind`, by family of forms.
mod control;
mod literal;
mod name;
mod operator;
mod record;

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
/// Lock: the goldens tests/golden/lang/errors/diagnostic_suggestion__*.
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
        "break" | "continue" => Some(NO_BREAK),
        "while" | "for" => Some(NO_WHILE_FOR),
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
        let mut in_scope = self.bounds.remove(&var).unwrap_or_default();
        self.bound_closure(&mut in_scope, trait_name, args);
        self.bounds.insert(var, in_scope);
    }

    /// Add the bound `trait_name(args)` to the bounds `in_scope` of one
    /// variable, with its supertraits.
    pub(super) fn bound_closure(
        &self,
        in_scope: &mut Vec<(TraitKey, Vec<Type>)>,
        trait_name: TraitKey,
        args: Vec<Type>,
    ) {
        // (Two bounds of one trait at different arguments are two
        // bounds: `where a: Conv(Int) + Conv(String)`.)
        if in_scope.iter().any(|(t, a)| *t == trait_name && *a == args) {
            return;
        }
        in_scope.push((trait_name, args.clone()));
        let Some(info) = self.tables.traits.get(&trait_name) else {
            return;
        };
        for (i, super_name) in info.supertraits.iter().enumerate() {
            let super_args: Vec<Type> = info.supertrait_args_at(i, &args);
            self.bound_closure(in_scope, *super_name, super_args);
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
        // (A refusal of `T.decode` names it so, a type of another module
        // with its module: `mail.Box.decode` beside a `Box` of the
        // module's own.)
        let callee = match self.other_module(head) {
            Some(module) => intern(&format!("{module}.{}.{method}", head.name)),
            None => intern(&format!("{head}.{method}")),
        };
        // (The decoder a note names, through a function that hands its
        // type on: `bm` passes it to `Box.decode`.)
        let asker = Asker {
            by: Asked::Builtin(callee),
            passer: None,
        };
        let ev = self.want(
            Pred::Trait {
                tr,
                args: Vec::new(),
                subject: recv.clone(),
                asker: Some(asker),
            },
            Origin {
                span,
                callee: Some(callee),
                op: None,
                header: None,
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
                header: None,
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
                header: None,
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

    /// A method of `tr`, a trait that has `json.Decode` among its
    /// supertraits, called on the type `ty` (`Box(Int).make(s)`):
    /// `ty` is what the decoder builds, as through a bound of the trait,
    /// and is judged so (a type with no decoder, a generic type with
    /// impls for other instantiations only, an opaque type of another
    /// module).
    /// `callee` is the call as it is written (`Box.make`, `Dec.again`).
    fn decoded_through_type(&mut self, tr: TraitKey, ty: &Type, callee: String, span: Span) {
        if !self.implies_decode(tr) {
            return;
        }
        // (A type without arguments is judged where its impl is declared.
        // Of one whose arguments are not known yet, what the code decides
        // of them later is judged: `settle_decodes_through_calls`.)
        let ty = self.apply(ty);
        let Some(head) = self.type_name_for_impl(&ty) else {
            return;
        };
        if !self.generic_for_decode(head) || self.type_args_of(&ty).is_empty() {
            return;
        }
        let origin = Origin {
            span,
            callee: Some(intern(&callee)),
            op: None,
            header: None,
        };
        // (The trait's methods are definitions of its module, which
        // decodes its own opaque types.)
        let owners: Vec<crate::defs::ModuleId> = self
            .def(tr.id.0)
            .map(|def| def.module)
            .into_iter()
            .collect();
        self.want_decode_through_call(&ty, origin, owners);
    }

    /// The evidence of `Type.method` at `span`, the method of an impl
    /// reached through its type `head`, just instantiated: as a call of
    /// the impl's method passes it on (`method_site`).
    fn type_method_site(
        &mut self,
        head: TypeRef,
        entry: &MethodEntry,
        method: Symbol,
        full: Option<Type>,
        span: Span,
    ) {
        let owed = self.ev.use_ev.take().unwrap_or_default();
        if let Some(key) = self.entry_impl(head, entry, method) {
            let site = self.method_site(key, method, owed, full);
            self.ev.calls.insert(span, site);
        }
    }

    /// The type `Self` stands for in `ty`, the type of a use of the
    /// method `method` of an impl reached through its type `head`
    /// (`Box(Int).mk`), if the impl's dictionary carries its type in full.
    fn full_self(
        &mut self,
        head: TypeRef,
        entry: &MethodEntry,
        method: Symbol,
        ty: &Type,
    ) -> Option<Type> {
        let key = self.entry_impl(head, entry, method)?;
        if !self.carries_full_type(key) {
            return None;
        }
        self.self_of(key.0, method, ty)
    }

    /// The type `Self` stands for in `ty`, the type of a use of the
    /// method `method` of `tr`: the trait's declaration of the method
    /// matched against `ty`.
    fn self_of(&mut self, tr: TraitKey, method: Symbol, ty: &Type) -> Option<Type> {
        let info = self.tables.traits.get(&tr)?;
        let self_var = info.self_var;
        let (_, declared) = info.methods.iter().find(|(n, _)| *n == method)?.clone();
        let mapping: HashMap<TyVar, Type> = (free_vars_in(&declared).into_iter())
            .map(|v| (v, self.fresh_var()))
            .collect();
        let self_ty = mapping.get(&self_var)?.clone();
        let _ = self.unify_types(&substitute_vars(&declared, &mapping), ty);
        Some(self_ty)
    }

    /// Refuse `ty`, a generic type the receiver `obj` names with no type
    /// arguments, before a dot that calls a method of `tr` (`Box.mk(s)`):
    /// a type with arguments is written with them (`Box(Int).mk(s)`).
    /// Whether it was. (A trait of type constructors is called on the
    /// constructor alone: `Option.pure(1)`.)
    fn bare_type_receiver(&mut self, obj: &Expr, ty: TypeRef, tr: Option<TraitKey>) -> bool {
        if tr.is_some_and(|tr| self.is_constructor_trait(tr)) {
            return false;
        }
        let shown = written_receiver(obj).unwrap_or_else(|| resolve(ty.name));
        self.bare_generic(&shown, ty, obj.span)
    }

    /// Make `Self` of `ty`, the type of a use at `span` of the method
    /// `method` of `tr`, the type the receiver names (`Box(Int)` of
    /// `Box(Int).mk(s)`, the type an alias stands for). A trait of type
    /// constructors is called on the constructor alone: a type with
    /// arguments the receiver writes (`Option(String)`, an alias of one:
    /// `written`) is refused there.
    fn self_is(
        &mut self,
        tr: TraitKey,
        method: Symbol,
        (ty, named, written): (&Type, &Type, bool),
        span: Span,
    ) {
        if self.is_constructor_trait(tr) {
            if written
                && !self.type_args_of(named).is_empty()
                && let Some(head) = self.type_name_for_impl(named)
            {
                let args = self
                    .method_call_args
                    .clone()
                    .unwrap_or_else(|| "..".to_string());
                self.error_help(
                    Code::TypeMismatch,
                    (
                        format!(
                            "'{}' is a type, but `{method}` is a method of trait '{}' of type \
                             constructors",
                            self.show_type(named),
                            self.show_trait(tr)
                        ),
                        Some(format!(
                            "call it on the type constructor: `{}.{method}({args})`",
                            self.spell_type(head)
                        )),
                    ),
                    span,
                );
            }
            return;
        }
        if let Some(self_ty) = self.self_of(tr, method, ty) {
            self.unify(named, &self_ty, span);
        }
    }

    /// Refuse the call at `span` of the method `field` of `tr` on a value
    /// of the type `recv` by the impl `key`, if the impl's dictionary
    /// carries its type in full (`Box(Int)`) and a type variable in
    /// `recv` has no value where the code runs: no `type a` parameter
    /// gives it, nor the impl whose method the code is in.
    pub(super) fn refuse_type_without_value(
        &mut self,
        key: ImplKey,
        recv: &Type,
        method: Option<(TraitKey, Symbol)>,
        callee: Option<Symbol>,
        span: Span,
    ) {
        if !self.carries_full_type(key)
            || self.full_type_refused.contains(&span)
            || (self.undecided_decodes.iter()).any(|(at, _)| *at == span)
        {
            return;
        }
        let recv = self.apply(recv);
        let names: Vec<Symbol> = (rigid_vars(&recv).into_iter())
            .filter(|r| {
                let header =
                    self.decode_params.contains(&r.name) && self.header_params.contains(&r.name);
                !(self.valued_vars.contains(&r.var) || header)
            })
            .map(|r| r.name)
            .collect();
        if names.is_empty() {
            return;
        }
        self.full_type_refused.insert(span);
        // (A variable of an impl's header has no `type a` parameter to
        // be given by.)
        let takeable: Vec<Symbol> = (names.iter().copied())
            .filter(|name| !self.header_params.contains(name))
            .collect();
        let (said, help) = known_only_by_type(&names, &takeable);
        let what = match method {
            Some((tr, field)) => format!("method '{field}' of trait '{}'", self.show_trait(tr)),
            None => match callee {
                Some(callee) => format!("`{callee}`"),
                None => format!("trait '{}'", self.show_trait(key.0)),
            },
        };
        self.error_help(
            Code::InvalidMethodCall,
            (
                format!(
                    "{what} needs the type `{}` in full here: {said}",
                    self.show_type(&recv)
                ),
                help,
            ),
            span,
        );
    }

    /// Whether the dictionaries of the impl `key` carry its type in full
    /// (the compiler's `Template::full`): an impl of a trait passed as a
    /// dictionary for a generic type over its parameters, whose header
    /// asks `json.Decode` of one.
    pub(super) fn carries_full_type(&self, key: ImplKey) -> bool {
        crate::defs::has_dictionary(key.0.id)
            && (self.tables.impl_self_types.get(&key))
                .is_some_and(|ty| !free_vars_in(ty).is_empty())
            && (self.tables.impl_preds.get(&key).into_iter().flatten())
                .any(|pred| matches!(pred, Pred::Trait { tr, .. } if Self::is_decode(*tr)))
    }

    /// The impl of the type `head` whose method `method` is `entry`.
    fn entry_impl(&self, head: TypeRef, entry: &MethodEntry, method: Symbol) -> Option<ImplKey> {
        let tr = self.entry_trait(entry, method)?;
        Some((self.tables.one_impl(tr, head)).unwrap_or((tr, head, ImplArgs::none())))
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
            header: None,
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
    /// The type variable the signature being checked (or its impl's
    /// header) names `name`, as its body sees it.
    fn sig_type_var(&self, name: Symbol) -> Option<Type> {
        let ty = self.sig_names.get(&name)?;
        match self.apply(ty) {
            Type::Var(v) => Some(self.rigid_of.get(&v).cloned().unwrap_or(Type::Var(v))),
            ty @ Type::Rigid(_) => Some(ty),
            _ => None,
        }
    }

    pub(super) fn resolve_type_descriptor_method(
        &mut self,
        inner: &Type,
        field: Symbol,
        span: Span,
    ) -> Option<Type> {
        let inner = self.apply(inner);
        match &inner {
            // A type still unknown has no methods to look up.
            Type::Var(_) | Type::Rigid(_) if let Some(name) = self.var_as_type => {
                let bounds: Vec<TraitKey> = match &inner {
                    Type::Rigid(r) => (self.bounds.get(&r.var).into_iter().flatten())
                        .map(|(t, _)| *t)
                        .collect(),
                    _ => Vec::new(),
                };
                // (A bound refused where it was written is not reported
                // again.)
                if matches!(&inner, Type::Rigid(r) if self.unknown_bounds.contains(&r.var)) {
                    return None;
                }
                if bounds.is_empty() {
                    // (The one trait in sight with a method of the name
                    // that makes a value is the bound to add.)
                    let help = match self.value_makers(field) {
                        (makers, _) if makers.len() == 1 => format!(
                            "add the bound: `where {name}: {}`",
                            self.show_trait(makers[0])
                        ),
                        _ => format!(
                            "add a bound whose trait has the method: `where {name}: SomeTrait`"
                        ),
                    };
                    self.error_help(
                        Code::UnknownMethod,
                        (
                            format!(
                                "no method '{field}' on type variable `{name}`: it has no bounds"
                            ),
                            Some(help),
                        ),
                        span,
                    );
                    return None;
                }
                if let Type::Rigid(r) = &inner
                    && self.bound_methods(*r, field).is_empty()
                {
                    let mut shown: Vec<TraitKey> = bounds.clone();
                    distinct(&mut shown);
                    let shown: Vec<String> = shown.iter().map(|t| self.show_trait(*t)).collect();
                    self.error(
                        Code::UnknownMethod,
                        format!(
                            "no method '{field}' on type variable `{name}`: its bounds ({}) give none",
                            shown.join(" + ")
                        ),
                        span,
                    );
                    return None;
                }
                // A method of a trait found by the value (`json.Decode`'s
                // `decode`) has nothing to find it by on a type variable
                // no value is named: where the code runs, only a `type a`
                // parameter, or the type a generic type's `decode` is
                // called at, says what `a` is. (`Self` in a trait's
                // default method is the type of the impl whose
                // dictionary it runs with.)
                if let Type::Rigid(r) = &inner
                    && let Some((tr, _)) = (self.bound_methods(*r, field).into_iter())
                        .find(|(tr, _)| !crate::defs::has_dictionary(tr.id))
                    && !self.decode_params.contains(&name)
                    && !(name == intern("Self") && self.default_body_of.is_some())
                {
                    // (A variable of an impl's header has no `type a`
                    // parameter to be given by.)
                    let help = (!self.header_params.contains(&name)).then(|| {
                        format!(
                            "take the type as a parameter, `type {name}`, and call \
                             `{name}.{field}(..)` on it"
                        )
                    });
                    self.error_help(
                        Code::InvalidMethodCall,
                        (
                            format!(
                                "method '{field}' of trait '{}' is called on a type variable that \
                                 has no value here: `{name}` is known only by its type",
                                self.show_trait(tr)
                            ),
                            help,
                        ),
                        span,
                    );
                    return None;
                }
                let inner = inner.clone();
                self.var_as_type = None;
                self.descriptor_var = Some(name);
                let found = self.resolve_type_descriptor_method(&inner, field, span);
                self.descriptor_var = None;
                self.var_as_type = Some(name);
                found
            }
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
                // Bounds of one trait with parameters (at one trait
                // argument or at several): the method as the trait declares
                // it, with the variable for `Self`; which bound the use
                // reaches its trait arguments say, as through the trait.
                // At several, a method whose type mentions no parameter of
                // the trait can never say which: ambiguous, as below.
                let mut traits: Vec<TraitKey> = matches.iter().map(|(t, _)| *t).collect();
                distinct(&mut traits);
                if let [tr] = traits.as_slice()
                    && (self.tables.traits.get(tr)).is_some_and(|info| {
                        !info.params.is_empty()
                            && (matches.len() == 1
                                || (info.methods.iter()).any(|(name, ty)| {
                                    *name == field
                                        && (free_vars_in(ty).iter())
                                            .any(|v| info.param_var_ids.contains(v))
                                }))
                    })
                    && self.self_param(*tr, field).is_none()
                {
                    let tr = *tr;
                    self.method_trait = Some(tr);
                    return Some(self.through_trait(tr, field, inner.clone(), None, span));
                }
                if matches.len() > 1 {
                    let traits: Vec<TraitKey> = matches.iter().map(|(t, _)| *t).collect();
                    // (A type variable used as a type is named as written.)
                    let on = match self.descriptor_var {
                        Some(name) => format!("`{name}`"),
                        None => format!("`type {inner}`"),
                    };
                    self.ambiguous_bound_method(field, &on, *r, &traits, span);
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
                if let Some(tr) = self.trait_of_impls_by_args(name, field)
                    && self.self_param(tr, field).is_none()
                    && self.private_owner(tr).is_none()
                {
                    self.method_trait = Some(tr);
                    return Some(self.through_trait(tr, field, inner.clone(), None, span));
                }
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
                self.instance_header = self.entry_impl(name, &entry, field);
                let instantiated = self.instantiate_method(&scheme, field, span);
                if let Some(tr) = self.method_trait {
                    // (A generic type's name alone, `Option`, holds open
                    // arguments.)
                    let written = !self.holds_open_vars(&inner);
                    self.self_is(tr, field, (&instantiated, &inner, written), span);
                    self.decoded_through_type(tr, &inner, format!("{}.{field}", name.name), span);
                    // (`Box(a).mk(s)` with no value of `a` where the code
                    // runs: `refuse_type_without_value`.)
                    if let Some(key) = self.entry_impl(name, &entry, field)
                        && self.carries_full_type(key)
                    {
                        (self.full_type_uses).push((inner.clone(), tr, field, span));
                    }
                }
                self.type_method_site(name, &entry, field, Some(inner.clone()), span);
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

    /// The type of an argument of the type `arg_ty` for the parameter
    /// `param`: a type given to a `type m` that the callee's signature
    /// applies is the type constructor of the type (`Option(_)` for
    /// `Option`, `Result(_, e)` for `Result`); a type of no type argument
    /// is an error (`shown`: the call's arguments as the help writes
    /// them, the type argument's place open), and so is a type written
    /// in full (`List(Int)`, an alias of it: `full`).
    fn constructor_argument(
        &mut self,
        param: &Type,
        (arg_ty, full): (Type, bool),
        callee: &Expr,
        shown: (&[String], usize),
        at: Span,
    ) -> Type {
        let Type::Generic(name, of) = self.apply(param) else {
            return arg_ty;
        };
        let ([m], Type::Generic(given_name, given)) = (of.as_slice(), self.apply(&arg_ty)) else {
            return arg_ty;
        };
        // (The variable may be decided by an earlier argument already:
        // `fn tp(x: m(Int), type m)`.)
        if !name.is_builtin(crate::defs::TYPE_OF)
            || !given_name.is_builtin(crate::defs::TYPE_OF)
            || !(self.of_constructors(m) || matches!(m, Type::Con(..)))
        {
            return arg_ty;
        }
        let [given] = given.as_slice() else {
            return arg_ty;
        };
        let given = self.apply(given);
        if matches!(given, Type::Var(_) | Type::Error | Type::Con(..))
            || self.of_constructors(&given)
        {
            return arg_ty;
        }
        match given.split_constructor().filter(|_| !full) {
            Some((head, rest, _)) => Type::type_of(Type::Con(head, rest)),
            None => {
                // (The parameter's variable, as its signature names it.)
                let var = match param {
                    Type::Generic(_, of) => match of.first() {
                        Some(Type::Var(v)) => Some(*v),
                        Some(Type::Rigid(r)) => Some(r.var),
                        _ => None,
                    },
                    _ => None,
                };
                let named = (var.and_then(|var| self.tables.constructor_names.get(&var)))
                    .map_or_else(|| "m".to_string(), |name| resolve(*name));
                let message = format!(
                    "'{}' is a type, but '{named}' is a type constructor",
                    self.show_type(&given)
                );
                let (args, place) = shown;
                let mut args = args.to_vec();
                let help = super::names::shown_path(callee).map(|callee| {
                    args[place] = "Option".to_string();
                    format!(
                        "give a type that takes a type argument, as in `{callee}({})`",
                        args.join(", ")
                    )
                });
                self.error_help(Code::KindMismatch, (message, help), at);
                Type::Error
            }
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
        if self.self_param(tr, field).is_none() {
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
                .with_help(match info.constructor {
                    // (Of a trait of type constructors, on a constructor.)
                    true => {
                        let args = match method_ty {
                            Type::Fun(params, _) if params.len() == 1 => "x",
                            Type::Fun(params, _) if !params.is_empty() => "..",
                            _ => "",
                        };
                        format!(
                            "call it on a type constructor, `Option.{field}({args})`, or on a type \
                             variable with the bound, `m.{field}({args})`"
                        )
                    }
                    false => format!(
                        "call it on a type, {}, or on a type variable with the bound, {}",
                        on_type_call("T", field, method_ty),
                        on_type_call("a", field, method_ty)
                    ),
                }),
            );
            return Type::Error;
        }
        // Called with its first argument a value of a type that has an
        // impl written: the method of that impl, as a function of the
        // impl's module calls it, owing what the impl's header and the
        // method's body ask (`dispatch_method_entry`).
        // (Of a trait with parameters, the method's type is the trait's:
        // which impl the call reaches its trait arguments say; of impls
        // for some instantiations of a type, the receiver's whole type.)
        if let Some((recv, _)) = &receiver
            && info.params.is_empty()
        {
            let recv = self.apply(recv);
            let entry = match crate::types::canonical::canonicalize(&self.tables.resolver, &recv) {
                Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => None,
                canon => (self.type_name_for_impl(&canon)).and_then(|head| {
                    if self.has_instances(tr, head) {
                        return None;
                    }
                    let entry = self.tables.impl_methods.get(head, field, tr)?;
                    (!entry.structural).then(|| (head, entry.clone()))
                }),
            };
            // (Of a trait that has `json.Encode` or `json.Decode` among
            // its supertraits, the call encodes or decodes the value's type
            // as a bound of the trait does: an opaque type of another
            // module, for a definition of that module alone, as the
            // trait's methods are.)
            if entry.is_some()
                && let Some((_, at)) = &receiver
                && let Some(refusal) = self.opaque_through_trait(tr, &recv, *at)
            {
                self.refuse_opaque(*at, refusal);
                return Type::Error;
            }
            if let Some((head, entry)) = entry {
                // (What it decodes is the value's type, as a bound of the
                // trait decodes it.)
                let callee = format!("{}.{field}", self.spell_trait(tr));
                self.decoded_through_type(tr, &recv, callee, span);
                self.method_trait = Some(tr);
                self.trait_call_self.insert(span, recv.clone());
                self.receiver_taken = true;
                let owed_before = self.wanted.len();
                let scheme = self.method_scheme(&entry);
                let key = (self.tables.one_impl(tr, head)).unwrap_or((tr, head, ImplArgs::none()));
                self.instance_header = Some(key);
                let ty = self.instantiate_method(&scheme, field, span);
                // The impl's header's predicates come first among the
                // method's.
                let owed = self.ev.use_ev.take().unwrap_or_default();
                let site = self.method_site(key, field, owed, Some(recv.clone()));
                self.ev.calls.insert(span, site);
                if let Type::Fun(params, _) = &ty
                    && let Some(first) = params.first()
                {
                    self.unify(&recv, first, span);
                }
                if self.carries_full_type(key) {
                    (self.full_type_uses).push((recv.clone(), tr, field, span));
                }
                self.solve_wanted(owed_before);
                return self.apply(&ty);
            }
        }
        // `Self` is the receiver's type, or, of a trait of type
        // constructors, its constructor (`Option(_)` of `Option(Int)`).
        let (self_ty, self_var) = self.fresh_tv();
        // (A type constructor variable a message writes `m`, as HK8's
        // inferred `fn twice(xs: m(Int)) -> m(Int)`.)
        if info.constructor {
            self.tables.constructor_names.insert(self_var, intern("m"));
        }
        if let Some((recv, _)) = &receiver {
            let receiver_ty = match info.constructor {
                true => Type::App(Box::new(self_ty.clone()), Box::new(self.fresh_var())),
                false => self_ty.clone(),
            };
            self.unify(recv, &receiver_ty, span);
        }
        let at = receiver.as_ref().map(|(_, at)| *at);
        let ty = self.through_trait(tr, field, self_ty, at, span);
        // The receiver is the method's `Self(a)` parameter, its element
        // the method's `a` (`String` of `m(String)`): the call does not
        // unify the receiver it takes with that parameter again.
        if info.constructor
            && let Some((recv, recv_at)) = &receiver
            && let Type::Fun(params, _) = &ty
            && let Some(param) = (self.self_param(tr, field)).and_then(|i| params.get(i))
        {
            let param = param.clone();
            self.unify(recv, &param, *recv_at);
            return self.apply(&ty);
        }
        ty
    }

    /// The type of a use of the method `field` of the trait `tr` on the
    /// type `self_ty` (at the receiver `receiver` of a call through the
    /// trait, if it has one): the method's type as the trait declares it,
    /// with `self_ty` for `Self` and new variables for the trait's
    /// parameters. The use owes `self_ty: tr(..)` at those, whose evidence
    /// is the impl the call reaches, and the method's own bounds.
    pub(super) fn through_trait(
        &mut self,
        tr: TraitKey,
        field: Symbol,
        self_ty: Type,
        receiver: Option<Span>,
        span: Span,
    ) -> Type {
        // (What the call reads of the trait, not the whole of it.)
        let Some(info) = self.tables.traits.get(&tr) else {
            return Type::Error;
        };
        let Some((_, method_ty)) = info.methods.iter().find(|(n, _)| *n == field).cloned() else {
            return Type::Error;
        };
        let (self_var, param_var_ids) = (info.self_var, info.param_var_ids.clone());
        let bounds = info.method_bounds.get(&field).cloned().unwrap_or_default();
        let method_ty = &method_ty;
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        mapping.insert(self_var, self_ty.clone());
        let params: Vec<Type> = param_var_ids
            .iter()
            .map(|v| {
                let fresh = self.fresh_var();
                mapping.insert(*v, fresh.clone());
                fresh
            })
            .collect();
        let ty = substitute_vars(method_ty, &mapping);
        let preds: Vec<Pred> = (bounds.iter())
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
        let recv_ev = self.want(
            Pred::Trait {
                tr,
                args: params,
                subject: self_ty.clone(),
                asker: None,
            },
            Origin {
                span: receiver.unwrap_or(span),
                callee: Some(field),
                op: None,
                header: None,
            },
        );
        self.method_trait = Some(tr);
        // (Its impl is known once the body is: `refuse_type_without_value`.
        // A use without a receiver, `Tr.m` passed as a value, is refused
        // as a call is.)
        (self.full_type_uses).push((self_ty.clone(), tr, field, span));
        if let Some(at) = receiver {
            (self.full_type_receivers).insert(at, (span, tr, field));
        }
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
        // (A tuple that holds a type is not named by a type: `(type Int,
        // type Int)` is no type a program writes, and `(Int, Int)` before
        // the dot is the tuple type, which may have the method.)
        let base = match ty {
            Type::Tuple(_) if holds_held_type(&self.apply(ty)) => {
                format!("a tuple that holds a type has no field '{field}'")
            }
            _ => format!("type '{}' has no field '{field}'", self.show_type(ty)),
        };
        let placeholder = match ty {
            Type::List(_) => "xs",
            _ => "x",
        };
        let written = match recv.map(|recv| &recv.kind) {
            Some(ExprKind::Ident(name)) => resolve(*name),
            _ => placeholder.to_string(),
        };
        // A value named as a type variable of the signature whose bound has
        // the method: the value is meant, as always.
        let hidden = match recv.map(|recv| &recv.kind) {
            Some(ExprKind::Ident(name)) => match self.sig_type_var(*name) {
                Some(Type::Rigid(r)) if !self.bound_methods(r, field).is_empty() => Some(*name),
                _ => None,
            },
            _ => None,
        };
        let help = match hidden {
            Some(name) => Some(format!(
                "`{name}` is also a type variable here, and the value is meant: rename one to \
                 call the method of the type variable's bound"
            )),
            None if let Some(help) =
                recv.and_then(|recv| self.variant_hides_trait(recv, field)) =>
            {
                Some(help)
            }
            None => match self.method_help(ty, field, &written) {
                Some(help) => Some(help),
                None => format_record_field_suggestion(base.clone(), field, fields).1,
            },
        };
        self.error_help(Code::UnknownField, (base, help), span);
    }

    /// The help for `V.field`, where the name `V` is a variant and a
    /// trait in sight of the same name has a method `field`: the
    /// variant hides the trait in an expression (a declaration of the
    /// module comes before the prelude's).
    fn variant_hides_trait(&self, recv: &Expr, field: Symbol) -> Option<String> {
        let ExprKind::Ident(name) = recv.kind else {
            return None;
        };
        let Some(crate::defs::Res::Def(id)) = recv.res else {
            return None;
        };
        let crate::defs::DefKind::Variant { ty, .. } = self.def(id)?.kind else {
            return None;
        };
        let written = resolve(name);
        // (A method no program calls, `Equal.equal`, `Display.display`,
        // is no reason to rename: `method_help` says what to write.)
        let hides = (self.tables.traits.iter())
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == field))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t) && self.private_owner(*t).is_none())
            .filter(|t| uncallable_builtin(*t, field, &written).is_none())
            .any(|t| self.spell_trait(t) == written);
        if !hides {
            return None;
        }
        let enum_name =
            (self.spell_def(ty.0)).or_else(|| self.def(ty.0).map(|d| resolve(d.name)))?;
        Some(format!(
            "`{written}` here is the variant `{enum_name}.{written}`, which hides the trait \
             `{written}` in expressions: rename the variant to call `{written}.{field}`"
        ))
    }

    /// The refusal of a record update whose base is of the type `ty`,
    /// not a record: a type a parameter holds (`t.{ n: 3 }` after
    /// `t.zero()`) is named by the parameter, as a type.
    pub(super) fn not_a_record_base(&self, ty: &Type) -> String {
        let shown = match self.apply(ty) {
            Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                let held = self.apply(&args[0]);
                let shown = match held {
                    Type::Var(v) => self.type_holder(v).map(resolve),
                    _ => None,
                };
                let shown = shown.unwrap_or_else(|| self.show_type(&held));
                return format!("record update requires a record base, got the type `{shown}`");
            }
            other => self.show_type(&other),
        };
        format!("record update requires a record base, but '{shown}' is not a record type")
    }

    /// The traits in sight with a method `field` that makes a value
    /// (takes none of the type), of a type, not of a type constructor,
    /// and those with one of that name that takes a value: `t.field(..)`
    /// on a `t` still unknown calls the method of the one such trait, if
    /// it is the only trait with the name.
    fn value_makers(&self, field: Symbol) -> (Vec<TraitKey>, Vec<TraitKey>) {
        let mut traits: Vec<TraitKey> = (self.tables.traits.iter())
            .filter(|(_, info)| !info.constructor)
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == field))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t) && self.private_owner(*t).is_none())
            .collect();
        traits.sort_by_key(|t| self.show_trait(*t));
        let (makers, taking) =
            (traits.into_iter()).partition(|t| self.self_param(*t, field).is_none());
        (makers, taking)
    }

    /// The traits of type constructors in sight with a method `field`
    /// that makes a value (`Applicative`'s `pure`): `m.pure(x)` on an `m`
    /// still unknown is not taken for one (`value_makers`).
    fn constructor_makers(&self, field: Symbol) -> Vec<TraitKey> {
        let mut traits: Vec<TraitKey> = (self.tables.traits.iter())
            .filter(|(_, info)| info.constructor)
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == field))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t) && self.private_owner(*t).is_none())
            .filter(|t| self.self_param(*t, field).is_none())
            .collect();
        traits.sort_by_key(|t| self.show_trait(*t));
        traits
    }

    /// Whether `tr` is a trait of type constructors.
    pub(super) fn of_constructors_trait(&self, tr: TraitKey) -> bool {
        (self.tables.traits.get(&tr)).is_some_and(|info| info.constructor)
    }

    /// Whether `ty` is the type of a parameter of an enclosing function
    /// or closure whose body reads a field of it (`FnFrame::read_fields`):
    /// a record, whose `t.zero()` calls its field.
    /// Where the read of a field of `obj`, whose type is unknown, is:
    /// `obj`'s name and whether it is a parameter, and the function the
    /// read is in (`traits`: see `ReadAt::traits`).
    fn read_at(&self, obj: &Expr, obj_ty: &Type, traits: Vec<TraitKey>) -> super::solve::ReadAt {
        let name = match obj.kind {
            ExprKind::Ident(name) => Some((
                name,
                (self.fn_frames.iter()).any(|frame| frame.names.contains(&Some(name))),
            )),
            _ => None,
        };
        let function = (self.fn_frames.last()).and_then(|frame| match frame.owner {
            Some(super::unused::FrameOwner::TopLevel(name)) => Some((name, true)),
            Some(super::unused::FrameOwner::Local(name)) => Some((name, false)),
            None => None,
        });
        let let_name = match self.apply(obj_ty) {
            Type::Var(v) => self.plain_let_vars.get(&v).copied(),
            _ => None,
        };
        super::solve::ReadAt {
            name,
            traits,
            function,
            let_name,
        }
    }

    fn read_as_record(&self, ty: &Type) -> bool {
        let ty = self.apply(ty);
        (self.fn_frames.iter()).any(|frame| {
            (frame.params.iter().zip(&frame.read_fields))
                .any(|(param, read)| *read && self.apply(param) == ty)
        })
    }

    /// Pass on how the parameter `i` of `callee` is read (`readings`) to
    /// the parameters of the enclosing functions and closures whose
    /// types the argument `arg_ty` shares (`fn again(s, t) { go(s, t) }`
    /// reads `t` as `go` reads its own).
    fn pass_on_reading(&mut self, callee: &Expr, i: usize, arg_ty: &Type) {
        let Some((reading, site)) = (Self::callee_named(callee))
            .and_then(|callee| self.readings.get(&(callee, i)))
            .map(|(_, reading, site)| (reading.clone(), *site))
        else {
            return;
        };
        // (The call is in `callee`, or in the function it is in.)
        let site = site.or_else(|| written_name(callee));
        for (outer, at, name) in self.named_params_sharing(arg_ty) {
            (self.readings)
                .entry((outer, at))
                .or_insert((name, reading.clone(), site));
        }
    }

    /// Replace the mismatch just reported (from `reported`) of the
    /// argument `arg`, the `i`th of a call of `callee`, a value given
    /// where the function takes a type (`json.parse(s, Cell(Int))`): the
    /// call says so, naming the parameter, and the variant the value is
    /// of; what giving the variant a type made the argument report (from
    /// `before_arg`, at `arg`) goes with it.
    fn value_for_type_argument(
        &mut self,
        (callee, i): (&Expr, usize),
        (arg, arg_ty): (&Expr, &Type),
        param: &Type,
        (before_arg, reported): (usize, usize),
    ) {
        let takes_type = |ty: &Type| matches!(ty, Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF));
        let given = self.apply(arg_ty);
        if !takes_type(&self.apply(param)) || takes_type(&given) {
            return;
        }
        let is_ours = |e: &Diagnostic| {
            e.message
                .starts_with("type mismatch: expected a type, got a value of type")
        };
        if self.errors.len() != reported + 1 || !is_ours(&self.errors[reported]) {
            return;
        }
        // (A part already reported, `(m(Int), m)`: that error is the one.)
        if self.apply(&given).holds_error() {
            self.errors.truncate(reported);
            return;
        }
        let Some(who) = Self::callee_label(callee) else {
            return;
        };
        // (A `type` parameter of the function's declaration: not one a
        // call made a type, `apply(fn_of(), rec)`.)
        let param_name = match self.res_def(callee.res) {
            Some(def) if def.module.is_builtin() => (crate::builtins::registry::registry())
                .named(&resolve(who))
                .and_then(|row| row.params.get(i).map(|p| (row.signature, p.to_string())))
                .filter(|(signature, p)| signature.contains(&format!("type {p}")))
                .map(|(_, p)| p),
            _ => (callee.res)
                .and_then(|res| match res {
                    crate::defs::Res::Def(id) => self.tables.fn_type_params.get(&id),
                    _ => None,
                })
                .and_then(|params| params.iter().find(|(at, _)| *at == i))
                .map(|(_, name)| resolve(*name)),
        };
        let Some(param_name) = param_name else {
            return;
        };
        let shown = self.show_type(&given);
        // (The variant the value is made by: `Cell` of `Cell(Int)`.)
        let made_by = match &arg.kind {
            ExprKind::Call(maker, _) => Some(&**maker),
            _ => Some(arg),
        }
        .and_then(|maker| match self.res_def(maker.res) {
            Some(def) if matches!(def.kind, crate::defs::DefKind::Variant { .. }) => Some(def.name),
            _ => None,
        });
        let at = self.errors[reported].span;
        let mut message =
            format!("`{who}` takes a type as `{param_name}`, not a value of type `{shown}`");
        let mut help = None;
        if let Some(variant) = made_by {
            message.push_str(&format!(": `{variant}` is a variant of `{shown}`"));
            help = Some(format!("give the type, `{shown}`"));
        }
        self.errors.truncate(reported);
        // (What giving the variant a type reported, a type where a value
        // is asked.)
        let mut kept = Vec::new();
        for e in self.errors.drain(before_arg..) {
            let cascade = e
                .message
                .starts_with("type mismatch: expected a value of type")
                && e.span.file == arg.span.file
                && e.span.start >= arg.span.start
                && e.span.end <= arg.span.end;
            if !cascade {
                kept.push(e);
            }
        }
        self.errors.extend(kept);
        let mut diagnostic = Diagnostic::error(Code::TypeMismatch, at, message);
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        // (Re-inserting a diagnostic built for the call, in place of the
        // mismatch: not a new emission site of `self.error(...)`.)
        self.errors.push(diagnostic);
    }

    /// Replace the mismatch just reported (the errors from `reported`)
    /// of the argument at `at`, the `i`th of a call of `callee`, when a
    /// call `x.m(..)` read the parameter as the other kind of argument
    /// (`readings`): a value given where it takes a type, a type given
    /// where it takes a record.
    /// The argument may also be such a function, given where a function
    /// is asked whose parameter has the other kind (`list.map([1], go)`).
    fn misread_argument(
        &mut self,
        (callee, i, named_args): (&Expr, usize, &[(Expr, Type)]),
        (arg, arg_ty): (&Expr, &Type),
        param_ty: &Type,
        reported: usize,
        at: Span,
    ) {
        let reading_of = |this: &Self, e: &Expr, at: usize| {
            let ExprKind::Ident(function) = e.kind else {
                return None;
            };
            let (param, reading, site) = (Self::callee_named(e))
                .and_then(|named| this.readings.get(&(named, at)))
                .cloned()?;
            // (A closure's parameter is annotated without `fn`.)
            let declared =
                (this.res_def(e.res)).is_some_and(|def| def.kind == crate::defs::DefKind::Fn);
            Some((function, declared, param, reading, site))
        };
        // (A closure given straight to the call, whose parameter's type
        // the parameter `i` has: no name calls it.)
        let of_closure = |this: &Self| {
            let asked = this.apply(param_ty);
            (this.closure_readings.iter())
                .find(|(ty, _, reading)| {
                    this.apply(ty) == asked && matches!(reading, Reading::Type { .. })
                })
                .map(|(_, name, reading)| {
                    (intern("closure"), false, Some(*name), reading.clone(), None)
                })
        };
        // (A closure given straight to a call whose parameter's type is
        // `ty`.)
        let closure_of = |this: &Self, ty: &Type| {
            let ty = this.apply(ty);
            (this.closure_readings.iter())
                .find(|(param, _, reading)| {
                    this.apply(param) == ty && matches!(reading, Reading::Type { .. })
                })
                .map(|(_, name, reading)| {
                    (intern("closure"), false, Some(*name), reading.clone(), None)
                })
        };
        // (A function a name given earlier to the call names, whose
        // parameter's type the parameter `i` has: `apply(g, rec)`.)
        let of_named = |this: &Self| {
            let asked = this.apply(param_ty);
            named_args
                .iter()
                .find_map(|(named, ty)| match this.apply(ty) {
                    Type::Fun(takes, _) => (takes.iter().enumerate())
                        .filter(|(_, take)| this.apply(take) == asked)
                        .find_map(|(j, _)| reading_of(this, named, j))
                        .filter(|(.., reading, _)| matches!(reading, Reading::Type { .. })),
                    _ => None,
                })
        };
        let mut anonymous = false;
        let found = match reading_of(self, callee, i) {
            Some(found) => Some((found, self.apply(arg_ty))),
            None => match (self.apply(arg_ty), self.apply(param_ty)) {
                (Type::Fun(takes, _), Type::Fun(asked, _)) => (0..takes.len().min(asked.len()))
                    .filter(|j| self.apply(&takes[*j]) != self.apply(&asked[*j]))
                    .find_map(|j| {
                        let found = reading_of(self, arg, j).or_else(|| {
                            // (A closure a call returns: `id({ r -> .. })`.)
                            let found = closure_of(self, &takes[j]);
                            anonymous |= found.is_some();
                            found
                        })?;
                        Some((found, self.apply(&asked[j])))
                    }),
                _ => of_named(self)
                    .map(|found| (found, self.apply(arg_ty)))
                    .or_else(|| {
                        of_closure(self).map(|found| {
                            anonymous = true;
                            (found, self.apply(arg_ty))
                        })
                    }),
            },
        };
        let Some(((function, declared, param, reading, site), given)) = found else {
            return;
        };
        let who = match anonymous {
            true => "the closure".to_string(),
            false => format!("'{function}'"),
        };
        // (Where the call `x.m(..)` is: the function called, or the one
        // it passes the parameter on to.)
        let place = site.map_or_else(|| "it".to_string(), |site| format!("'{site}'"));
        let held = match &given {
            Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                args.first().cloned()
            }
            _ => None,
        };
        let has_trait = |this: &Self, tr: TraitKey, ty: &Type| {
            (this.type_name_for_impl(ty))
                .is_some_and(|head| !this.impls_fitting(tr, head, Some(ty), &[]).is_empty())
        };
        let written_call = |this: &Self, receiver: &str, tr: TraitKey, method: Symbol| {
            let args = if this.method_arity(tr, method) > 0 {
                ".."
            } else {
                ""
            };
            format!("{receiver}.{method}({args})")
        };
        // (A parameter that is a pattern is named by the receiver.)
        let named = |receiver: &str| param.map_or_else(|| receiver.to_string(), |p| p.to_string());
        let (message, help) = match (reading, held) {
            (
                Reading::Type {
                    receiver,
                    tr,
                    method,
                },
                None,
            ) => {
                let param = named(&receiver);
                let shown = self.show_type(&given);
                let call = written_call(self, &receiver, tr, method);
                let trait_name = self.show_trait(tr);
                let has_field = match &given {
                    Type::Generic(name, _) => (self.tables.records.get(name))
                        .is_some_and(|record| record.fields.iter().any(|(n, _)| *n == method)),
                    _ => false,
                };
                let help = if has_trait(self, tr, &given) {
                    format!("pass the type itself: `{shown}`")
                } else if has_field {
                    let annotated = match declared {
                        true => format!("fn {function}({param}: {shown})"),
                        false => format!("{param}: {shown}"),
                    };
                    format!(
                        "to call the field `{method}` of the `{shown}`, annotate the parameter: \
                         `{annotated}`"
                    )
                } else {
                    format!("pass a type that has trait '{trait_name}'")
                };
                (
                    format!(
                        "{who} takes a type as `{param}`, not a value of type `{shown}`: \
                         `{call}` in {place} reads as the method of trait '{trait_name}' that makes a \
                         value"
                    ),
                    help,
                )
            }
            (
                Reading::Field {
                    receiver,
                    traits,
                    method,
                },
                Some(held),
            ) => {
                let param = named(&receiver);
                // (A generic type alone as the program writes it: `Option`.)
                let shown = match self.names_generic_type(arg) {
                    true => super::names::shown_arg(arg),
                    false => self.show_type(&held),
                };
                let call = written_call(self, &receiver, traits[0], method);
                let of: Vec<String> = (traits.iter())
                    .map(|t| format!("trait '{}'", self.show_trait(*t)))
                    .collect();
                let bound = (traits.iter().copied())
                    .find(|t| has_trait(self, *t, &held))
                    .unwrap_or(traits[0]);
                let why = match self.of_constructors_trait(bound) {
                    // (The one trait is of type constructors, which a call
                    // never makes a parameter owe.)
                    true => format!(
                        "is read as a field, since a trait of type constructors ('{}') is not \
                         inferred",
                        self.show_trait(bound)
                    ),
                    false => format!(
                        "can be the method of {}, so it is read as a field",
                        of.join(" or of ")
                    ),
                };
                (
                    format!(
                        "'{function}' takes a record with a field `{method}` as `{param}`, not the \
                         type `{shown}`: `{call}` in {place} {why}"
                    ),
                    match self.of_constructors_trait(bound) {
                        // (A type constructor is written with its type
                        // argument where it is used.)
                        true => {
                            let a = if param == "a" { "b" } else { "a" };
                            let example = format!(
                                "`fn wrap(x: {a}, type {param}) -> {param}({a}) where {param}: {}`",
                                self.show_trait(bound)
                            );
                            match declared {
                                true => format!(
                                    "say which: give '{function}' a type parameter with its \
                                     bound, and write it with its type argument, as in {example}"
                                ),
                                false => format!(
                                    "a closure cannot take a type parameter: declare a named \
                                     function with one and its bound, as in {example}"
                                ),
                            }
                        }
                        false => format!(
                            "say which: give '{function}' a type parameter with its bound, \
                             `fn {function}(type {param}) where {param}: {}`",
                            self.show_trait(bound)
                        ),
                    },
                )
            }
            _ => return,
        };
        self.errors.truncate(reported);
        self.error_help(Code::TypeMismatch, (message, Some(help)), at);
    }

    /// Replace the mismatch just reported (the errors from `reported`) of
    /// a pattern of the type `given` matched against `ty` at `at`, where
    /// `ty` is held by a parameter of an enclosing function that a call
    /// `t.m(..)` in it read as a type (`let Rec { n, .. } = t` after
    /// `t.zero()`): the note a call that gives it a value gets.
    pub(super) fn misread_pattern(&mut self, ty: &Type, given: &Type, reported: usize, at: Span) {
        let ty = self.apply(ty);
        let found = (self.fn_frames.iter()).find_map(|frame| {
            let owner = frame.owner?;
            let callee = self.callee_of(owner)?;
            let function = match owner {
                unused::FrameOwner::Local(name) | unused::FrameOwner::TopLevel(name) => name,
            };
            (frame.params.iter().enumerate())
                .filter(|(_, param)| self.apply(param) == ty)
                .find_map(|(k, _)| match self.readings.get(&(callee, k)) {
                    Some((param, reading @ Reading::Type { .. }, site)) => {
                        Some((function, *param, reading.clone(), *site))
                    }
                    _ => None,
                })
        });
        let Some((
            function,
            param,
            Reading::Type {
                receiver,
                tr,
                method,
            },
            site,
        )) = found
        else {
            return;
        };
        let param = param.map_or_else(|| receiver.clone(), |p| p.to_string());
        let shown = self.show_type(given);
        let args = if self.method_arity(tr, method) > 0 {
            ".."
        } else {
            ""
        };
        let place = site.map_or_else(|| "it".to_string(), |site| format!("'{site}'"));
        let message = format!(
            "'{function}' takes a type as `{param}`, not a value of type `{shown}`: \
             `{receiver}.{method}({args})` in {place} reads as the method of trait '{}' that \
             makes a value",
            self.show_trait(tr)
        );
        let has_field = match given {
            Type::Generic(name, _) => (self.tables.records.get(name))
                .is_some_and(|record| record.fields.iter().any(|(n, _)| *n == method)),
            _ => false,
        };
        let help = has_field.then(|| {
            format!(
                "to call the field `{method}` of the `{shown}`, annotate the parameter: \
                 `fn {function}({param}: {shown})`"
            )
        });
        self.errors.truncate(reported);
        self.error_help(Code::TypeMismatch, (message, help), at);
    }

    /// Keep the parameter `param` of the type `ty` if it has no
    /// annotation: it may hold a type (`held_params`).
    fn note_held_param(&mut self, param: &crate::ast::Param, ty: &Type) {
        if param.ty.is_none() && param.kind == crate::ast::ParamKind::Data {
            self.unannotated_params.push(ty.clone());
        }
        if param.ty.is_none()
            && param.kind == crate::ast::ParamKind::Data
            && matches!(param.pattern.kind, PatternKind::Ident(_))
        {
            self.held_params.push((param.pattern.span, ty.clone()));
        }
    }

    /// Whether a parameter with no annotation holds the type variable `v`
    /// as a type (`held_params`): a value gives it where the code runs.
    pub(super) fn held_by_param(&self, v: TyVar) -> bool {
        let held = Type::type_of(Type::Var(v));
        (self.held_params.iter()).any(|(_, ty)| self.apply(ty) == held)
    }

    /// Write the parameters with no annotation that hold a type, with
    /// the variable each holds (`Program::held_types`): the compiler
    /// gives the variable the parameter's value, as it gives a `type a`
    /// parameter's to `a`.
    pub(super) fn write_held_types(&mut self, program: &mut crate::ast::Program) {
        // (Kept: a builtin's argument checked after this asks
        // `held_by_param`.)
        program.held_types = (self.held_params.iter())
            .filter_map(|(span, ty)| match self.apply(ty) {
                Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                    match args.first() {
                        Some(var @ (Type::Var(_) | Type::Rigid(_))) => Some((*span, var.clone())),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
    }

    /// Note that the parameter `obj` (a name) holds the type `inner`.
    fn note_type_holder(&mut self, obj: &Expr, inner: &Type) {
        if let (ExprKind::Ident(name), Type::Var(v)) = (&obj.kind, self.apply(inner)) {
            self.type_holders.insert(v, *name);
        }
    }

    /// The name of the parameter that holds the type variable `r` as a
    /// type (`note_type_holder`), if one does.
    fn type_holder(&self, r: TyVar) -> Option<Symbol> {
        (self.type_holders.iter())
            .find(|(held, _)| self.apply(&Type::Var(**held)) == Type::Var(r))
            .map(|(_, name)| *name)
    }

    /// `ty` as the type of a place that later values join (a map's keys
    /// and values, a loop's binding): a variable resolved to it, so that a
    /// function that never returns, taken first, does not keep `Never` in
    /// the place's type (see `TyVarSupply::bind`).
    fn kept(&mut self, ty: Type) -> Type {
        let place = self.fresh_var();
        self.unify(&ty, &place, self.at);
        place
    }

    /// Note how `x.m(..)` read `x`, for every parameter of the enclosing
    /// functions and closures whose type is `x`'s, for a call of one of
    /// them that gives it the other kind of argument (`misread_argument`).
    fn note_reading(&mut self, obj: &Expr, obj_ty: &Type, reading: impl Fn(String) -> Reading) {
        let receiver = written_receiver(obj);
        for (callee, at, name) in self.named_params_sharing(obj_ty) {
            // (A receiver too long to write is named by the parameter.)
            let receiver = (receiver.clone())
                .or_else(|| name.map(|name| name.to_string()))
                .unwrap_or_else(|| "_".to_string());
            self.readings
                .insert((callee, at), (name, reading(receiver), None));
        }
        // (A parameter of a closure no `let` names.)
        let obj_ty = self.apply(obj_ty);
        let closure_param = (self.fn_frames.last())
            .filter(|frame| frame.owner.is_none())
            .and_then(|frame| {
                (frame.params.iter().zip(&frame.names))
                    .find(|(param, _)| self.apply(param) == obj_ty)
                    .and_then(|(_, name)| *name)
            });
        if let Some(name) = closure_param {
            let receiver = (written_receiver(obj)).unwrap_or_else(|| name.to_string());
            (self.closure_readings).push((obj_ty, name, reading(receiver)));
        }
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
            // (A type variable with the bound is called on as a type.)
            // A type held as a value (`type W`) is called on as itself.
            let ty = match ty {
                Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                    args.first().unwrap_or(ty)
                }
                _ => ty,
            };
            // (A type with arguments is called on with them: `Box(Int)`,
            // `Box(a)` where they are not known.)
            let on = match ty {
                Type::Rigid(r) => r.name.to_string(),
                // (A type held as a value among them is that type: the
                // variant `Cell(Int)` of `type Cell(a) { Cell(a), .. }`
                // is a `Cell(type Int)`, called on as `Cell(Int)`.)
                _ if !self.type_args_of(ty).is_empty() && free_vars_in(ty).is_empty() => {
                    self.show_type(&without_held(ty))
                }
                _ => match self.type_name_for_impl(ty) {
                    Some(t) => {
                        let name = self.spell_type(t);
                        match self.type_param_names(t).join(", ") {
                            params if params.is_empty() => name,
                            params => format!("{name}({params})"),
                        }
                    }
                    None => "T".to_string(),
                },
            };
            let args = if params > 0 { ".." } else { "" };
            return Some(format!(
                "`{field}` is a method of trait `{shown}` that makes a value: call it on the type, \
                 `{on}.{field}({args})`"
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
        let written_module = (crate::defs::ModuleId::builtin(module))
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
            Type::Fun(params, _) => params.iter().position(|p| match p {
                Type::Var(v) => *v == info.self_var,
                // `xs: Self(a)` of a trait of type constructors.
                Type::App(head, _) => matches!(&**head, Type::Var(v) if *v == info.self_var),
                _ => false,
            }),
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
                // (A generic alias alone, `L` of `type L(a) = List(a)`,
                // holds open arguments, as `List` alone does.)
                if self.type_arity(alias).is_some_and(|arity| arity > 0) {
                    for v in free_vars_in(&target) {
                        self.open_vars.insert(v);
                        self.open_var_names.insert(v, alias.name);
                    }
                }
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
        } else if let (makers, _) = self.value_makers(field)
            && let [tr] = makers.as_slice()
        {
            // (The one trait in sight that makes a value by the name,
            // which the type does not implement: `String.from_int(1)`.)
            d = d.with_help(format!(
                "`{field}` is a method of trait `{}`, which type '{shown}' does not implement",
                self.spell_trait(*tr)
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
        // (The type variables its signature writes.)
        let mut written = Vec::new();
        for te in (f.params.iter().filter_map(|p| p.ty.as_ref())).chain(f.return_type.as_ref()) {
            super::names::type_vars_of(te, &mut written);
        }
        // (`main` and the function of a REPL entry's statements take no
        // parameter a program gives.)
        if f.name != intern("main") && !crate::program::is_cell_name(&resolve(f.name)) {
            let params = (f.params.iter().zip(&sig.params))
                .map(|(p, ty)| match p.pattern.kind {
                    PatternKind::Ident(name) => (name, ty.clone()),
                    _ => (intern("_"), ty.clone()),
                })
                .collect();
            self.fn_spans.push(super::FnSite {
                span: f.span,
                name: f.name,
                params,
                written: written.clone(),
            });
        }
        let outer_written = std::mem::replace(&mut self.written_vars, written);
        let params = (f.params.iter())
            .map(|p| match p.pattern.kind {
                PatternKind::Ident(name) => (name, p.kind == ParamKind::Data),
                _ => (intern("_"), false),
            })
            .collect();
        let outer = self.checked_fn.replace((f.name, params));
        let uses = std::mem::take(&mut self.full_type_uses);
        self.check_body_in(f, sig, env);
        self.checked_fn = outer;
        self.written_vars = outer_written;
        // (As the body left the receivers' types.)
        for (recv, tr, field, span) in std::mem::replace(&mut self.full_type_uses, uses) {
            let recv = self.apply(&recv);
            if let Some(head) = self.type_name_for_impl(&recv)
                && let Some(key) = self.tables.one_impl(tr, head)
            {
                self.refuse_type_without_value(key, &recv, Some((tr, field)), None, span);
            }
            // (A variable of a body with no annotations is known once
            // every body is: `refuse_unheld_full_types`.)
            if !free_vars_in(&recv).is_empty() {
                (self.unheld_full_uses).push((recv, tr, field, span, f.name));
            }
        }
    }

    /// Refuse each use of a method whose impl's dictionary carries its
    /// type in full, kept by `check_body`, whose `Self` still has a type
    /// variable of a definition with no annotations that no parameter
    /// holding a type gives a value where the code runs (`Dec.again(Box {
    /// v: x }, s)` in `fn wrap(x, s)`), as `refuse_type_without_value`
    /// refuses an annotated one: the variable named as the definition's
    /// type names it (`Box(a)`).
    pub(super) fn refuse_unheld_full_types(&mut self, env: &TypeEnv) {
        let held: std::collections::HashSet<TyVar> = (self.held_params.iter())
            .filter_map(|(_, ty)| match self.apply(ty) {
                Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                    match args.first() {
                        Some(Type::Var(v)) => Some(*v),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        for (recv, tr, field, span, owner) in std::mem::take(&mut self.unheld_full_uses) {
            let recv = self.apply(&recv);
            let Some(key) =
                (self.type_name_for_impl(&recv)).and_then(|head| self.tables.one_impl(tr, head))
            else {
                continue;
            };
            if let Some(named) = self.named_unheld(&recv, &held, Some(owner), env) {
                self.refuse_type_without_value(key, &named, Some((tr, field)), None, span);
            }
        }
        // (A use of a definition with the bound, `twice([x], s)`, of a
        // builtin type: by the impl the solver chose. A generic type of
        // the program there, `Box { v: x }`, is the decoder's refusal.)
        for (ty, key, callee, span, owner) in std::mem::take(&mut self.unheld_full_calls) {
            let ty = self.apply(&ty);
            let builtin = (self.type_name_for_impl(&ty)).is_some_and(|head| {
                crate::defs::builtin_types()
                    .get(head.id.0.0 as usize)
                    .is_some()
            });
            if !builtin {
                continue;
            }
            if let Some(named) = self.named_unheld(&ty, &held, owner, env) {
                self.refuse_type_without_value(key, &named, None, callee, span);
            }
        }
    }

    /// `ty` with each type variable no parameter holding a type gives a
    /// value (none of `held`) named as the scheme of `owner` names it,
    /// or by a spare letter; `None` if it has none.
    fn named_unheld(
        &self,
        ty: &Type,
        held: &std::collections::HashSet<TyVar>,
        owner: Option<Symbol>,
        env: &TypeEnv,
    ) -> Option<Type> {
        let unheld: Vec<TyVar> = (free_vars_in(ty).into_iter())
            .filter(|v| !held.contains(v))
            .collect();
        if unheld.is_empty() {
            return None;
        }
        let letters = (owner.and_then(|owner| env.lookup(owner)))
            .map(Scheme::letters)
            .unwrap_or_default();
        let mut spare = (b'a'..=b'z')
            .map(|letter| intern(&char::from(letter).to_string()))
            .filter(|letter| !letters.values().any(|used| used == letter));
        let mapping: HashMap<TyVar, Type> = (unheld.into_iter())
            .map(|var| {
                let name = (letters.get(&var).copied())
                    .or_else(|| spare.next())
                    .unwrap_or_else(|| intern("_"));
                (var, Type::Rigid(crate::types::RigidId { var, name }))
            })
            .collect();
        Some(substitute_vars(ty, &mapping))
    }

    /// `check_body`, with the body's `type a` parameters known.
    fn check_body_in(&mut self, f: &mut FnDecl, sig: &FnSig, env: &mut TypeEnv) {
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
                self.note_held_param(param, ty);
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
        let prev_fn = self.current_fn.replace(f.name);
        // Fresh `?`-site tracking per fn body, so a failed
        // body/return unify can point back at the `?` that demanded a
        // Result/Option return.
        let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

        // Infer the body and unify with declared return type
        let owner = self.frame_owner.take();
        let names: Vec<Option<Symbol>> = (f.params.iter())
            .map(|p| match p.pattern.kind {
                PatternKind::Ident(name) => Some(name),
                _ => None,
            })
            .collect();
        let read_fields = read_fields(&mut f.body, &names);
        self.fn_frames.push(unused::FnFrame {
            owner,
            params: param_types,
            names,
            read_fields,
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
        self.settle_closure_returns();

        self.never_fn = prev_never_fn;
        self.current_fn = prev_fn;
        self.current_return_type = prev_return_type;
        self.current_qmark_spans = prev_qmark_spans;
        self.sig_names = prev_names;
    }

    /// Unify the type `body_type` of a function's or closure's `body`
    /// with its result `ret`, at the value the body gives: a block's tail
    /// expression, where a mismatch is underlined and the Ok-wrap fix
    /// wraps. A block without one is underlined whole and gets no fix.
    /// True when the mismatch was a closure's `?`, reported as such.
    fn unify_body_result(&mut self, body: &Expr, body_type: &Type, ret: &Type) -> bool {
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
        self.note_qmark_requirement_on_ret_mismatch(from, body_type, ret)
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
    /// of a `?` in such a function (`try_operand`). True when that
    /// function is a closure.
    fn note_qmark_requirement_on_ret_mismatch(
        &mut self,
        err_count_before: usize,
        body_ty: &Type,
        ret_ty: &Type,
    ) -> bool {
        if self.errors.len() == err_count_before {
            return false;
        }
        let Some(qspan) = self.current_qmark_spans.first().copied() else {
            return false;
        };
        let body_resolved = self.apply(body_ty);
        let wrapper = |t: &Type| matches!(t, Type::Generic(n, _) if n.is_builtin("Result") || n.is_builtin("Option"));
        if let Some(operand) = self.qmark_decided.get(&qspan).cloned()
            && !wrapper(&body_resolved)
            && !matches!(body_resolved, Type::Var(_) | Type::Error | Type::Never)
        {
            // (A closure's `?` ends the closure, which is what returns
            // the body's type.)
            let closure = !self.closure_returns.is_empty();
            let note = format!(
                "the ?-ed expression is {}, but the {} returns {}",
                self.show_type(&self.apply(&operand)),
                if closure {
                    "closure it is in"
                } else {
                    "enclosing function"
                },
                self.show_type(&body_resolved)
            );
            self.errors.truncate(err_count_before);
            let mut diagnostic = Diagnostic::error(
                Code::InvalidQuestion,
                qspan,
                "? operator can only be used inside a function that returns Result or Option",
            )
            .with_note(note);
            if closure {
                let fails = match self.apply(&operand) {
                    Type::Generic(n, _) if n.is_builtin("Option") => "None",
                    _ => "Err",
                };
                diagnostic = diagnostic.with_help(format!(
                    "a `?` in a closure ends the closure, not the function; over a list, \
                     `list.traverse(xs) {{ x -> ... }}?` stops at the first `{fails}`"
                ));
            }
            self.errors.push(diagnostic);
            return closure;
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
            return false;
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
        false
    }

    /// The mismatches of each closure whose `return` did not fit (its
    /// body's value with the `return`'s, the closure with the place it
    /// is given to) as one error, at the `return`: it ends the closure,
    /// not the function it is written in.
    pub(super) fn settle_closure_returns(&mut self) {
        for (closure, at, returned, misfits) in std::mem::take(&mut self.closure_return_misfits) {
            // (Only where the `return` and the closure's body disagree:
            // a `return` meant to leave the function. A closure that
            // returns a value on purpose, of a type its place does not
            // take, keeps its type mismatch.)
            if !self
                .errors
                .iter()
                .any(|e| e.code == Code::TypeMismatch && misfits.contains(&e.span))
            {
                continue;
            }
            // (At the closure: of the type the `return` gives, not of a
            // parameter's.)
            let of_return = |e: &Diagnostic| {
                e.message.ends_with(&format!("got {returned}"))
                    || e.message.contains(&format!("expected {returned}, got"))
            };
            let at_closure =
                |e: &Diagnostic| e.code == Code::TypeMismatch && e.span == closure && of_return(e);
            let misfit = |e: &Diagnostic| {
                e.code == Code::TypeMismatch && (misfits.contains(&e.span) || at_closure(e))
            };
            // The types, kept as a note: what the place of the closure
            // expects, or else what its body gives.
            let mismatch = |e: &Diagnostic| {
                let rest = e.message.strip_prefix("type mismatch: expected ")?;
                let (expected, got) = rest.split_once(", got ")?;
                Some((expected.to_string(), got.to_string()))
            };
            let pairs: Vec<(String, String)> = self
                .errors
                .iter()
                .filter(|e| misfit(e))
                .filter_map(mismatch)
                .collect();
            let expected = pairs.iter().find(|(e, g)| *g == returned && *e != returned);
            let body = pairs.iter().find(|(e, g)| *e == returned && *g != returned);
            let note = match (expected, body) {
                (Some((expected, _)), _) => format!(
                    "the closure returns `{returned}` by its `return`, where `{expected}` is expected"
                ),
                (None, Some((_, body))) => format!(
                    "the closure returns `{returned}` by its `return`, and `{body}` by its last \
                     expression"
                ),
                (None, None) => format!("the closure returns `{returned}` by its `return`"),
            };
            self.errors.retain(|e| !misfit(e));
            let function = match self.current_fn {
                Some(name) => format!("`{name}`"),
                None => "the function around it".to_string(),
            };
            self.errors.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    at,
                    format!(
                        "`return` here ends the closure, not {function}: the value becomes the \
                         closure's result"
                    ),
                )
                .with_note(note)
                .with_help(
                    "to stop early, use `list.find`, `list.any` or `list.fold_until`, or a \
                     `loop`",
                ),
            );
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
            if let Some(Type::Generic(rec, args)) = target
                && self.tables.records.contains_key(&rec)
            {
                ty = Some(rec);
                fixed = Some(args);
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
        if def.kind != crate::defs::DefKind::TypeAlias {
            return None;
        }
        // (Another module's, as written: `m.IntBox`.)
        match &arg.kind {
            ExprKind::FieldAccess(obj, field, _) if let ExprKind::Ident(module) = obj.kind => {
                Some(intern(&format!("{module}.{field}")))
            }
            _ => Some(def.name),
        }
    }

    /// The type `arg` writes as a value if it writes a type of types:
    /// a type applied to types (`List(Int)`, `Map(String, Int)`,
    /// `Box(Int)`, `Pairs(Int)` of an alias `type Pairs(a) = List((a,
    /// a))`) or a tuple of types (`(Int, String)`), at any depth, a type
    /// variable among them (`List(a)`). `None` for any other expression,
    /// a type alone among them (its value has its type already), and one
    /// whose arguments do not fit the type. The expression, and each
    /// type alone in it, gets its type: the compiler builds the value of
    /// the type from them.
    fn spelled_type(&mut self, arg: &mut Expr, env: &mut TypeEnv) -> Option<Type> {
        let written = match arg.kind {
            ExprKind::Call(..) | ExprKind::Tuple(_) => self.writes_types(arg),
            _ => self.plain_alias(arg).is_some(),
        };
        if !written {
            return None;
        }
        let ty = self.spell(arg, env)?;
        arg.ty = Some(Type::type_of(ty.clone()));
        Some(ty)
    }

    /// Report `ty`, the type `arg` writes as a value, if a part of it
    /// that the value of the type is made of has no value as a type: a
    /// function type (of an alias written alone, the type it stands
    /// for; of a type written in full, any of its parts). `takes` names
    /// the function and its type parameter when `arg` is a type
    /// argument. Whether it was reported.
    fn valueless_type(&mut self, arg: &Expr, ty: &Type, takes: Option<(Symbol, Symbol)>) -> bool {
        fn part(ty: &Type, whole: bool) -> Option<&Type> {
            match ty {
                Type::Fun(..) => Some(ty),
                _ if !whole => None,
                Type::List(t) | Type::Set(t) | Type::Channel(t) => part(t, true),
                Type::Map(k, v) => part(k, true).or_else(|| part(v, true)),
                Type::Tuple(ts) | Type::Generic(_, ts) => ts.iter().find_map(|t| part(t, true)),
                _ => None,
            }
        }
        let alone = matches!(arg.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..));
        let Some(part) = part(ty, !alone) else {
            return false;
        };
        let shown = self.show_type(part);
        let message = match takes {
            Some((who, param)) => format!(
                "`{who}` takes a type as `{param}`, but `{shown}` is a function type, which has \
                 no value as a type"
            ),
            None => format!("`{shown}` is a function type, which has no value as a type"),
        };
        self.error(Code::TypeMismatch, message, arg.span);
        true
    }

    /// The name `callee` is called by and the name of its type parameter
    /// at the argument `i`, if it declares one there.
    fn type_param_of(&self, callee: &Expr, i: usize) -> Option<(Symbol, Symbol)> {
        let who = Self::callee_label(callee)?;
        let Some(crate::defs::Res::Def(id)) = callee.res else {
            return None;
        };
        let params = self.tables.fn_type_params.get(&id)?;
        let (_, name) = params.iter().find(|(at, _)| *at == i)?;
        Some((who, *name))
    }

    /// Keep `e`, a generic type or alias written alone, as written, for a
    /// decoder's refusal at the argument `at` (`bare_written`).
    fn note_bare_written(&mut self, e: &Expr, at: Span) {
        let Some(Type::Generic(name, args)) = e.ty.as_ref().map(|ty| self.apply(ty)) else {
            return;
        };
        if !name.is_builtin(crate::defs::TYPE_OF) {
            return;
        }
        let Some(ty) = args.first().map(|ty| self.apply(ty)) else {
            return;
        };
        let head = super::builtin_checks::head_ref(&ty);
        // (Of an alias of a type with no arguments, `K` of `type K(a) =
        // Int`, nothing in the type says it was written alone.)
        if head.is_none() {
            self.open_type_arguments.insert(at);
        }
        let written = intern(&super::names::shown_arg(e));
        self.bare_written.entry(at).or_insert((written, head));
    }

    /// `note_bare_written` of the first generic type or alias written
    /// alone among the types `e` gives another type (`L` of `List(L)`).
    fn note_bare_part(&mut self, e: &Expr, at: Span) {
        match &e.kind {
            ExprKind::Call(_, args) => {
                for arg in args {
                    if self.names_generic_type(arg) {
                        self.note_bare_written(arg, at);
                        return;
                    }
                    self.note_bare_part(arg, at);
                }
            }
            ExprKind::Tuple(elems) => {
                for elem in elems {
                    if self.names_generic_type(elem) {
                        self.note_bare_written(elem, at);
                        return;
                    }
                    self.note_bare_part(elem, at);
                }
            }
            _ => {}
        }
    }

    /// Report the type `ty`, given types where it takes another number
    /// of them, at `span`: the type is unknown.
    fn type_arguments_mismatch(&mut self, ty: TypeRef, span: Span) -> Type {
        let message = self.type_arguments_message(&resolve(ty.name), ty);
        self.error(Code::ArityMismatch, message, span);
        Type::Error
    }

    /// The error of a call that gives type arguments to `callee`, a local
    /// name that holds a type (`m(Int)` in `fn dec(s, m)`): it takes
    /// none, the whole type is passed, as the call of the function being
    /// checked shows where `callee` is its data parameter.
    fn held_type_applied(&self, callee: &Expr, callee_ty: &Type) -> Option<String> {
        let ExprKind::Ident(name) = callee.kind else {
            return None;
        };
        if callee.res != Some(crate::defs::Res::Local)
            || !matches!(callee_ty, Type::Generic(head, _) if head.is_builtin(crate::defs::TYPE_OF))
        {
            return None;
        }
        let said =
            format!("`{name}` holds a type and takes no type arguments: pass the whole type");
        let Some((function, params)) = self.checked_fn.as_ref() else {
            return Some(said);
        };
        if !params.contains(&(name, true)) {
            return Some(said);
        }
        let args: Vec<String> = (params.iter())
            .map(|(param, _)| match *param == name {
                true => "List(Int)".to_string(),
                false => resolve(*param).to_string(),
            })
            .collect();
        Some(format!("{said}, as in `{function}({})`", args.join(", ")))
    }

    /// Whether what a value owes since `from` has a decode that only the
    /// types its parameters' values take decide: a trait with
    /// `json.Decode` among its supertraits owed of a type with a part
    /// still unknown (the impl it picks is the one of that part), or
    /// `json.Decode` of an unknown part asked by an impl's header whose
    /// dictionary carries its type in full. Generalised, such a closure
    /// is refused; of one type, its later call decides it.
    pub(super) fn decodes_by_its_values(&self, from: usize) -> bool {
        (self.wanted[from.min(self.wanted.len())..].iter())
            .filter(|wanted| !wanted.solved)
            .any(|wanted| match &wanted.goal {
                Goal::Pred(Pred::Trait { tr, subject, .. }) => {
                    let subject = self.apply(subject);
                    let open = !free_vars_in(&subject).is_empty();
                    let compound = !matches!(subject, Type::Var(_));
                    let full = (wanted.origin.header).is_some_and(|(key, _)| {
                        Self::is_decode(*tr) && self.carries_full_type(key)
                    });
                    open && ((compound && self.implies_decode(*tr)) || full)
                }
                _ => false,
            })
    }

    /// Whether `e` names a generic type alone (`List`, `Box`, an alias
    /// `L` of `type L(a) = List(a)`).
    fn names_generic_type(&mut self, e: &Expr) -> bool {
        if !matches!(e.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return false;
        }
        let Some(def) = self.res_def(e.res) else {
            return false;
        };
        match def.kind {
            crate::defs::DefKind::Type(_) => (self.res_type(e.res))
                .and_then(|ty| self.type_instance(ty))
                .is_some_and(|ty| !free_vars_in(&ty).is_empty()),
            crate::defs::DefKind::TypeAlias => (self.res_type(e.res))
                .and_then(|ty| self.type_arity(ty))
                .is_some_and(|arity| arity > 0),
            _ => false,
        }
    }

    /// The alias without type parameters `e` names, if it names one.
    fn plain_alias(&self, e: &Expr) -> Option<TypeRef> {
        if !matches!(e.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..)) {
            return None;
        }
        let alias = self.res_type(e.res)?;
        let is_alias = self
            .res_def(e.res)
            .is_some_and(|def| def.kind == crate::defs::DefKind::TypeAlias);
        (is_alias && self.type_arity(alias) == Some(0)).then_some(alias)
    }

    /// Whether `e` is made of names of types only (a type variable's
    /// too), applied to each other or in tuples: what `spell` reads.
    fn writes_types(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(_) | ExprKind::FieldAccess(..) => match e.res {
                Some(crate::defs::Res::Local) => matches!(e.kind, ExprKind::Ident(_)),
                Some(crate::defs::Res::Def(id)) => {
                    (self.defs.as_ref()).is_some_and(|defs| match defs.get(id).kind {
                        crate::defs::DefKind::Type(_) | crate::defs::DefKind::TypeAlias => true,
                        // (A variant named like its type: the type.)
                        _ => defs
                            .variant_type(id)
                            .is_some_and(|ty| ty.name == defs.get(id).name),
                    })
                }
                _ => false,
            },
            ExprKind::Call(callee, args) => {
                self.writes_types(callee) && args.iter().all(|a| self.writes_types(a))
            }
            ExprKind::Tuple(elems) => elems.iter().all(|e| self.writes_types(e)),
            _ => false,
        }
    }

    /// The type `e`, which `writes_types`, writes.
    fn spell(&mut self, e: &mut Expr, env: &mut TypeEnv) -> Option<Type> {
        let span = e.span;
        // An alias without parameters: the type it stands for (`type
        // Pair = (Int, String)`).
        if let Some(alias) = self.plain_alias(e) {
            let ty = Type::Generic(alias, Vec::new());
            let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &ty);
            e.ty = Some(Type::type_of(ty.clone()));
            return Some(ty);
        }
        match &mut e.kind {
            ExprKind::Call(callee, args) => {
                self.type_named_like_its_variant(callee);
                let is_alias = (self.res_def(callee.res))
                    .is_some_and(|def| def.kind == crate::defs::DefKind::TypeAlias);
                let alias = self.res_type(callee.res).filter(|_| is_alias);
                let args: Vec<Type> = (args.iter_mut())
                    .map(|a| self.spell(a, env))
                    .collect::<Option<_>>()?;
                // An alias given its arguments: the type it stands for.
                if let Some(alias) = alias {
                    let expected = self.type_arity(alias).unwrap_or(0);
                    if expected != args.len() {
                        return Some(self.type_arguments_mismatch(alias, span));
                    }
                    let ty = Type::Generic(alias, args);
                    return Some(crate::types::canonical::canonicalize(
                        &self.tables.resolver,
                        &ty,
                    ));
                }
                let head = self.spell(callee, env)?;
                // (A type whose parameters the value leaves open.)
                let open = |t: &Type| matches!(t, Type::Var(_));
                match (head, args.as_slice()) {
                    (Type::Error, _) => Some(Type::Error),
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
                    // A type given more or fewer types than it takes.
                    (head, _) => {
                        let (ty, expected) = match head.head_and_args() {
                            Some((Head::Named(r), _)) if r.is_builtin(crate::defs::TYPE_OF) => {
                                return None;
                            }
                            Some((head, params)) => (head.type_ref(), params.len()),
                            None => return None,
                        };
                        if expected == args.len() {
                            return None;
                        }
                        Some(self.type_arguments_mismatch(ty, span))
                    }
                }
            }
            ExprKind::Tuple(elems) => (elems.iter_mut())
                .map(|e| self.spell(e, env))
                .collect::<Option<_>>()
                .map(Type::Tuple),
            _ => {
                self.type_named_like_its_variant(e);
                match self.infer_expr(e, env) {
                    Type::Error => Some(Type::Error),
                    Type::Generic(head, args) if head.is_builtin(crate::defs::TYPE_OF) => {
                        let ty = args.first().map(|t| self.apply(t))?;
                        e.ty = Some(Type::type_of(ty.clone()));
                        Some(ty)
                    }
                    // A parameter of a type still unknown, written among
                    // types: a type (`fn load(text, t) { json.parse(text,
                    // List(t)) }`).
                    unknown @ Type::Var(_) => {
                        let ty = self.fresh_var();
                        self.unify(&unknown, &Type::type_of(ty.clone()), span);
                        e.ty = Some(Type::type_of(ty.clone()));
                        Some(ty)
                    }
                    _ => None,
                }
            }
        }
    }

    /// Keep the definition of another module a `let` binds a name to
    /// (`let oenc = other.enc`), or forget what the name was bound to.
    pub(super) fn note_let_link(&mut self, pattern: &Pattern, value: &Expr) {
        let PatternKind::Ident(name) = pattern.kind else {
            return;
        };
        match Self::callee_label(value).filter(|label| resolve(*label).contains('.')) {
            Some(label) => self.let_links.insert(name, label),
            None => self.let_links.remove(&name),
        };
    }

    /// The name a callee is written with, for a message about a call of
    /// it: `f`, or `m.f` for a member of a module.
    pub(super) fn callee_label(callee: &Expr) -> Option<Symbol> {
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
                    name: *name,
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
        self.keep_owners(&scheme);
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
            // (What it gives is not known either: the error type, of
            // which nothing more is asked, `inc(1) + inc(2)`.)
            return Type::Error;
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
            header: None,
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
        // (The arguments as written, for a help that writes the call.)
        self.method_call_args = matches!(callee.kind, ExprKind::FieldAccess(..)).then(|| {
            (args.iter())
                .map(|arg| super::names::shown_arg(arg))
                .collect::<Vec<_>>()
                .join(", ")
        });
        let callee_ty = self.infer_expr(callee, env);
        self.method_call_args = None;
        self.trait_receiver = None;
        self.callee_position = false;
        self.named_use = None;
        let callee_ty = self.apply(&callee_ty);
        let receiver_taken_by_callee = self.receiver_taken;
        self.receiver_taken = false;
        // `x.f(args)` where the type of `x` is unknown: the call waits
        // for it.
        if let Some((recv, name, at)) = self.unknown_receiver.take() {
            let args: Vec<Type> = args.iter_mut().map(|a| self.infer_expr(a, env)).collect();
            let result = self.fresh_var();
            self.want_goal(
                Goal::Select {
                    recv,
                    name,
                    args: Some(args),
                    result: result.clone(),
                    at,
                },
                Origin {
                    span: callee.span,
                    callee: Some(name),
                    op: None,
                    header: None,
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
                    // (A trait method's call: its method's type, once.)
                    let via_ty = match &self.wanted[k].goal {
                        Goal::Pred(Pred::Via { method_ty, .. }) => Some(self.apply(method_ty)),
                        _ => None,
                    };
                    // The call of a trait method itself (its type is the
                    // callee's): each parameter's argument, for what the
                    // impl's method gives a builtin. (A call inside this
                    // one placed its own.)
                    if let (Some(via_ty), CallForm::Call) = (&via_ty, form)
                        && self.wanted[k].arguments.is_empty()
                        && *via_ty == callee_ty
                    {
                        let origin = self.wanted[k].origin;
                        let arguments = (0..params.len())
                            .map(|param| {
                                let arg = args.get(param.checked_sub(implicit_self)?)?;
                                let origin = Origin {
                                    span: arg.span,
                                    ..origin
                                };
                                Some((origin, self.alias_named(arg)))
                            })
                            .collect();
                        self.wanted[k].arguments = arguments;
                    }
                    // (A trait method's call is at the type it is given,
                    // not at its receiver: where it was given, if that is
                    // known.)
                    if let Goal::Pred(Pred::Via { at: Some(at), .. }) = &self.wanted[k].goal {
                        self.wanted[k].origin.span = *at;
                        continue;
                    }
                    let type_given = via_ty.as_ref().and_then(type_of_var);
                    let Type::Var(v) = (match (&self.wanted[k].goal, &type_given) {
                        (_, Some(given)) => given,
                        (Goal::Pred(pred), None) => pred.subject(),
                        _ => continue,
                    }) else {
                        continue;
                    };
                    let at = params
                        .iter()
                        .skip(implicit_self)
                        .position(|param| free_vars_in(param).contains(v))
                        .and_then(|i| args.get(i));
                    // (In both forms: `x |> f` is `x |> f()`, and what the
                    // piped value fails is shown at it.)
                    if let Some(arg) = at {
                        if self.decode_places.contains(&self.wanted[k].origin.span) {
                            self.decode_places.insert(arg.span);
                        }
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
                    if let Some(arg) = at {
                        let alias = self.alias_named(arg);
                        let use_ = &mut self.given_waiting[k];
                        if self.decode_places.contains(&use_.origin.span) {
                            self.decode_places.insert(arg.span);
                        }
                        use_.origin.span = arg.span;
                        use_.written = alias;
                    }
                }
                let mut mismatched = false;
                // (A type given where the callee reads a record's fields,
                // `f(Rec)`: what the call returns is of no type a later
                // message could judge.)
                let mut type_for_record = false;
                // Each argument's place, the alias it names a type by,
                // and its type.
                let mut given: Vec<(Span, Option<Symbol>, Type)> = Vec::new();
                // The calls of trait methods held in a closure (returned
                // by a function, kept in a record) whose type argument is
                // still unknown: this call may be the one that gives it.
                let open_vias = self.open_vias();
                // (The uses the callee owes are at this call's arguments.)
                let callee_owes = given_before..self.given_waiting.len();
                // (The arguments as a help writes them.)
                let shown_args: Vec<String> = args
                    .iter()
                    .map(|arg| super::names::shown_arg(arg))
                    .collect();
                // A closure is checked after the call's other arguments,
                // which may decide the types of its parameters
                // (`apply({ r -> r.zero() }, rec)` reads `r` as `rec`'s
                // type, as `apply2(rec, { r -> r.zero() })` does).
                let (closures, others): (Vec<usize>, Vec<usize>) =
                    (0..args.len()).partition(|k| matches!(args[*k].kind, ExprKind::Lambda { .. }));
                // (Each argument's place, the alias and the type, in the
                // order the call writes them.)
                // (The names given as arguments so far, with their types:
                // a closure a `let` names, for `misread_argument`.)
                let mut named_args: Vec<(Expr, Type)> = Vec::new();
                let mut given_at: Vec<Option<(Span, Option<Symbol>, Type)>> =
                    vec![None; args.len()];
                for i in others.into_iter().chain(closures) {
                    let arg = &mut args[i];
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
                    // A type argument written as a type of types
                    // (`List(Int)`, `(Int, String)`) is that type.
                    let spelled = match param {
                        Some(param)
                            if matches!(self.apply(param),
                                Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF)) =>
                        {
                            // (json's and toml's decoders say what they
                            // cannot decode themselves; postgres's rows
                            // would reach the compiler.)
                            let builtin = (self.res_def(callee.res)).is_some_and(|def| {
                                matches!(def.module.builtin_name(), Some("json" | "toml"))
                            });
                            let takes = self.type_param_of(callee, i);
                            match self.spelled_type(arg, env) {
                                Some(ty) if !builtin && self.valueless_type(arg, &ty, takes) => {
                                    arg.ty = Some(Type::Error);
                                    Some(Type::Error)
                                }
                                spelled => spelled,
                            }
                        }
                        _ => None,
                    };
                    let full = spelled.is_some();
                    let alone = spelled.is_none() && self.names_generic_type(arg);
                    if alone {
                        self.open_type_arguments.insert(arg.span);
                    }
                    let before_arg = self.errors.len();
                    let arg_ty = match spelled {
                        Some(ty) => Type::type_of(ty),
                        None => self.infer_expr(arg, env),
                    };
                    if alone {
                        self.note_bare_written(arg, arg.span);
                    } else if full {
                        self.note_bare_part(arg, arg.span);
                    }
                    self.expected_closure = None;
                    let reported = self.errors.len();
                    let arg_ty = match param {
                        Some(param) => self.constructor_argument(
                            param,
                            (arg_ty, full),
                            callee,
                            (&shown_args, i),
                            arg.span,
                        ),
                        None => arg_ty,
                    };
                    mismatched |= self.errors.len() > reported;
                    given_at[i] = Some((arg.span, self.alias_named(arg), arg_ty.clone()));
                    if let ExprKind::Ident(name) = arg.kind {
                        self.arg_names.insert(arg.span, name);
                    }
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
                        // A type argument already reported (`_` of
                        // `wrap(1, Result(_, String))`): what it would
                        // decide is asked about no more.
                        if self.apply(&arg_ty) == Type::Error
                            && let Type::Generic(name, args) = self.apply(param)
                            && name.is_builtin(crate::defs::TYPE_OF)
                        {
                            for v in args.iter().flat_map(free_vars_in) {
                                self.unify(&Type::Var(v), &Type::Error, at);
                            }
                        }
                        mismatched |= self.errors.len() > reported;
                        type_for_record |= (self.errors[reported..].iter()).any(|e| {
                            e.message
                                .starts_with("type mismatch: expected a value with a field")
                        });
                        if self.errors.len() > reported {
                            self.misread_argument(
                                (callee, i, &named_args),
                                (arg, &arg_ty),
                                param,
                                reported,
                                at,
                            );
                            self.value_for_type_argument(
                                (callee, i),
                                (arg, &arg_ty),
                                param,
                                (before_arg, reported),
                            );
                        } else {
                            self.pass_on_reading(callee, i, &arg_ty);
                        }
                        if let (Some(statement), Some(passed_on)) = (statement, passed_on) {
                            self.name_statement_unit(statement, reported, passed_on);
                        }
                    }
                    if matches!(arg.kind, ExprKind::Ident(_)) {
                        named_args.push(((**arg).clone(), arg_ty.clone()));
                    }
                }
                given.extend(given_at.into_iter().flatten());
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
                if type_for_record {
                    return Type::Error;
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
                    // A type given an argument already reported (`List(t)`,
                    // `t` undefined) was meant as a type of types.
                    (Type::Generic(name, _), _)
                        if name.is_builtin(crate::defs::TYPE_OF)
                            && (arg_types.iter()).any(|ty| self.apply(ty) == Type::Error) =>
                    {
                        Type::Error
                    }
                    // A type given a value (`List(())`): the argument is
                    // the error, not the call.
                    (Type::Generic(name, _), CallForm::Call)
                        if name.is_builtin(crate::defs::TYPE_OF)
                            && let Some((arg, ty)) = (args.iter().zip(&arg_types))
                                .map(|(arg, ty)| (arg, self.apply(ty)))
                                .find(|(_, ty)| match ty {
                                    Type::Generic(name, _) => {
                                        !name.is_builtin(crate::defs::TYPE_OF)
                                    }
                                    Type::Var(_) | Type::Error | Type::Never => false,
                                    _ => true,
                                }) =>
                    {
                        let name = match Self::callee_label(callee) {
                            Some(name) => resolve(name).to_string(),
                            None => self.show_type(other),
                        };
                        // (The value by its text when that is short.)
                        let text = match &arg.kind {
                            ExprKind::Unit => Some("()".to_string()),
                            ExprKind::Int(n) => Some(n.to_string()),
                            ExprKind::Float(x) => Some(x.to_string()),
                            ExprKind::Bool(b) => Some(b.to_string()),
                            ExprKind::StringLit(s)
                                if s.len() <= 20 && !s.contains(['"', '\\', '\n', '`']) =>
                            {
                                Some(format!("\"{s}\""))
                            }
                            _ => None,
                        };
                        let message = match text {
                            Some(text) => {
                                format!("`{name}` takes a type here, and `{text}` is a value")
                            }
                            None => format!(
                                "`{name}` takes a type here, and a value of type `{}` is given",
                                self.show_type(&ty)
                            ),
                        };
                        self.error(Code::TypeMismatch, message, arg.span);
                        Type::Error
                    }
                    (_, CallForm::Call) => {
                        // A parameter that holds a type, given type
                        // arguments (`m(Int)`): the whole type is passed.
                        if let Some(message) = self.held_type_applied(callee, other) {
                            self.error(Code::TypeMismatch, message, span);
                            return Type::Error;
                        }
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
        // A variant of an opaque type of another module builds a value of
        // it, called or not.
        if matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..))
            && let Some(enum_ty) = self.res_variant_enum(expr.res)
            && let Some(module) = self.opaque_outside(enum_ty)
        {
            let message = format!(
                "cannot build a '{}' outside module '{module}': the type is opaque",
                super::opaque::opaque_name(enum_ty, module)
            );
            self.opaque_error(message, module, span);
            expr.ty = Some(Type::Error);
            return Type::Error;
        }
        // A type applied to types (`Box(Int)`, `List(Int)`) is the value
        // of that type wherever it stands, as a type alone (`W`) is: in
        // a `let`, before a dot (`Box(Int).decode(v)`), as an argument.
        // (A variant named like its type stays the variant here: a type
        // is meant only where a `type` parameter asks one.)
        if let ExprKind::Call(callee, _) = &expr.kind
            && (self.res_def(callee.res)).is_some_and(|def| {
                matches!(
                    def.kind,
                    crate::defs::DefKind::Type(_) | crate::defs::DefKind::TypeAlias
                )
            })
            && let Some(ty) = self.spelled_type(expr, env)
        {
            let ty = match ty {
                Type::Error => Type::Error,
                ty if self.valueless_type(expr, &ty, None) => Type::Error,
                ty => Type::type_of(ty),
            };
            expr.ty = Some(ty.clone());
            return ty;
        }
        let ty = match &mut expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::StringLit(..) => Type::String,
            ExprKind::Unit => Type::Unit,

            ExprKind::StringInterp(..) => self.infer_string_interp(expr, env),

            ExprKind::List(..) => self.infer_list(expr, env),

            ExprKind::Map(..) => self.infer_map(expr, env, span),

            ExprKind::SetLit(..) => self.infer_set(expr, env, span),

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems.iter_mut().map(|e| self.infer_expr(e, env)).collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(..) => self.infer_ident(expr, env, span),

            ExprKind::FieldAccess(..) => match self.infer_field_access(expr, env, span, called) {
                ControlFlow::Continue(ty) => ty,
                ControlFlow::Break(ty) => return ty,
            },

            ExprKind::Binary(..) => self.infer_binary(expr, env, span),

            ExprKind::Unary(..) => self.infer_unary(expr, env),

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
                    let result = self.check_call(callee, args, span, CallForm::Call, env);
                    self.call_results.push((span, result.clone()));
                    result
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

            ExprKind::QuestionMark(..) => self.infer_question_mark(expr, env, span),

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
                let args = args.iter_mut().collect();
                let result = self.check_call(callee, args, span, CallForm::Call, env);
                self.call_results.push((span, result.clone()));
                result
            }

            ExprKind::Lambda { .. } => self.infer_lambda(expr, env, span),

            ExprKind::RecordCreate { .. } => match self.infer_record_create(expr, env, span) {
                ControlFlow::Continue(ty) => ty,
                ControlFlow::Break(ty) => return ty,
            },

            ExprKind::RecordUpdate { .. } => match self.infer_record_update(expr, env, span) {
                ControlFlow::Continue(ty) => ty,
                ControlFlow::Break(ty) => return ty,
            },

            ExprKind::Match { .. } => self.infer_match(expr, env, span),

            ExprKind::Return(..) => self.infer_return(expr, env, span),

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

            ExprKind::Loop { .. } => self.infer_loop(expr, env),

            ExprKind::Recur(..) => self.infer_recur(expr, env, span),
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
            ExprKind::Lambda { body, .. } => {
                self.check_recur_tail_positions(body, RecurPos::InClosure)
            }
            _ => expr.for_each_child(|child| self.check_recur_tail_positions(child, inner)),
        }
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                self.note_let_link(pattern, value);
                // (`let h = go`: a call of `h` reads its parameters as
                // `go` does.)
                if let PatternKind::Ident(name) = pattern.kind
                    && let Some(of) = Self::callee_named(value)
                {
                    self.copy_readings((of, written_name(value)), unused::Callee::Local(name));
                }
                if let (PatternKind::Ident(name), ExprKind::FieldAccess(..)) =
                    (&pattern.kind, &value.kind)
                {
                    self.method_value_lets.insert(value_span, *name);
                }
                // A `let` generalises only a syntactic value: its value is
                // then checked one level deeper, and the variables left
                // at that level are the ones to quantify.
                let is_value = self.is_syntactic_value(value);
                if is_value {
                    self.enter_level();
                }
                // (What the value owes from here: a closure whose decode
                // only its later call decides stays of one type.)
                let owed_from = self.wanted.len();
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

                if let (PatternKind::Ident(name), None, Type::Var(v)) =
                    (&pattern.kind, &ty, self.apply(&val_ty))
                {
                    self.plain_let_vars.entry(v).or_insert(*name);
                }
                // The type variables the annotation introduces.
                let mut own: Vec<RigidId> = Vec::new();
                if let Some(te) = &ty {
                    let (declared, introduced) = self.resolve_let_annotation(te);
                    own = introduced;
                    self.unify(&val_ty, &declared, value_span);
                    // The name is of the declared type, whatever the
                    // value's: a value that never is (`todo()`, of type
                    // `Never`, alone or inside: `(todo(), 1)`, `panic`
                    // as a `Fn(String) -> Int`) meets the annotation
                    // without taking its type, and the name must not
                    // stay of every type. The same for a value of
                    // unknown type (from a module that failed to load)
                    // and for one that is not of the declared type: that
                    // is reported here, and what follows reads the name
                    // as it is declared.
                    val_ty = declared;
                }

                // A call may return a type with shared mutable state (a
                // channel), which must stay monomorphic so that the
                // element type is shared across all uses.
                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        // A closure whose decode picks an impl or needs its
                        // type in full by a type only its parameters' values
                        // give (`{ x -> twice(Box { v: x }, s) }`): of one
                        // type, which its later call decides, as a value's
                        // later use decides a part of the value's type.
                        let decided_later = matches!(value.kind, ExprKind::Lambda { .. })
                            && self.decodes_by_its_values(owed_from);
                        let scheme = if is_value && decided_later {
                            self.decided_closures.push(value.span);
                            self.exit_level();
                            // (Its variables are the scope's: a later `let`
                            // of a value holding it, `[k]`, is not general
                            // in them either.)
                            let ty = self.apply(&val_ty);
                            let level = self.tables.vars.level();
                            self.tables.vars.lower(&ty, level);
                            Scheme::mono(ty)
                        } else if is_value {
                            self.exit_level();
                            self.passing_on =
                                matches!(value.kind, ExprKind::Lambda { .. }).then_some(*name);
                            let scheme = self.generalize_let(&val_ty, &own);
                            self.passing_on = None;
                            let bounds = self.trait_bounds(&scheme.preds);
                            if !bounds.is_empty() {
                                let vars = (self.bound_preds(&scheme.preds).into_iter())
                                    .map(|(_, raw)| raw)
                                    .collect();
                                self.ev.let_vars.insert(value_span, vars);
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
                        // is general as the part is; the value's bounds are
                        // all the names'.
                        if is_value {
                            self.exit_level();
                            let mut bounds = Vec::new();
                            for (name, at) in pattern.binders() {
                                if let Some(bound) = env.lookup(name).cloned() {
                                    let scheme = self.generalize_let(&bound.ty, &own);
                                    let places = self.pattern_places(&scheme, &mut bounds);
                                    if !places.is_empty() {
                                        self.ev.pattern_lets.insert(at, places);
                                    }
                                    env.define(name, scheme);
                                }
                            }
                            if !bounds.is_empty() {
                                self.ev.lets.insert(value_span, bounds);
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
        matches!(ty, Type::Error | Type::Never) || self.tables.trait_impl_set.contains(&(tr, head))
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
            "operator {op} requires Int, Float, String, Bool, Bytes, List, Tuple, Record, or Variant, got '{shown}'"
        ),
    }
}

/// The type a type expression written in a trait declaration names: its
/// resolution (a type's id is its definition's), else the builtin type
/// of that name.
pub(super) fn written_type(te: &TypeExpr, name: Symbol) -> Option<TypeRef> {
    match te.res {
        Some(crate::defs::Res::Def(id)) => Some(TypeRef {
            id: crate::defs::TypeId(id),
            name,
        }),
        Some(_) => None,
        None => crate::defs::builtin_type_id(&resolve(name)).map(|id| TypeRef { id, name }),
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

/// The name a callee is written by (`go`, `m.go`'s `go`).
pub(super) fn written_name(e: &Expr) -> Option<Symbol> {
    match e.kind {
        ExprKind::Ident(name) | ExprKind::FieldAccess(_, name, _) => Some(name),
        _ => None,
    }
}

/// The receiver `e` of a call `e.m(..)` as a message writes it: a name,
/// a field of one, a short literal, or a call of such ones
/// (`first(x)`); `None` for anything longer.
fn written_receiver(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Call(callee, args) => {
            let args: Vec<String> = args.iter().map(written_receiver).collect::<Option<_>>()?;
            Some(format!(
                "{}({})",
                written_receiver(callee)?,
                args.join(", ")
            ))
        }
        _ => Some(super::names::shown_arg(e)).filter(|shown| !shown.contains("...")),
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

/// What an in-full refusal says of the letters `names` only a value
/// holds (`` `a` is known only by its type ``), and the help that takes
/// those of them in `takeable` as type parameters.
pub(super) fn known_only_by_type(
    names: &[Symbol],
    takeable: &[Symbol],
) -> (String, Option<String>) {
    let quoted = |names: &[Symbol]| -> Vec<String> {
        names.iter().map(|name| format!("`{name}`")).collect()
    };
    let listed = |names: Vec<String>| match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.concat(),
    };
    let said = match names {
        [_] => format!("{} is known only by its type", listed(quoted(names))),
        _ => format!("{} are known only by their types", listed(quoted(names))),
    };
    let params: Vec<String> = takeable.iter().map(|name| format!("type {name}")).collect();
    let help = match takeable {
        [] => None,
        [_] => Some(format!("take the type as a parameter, `{}`", params[0])),
        _ => Some(format!(
            "take the types as parameters, `{}`",
            params.join(", ")
        )),
    };
    (said, help)
}

/// The rigid type variables in `ty`, in order.
pub(super) fn rigid_vars(ty: &Type) -> Vec<RigidId> {
    fn walk(ty: &Type, out: &mut Vec<RigidId>) {
        match ty {
            Type::Rigid(r) if !out.contains(r) => out.push(*r),
            Type::App(head, arg) => {
                walk(head, out);
                walk(arg, out);
            }
            Type::Fun(params, ret) => {
                params.iter().for_each(|t| walk(t, out));
                walk(ret, out);
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => walk(t, out),
            Type::Map(k, v) => {
                walk(k, out);
                walk(v, out);
            }
            Type::Tuple(ts) | Type::Generic(_, ts) | Type::Con(_, ts) => {
                ts.iter().for_each(|t| walk(t, out))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
}

/// Whether `ty` has a type held as a value (`type Int`) in it, at any
/// depth: `(1, [Int])` does.
fn holds_held_type(ty: &Type) -> bool {
    match ty {
        Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF) => true,
        Type::Generic(_, ts) | Type::Tuple(ts) | Type::Con(_, ts) => ts.iter().any(holds_held_type),
        Type::Fun(params, ret) => params.iter().any(holds_held_type) || holds_held_type(ret),
        Type::List(t) | Type::Set(t) | Type::Channel(t) => holds_held_type(t),
        Type::Map(k, v) => holds_held_type(k) || holds_held_type(v),
        Type::App(head, arg) => holds_held_type(head) || holds_held_type(arg),
        _ => false,
    }
}

/// `ty` with each type held as a value in its arguments (`type Int`)
/// the type it holds (`Int`).
fn without_held(ty: &Type) -> Type {
    match ty {
        Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
            args.first().map_or_else(|| ty.clone(), without_held)
        }
        Type::Generic(name, args) => Type::Generic(*name, args.iter().map(without_held).collect()),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(without_held).collect()),
        _ => ty.clone(),
    }
}

/// For each parameter `names` names, whether `body` reads a field of it
/// (`t.n`, or a record update `t.{ n: 3 }`; not a call `t.zero()`).
fn read_fields(body: &mut Expr, names: &[Option<Symbol>]) -> Vec<bool> {
    (names.iter())
        .map(|name| name.is_some_and(|name| reads_field_of(body, name)))
        .collect()
}

/// Whether `expr` reads a field of the local `name` (`name.n`) where no
/// binding in `expr` hides it. A dot that is called (`name.f(..)`, `x |>
/// name.f`) reads none.
fn reads_field_of(expr: &mut Expr, name: Symbol) -> bool {
    let binds = |pattern: &Pattern| pattern.binder_names().contains(&name);
    // (The receiver of a dot that is called.)
    let callee_reads = |callee: &mut Expr| match &mut callee.kind {
        ExprKind::FieldAccess(obj, ..) => reads_field_of(obj, name),
        _ => reads_field_of(callee, name),
    };
    match &mut expr.kind {
        ExprKind::FieldAccess(obj, ..)
            if matches!(obj.kind, ExprKind::Ident(n) if n == name)
                && obj.res == Some(crate::defs::Res::Local) =>
        {
            true
        }
        // (A record update of it reads its fields as a field read does.)
        ExprKind::RecordUpdate { expr: base, .. }
            if matches!(base.kind, ExprKind::Ident(n) if n == name)
                && base.res == Some(crate::defs::Res::Local) =>
        {
            true
        }
        ExprKind::Call(callee, args) => {
            callee_reads(callee) || args.iter_mut().any(|arg| reads_field_of(arg, name))
        }
        ExprKind::Pipe(value, callee) => reads_field_of(value, name) || callee_reads(callee),
        ExprKind::Lambda { params, body } => {
            !params.iter().any(|param| binds(&param.pattern)) && reads_field_of(body, name)
        }
        ExprKind::Match {
            expr: scrutinee,
            arms,
        } => {
            (scrutinee.as_deref_mut()).is_some_and(|s| reads_field_of(s, name))
                || arms.iter_mut().any(|arm| {
                    !binds(&arm.pattern)
                        && ((arm.guard.as_deref_mut()).is_some_and(|g| reads_field_of(g, name))
                            || reads_field_of(&mut arm.body, name))
                })
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                let (reads, hidden) = match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        (reads_field_of(value, name), binds(pattern))
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                    } => (
                        reads_field_of(expr, name) || reads_field_of(else_body, name),
                        binds(pattern),
                    ),
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => (
                        reads_field_of(condition, name) || reads_field_of(else_body, name),
                        false,
                    ),
                    Stmt::Expr(e) => (reads_field_of(e, name), false),
                };
                if reads || hidden {
                    return reads;
                }
            }
            false
        }
        ExprKind::Loop { bindings, body } => {
            bindings
                .iter_mut()
                .any(|(_, _, value)| reads_field_of(value, name))
                || (!bindings.iter().any(|(bound, ..)| *bound == name)
                    && reads_field_of(body, name))
        }
        _ => {
            let mut found = false;
            expr.for_each_child_mut(|child| {
                found |= reads_field_of(child, name);
            });
            found
        }
    }
}
