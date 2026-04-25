//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

mod builtins;
mod exhaustiveness;
mod inference;
mod resolve;
mod suggest;

pub(super) use std::collections::{BTreeSet, HashMap};

pub(super) use crate::ast::*;
pub(super) use crate::intern::{Symbol, intern, resolve};
pub(super) use crate::lexer::Span;
pub(super) use crate::types::*;

pub use crate::types::{Scheme, Severity, TyVar, Type, TypeError};

/// Names of builtin traits that the compiler registers automatically
/// with auto-derived impls for every primitive and builtin container.
/// User code cannot redeclare a trait with any of these names — doing
/// so would shadow the compiler's TraitInfo (different method names,
/// different signatures) and produce nonsensical cascade errors when
/// the preregistered impls get revalidated against the user's body.
pub(super) const BUILTIN_TRAIT_NAMES: &[&str] = &["Equal", "Compare", "Hash", "Display", "Error"];

/// Subset of [`BUILTIN_TRAIT_NAMES`] that is auto-derived for every
/// primitive and builtin container. `Error` is intentionally excluded:
/// user types and stdlib types must implement `trait Error for ...`
/// explicitly.
pub(super) const BUILTIN_AUTO_DERIVED_TRAIT_NAMES: &[&str] =
    &["Equal", "Compare", "Hash", "Display"];

// ── Type environment ────────────────────────────────────────────────

/// A typing environment mapping names to type schemes.
#[derive(Debug, Clone)]
pub(super) struct TypeEnv {
    pub(super) bindings: HashMap<Symbol, Scheme>,
    parent: Option<Box<TypeEnv>>,
}

impl TypeEnv {
    pub(super) fn new() -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    pub(super) fn child(&self) -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    pub(super) fn define(&mut self, name: Symbol, scheme: Scheme) {
        self.bindings.insert(name, scheme);
    }

    pub(super) fn lookup(&self, name: Symbol) -> Option<&Scheme> {
        if let Some(s) = self.bindings.get(&name) {
            Some(s)
        } else if let Some(ref parent) = self.parent {
            parent.lookup(name)
        } else {
            None
        }
    }

    /// Collect all in-scope names into `out`. Walks the scope chain from
    /// innermost (self) to outermost (root), inserting each `Symbol`
    /// once. Used by the "did you mean ...?" suggestion path so the type
    /// checker can enumerate candidate names to match against a typo.
    pub(super) fn collect_names(&self, out: &mut BTreeSet<Symbol>) {
        for k in self.bindings.keys() {
            out.insert(*k);
        }
        if let Some(ref parent) = self.parent {
            parent.collect_names(out);
        }
    }

    /// Collect all free type variables in the environment.
    pub(super) fn free_vars(&self, checker: &TypeChecker) -> Vec<TyVar> {
        let mut fvs = Vec::new();
        for scheme in self.bindings.values() {
            let ty = checker.apply(&scheme.ty);
            let mut ty_fvs = free_vars_in(&ty);
            // Remove the scheme's own quantified variables
            ty_fvs.retain(|v| !scheme.vars.contains(v));
            for v in ty_fvs {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        if let Some(ref parent) = self.parent {
            for v in parent.free_vars(checker) {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        fvs
    }
}

// ── Type declaration info ───────────────────────────────────────────

/// Information about a declared enum type.
#[derive(Debug, Clone)]
pub(super) struct EnumInfo {
    pub(super) params: Vec<Symbol>,
    /// The actual TyVar ids assigned to each type parameter (same order as `params`).
    pub(super) param_var_ids: Vec<TyVar>,
    pub(super) variants: Vec<VariantInfo>,
}

#[derive(Debug, Clone)]
pub(super) struct VariantInfo {
    pub(super) name: Symbol,
    pub(super) field_types: Vec<Type>,
}

/// Information about a declared record type.
#[derive(Debug, Clone)]
pub(super) struct RecordInfo {
    pub(super) fields: Vec<(Symbol, Type)>,
}

/// Information about a declared trait.
#[derive(Debug, Clone)]
pub(super) struct TraitInfo {
    /// Type-parameter names on the trait itself (e.g.
    /// `trait TryInto(b)` yields `[b]`). Empty for parameter-less
    /// traits — the common case.
    pub(super) params: Vec<Symbol>,
    /// Fresh TyVars allocated for each trait parameter at
    /// `register_trait_decl`. Stored so impls (and where clauses with
    /// trait args) can substitute their args into the trait's method
    /// signatures via `substitute_vars`. Parallel to `params`.
    pub(super) param_var_ids: Vec<TyVar>,
    /// Bounds declared directly on trait params, e.g.
    /// `trait HashTable(k) where k: Hash`. Each `(param_name, trait_name)`
    /// entry is checked at `register_trait_impl` against the concrete
    /// type the impl supplies for that param.
    pub(super) param_where_clauses: Vec<(Symbol, Symbol)>,
    /// Supertrait names (e.g. `trait Ordered: Equal` yields `[Equal]`).
    /// Implementing this trait on a type requires every supertrait to also
    /// be implemented for the same type (validated in
    /// `validate_trait_impls`). `expand_with_supertraits` walks this list
    /// transitively to enable supertrait method calls inside `where`
    /// clauses.
    pub(super) supertraits: Vec<Symbol>,
    /// Parallel to `supertraits`: the TypeExpr args supplied to each
    /// supertrait reference. For `trait Sub(a): Super(a)` the entry for
    /// `Super` is `[TypeExpr::Named("a")]`. Empty when the supertrait
    /// is referenced without args. The `expand_with_supertraits_args`
    /// path uses these to propagate the enclosing trait's args into
    /// the supertrait's `trait_arg_bindings` during where-clause
    /// activation. Arg-less entries keep the bare-name behaviour.
    pub(super) supertrait_args: Vec<Vec<TypeExpr>>,
    pub(super) methods: Vec<(Symbol, Type)>,
    /// Source span of the trait declaration. Used by
    /// `validate_trait_impls` to report unknown-supertrait errors at the
    /// declaration site.
    pub(super) decl_span: Span,
    /// Default method bodies declared inside the trait. Maps method name
    /// to the full FnDecl (with body). Impls that omit a method whose
    /// name appears here are not "missing method" errors — instead the
    /// FnDecl is cloned into the impl's `methods` vec by
    /// `synthesize_default_methods` so the rest of the pipeline (signature
    /// registration, body checking, dispatch, compilation) treats it
    /// identically to an explicitly-written method.
    pub(super) default_method_bodies: HashMap<Symbol, FnDecl>,
}

/// A registered trait method implementation (new trait system).
#[derive(Debug, Clone)]
pub(super) struct MethodEntry {
    pub(super) method_type: Type,
    pub(super) span: Span,
    pub(super) is_auto_derived: bool,
    /// GAP (round 17 F3): name of the trait that provided this method.
    /// Used for coherence diagnostics when two distinct traits supply
    /// a method with the same name for the same target type. `None`
    /// for auto-derived entries (Showable on every type, etc.) that
    /// don't participate in user-visible coherence rules.
    pub(super) trait_name: Option<Symbol>,
    /// Trait constraints that must hold at every call site of this
    /// method. Accumulated from:
    ///
    /// (a) impl-level `where` clauses on the trait-impl header
    ///     (`trait Greet for Box(a) where a: Greet { ... }`) — attached
    ///     to every method in the impl.
    /// (b) method-level `where` clauses on individual impl methods
    ///     (`fn greet(self) -> String where a: Greet { ... }`) — where
    ///     `a` is either an impl-level binder or a method param binder.
    ///
    /// TyVars here live in the same TyVar space as `method_type`, so
    /// `instantiate_method_entry` can substitute both through a shared
    /// mapping. Empty for auto-derived entries and for impls with no
    /// where clauses (which is every impl today prior to this feature).
    pub(super) method_constraints: Vec<(TyVar, Symbol)>,
}

/// A deferred where-clause obligation captured at a call site whose
/// type argument was still an unresolved type variable. Resolved at
/// the end of inference in `finalize_deferred_checks`.
#[derive(Debug, Clone)]
pub(super) struct PendingWhereConstraint {
    /// The tyvar at the call site that carries the obligation.
    pub(super) tyvar: TyVar,
    /// The trait name the obligation requires.
    pub(super) trait_name: Symbol,
    /// Name of the callee function (for nicer diagnostics).
    pub(super) callee_fn_name: Option<Symbol>,
    /// Span of the call site.
    pub(super) span: Span,
    /// Snapshot of the enclosing fn's active constraints at the
    /// time of the call (used to decide whether the obligation is
    /// already covered).
    pub(super) active_snapshot: HashMap<TyVar, Vec<Symbol>>,
    /// Snapshot of the enclosing fn's param tyvars at the time of
    /// the call (used to decide whether the obligation touches the
    /// enclosing fn's own polymorphism).
    pub(super) param_tyvars: Vec<TyVar>,
    /// Snapshot of the enclosing fn's trait arg bindings for this
    /// `(tyvar, trait_name)` pair at the time of the call, e.g.
    /// `[Int]` for `where a: TryInto(Int)`. Empty for parameterless
    /// traits. Used during finalize so parameterized-trait verification
    /// can compare bound args against the matched impl's args.
    pub(super) bound_trait_args: Vec<Type>,
}

// ── The type checker ────────────────────────────────────────────────

pub struct TypeChecker {
    /// The substitution: maps type variables to their resolved types.
    pub(super) subst: Vec<Option<Type>>,
    /// Counter for generating fresh type variables.
    pub(super) next_var: TyVar,
    /// Declared enum types (type name -> enum info).
    pub(super) enums: HashMap<Symbol, EnumInfo>,
    /// Maps variant constructor name -> parent enum type name.
    pub(super) variant_to_enum: HashMap<Symbol, Symbol>,
    /// Declared record types (type name -> record info).
    pub(super) records: HashMap<Symbol, RecordInfo>,
    /// Declared traits.
    pub(super) traits: HashMap<Symbol, TraitInfo>,
    /// Method table: (type_name, method_name) → method entry.
    pub(super) method_table: HashMap<(Symbol, Symbol), MethodEntry>,
    /// Tracks which (trait_name, type_name) pairs have been implemented.
    pub(super) trait_impl_set: std::collections::HashSet<(Symbol, Symbol)>,
    /// GAP-2: Maps `(trait_name, type_name)` → the span of the
    /// `trait T for U { ... }` declaration, so the missing-method
    /// diagnostic in `validate_trait_impls` can point at the impl
    /// block's real source location instead of `Span::new(0, 0)`.
    pub(super) trait_impl_spans: HashMap<(Symbol, Symbol), Span>,
    /// Maps `(trait_name, target_head)` → impl-level where-clause
    /// obligations expressed as `(target_arg_index, required_trait)`
    /// pairs. Populated from `register_trait_impl` so that constraint
    /// resolution at call sites can recursively verify that the
    /// concrete type arguments of the matched impl themselves satisfy
    /// the impl's own where clauses (e.g. `Box(Box(String)): Greet`
    /// with `trait Greet for Box(a) where a: Greet` must reject because
    /// String does not impl Greet, even though `(Greet, Box)` is in
    /// `trait_impl_set`).
    pub(super) impl_constraints: HashMap<(Symbol, Symbol), Vec<(usize, Symbol)>>,
    /// Maps `(trait_name, target_head)` → the resolved trait args supplied
    /// at impl site. For `trait TryInto(Float) for String { ... }` this
    /// stores `(TryInto, String) -> [Float]`. `verify_trait_obligation`
    /// consults this when the where-clause bound also carries trait args
    /// (e.g. `where a: TryInto(Int)`) so that a concrete mismatch (Int vs
    /// Float) is rejected — closing the soundness hole where parameterized-
    /// trait where-clause verification previously ignored trait args.
    /// Absent for parameter-less traits.
    pub(super) impl_trait_args: HashMap<(Symbol, Symbol), Vec<Type>>,
    /// Maps function names to their where clauses as (param_index, trait_name).
    /// Accumulated type errors.
    pub errors: Vec<TypeError>,
    /// Tracks the types of bindings in the enclosing `loop` (if any),
    /// so that `recur` arity and types can be validated.
    pub(super) loop_binding_types: Option<Vec<Type>>,
    /// Active trait constraints for type variables in the current function body.
    /// Maps type variable → list of trait names it must satisfy.
    /// Populated during `check_fn_body` to enable method resolution on constrained vars.
    pub(super) active_constraints: HashMap<TyVar, Vec<Symbol>>,
    /// Side channel holding trait arguments for parameterized-trait
    /// constraints, e.g. `where a: TryInto(Int)` stores `[Int]` under
    /// the key `(tyvar_of_a, TryInto)`. Populated during
    /// `register_fn_decl` and `register_trait_impl` alongside the
    /// parallel monomorphic `active_constraints`; consumed during
    /// descriptor method resolution to substitute trait params.
    /// Absent for bare `where a: Display` entries.
    pub(super) trait_arg_bindings: HashMap<(TyVar, Symbol), Vec<Type>>,
    /// The expected return type of the enclosing function (if any).
    pub(super) current_return_type: Option<Type>,
    /// Maps record type names to their type parameter TyVar ids.
    pub(super) record_param_var_ids: HashMap<Symbol, Vec<TyVar>>,
    /// Maps function names to their body-constrained types (populated during check_fn_body).
    pub(super) fn_body_types: HashMap<Symbol, Type>,
    /// Deferred checks for field access on type variables (B4).
    /// Each entry is `(object_type, field_name, result_type, span)`.
    /// Re-examined after all function bodies are inferred: if the object type
    /// is still a Var, we emit an error.
    pub(super) pending_field_accesses: Vec<(Type, Symbol, Type, Span)>,
    /// Deferred checks for numeric operations on type variables (B5 / B2).
    /// Each entry is `(operand_type, op_description, span)`. Re-examined after
    /// all function bodies are inferred: if the operand is still a Var, we
    /// emit an error.
    pub(super) pending_numeric_checks: Vec<(Type, &'static str, Span)>,
    /// Names that have been registered as user-defined top-level declarations
    /// (functions, let bindings, type/record/enum names). Used by G1 to
    /// detect duplicate top-level definitions without also flagging
    /// user code that shadows a builtin (which remains a warning).
    pub(super) top_level_names: std::collections::HashSet<Symbol>,
    /// Set by the exhaustiveness checker when its recursion depth bound is
    /// exceeded during a single `check_exhaustiveness` call. Interior
    /// mutability lets the `&self`-taking `is_useful` recursion record the
    /// event without threading a result type through every recursive call.
    /// Reset at the start of each `check_exhaustiveness` invocation.
    pub(super) exhaustiveness_depth_exceeded: std::cell::Cell<bool>,
    /// Names of function declarations that were synthesized by parser
    /// error recovery (Option B). Populated in register_fn_decl when the
    /// FnDecl has `is_recovery_stub == true`. Used by the `ExprKind::Call`
    /// arm to suppress cascade errors (arity/arg-type) — the real parse
    /// error already told the user what went wrong, so reporting N bogus
    /// "undefined variable 'f'" or "function expects 2 args, got 1"
    /// errors would just be noise.
    pub(super) recovery_stub_names: std::collections::HashSet<Symbol>,
    /// B2: span used by `resolve_type_expr` when reporting arity errors
    /// on user type annotations. Callers set this to the surrounding
    /// declaration's span (e.g. `f.span`) before calling resolve, and
    /// reset it afterward. Defaults to a sentinel zero-span when no
    /// caller has populated it.
    pub(super) current_type_anno_span: Option<Span>,
    /// B4: deferred where-clause obligations seen at call sites where
    /// the type argument stayed an unresolved type variable.
    /// Finalize re-applies the substitution after all bodies are
    /// inferred: if the var resolved to a concrete type with a
    /// matching impl, the obligation is satisfied; if it resolved to
    /// a type variable still not covered by the enclosing fn's
    /// active constraints at the time of the call, a clean
    /// diagnostic is emitted.
    pub(super) pending_where_constraints: Vec<PendingWhereConstraint>,
    /// B4: the instantiated type-variable IDs of the enclosing function's
    /// parameters at the time `check_fn_body` is running. Used to decide
    /// whether a call-site where-constraint is touching the enclosing fn's
    /// own polymorphism (in which case the enclosing fn must declare the
    /// constraint) or a top-level unrelated Var (in which case we leave
    /// the obligation alone — the value will resolve via pass-3 narrowing).
    pub(super) current_fn_param_tyvars: Vec<TyVar>,
    /// Audit round 19: tracks trait constraints on type variables created
    /// by `instantiate_with_constraints`. When a scheme with where-clause
    /// constraints is instantiated, the fresh type variables inherit the
    /// constraints here. `generalize` then consults this map to propagate
    /// constraints into newly created schemes (e.g. `let f = constrained_fn`).
    pub(super) tyvar_trait_constraints: HashMap<TyVar, Vec<Symbol>>,
    /// Set by the FieldAccess arm of infer_expr: `true` when the last
    /// FieldAccess resolved via method dispatch (trait method table),
    /// `false` when it resolved via record-field or module-qualified
    /// lookup.  Read by the Call arm immediately after inferring the
    /// callee to decide arity semantics (method call adds implicit self;
    /// field/module calls do not).
    pub(super) last_field_access_was_method: bool,
    /// Round 56 item 4: the set of module names visible through `import`
    /// statements in the current program. Populated at the start of
    /// `check()`. Used by the `FieldAccess` path to decide whether
    /// `list.sum(...)` should typecheck or emit an
    /// "module 'X' is not imported; add `import X`" error. Stdlib
    /// module names (`list`, `string`, ...) have all their qualified
    /// members pre-registered in the environment, so without this
    /// gate they'd typecheck silently even when never imported; the
    /// compiler would then emit the import-recommendation at its own
    /// layer but the typechecker said nothing. The audit decision
    /// (round 52 item 4) is that stdlib should be opaque until
    /// imported, so this gate fires at typecheck time.
    ///
    /// Entries:
    ///   - `import list` / `import list.{sum}` → contains `list`.
    ///   - `import list as l` → contains `l` (alias), NOT `list` —
    ///     the user renamed the module, so its original name is no
    ///     longer in scope.
    pub(super) imported_modules: std::collections::HashSet<Symbol>,
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeChecker {
    pub fn new() -> Self {
        TypeChecker {
            subst: Vec::new(),
            next_var: 0,
            enums: HashMap::new(),
            variant_to_enum: HashMap::new(),
            records: HashMap::new(),
            traits: HashMap::new(),
            method_table: HashMap::new(),
            trait_impl_set: std::collections::HashSet::new(),
            trait_impl_spans: HashMap::new(),
            impl_constraints: HashMap::new(),
            impl_trait_args: HashMap::new(),
            errors: Vec::new(),
            loop_binding_types: None,
            active_constraints: HashMap::new(),
            trait_arg_bindings: HashMap::new(),
            current_return_type: None,
            record_param_var_ids: HashMap::new(),
            fn_body_types: HashMap::new(),
            pending_field_accesses: Vec::new(),
            pending_numeric_checks: Vec::new(),
            top_level_names: std::collections::HashSet::new(),
            exhaustiveness_depth_exceeded: std::cell::Cell::new(false),
            recovery_stub_names: std::collections::HashSet::new(),
            current_type_anno_span: None,
            pending_where_constraints: Vec::new(),
            current_fn_param_tyvars: Vec::new(),
            tyvar_trait_constraints: HashMap::new(),
            last_field_access_was_method: false,
            imported_modules: std::collections::HashSet::new(),
        }
    }

    // ── Fresh variables ─────────────────────────────────────────────

    pub(super) fn fresh_var(&mut self) -> Type {
        let v = self.next_var;
        self.next_var += 1;
        self.subst.push(None);
        Type::Var(v)
    }

    // ── Substitution / apply ────────────────────────────────────────

    /// Walk the substitution chain to find the most resolved type.
    pub(super) fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::Var(v) => {
                if let Some(Some(resolved)) = self.subst.get(*v) {
                    self.apply(resolved)
                } else {
                    ty.clone()
                }
            }
            Type::Fun(params, ret) => {
                let params = params.iter().map(|p| self.apply(p)).collect();
                let ret = Box::new(self.apply(ret));
                Type::Fun(params, ret)
            }
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Range(inner) => Type::Range(Box::new(self.apply(inner))),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.apply(e)).collect()),
            Type::Record(name, fields) => {
                let fields = fields.iter().map(|(n, t)| (*n, self.apply(t))).collect();
                Type::Record(*name, fields)
            }
            Type::Generic(name, args) => {
                let args = args.iter().map(|a| self.apply(a)).collect();
                Type::Generic(*name, args)
            }
            Type::Map(k, v) => Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v))),
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            Type::Channel(inner) => Type::Channel(Box::new(self.apply(inner))),
            _ => ty.clone(),
        }
    }

    // ── Unification ─────────────────────────────────────────────────

    pub(super) fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        match (&t1, &t2) {
            (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never) => {}
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::ExtFloat, Type::ExtFloat)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => {}

            (Type::Var(v1), Type::Var(v2)) if v1 == v2 => {}

            (Type::Var(v), t) | (t, Type::Var(v)) => {
                if occurs_in(*v, t) {
                    // Special case: trying to unify a type var `a` with
                    // `TypeOf(a)` means the user returned a `type a`
                    // parameter's value where they promised a value of
                    // type `a`. The descriptor is the *type*, not an
                    // instance of it.
                    if let Type::Generic(name, args) = t
                        && resolve(*name) == "TypeOf"
                        && args.len() == 1
                        && matches!(&args[0], Type::Var(inner) if inner == v)
                    {
                        self.error(
                            "cannot return a `type a` parameter as a value of type `a` — \
                             the parameter is a type descriptor, not an instance. \
                             Construct an `a` in the body instead."
                                .to_string(),
                            span,
                        );
                    } else {
                        self.error(
                            format!("infinite type: the type variable appears inside {t}"),
                            span,
                        );
                    }
                } else {
                    self.subst[*v] = Some(t.clone());
                }
            }

            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    // Directional convention: t1 is the "got" side, t2 is
                    // the "expected" side (see the Record arm below and
                    // every unify() call site — e.g. `unify(body_ty,
                    // ret_ty, span)` passes got then expected). Earlier
                    // rounds formatted `p1.len()` as "expected", which
                    // reversed the diagnostic.
                    let (exp, got) = (p2.len(), p1.len());
                    self.error(
                        format!(
                            "function expects {exp} {arg_word}, got {got}",
                            arg_word = if exp == 1 { "argument" } else { "arguments" }
                        ),
                        span,
                    );
                } else {
                    for (a, b) in p1.iter().zip(p2.iter()) {
                        self.unify(a, b, span);
                    }
                    self.unify(r1, r2, span);
                }
            }

            (Type::List(a), Type::List(b)) => {
                self.unify(a, b, span);
            }

            (Type::Range(a), Type::Range(b)) => {
                self.unify(a, b, span);
            }

            // Range(T) is a nominal zero-cost alias for List(T): they
            // unify bidirectionally at the element level. `1..10` infers
            // as `Range(Int)` so annotations `let r: Range(Int) = 1..10`
            // typecheck, but existing `list.*` call sites still accept
            // ranges and `let r: List(Int) = 1..10` still typechecks.
            // Runtime representation is unchanged (Vec<Value>).
            (Type::Range(a), Type::List(b)) | (Type::List(b), Type::Range(a)) => {
                self.unify(a, b, span);
            }

            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                self.unify(k1, k2, span);
                self.unify(v1, v2, span);
            }

            (Type::Set(a), Type::Set(b)) => {
                self.unify(a, b, span);
            }

            (Type::Channel(a), Type::Channel(b)) => {
                self.unify(a, b, span);
            }

            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.len() != b.len() {
                    // Directional convention: t1 (=a) is the "got" side,
                    // t2 (=b) is the "expected" side. Earlier wording
                    // "expected {a.len()}, got {b.len()}" reversed this.
                    self.error(
                        format!(
                            "tuple length mismatch: expected {}, got {}",
                            b.len(),
                            a.len()
                        ),
                        span,
                    );
                } else {
                    for (x, y) in a.iter().zip(b.iter()) {
                        self.unify(x, y, span);
                    }
                }
            }

            (Type::Record(n1, f1), Type::Record(n2, f2)) => {
                if n1 != n2 {
                    self.error(
                        format!("record type mismatch: expected {n2}, got {n1}"),
                        span,
                    );
                } else {
                    // Unify fields by name. Messages are directional:
                    // `t1` is the got side, `t2` is the expected side
                    // (see the tuple/Generic arms above — `unify(t1, t2)`
                    // treats `t2` as expected, `t1` as got). The symmetric
                    // "record is missing field" wording was ambiguous
                    // about which side was at fault; split into distinct
                    // "unexpected field" (got has a surplus) and
                    // "missing field" (got is short) diagnostics so the
                    // caret + message unambiguously identifies the fault.
                    for (name, t1_inner) in f1 {
                        if let Some((_, t2_inner)) = f2.iter().find(|(n, _)| n == name) {
                            self.unify(t1_inner, t2_inner, span);
                        } else {
                            self.error(
                                format!(
                                    "unexpected field '{name}' in record; type '{n1}' has no such field"
                                ),
                                span,
                            );
                        }
                    }
                    for (name, _t2_inner) in f2 {
                        if !f1.iter().any(|(n, _)| n == name) {
                            self.error(
                                format!(
                                    "missing field '{name}' in record; type '{n1}' requires it"
                                ),
                                span,
                            );
                        }
                    }
                }
            }

            // Record(name, fields) is compatible with Generic(name, args)
            (Type::Record(n1, f1), Type::Generic(n2, a2)) if n1 == n2 && !a2.is_empty() => {
                // B2 (round 60): a parameterless record carries Generic args
                // here only when the user wrote `Point(Bool)` against a
                // `type Point { ... }` with no params — `record_param_var_ids`
                // is absent for parameterless records, so the silent no-op
                // path swallowed the arity violation. Reject explicitly.
                if !self.record_param_var_ids.contains_key(n1)
                    && self.records.contains_key(n1)
                {
                    self.error(
                        format!(
                            "type argument count mismatch for {n1}: expected 0, got {}",
                            a2.len()
                        ),
                        span,
                    );
                    return;
                }
                if let (Some(rec_info), Some(param_var_ids)) = (
                    self.records.get(n1).cloned(),
                    self.record_param_var_ids.get(n1).cloned(),
                ) && param_var_ids.len() == a2.len()
                {
                    for (field_name, field_template_ty) in &rec_info.fields {
                        let substituted =
                            substitute_enum_params(field_template_ty, &param_var_ids, a2);
                        if let Some((_, concrete_ty)) = f1.iter().find(|(n, _)| n == field_name) {
                            self.unify(concrete_ty, &substituted, span);
                        }
                    }
                }
            }
            (Type::Record(n1, _), Type::Generic(n2, a2)) if n1 == n2 && a2.is_empty() => {
                // Only allow bare `Generic(name, [])` to match a Record if
                // the record is actually parameterless. For parameterized
                // records the Generic side must carry type args — otherwise
                // silently accepting it would let distinct uses pollute
                // the shared template TyVars (T1 audit fix).
                let expected = self
                    .record_param_var_ids
                    .get(n1)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if expected != 0 {
                    self.error(
                        format!(
                            "type argument count mismatch for {n1}: expected {expected}, got 0"
                        ),
                        span,
                    );
                }
            }
            (Type::Generic(n1, a1), Type::Record(n2, f2)) if n1 == n2 && !a1.is_empty() => {
                // B2 (round 60) mirror: parameterless record with Generic args.
                if !self.record_param_var_ids.contains_key(n2)
                    && self.records.contains_key(n2)
                {
                    self.error(
                        format!(
                            "type argument count mismatch for {n2}: expected 0, got {}",
                            a1.len()
                        ),
                        span,
                    );
                    return;
                }
                if let (Some(rec_info), Some(param_var_ids)) = (
                    self.records.get(n2).cloned(),
                    self.record_param_var_ids.get(n2).cloned(),
                ) && param_var_ids.len() == a1.len()
                {
                    for (field_name, field_template_ty) in &rec_info.fields {
                        let substituted =
                            substitute_enum_params(field_template_ty, &param_var_ids, a1);
                        if let Some((_, concrete_ty)) = f2.iter().find(|(n, _)| n == field_name) {
                            self.unify(concrete_ty, &substituted, span);
                        }
                    }
                }
            }
            (Type::Generic(n1, a1), Type::Record(n2, _)) if n1 == n2 && a1.is_empty() => {
                // Mirror image of the Record/Generic arm above.
                let expected = self
                    .record_param_var_ids
                    .get(n2)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if expected != 0 {
                    self.error(
                        format!(
                            "type argument count mismatch for {n2}: expected {expected}, got 0"
                        ),
                        span,
                    );
                }
            }

            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                if n1 != n2 {
                    let mut msg = format!("type mismatch: expected {n2}, got {n1}");
                    if let Some(hint) = Self::chain_hint(&t1, &t2) {
                        msg.push('\n');
                        msg.push_str(&hint);
                    }
                    self.error(msg, span);
                } else if a1.len() != a2.len() {
                    // Directional convention: t1 (=a1) is the "got" side,
                    // t2 (=a2) is the "expected" side (see the Record arm
                    // above and the unify() callsite convention). Earlier
                    // wording had a1/a2 reversed.
                    self.error(
                        format!(
                            "type argument count mismatch for {n1}: expected {}, got {}",
                            a2.len(),
                            a1.len()
                        ),
                        span,
                    );
                } else {
                    for (x, y) in a1.iter().zip(a2.iter()) {
                        self.unify(x, y, span);
                    }
                }
            }

            _ => {
                // Suppress cascade errors where either side is already in
                // error state — a previous diagnostic explained the root
                // cause and further mismatch reports would confuse.
                if matches!(&t1, Type::Error) || matches!(&t2, Type::Error) {
                    return;
                }
                // When either side is an unresolved type variable, the
                // user doesn't have a user-facing name for it yet
                // (`?17` is internal). Report as "cannot determine" and
                // nudge toward an annotation.
                match (&t1, &t2) {
                    (Type::Var(_), other) | (other, Type::Var(_)) => {
                        self.error(
                            format!(
                                "cannot determine a consistent type here; \
                                 one side resolved to `{other}` but the other \
                                 is still unspecified — add a type annotation"
                            ),
                            span,
                        );
                    }
                    _ => {
                        let mut msg = format!("type mismatch: expected {t2}, got {t1}");
                        if let Some(hint) = Self::chain_hint(&t1, &t2) {
                            msg.push('\n');
                            msg.push_str(&hint);
                        }
                        self.error(msg, span);
                    }
                }
            }
        }
    }

    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme by quantifying over free variables
    /// not present in the environment.
    ///
    /// Audit round 19: constraints are no longer unconditionally empty.
    /// We scan `tyvar_trait_constraints` for any recorded constraint whose
    /// tyvar resolves (via `apply`) to one of the quantified vars, and
    /// include those constraints in the resulting scheme. This ensures that
    /// `let f = constrained_fn` and `let f = { x -> constrained_fn(x) }`
    /// preserve where-clause obligations.
    pub(super) fn generalize(&self, env: &TypeEnv, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let env_fvs = env.free_vars(self);
        let ty_fvs = free_vars_in(&ty);
        let vars: Vec<TyVar> = ty_fvs
            .into_iter()
            .filter(|v| !env_fvs.contains(v))
            .collect();
        // Collect constraints: for each entry in tyvar_trait_constraints,
        // resolve the tyvar and check if it matches a quantified var.
        let mut constraints: Vec<(TyVar, Symbol)> = Vec::new();
        if !vars.is_empty() {
            for (&tv, trait_names) in &self.tyvar_trait_constraints {
                let resolved = self.apply(&Type::Var(tv));
                if let Type::Var(rv) = resolved
                    && vars.contains(&rv)
                {
                    for &trait_name in trait_names {
                        if !constraints.contains(&(rv, trait_name)) {
                            constraints.push((rv, trait_name));
                        }
                    }
                }
            }
        }
        Scheme {
            vars,
            ty,
            constraints,
        }
    }

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Type {
        self.instantiate_with_constraints(scheme).0
    }

    /// Instantiate a `MethodEntry`'s template type by generating fresh type
    /// variables for every free type variable in it.
    ///
    /// Method entries store a raw `Type` (not a `Scheme`) for historical
    /// reasons. Without this instantiation, the first call to a polymorphic
    /// auto-derived method (e.g. `equal`) would permanently bind its
    /// parameter type variables via unification, breaking subsequent calls
    /// with different argument types.
    pub(super) fn instantiate_method_type(&mut self, ty: &Type) -> Type {
        let ty = self.apply(ty);
        let fvs = free_vars_in(&ty);
        if fvs.is_empty() {
            return ty;
        }
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for v in fvs {
            mapping.insert(v, self.fresh_var());
        }
        substitute_vars(&ty, &mapping)
    }

    /// Instantiate a `MethodEntry`'s template type AND its where-clause
    /// constraints through a single shared substitution, so the returned
    /// `(Type, Vec<(TyVar, Symbol)>)` pair uses consistent fresh TyVars.
    ///
    /// Constraint TyVars that appear in `method_type`'s free-var set map
    /// through the same fresh-var substitution as the type itself; any
    /// constraint TyVars not in the free set (edge case: a constraint on
    /// a binder that doesn't appear in the method's signature — possible
    /// in principle for phantom binders, but not reachable today) get
    /// their own fresh substitution so downstream handling stays uniform.
    ///
    /// Callers push the returned constraints into `pending_where_constraints`
    /// with the current call-site span; the finalize pass then checks each
    /// obligation against concrete types and caller-active constraints,
    /// same as fn-call sites registered at the Call arm of `infer_expr`.
    pub(super) fn instantiate_method_entry(
        &mut self,
        entry: &MethodEntry,
    ) -> (Type, Vec<(TyVar, Symbol)>) {
        let ty = self.apply(&entry.method_type);
        let mut fvs: Vec<TyVar> = free_vars_in(&ty);
        for (tv, _) in &entry.method_constraints {
            if !fvs.contains(tv) {
                fvs.push(*tv);
            }
        }
        if fvs.is_empty() {
            return (ty, entry.method_constraints.clone());
        }
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for v in fvs {
            mapping.insert(v, self.fresh_var());
        }
        let new_ty = substitute_vars(&ty, &mapping);
        let new_constraints: Vec<(TyVar, Symbol)> = entry
            .method_constraints
            .iter()
            .map(|(tv, trait_name)| match mapping.get(tv) {
                Some(Type::Var(new_tv)) => (*new_tv, *trait_name),
                _ => (*tv, *trait_name),
            })
            .collect();
        (new_ty, new_constraints)
    }

    /// Instantiate a scheme and remap its where clause constraints.
    /// Returns (instantiated_type, remapped_constraints).
    pub(super) fn instantiate_with_constraints(
        &mut self,
        scheme: &Scheme,
    ) -> (Type, Vec<(TyVar, Symbol)>) {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        let ty = substitute_vars(&scheme.ty, &mapping);
        let constraints: Vec<(TyVar, Symbol)> = scheme
            .constraints
            .iter()
            .map(|(v, trait_name)| match mapping.get(v) {
                Some(Type::Var(new_v)) => (*new_v, *trait_name),
                _ => (*v, *trait_name),
            })
            .collect();
        // Audit round 19: record constraints on the fresh tyvars so that
        // `generalize` can propagate them into any scheme built from a
        // type that contains these variables (e.g. `let f = constrained_fn`
        // or `let f = { x -> constrained_fn(x) }`).
        for &(tv, trait_name) in &constraints {
            self.tyvar_trait_constraints
                .entry(tv)
                .or_default()
                .push(trait_name);
        }
        // Round 58 soundness fix: propagate trait arg bindings across
        // instantiation so that call sites of `fn f() where a: TryInto(Int)`
        // still see `[Int]` on the fresh tyvar when verifying against
        // impl_trait_args. Without this remap, instantiate would erase the
        // args and verify_trait_obligation would fall back to the bare
        // "implements trait" check, letting mismatched parameterized impls
        // silently satisfy the obligation.
        for (&old_tv, new_ty) in &mapping {
            if let Type::Var(new_tv) = new_ty {
                for (&(tv, trait_name), args) in self.trait_arg_bindings.clone().iter() {
                    if tv == old_tv {
                        self.trait_arg_bindings
                            .insert((*new_tv, trait_name), args.clone());
                    }
                }
            }
        }
        (ty, constraints)
    }

    // ── Type name for trait impl matching ────────────────────────────

    /// Convert a resolved Type to a type name string suitable for matching
    /// against `TraitImplInfo.target_type`. Returns `None` if the type is
    /// unresolved (still a type variable) or cannot be mapped to a name.
    pub(super) fn type_name_for_impl(&self, ty: &Type) -> Option<Symbol> {
        match ty {
            Type::Int => Some(intern("Int")),
            Type::Float => Some(intern("Float")),
            Type::Bool => Some(intern("Bool")),
            Type::String => Some(intern("String")),
            Type::Unit => Some(intern("()")),
            Type::Record(name, _) => Some(*name),
            Type::Generic(name, _) => Some(*name),
            Type::List(_) => Some(intern("List")),
            // Range acts as List for trait-impl lookup: traits registered
            // against List (e.g. Iterable, Sized) apply to Range since the
            // runtime representation is identical. Without this mapping,
            // auto-derived trait methods on Range receivers (e.g. `.len()`)
            // would fail to resolve.
            Type::Range(_) => Some(intern("List")),
            Type::Map(_, _) => Some(intern("Map")),
            Type::Set(_) => Some(intern("Set")),
            Type::Channel(_) => Some(intern("Channel")),
            Type::Tuple(_) => Some(intern("Tuple")),
            Type::ExtFloat => Some(intern("ExtFloat")),
            // GAP-1: function values must resolve to a type name so that
            // `where a: Trait` constraints are actually checked. No traits
            // are registered for "Fun", so the lookup always fails and the
            // user gets a real error instead of the constraint being
            // silently dropped.
            Type::Fun(_, _) => Some(intern("Fun")),
            Type::Var(_) => None, // unresolved
            _ => None,
        }
    }

    /// Return the positional type arguments of a (concrete) type. Mirrors
    /// the inverse of `register_trait_impl`'s self_type construction:
    /// `Type::Generic(_, args)` yields `args`; the parameterized builtin
    /// containers (List, Set, Channel, Map) yield their element types in
    /// declaration order. Anything else (Int, String, Record without type
    /// params, etc.) has no positional args. Used by `verify_trait_obligation`
    /// to walk into an impl's where-clause obligations.
    pub(super) fn type_args_of(ty: &Type) -> Vec<Type> {
        match ty {
            Type::Generic(_, args) => args.clone(),
            Type::List(inner)
            | Type::Range(inner)
            | Type::Set(inner)
            | Type::Channel(inner) => {
                vec![(**inner).clone()]
            }
            Type::Map(k, v) => vec![(**k).clone(), (**v).clone()],
            _ => Vec::new(),
        }
    }

    /// Recursively verify that `ty` implements `trait_name`, walking the
    /// matched impl's own where clauses against `ty`'s positional type
    /// arguments. Emits `"type 'X' does not implement trait 'Y'"` once for
    /// each unsatisfied obligation in the chain.
    ///
    /// This is the fix for the nested-where-clause propagation bug:
    /// `Box(Box(String)): Greet` with `trait Greet for Box(a) where a: Greet`
    /// previously typechecked because `(Greet, Box)` was in `trait_impl_set`.
    /// Now we additionally consult `impl_constraints` and recurse into the
    /// impl's `(target_arg_index, required_trait)` obligations against the
    /// matched `ty`'s type arguments.
    ///
    /// Recursion terminates because each step strips one layer of type
    /// wrapping; finite types finish in O(depth).
    pub(super) fn verify_trait_obligation(
        &mut self,
        trait_name: Symbol,
        bound_trait_args: &[Type],
        ty: &Type,
        span: Span,
    ) {
        let resolved = self.apply(ty);
        if matches!(resolved, Type::Error | Type::Never) {
            return;
        }
        let Some(type_name) = self.type_name_for_impl(&resolved) else {
            // Unresolved tyvar — caller is responsible for deferring or
            // reporting (e.g. via active_constraints or pending_where).
            return;
        };
        if !self.trait_impl_set.contains(&(trait_name, type_name)) {
            self.error(
                format!(
                    "type '{}' does not implement trait '{}'",
                    type_name, trait_name
                ),
                span,
            );
            return;
        }
        // Parameterized-trait verification: if the bound carries trait
        // args (e.g. `where a: TryInto(Int)`) and the matched impl also
        // registered its own args (`trait TryInto(Float) for String`),
        // the two arg lists must be positionally compatible. Concrete
        // mismatches reject — this is the soundness hole closed in
        // round 58. Bare `verify_trait_obligation(trait, &[], ty, ..)`
        // (supertrait chains, old call sites) keeps the fast path.
        if !bound_trait_args.is_empty()
            && let Some(impl_args) = self.impl_trait_args.get(&(trait_name, type_name)).cloned()
            && impl_args.len() == bound_trait_args.len()
        {
            for (bound_arg, impl_arg) in bound_trait_args.iter().zip(impl_args.iter()) {
                let b = self.apply(bound_arg);
                let i = self.apply(impl_arg);
                if !Self::trait_arg_compatible(&b, &i) {
                    self.error(
                        format!(
                            "type '{}' does not implement trait '{}({})': \
                             the matched impl is '{}({})'",
                            type_name,
                            resolve(trait_name),
                            bound_trait_args
                                .iter()
                                .map(|t| format!("{t}"))
                                .collect::<Vec<_>>()
                                .join(", "),
                            resolve(trait_name),
                            impl_args
                                .iter()
                                .map(|t| format!("{t}"))
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                        span,
                    );
                    return;
                }
            }
        }
        // Walk the matched impl's own where clauses against the actual
        // type arguments. Clone the obligation list so the recursive
        // `self.error` call doesn't conflict with the borrow.
        let Some(obligations) = self.impl_constraints.get(&(trait_name, type_name)).cloned() else {
            return;
        };
        let args = Self::type_args_of(&resolved);
        for (idx, sub_trait) in obligations {
            if let Some(arg_ty) = args.get(idx).cloned() {
                // Sub-obligations on impl target args: no trait args to
                // thread (impl-level where clauses are `where a: Trait`,
                // no parameterized form yet).
                self.verify_trait_obligation(sub_trait, &[], &arg_ty, span);
            }
        }
    }

    /// Side-effect-free compatibility check between a bound's trait-arg
    /// and an impl's trait-arg. Returns true when the pair could unify:
    /// either side is a type variable (defer), or both are concrete and
    /// structurally equal. Used by `verify_trait_obligation` to reject
    /// `where a: TryInto(Int)` when only `TryInto(Float) for ...` exists.
    fn trait_arg_compatible(a: &Type, b: &Type) -> bool {
        match (a, b) {
            (Type::Error, _) | (_, Type::Error) => true,
            (Type::Var(_), _) | (_, Type::Var(_)) => true,
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::ExtFloat, Type::ExtFloat)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => true,
            (Type::List(x), Type::List(y))
            | (Type::Range(x), Type::Range(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => Self::trait_arg_compatible(x, y),
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                Self::trait_arg_compatible(k1, k2) && Self::trait_arg_compatible(v1, v2)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs.iter().zip(ys.iter()).all(|(x, y)| Self::trait_arg_compatible(x, y))
            }
            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2.iter()).all(|(x, y)| Self::trait_arg_compatible(x, y))
                    && Self::trait_arg_compatible(r1, r2)
            }
            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1.iter().zip(a2.iter()).all(|(x, y)| Self::trait_arg_compatible(x, y))
            }
            (Type::Record(n1, _), Type::Record(n2, _)) => n1 == n2,
            (Type::Record(n1, _), Type::Generic(n2, _))
            | (Type::Generic(n1, _), Type::Record(n2, _)) => n1 == n2,
            _ => false,
        }
    }

    // ── Error reporting ─────────────────────────────────────────────

    /// If `got` is a `Result(_, _)` or `Option(_)` but `expected` is
    /// not, append a `help:` continuation explaining how to thread
    /// the monadic value through. The raw "expected String, got
    /// Result(String, _)" message is correct but doesn't tell users
    /// how to fix it; the hint points at `?` and `result.flat_map` /
    /// `option.flat_map`.
    fn chain_hint(got: &Type, expected: &Type) -> Option<std::string::String> {
        let is_wrapper = |t: &Type, name: &str| -> bool {
            matches!(t, Type::Generic(n, _) if resolve(*n) == name)
        };
        if is_wrapper(expected, "Result") || is_wrapper(expected, "Option") {
            return None;
        }
        if is_wrapper(got, "Result") {
            return Some(
                "help: to chain through a `Result`, use `?` to propagate the \
                 error, or `|> result.flat_map { x -> ... }` to continue the \
                 pipeline on the Ok value"
                    .to_string(),
            );
        }
        if is_wrapper(got, "Option") {
            return Some(
                "help: to chain through an `Option`, use `?` to propagate \
                 `None`, or `|> option.flat_map { x -> ... }` to continue the \
                 pipeline on the Some value"
                    .to_string(),
            );
        }
        None
    }

    pub(super) fn error(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            severity: Severity::Error,
        });
    }

    pub(super) fn warning(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            severity: Severity::Warning,
        });
    }

    // ── Check a full program ────────────────────────────────────────

    pub fn check_program(&mut self, program: &mut Program) {
        let mut env = TypeEnv::new();

        // Register builtins in the type environment
        self.register_builtins(&mut env);

        // Register built-in traits and auto-derived impls (shared with
        // `ReplTypeContext::new` so `silt check` and the REPL stay in
        // sync on derive policy).
        register_builtin_trait_impls(self);

        // Round 56 item 4: reset the import set so a fresh check_program
        // call doesn't inherit modules imported by a previous run.
        self.imported_modules.clear();

        // Process imports: register selective/aliased import names in the type environment
        for decl in &program.decls {
            if let Decl::Import(ImportTarget::Items(module, items), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    self.imported_modules.insert(*module);
                    for item in items {
                        let qualified = intern(&format!("{module}.{item}"));
                        if let Some(scheme) = env.lookup(qualified).cloned() {
                            env.define(*item, scheme);
                        }
                        // Gated constructors (like Monday, GET) are already
                        // registered under their bare name — no alias needed.
                    }
                } else {
                    self.warning(
                        format!(
                            "unknown module '{module_str}'; imported items will not be type-checked"
                        ),
                        *span,
                    );
                }
            } else if let Decl::Import(ImportTarget::Alias(module, alias), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    // Track the alias (not the original module name) — the
                    // user chose to rename the module, so the original
                    // symbol is no longer in scope under its bare name.
                    self.imported_modules.insert(*alias);
                    // Mirror every qualified `{module}.{suffix}` binding
                    // under the alias. Before round 58, this loop iterated
                    // `builtin_module_functions(module_str)` and copied
                    // only names in that curated list — which excluded
                    // schemes registered directly in the typechecker's
                    // submodules (e.g. `list.sum`, `list.product`), so
                    // `l.sum` under `import list as l` failed with
                    // "undefined variable 'l'" while `list.sum` worked.
                    // Iterating the env by prefix captures every
                    // qualified entry regardless of which registrar
                    // defined it.
                    let alias_str = resolve(*alias);
                    let prefix = format!("{module_str}.");
                    let to_alias: Vec<(Symbol, Scheme)> = env
                        .bindings
                        .iter()
                        .filter_map(|(k, scheme)| {
                            let k_str = resolve(*k);
                            k_str
                                .strip_prefix(&prefix)
                                .map(|suffix| (intern(&format!("{alias_str}.{suffix}")), scheme.clone()))
                        })
                        .collect();
                    for (aliased, scheme) in to_alias {
                        env.define(aliased, scheme);
                    }
                } else {
                    self.warning(
                        format!("unknown module '{module_str}'; aliased imports will not be type-checked"),
                        *span,
                    );
                }
            } else if let Decl::Import(ImportTarget::Module(module), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    self.imported_modules.insert(*module);
                    // Built-in module names are already bound via register_builtins
                    // under their `module.func` qualified form — no additional
                    // action required here.
                } else {
                    // Non-builtin (user) module: the compiler handles these at
                    // link time. Emit the same "unknown module" warning we use
                    // for Items/Alias so the CLI's diagnostic-suppression
                    // heuristic in main.rs fires, and add a minimal binding for
                    // the module name itself so downstream `module.foo(...)`
                    // calls don't cascade into "undefined variable" errors.
                    self.warning(
                        format!(
                            "unknown module '{module_str}'; imported module will not be type-checked"
                        ),
                        *span,
                    );
                    // Bind the module name to a fresh variable so member access
                    // on it degrades gracefully rather than failing lookup.
                    let placeholder = self.fresh_var();
                    env.define(*module, Scheme::mono(placeholder));
                }
            }
        }

        // First pass: pre-register every type name with a placeholder
        // body. This makes recursive type references (e.g.
        // `type Expr { Add(Expr, Expr), ... }`) resolve during variant /
        // field type resolution — without this, the B3 unknown-type
        // check introduced in round 60 would reject the self-reference
        // because `self.enums` / `self.records` don't contain the name
        // until after the body is processed. The real registration loop
        // below overwrites the placeholders.
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                let td_name_str = resolve(td.name);
                if td_name_str == "TypeOf" {
                    continue;
                }
                match &td.body {
                    TypeBody::Enum(_) => {
                        self.enums.entry(td.name).or_insert_with(|| EnumInfo {
                            variants: Vec::new(),
                            params: td.params.clone(),
                            param_var_ids: Vec::new(),
                        });
                    }
                    TypeBody::Record(_) => {
                        self.records.entry(td.name).or_insert_with(|| RecordInfo {
                            fields: Vec::new(),
                        });
                    }
                }
            }
        }
        // First pass: register all type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.register_type_decl(td, &mut env);
            }
        }

        // Second pass: register trait declarations FIRST (so default
        // method bodies are recorded in TraitInfo) before synthesizing
        // missing defaults into trait impls. We split the original
        // single-pass loop into three sub-passes so the synthesis step
        // can mutate `program.decls` after every TraitInfo is known but
        // before any TraitImpl is registered into method_table.
        for decl in &program.decls {
            if let Decl::Trait(t) = decl {
                self.register_trait_decl(t);
            }
        }

        // 2b: Synthesize default-method bodies into impls that omitted
        // them. Mutates `program.decls`. After this pass, any impl that
        // "uses the default" looks identical (in the AST) to one that
        // re-typed the default body inline — so signature registration,
        // body checking, dispatch, and code generation all flow through
        // the existing machinery unmodified.
        self.synthesize_default_methods(&mut program.decls);

        // 2c: Register fn signatures and trait impls (now seeing
        // synthesized methods alongside explicit ones).
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.register_fn_decl(f, &mut env);
                }
                Decl::TraitImpl(ti) => {
                    self.register_trait_impl(ti, &mut env);
                }
                _ => {}
            }
        }

        // Process top-level let bindings (after functions are registered so
        // the value expression can call functions, and before function body
        // checking so functions can reference the constants).
        for i in 0..program.decls.len() {
            if let Decl::Let {
                ref mut value,
                ref pattern,
                ref ty,
                span,
                ..
            } = program.decls[i]
            {
                let is_value = inference::is_syntactic_value(&value.kind);
                let val_ty = self.infer_expr(value, &mut env);
                if let Some(te) = ty {
                    // B2: populate the arity-error span hint with the
                    // annotation's own span so diagnostics from
                    // `resolve_type_expr` point at the user-written type,
                    // not a zero-span sentinel. Without this, errors in
                    // `let x: Box(Int) = ...` where `Box` is parameterized
                    // emitted a span-less first error followed by a
                    // duplicate from the subsequent unify.
                    let prev_type_span = self.current_type_anno_span.replace(te.span);
                    let declared =
                        self.resolve_type_expr(te, &mut std::collections::HashMap::new());
                    self.current_type_anno_span = prev_type_span;
                    self.unify(&val_ty, &declared, span);
                }
                let scheme = if is_value {
                    self.generalize(&env, &val_ty)
                } else {
                    Scheme::mono(self.apply(&val_ty))
                };
                if let PatternKind::Ident(name) = &pattern.kind {
                    // G1: top-level duplicate let binding.
                    if self.top_level_names.contains(name) {
                        self.error(
                            format!(
                                "duplicate top-level definition of '{}'; names must be unique at module scope",
                                name
                            ),
                            span,
                        );
                    }
                    self.top_level_names.insert(*name);
                    env.define(*name, scheme);
                } else {
                    // B1: reject refutable Constructor patterns in
                    // top-level `let` before binding.
                    self.reject_refutable_constructor_in_let(pattern, span);
                    self.bind_pattern(pattern, &val_ty, &mut env, span);
                }
            }
        }

        // Validate trait implementations against their declarations
        self.validate_trait_impls();

        // Third pass: type check function bodies to discover constraints.
        // Recovery stubs (Option B) are skipped: their synthetic empty
        // body is not user code and must not produce "return type
        // mismatch", "unused binding", "unreachable", etc.
        let pre_pass3_error_count = self.errors.len();
        let pre_pass3_field_count = self.pending_field_accesses.len();
        let pre_pass3_numeric_count = self.pending_numeric_checks.len();
        for i in 0..program.decls.len() {
            if let Decl::Fn(ref mut f) = program.decls[i]
                && !f.is_recovery_stub
            {
                self.check_fn_body(f, &env);
            }
        }
        for i in 0..program.decls.len() {
            if let Decl::TraitImpl(ref mut ti) = program.decls[i] {
                let target = ti.target_type;
                for j in 0..ti.methods.len() {
                    let method_name = ti.methods[j].name;
                    let key = intern(&format!("{target}.{method_name}"));
                    let constrained = self.check_fn_body_with_name(&mut ti.methods[j], &env, key);
                    // Write the body-inferred type back into method_table so
                    // that downstream call sites see the concrete return
                    // type instead of the still-polymorphic template.
                    //
                    // Remap any impl/method-level where-clause constraints
                    // from the pre-body-check tyvar space to the new
                    // body-inferred tyvar space via align_tyvars — same
                    // structural walk used for scheme narrowing at round 17
                    // F1. Without this, dispatch_method_entry sees two
                    // disjoint fresh-var groups (one from free_vars on
                    // the body-inferred method_type, one from stale
                    // constraint tyvars) and produces inconsistent
                    // substitutions at call sites.
                    if let Some(ty) = constrained
                        && let Some(entry) = self.method_table.get_mut(&(target, method_name))
                    {
                        if !entry.method_constraints.is_empty() {
                            let remap = align_tyvars(&entry.method_type, &ty);
                            entry.method_constraints = entry
                                .method_constraints
                                .iter()
                                .filter_map(|(old_tv, trait_name)| {
                                    remap.get(old_tv).map(|&new_tv| (new_tv, *trait_name))
                                })
                                .collect();
                        }
                        entry.method_type = ty;
                    }
                }
            }
        }

        // Narrow function schemes based on body constraints, then re-check.
        //
        // Invariant (audit-round-36 LATENT doc): when `finalize_deferred_checks`
        // runs below, `pending_field_accesses` / `pending_numeric_checks` /
        // `pending_where_constraints` must contain EXACTLY the pushes from the
        // most recent body-check pass — not a mix of pass-2 + pass-3 entries.
        // Two paths preserve that:
        //   (1) `any_narrowed == false`: no re-check happens, so pass 3's
        //       pushes ARE the "most recent" pool and finalize consumes them
        //       as-is.
        //   (2) `any_narrowed == true`: the truncate/clear inside the branch
        //       rolls the pools back to their pre-pass-3 baseline before the
        //       re-check repopulates them, so finalize again sees only the
        //       most-recent pass's entries.
        // If a future edit adds a THIRD re-check path it MUST either set
        // `any_narrowed = true` (to go through the truncate branch) or add its
        // own equivalent reset/repopulate pairing, or this invariant breaks
        // and duplicate obligations leak into finalize.
        let body_types: HashMap<Symbol, Type> = std::mem::take(&mut self.fn_body_types);
        if !body_types.is_empty() {
            let mut any_narrowed = false;
            for (name, constrained_type) in &body_types {
                let new_scheme = self.generalize(&env, constrained_type);
                // Preserve where-clause constraints from the original scheme
                if let Some(original_scheme) = env.lookup(*name).cloned()
                    && original_scheme.vars.len() != new_scheme.vars.len()
                {
                    // Scheme was narrowed — some vars got constrained
                    any_narrowed = true;
                    let mut final_scheme = new_scheme.clone();
                    // BROKEN (round 17 F1): `original_scheme.constraints` uses
                    // the pass-2 tyvars, while `new_scheme.vars` uses fresh
                    // pass-3 tyvars from `instantiate_with_constraints` that
                    // flowed through body inference into `fn_body_types`.
                    // A direct `new_scheme.vars.contains(old_tv)` check never
                    // matches, so constraints were silently dropped and calls
                    // like `use_doublable("text")` slipped through typecheck
                    // and crashed at runtime with "no method doubled for
                    // String". Walk the two `Type` trees structurally in
                    // lockstep to build an old→new tyvar remap, then rewrite
                    // the original constraints through it. Narrowing can only
                    // tighten the scheme (never introduce new vars), so any
                    // original constraint whose old var is still free in the
                    // new scheme maps to a concrete new var.
                    let remap = align_tyvars(&original_scheme.ty, &new_scheme.ty);
                    for (old_tv, trait_name) in &original_scheme.constraints {
                        if let Some(&new_tv) = remap.get(old_tv)
                            && new_scheme.vars.contains(&new_tv)
                            && !final_scheme.constraints.contains(&(new_tv, *trait_name))
                        {
                            final_scheme.constraints.push((new_tv, *trait_name));
                        }
                    }
                    env.define(*name, final_scheme);
                }
            }

            if any_narrowed {
                // Discard pass 3 errors — they'll be re-emitted with better accuracy
                self.errors.truncate(pre_pass3_error_count);
                self.fn_body_types.clear();
                // Also truncate deferred checks back to the pre-pass-3
                // baseline (preserving any obligations recorded by the
                // top-level let inference earlier). They'll be re-collected
                // during the re-check with narrowed schemes.
                self.pending_field_accesses.truncate(pre_pass3_field_count);
                self.pending_numeric_checks
                    .truncate(pre_pass3_numeric_count);
                // B4: discard the pending where-clause obligations so
                // the re-check with narrowed schemes re-collects them
                // from scratch. Otherwise stale entries pollute the
                // finalize pass with obligations that belong to
                // pre-narrowed instantiations.
                self.pending_where_constraints.clear();

                // Re-check function bodies with narrowed schemes.
                // Recovery stubs still skipped (same reason as pass 3).
                for i in 0..program.decls.len() {
                    if let Decl::Fn(ref mut f) = program.decls[i]
                        && !f.is_recovery_stub
                    {
                        self.check_fn_body(f, &env);
                    }
                }
                for i in 0..program.decls.len() {
                    if let Decl::TraitImpl(ref mut ti) = program.decls[i] {
                        let target = ti.target_type;
                        for j in 0..ti.methods.len() {
                            let method_name = ti.methods[j].name;
                            let key = intern(&format!("{target}.{method_name}"));
                            let constrained =
                                self.check_fn_body_with_name(&mut ti.methods[j], &env, key);
                            if let Some(ty) = constrained
                                && let Some(entry) =
                                    self.method_table.get_mut(&(target, method_name))
                            {
                                if !entry.method_constraints.is_empty() {
                                    let remap = align_tyvars(&entry.method_type, &ty);
                                    entry.method_constraints = entry
                                        .method_constraints
                                        .iter()
                                        .filter_map(|(old_tv, trait_name)| {
                                            remap.get(old_tv).map(|&new_tv| (new_tv, *trait_name))
                                        })
                                        .collect();
                                }
                                entry.method_type = ty;
                            }
                        }
                    }
                }
            }
        }

        // Resolve any deferred checks (field-access / numeric ops on type
        // variables) before generating "unresolved type" errors.
        self.finalize_deferred_checks();

        // Fourth pass: detect unresolved type variables on let-binding values
        // where the user did not provide a type annotation.
        self.check_unresolved_let_types(program);

        // After all passes, resolve any remaining type variables in annotations
        self.resolve_all_types(program);
    }

    // ── Validate trait implementations ────────────────────────────────

    fn validate_trait_impls(&mut self) {
        // (a) Validate supertrait names: every supertrait listed on every
        // declared trait must itself be a declared trait. Done up-front so
        // unknown-supertrait errors are surfaced even when the declaring
        // trait has no impls. We snapshot the names+supertraits up-front
        // because `self.error` borrows `self` mutably.
        let trait_supertrait_pairs: Vec<(Symbol, Vec<Symbol>, Span)> = self
            .traits
            .iter()
            .map(|(name, info)| (*name, info.supertraits.clone(), info.decl_span))
            .collect();
        for (trait_name, supertraits, decl_span) in &trait_supertrait_pairs {
            for sup in supertraits {
                if !self.traits.contains_key(sup) {
                    self.error(
                        format!("trait '{trait_name}' lists unknown supertrait '{sup}'"),
                        *decl_span,
                    );
                }
            }
        }

        // Validate using method_table + trait_impl_set (the new system).
        let impl_pairs: Vec<(Symbol, Symbol)> = self.trait_impl_set.iter().cloned().collect();
        for (trait_name, type_name) in &impl_pairs {
            // GAP-2: Prefer the impl block's real span (stored at
            // registration time) over a method span or the sentinel
            // `Span::new(0, 0)`. Fall back to the method table only for
            // auto-derived impls that have no user-visible source site.
            let diag_span = self
                .trait_impl_spans
                .get(&(*trait_name, *type_name))
                .copied()
                .or_else(|| {
                    self.method_table
                        .iter()
                        .find(|((t, _), _)| t == type_name)
                        .map(|(_, e)| e.span)
                })
                .unwrap_or_else(|| Span::new(0, 0));

            // Check that the trait exists first.
            let Some(trait_info) = self.traits.get(trait_name).cloned() else {
                self.error(format!("trait '{trait_name}' is not declared"), diag_span);
                continue;
            };

            // Skip auto-derived impls (builtin traits on all types).
            let is_auto = trait_info
                .methods
                .first()
                .and_then(|(m, _)| self.method_table.get(&(*type_name, *m)))
                .map(|e| e.is_auto_derived)
                .unwrap_or(false);
            if is_auto {
                continue;
            }

            // (b) Supertrait obligation: implementing a trait on a type
            // requires every supertrait to also be implemented for the
            // same type. Auto-derived builtins (Display/Equal/Hash/Compare)
            // do show up in `trait_impl_set`, so this also catches the
            // common case `trait Ordered: Equal { ... }` followed by
            // `trait Ordered for MyType { ... }` where MyType has not
            // overridden Equal — auto-derived counts as implementing.
            //
            // B1 (round 60): when the supertrait reference carries args
            // (`trait Holds(b): Carry(b)` + `impl Holds(Int) for Bag`),
            // resolve those args through the enclosing trait's
            // params→impl-args mapping and require the matching impl to
            // exist with positionally-compatible args. Otherwise
            // `impl Carry(String) for Bag` would silently satisfy the
            // obligation for `impl Holds(Int) for Bag`, causing a
            // runtime method-resolution failure.
            let enclosing_args: Vec<Type> = self
                .impl_trait_args
                .get(&(*trait_name, *type_name))
                .cloned()
                .unwrap_or_default();
            for (i, supertrait) in trait_info.supertraits.iter().enumerate() {
                if !self.trait_impl_set.contains(&(*supertrait, *type_name)) {
                    self.error(
                        format!(
                            "type '{type_name}' implements '{trait_name}' but does not implement supertrait '{supertrait}'"
                        ),
                        diag_span,
                    );
                    continue;
                }
                // Resolve the supertrait's expected args against the
                // enclosing trait's impl args. Skip when there are no
                // supertrait args declared (bare `: Super` form).
                let arg_exprs = trait_info.supertrait_args.get(i);
                let expected_super_args: Vec<Type> = match arg_exprs {
                    Some(exprs) if !exprs.is_empty() => exprs
                        .iter()
                        .map(|te| {
                            crate::typechecker::inference::resolve_supertrait_arg(
                                te,
                                &trait_info,
                                &enclosing_args,
                            )
                        })
                        .collect(),
                    _ => continue,
                };
                let actual_super_args = self
                    .impl_trait_args
                    .get(&(*supertrait, *type_name))
                    .cloned()
                    .unwrap_or_default();
                let len_ok = actual_super_args.len() == expected_super_args.len();
                let pos_ok = len_ok
                    && expected_super_args
                        .iter()
                        .zip(actual_super_args.iter())
                        .all(|(e, a)| {
                            let e = self.apply(e);
                            let a = self.apply(a);
                            Self::trait_arg_compatible(&e, &a)
                        });
                if !pos_ok {
                    let fmt_args = |args: &[Type]| -> String {
                        args.iter()
                            .map(|t| format!("{t}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    self.error(
                        format!(
                            "impl {}({}) for {} requires impl {}({}) for {}, but found impl {}({}) for {}",
                            resolve(*trait_name),
                            fmt_args(&enclosing_args),
                            type_name,
                            resolve(*supertrait),
                            fmt_args(&expected_super_args),
                            type_name,
                            resolve(*supertrait),
                            fmt_args(&actual_super_args),
                            type_name,
                        ),
                        diag_span,
                    );
                }
            }

            // Check that all required methods are implemented with correct signature.
            for (method_name, trait_method_type) in &trait_info.methods {
                let key = (*type_name, *method_name);
                if let Some(entry) = self.method_table.get(&key) {
                    let stored_impl_type = entry.method_type.clone();
                    let impl_span = entry.span;
                    // Instantiate BOTH the impl's stored template and the
                    // trait's declared type with fresh variables so that
                    // unification doesn't permanently bind either — the
                    // stored method_table entries are templates reused by
                    // every lookup site via `instantiate_method_type`.
                    let impl_type = self.instantiate_method_type(&stored_impl_type);
                    let fvs = free_vars_in(trait_method_type);
                    let mapping: HashMap<TyVar, Type> =
                        fvs.into_iter().map(|v| (v, self.fresh_var())).collect();
                    let expected = substitute_vars(trait_method_type, &mapping);
                    self.unify(&impl_type, &expected, impl_span);
                } else if !trait_info.default_method_bodies.contains_key(method_name) {
                    // No impl method AND the trait does not provide a
                    // default body — the impl is genuinely missing a
                    // required method. Methods with default bodies are
                    // synthesized into the impl by
                    // `synthesize_default_methods` before this validator
                    // runs the second time, so a missing-with-default
                    // entry here means synthesis hasn't happened yet
                    // (which is the normal pre-synthesis path) — silent
                    // is correct.
                    self.error(
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            trait_name, type_name, method_name
                        ),
                        diag_span,
                    );
                }
            }
        }
    }

    /// Helper: create a fresh type variable and return both the Type::Var and
    /// its TyVar id.
    pub(super) fn fresh_tv(&mut self) -> (Type, TyVar) {
        let t = self.fresh_var();
        let v = match &t {
            Type::Var(v) => *v,
            _ => unreachable!(),
        };
        (t, v)
    }

    // ── Register type declarations ──────────────────────────────────

    fn register_type_decl(&mut self, td: &TypeDecl, env: &mut TypeEnv) {
        // BROKEN #1: Reject redefinition of reserved type-system sentinel
        // names. `TypeOf` is used internally as the head of
        // `Type::Generic(intern("TypeOf"), [..])` to represent a type
        // descriptor (e.g. the runtime value produced by `Employee` when
        // used as a first-class type argument to `json.parse`). A user
        // declaring `type TypeOf(a) { Foo(a) }` would bind `Foo` as a
        // constructor returning a value structurally indistinguishable
        // from that internal descriptor, which silently typechecks and
        // then fails at runtime with "type argument must be a record
        // type". Reject at declaration time for a clear diagnostic. See
        // the sibling guard for builtin trait names in
        // `register_trait_decl` below.
        let td_name_str = resolve(td.name);
        if td_name_str == "TypeOf" {
            self.error(
                format!(
                    "'{td_name_str}' is a reserved type name used by the type system"
                ),
                td.span,
            );
            return;
        }
        // G1: Detect duplicate top-level type declarations. Only user-defined
        // top-level names count; collision with a builtin type (Option,
        // Result, ChannelResult, Step) is handled by the shadow-warning
        // path elsewhere.
        if self.top_level_names.contains(&td.name) {
            self.error(
                format!(
                    "duplicate top-level type declaration '{}'; type names must be unique at module scope",
                    td.name
                ),
                td.span,
            );
        }
        self.top_level_names.insert(td.name);
        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on field / variant type annotations.
        let prev_type_span = self.current_type_anno_span.replace(td.span);
        // Create a mapping from type param names to placeholder type vars
        let mut param_vars: HashMap<Symbol, Type> = HashMap::new();
        for p in &td.params {
            let tv = self.fresh_var();
            param_vars.insert(*p, tv);
        }

        match &td.body {
            TypeBody::Enum(variants) => {
                let mut variant_infos = Vec::new();

                // Compute the TyVar ids for each type parameter once,
                // before the variant loop (they are the same for every variant).
                let var_ids: Vec<TyVar> = td
                    .params
                    .iter()
                    .map(|p| match &param_vars[p] {
                        Type::Var(v) => *v,
                        _ => unreachable!(),
                    })
                    .collect();

                // G3: detect duplicate variant names within the same enum.
                // Previously `type Color { Red, Green, Red }` compiled
                // silently — the second `Red` overwrote the first's
                // constructor binding and no diagnostic was emitted.
                let mut seen_variants: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                for variant in variants {
                    if !seen_variants.insert(variant.name) {
                        self.error(
                            format!("duplicate variant '{}' in enum '{}'", variant.name, td.name),
                            td.span,
                        );
                    }
                }

                for variant in variants {
                    let field_types: Vec<Type> = variant
                        .fields
                        .iter()
                        .map(|te| self.resolve_type_expr(te, &mut param_vars))
                        .collect();

                    variant_infos.push(VariantInfo {
                        name: variant.name,
                        field_types: field_types.clone(),
                    });

                    // Register the constructor in the type environment
                    let type_params: Vec<Type> =
                        td.params.iter().map(|p| param_vars[p].clone()).collect();

                    let result_type = if type_params.is_empty() {
                        Type::Generic(td.name, vec![])
                    } else {
                        Type::Generic(td.name, type_params)
                    };

                    if field_types.is_empty() {
                        // No-arg constructor is just a value
                        env.define(
                            variant.name,
                            Scheme {
                                vars: var_ids.clone(),
                                ty: result_type,
                                constraints: vec![],
                            },
                        );
                    } else {
                        // Constructor function
                        env.define(
                            variant.name,
                            Scheme {
                                vars: var_ids.clone(),
                                ty: Type::Fun(field_types, Box::new(result_type)),
                                constraints: vec![],
                            },
                        );
                    }

                    // Round-23 GAP #2: detect cross-enum variant name
                    // collisions. Previously `type A { Red }` followed by
                    // `type B { Red }` silently overwrote the owning-enum
                    // entry, so later `match x: A { Red -> ... }` resolved
                    // `Red` to `B` and produced misleading "expected B,
                    // got A" errors. The same hazard applies when a user
                    // `type Result { ... }` shadows the builtin Result,
                    // because builtins populate variant_to_enum first.
                    // Emit a warning (not a hard error: the language
                    // allows this and resolves by most-recent-wins, but
                    // the user should know the prior variant is now
                    // unreachable). Same-enum duplicates are caught
                    // above as a hard error (G3).
                    //
                    // The shadowing case breaks into two sub-cases:
                    //   a. prev_owner != td.name — two distinct enums,
                    //      whether user/user or user/builtin (e.g. user
                    //      `type X { Ok, Err }` vs builtin Result).
                    //   b. prev_owner == td.name — same Symbol but the
                    //      previously-registered enum entry is a builtin
                    //      we're about to overwrite (e.g. user
                    //      `type Result { ... }` replacing builtin Result).
                    //      We detect this by the presence of a prior
                    //      `self.enums[td.name]` entry at this point; the
                    //      insert for the *current* td happens below, so
                    //      any existing key must be a prior registration.
                    //      Collisions with another user decl are already
                    //      flagged as a hard error by top_level_names, so
                    //      anything we see here is a builtin shadow.
                    if let Some(prev_owner) = self.variant_to_enum.get(&variant.name).copied() {
                        if prev_owner != td.name {
                            self.warning(
                                format!(
                                    "variant '{}' of enum '{}' shadows same-named variant of enum '{}'; \
                                     earlier variant is no longer resolvable by bare name",
                                    resolve(variant.name),
                                    resolve(td.name),
                                    resolve(prev_owner)
                                ),
                                td.span,
                            );
                        } else if self.enums.contains_key(&td.name) {
                            // Sub-case (b): user type shadowing a builtin
                            // of the same name.
                            self.warning(
                                format!(
                                    "variant '{}' of enum '{}' shadows same-named variant of builtin enum '{}'; \
                                     builtin variant is no longer resolvable by bare name",
                                    resolve(variant.name),
                                    resolve(td.name),
                                    resolve(prev_owner)
                                ),
                                td.span,
                            );
                        }
                    }
                    self.variant_to_enum.insert(variant.name, td.name);
                }

                // Register the enum type name as a value so it can be
                // passed to `type a` parameters (`json.parse(body, Color)`,
                // user-defined decoders, etc.). Mirrors the record path.
                // Skipped when a variant shares the enum's name
                // (e.g. `type Box(T) { Box(T) }`) because the variant
                // constructor is already bound under the same symbol.
                let variant_shares_name = variant_infos.iter().any(|v| v.name == td.name);
                if !variant_shares_name {
                    let enum_ty = if td.params.is_empty() {
                        Type::Generic(td.name, vec![])
                    } else {
                        let args: Vec<Type> =
                            td.params.iter().map(|p| param_vars[p].clone()).collect();
                        Type::Generic(td.name, args)
                    };
                    let scheme = Scheme {
                        vars: var_ids.clone(),
                        ty: Type::Generic(intern("TypeOf"), vec![enum_ty]),
                        constraints: vec![],
                    };
                    env.define(td.name, scheme);
                }

                self.enums.insert(
                    td.name,
                    EnumInfo {
                        params: td.params.clone(),
                        param_var_ids: var_ids,
                        variants: variant_infos,
                    },
                );
            }
            TypeBody::Record(fields) => {
                // G2: detect duplicate field names in the same record.
                // Previously `type R { a: Int, a: String }` compiled
                // silently and the first field's type was overwritten
                // by the second at the VM record layout level.
                let mut seen_fields: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                for f in fields {
                    if !seen_fields.insert(f.name) {
                        self.error(
                            format!("duplicate field '{}' in record type '{}'", f.name, td.name),
                            td.span,
                        );
                    }
                }
                let field_types: Vec<(Symbol, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.resolve_type_expr(&f.ty, &mut param_vars);
                        (f.name, ty)
                    })
                    .collect();

                // Store param_var_ids for parameterized record types
                if !td.params.is_empty() {
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    self.record_param_var_ids.insert(td.name, var_ids);
                }

                self.records.insert(
                    td.name,
                    RecordInfo {
                        fields: field_types.clone(),
                    },
                );

                // Register the record type name as a value so it can be
                // passed to a `type a` parameter, e.g. `json.parse(body, Employee)`.
                // The value is a TYPE DESCRIPTOR at runtime (represented
                // as `Value::TypeDescriptor(name)`), so its type must
                // be `TypeOf(Employee)` rather than `Employee` itself —
                // otherwise the typechecker would let users write things
                // like `Employee.field` or use the descriptor as an
                // instance (T2 audit fix; mirrors primitive descriptors).
                //
                // For parameterized records (`type Box(a) { ... }`),
                // fresh type vars are generated for each param so
                // `json.parse(Box, ...)` can unify with a monomorphic
                // instance at the call site.
                let record_ty = Type::Record(td.name, field_types);
                let scheme = if td.params.is_empty() {
                    Scheme {
                        vars: vec![],
                        ty: Type::Generic(intern("TypeOf"), vec![record_ty]),
                        constraints: vec![],
                    }
                } else {
                    // Re-use the param TyVars that parameterize the
                    // record's fields so the descriptor type is
                    // `forall a. TypeOf(Box(a))` — generalizing makes
                    // each call instantiate its own fresh vars.
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    let args: Vec<Type> = td.params.iter().map(|p| param_vars[p].clone()).collect();
                    let generic_record = Type::Generic(td.name, args);
                    Scheme {
                        vars: var_ids,
                        ty: Type::Generic(intern("TypeOf"), vec![generic_record]),
                        constraints: vec![],
                    }
                };
                env.define(td.name, scheme);
            }
        }

        // Auto-derive builtin traits for user-defined types.
        // All enums and records get Equal, Compare, Hash, Display since
        // the runtime supports Eq/Ord/Hash on all Value variants.
        let dummy_span = Span {
            line: 0,
            col: 0,
            offset: 0,
        };
        for trait_name in &["Equal", "Compare", "Hash", "Display"] {
            self.trait_impl_set.insert((intern(trait_name), td.name));
        }
        // Register auto-derived method entries
        let builtin_methods: &[(&str, Type)] = &[
            (
                "display",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
            ),
            (
                "equal",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ),
            (
                "compare",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Int),
                ),
            ),
            (
                "hash",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
            ),
        ];
        for (method_name, method_type) in builtin_methods {
            self.method_table.insert(
                (td.name, intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                    trait_name: None,
                    method_constraints: Vec::new(),
                },
            );
        }
        self.current_type_anno_span = prev_type_span;
    }

    /// Resolve a TypeExpr AST node to our internal Type representation.
    pub(super) fn resolve_type_expr(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        match &te.kind {
            TypeExprKind::Named(name) => {
                // Check if it's a type param variable
                if let Some(tv) = param_vars.get(name) {
                    return tv.clone();
                }
                let name_str = resolve(*name);
                match name_str.as_str() {
                    "Int" => Type::Int,
                    "Float" => Type::Float,
                    "ExtFloat" => Type::ExtFloat,
                    "Bool" => Type::Bool,
                    "String" => Type::String,
                    "()" | "Unit" => Type::Unit,
                    "List" => {
                        // List without explicit type param => List(fresh_var)
                        Type::List(Box::new(self.fresh_var()))
                    }
                    "Range" => {
                        // Range without explicit type param => Range(fresh_var).
                        // Range is a nominal alias for List (see Type::Range
                        // in src/types.rs); inference is bidirectional at
                        // unify time.
                        Type::Range(Box::new(self.fresh_var()))
                    }
                    "Map" => {
                        // Map without explicit type params => Map(fresh_var, fresh_var)
                        Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                    }
                    "Set" => {
                        // Set without explicit type param => Set(fresh_var)
                        Type::Set(Box::new(self.fresh_var()))
                    }
                    "Channel" => {
                        // Channel without explicit type param => Channel(fresh_var)
                        Type::Channel(Box::new(self.fresh_var()))
                    }
                    _ => {
                        // Lowercase names in type annotations are type variables
                        // (e.g., `a` in `List(a)` or `fn foo(x: a) -> a`)
                        let first_char = name_str.chars().next().unwrap_or('A');
                        if first_char.is_lowercase() {
                            let tv = self.fresh_var();
                            param_vars.insert(*name, tv.clone());
                            tv
                        } else {
                            // Uppercase: a record or enum type. If the type
                            // is parameterized and the user wrote it bare
                            // (no type args), instantiate a fresh type
                            // variable for each parameter so distinct uses
                            // don't cross-pollute through the shared
                            // template TyVars (T1 audit fix). This mirrors
                            // the List/Map/Set/Channel special-case paths
                            // above and the fresh-var pattern in
                            // check_pattern for Pattern::Record.
                            //
                            // B3 (round 60): reject uppercase names that
                            // refer to nothing (no record / no enum). The
                            // pre-fix path silently returned
                            // `Type::Generic(name, vec![])` which then
                            // cascaded into "does not implement Display"
                            // and "type mismatch" diagnostics far from the
                            // annotation site. The whitelist mirrors the
                            // one used by the trait-impl-target check at
                            // `register_trait_impl` (round 23 GAP #1).
                            let is_user_record = self.records.contains_key(name);
                            let is_user_enum = self.enums.contains_key(name);
                            if !is_user_record && !is_user_enum {
                                self.error(
                                    format!("unknown type '{name_str}'"),
                                    te.span,
                                );
                                return Type::Error;
                            }
                            let arity = self
                                .record_param_var_ids
                                .get(name)
                                .map(|v| v.len())
                                .or_else(|| self.enums.get(name).map(|e| e.params.len()))
                                .unwrap_or(0);
                            if arity == 0 {
                                Type::Generic(*name, vec![])
                            } else {
                                let args: Vec<Type> =
                                    (0..arity).map(|_| self.fresh_var()).collect();
                                Type::Generic(*name, args)
                            }
                        }
                    }
                }
            }
            TypeExprKind::Generic(name, args) => {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_type_expr(a, param_vars))
                    .collect();
                let name_str = resolve(*name);
                match name_str.as_str() {
                    "List" if resolved_args.is_empty() => Type::List(Box::new(self.fresh_var())),
                    "List" if resolved_args.len() == 1 => {
                        Type::List(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Range" if resolved_args.is_empty() => {
                        Type::Range(Box::new(self.fresh_var()))
                    }
                    "Range" if resolved_args.len() == 1 => {
                        Type::Range(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Map" if resolved_args.is_empty() => {
                        Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                    }
                    "Map" if resolved_args.len() == 2 => {
                        let mut iter = resolved_args.into_iter();
                        Type::Map(
                            Box::new(iter.next().unwrap()),
                            Box::new(iter.next().unwrap()),
                        )
                    }
                    "Set" if resolved_args.is_empty() => Type::Set(Box::new(self.fresh_var())),
                    "Set" if resolved_args.len() == 1 => {
                        Type::Set(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Channel" if resolved_args.is_empty() => {
                        Type::Channel(Box::new(self.fresh_var()))
                    }
                    "Channel" if resolved_args.len() == 1 => {
                        Type::Channel(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    _ => {
                        // B2: enforce arity for user-declared parameterized
                        // records and enums. Without this check, an
                        // annotation like `Box(Int, String)` against a
                        // `type Box(a) { ... }` silently produced a
                        // `Type::Generic("Box", [Int, String])` whose extra
                        // arg was dropped at unify time (the `Record /
                        // Generic` arms in `unify` only run when the arities
                        // agree, so mismatched ones no-op'd), leaving the
                        // user with no diagnostic and a runtime type
                        // error at first use of the field.
                        // B2 (round 60): parameterless records (`type Point { x: Int }`)
                        // are NOT entered into `record_param_var_ids` (only
                        // parameterized ones are; see :1935 insert-gate).
                        // Without this `.or_else` chain, `Point(Bool)` got
                        // `expected_arity = None` and silently became
                        // `Type::Generic("Point", [Bool])`, which the
                        // Record/Generic unify arms also no-op'd. Chain to
                        // `records.contains_key` so arity-0 records emit the
                        // standard "expected 0, got N" diagnostic.
                        let expected_arity = self
                            .record_param_var_ids
                            .get(name)
                            .map(|v| v.len())
                            .or_else(|| self.enums.get(name).map(|e| e.params.len()))
                            .or_else(|| self.records.contains_key(name).then_some(0));
                        // B3 (round 60): the generic form `Frobnitz(Int)`
                        // for an undeclared `Frobnitz` should also report
                        // "unknown type 'Frobnitz'" at the annotation span,
                        // matching the bare-name path. Without this, the
                        // ghost `Type::Generic("Frobnitz", [Int])` cascaded
                        // into Display / type-mismatch noise.
                        if expected_arity.is_none() {
                            self.error(
                                format!("unknown type '{name_str}'"),
                                te.span,
                            );
                            return Type::Error;
                        }
                        if let Some(expected) = expected_arity
                            && expected != resolved_args.len()
                        {
                            let kind = if self.records.contains_key(name) {
                                "record"
                            } else {
                                "enum"
                            };
                            let err_span = self.current_type_anno_span.unwrap_or(Span {
                                line: 0,
                                col: 0,
                                offset: 0,
                            });
                            self.error(
                                format!(
                                    "type argument count mismatch for {kind} '{name}': expected {expected}, got {}",
                                    resolved_args.len()
                                ),
                                err_span,
                            );
                            // Return Error so the subsequent unify doesn't
                            // cascade a second "arity mismatch" diagnostic
                            // (the Generic/Generic arm would re-detect the
                            // same problem). The first report already has
                            // the user-facing span; extras only confuse.
                            return Type::Error;
                        }
                        Type::Generic(*name, resolved_args)
                    }
                }
            }
            TypeExprKind::Tuple(elems) => {
                // `()` is the canonical unit type — not a zero-arity tuple.
                if elems.is_empty() {
                    return Type::Unit;
                }
                let types: Vec<Type> = elems
                    .iter()
                    .map(|e| self.resolve_type_expr(e, param_vars))
                    .collect();
                Type::Tuple(types)
            }
            TypeExprKind::Function(params, ret) => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, param_vars))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, param_vars);
                Type::Fun(param_types, Box::new(ret_type))
            }
            TypeExprKind::SelfType => {
                if let Some(ty) = param_vars.get(&intern("Self")) {
                    ty.clone()
                } else {
                    // Self used outside of a trait context
                    self.fresh_var()
                }
            }
        }
    }

    // ── Register function declarations ──────────────────────────────

    fn register_fn_decl(&mut self, f: &FnDecl, env: &mut TypeEnv) {
        // Recovery-stub special case (Option B): record the name and bind
        // its signature just like a real fn, so downstream references in
        // unrelated code do not cascade into "undefined variable" errors.
        // The stub is NOT registered as a "real" top-level name because
        // duplicate-definition checks shouldn't flag a later *real* fn
        // with the same name as a stubbed-out earlier one — the user is
        // fixing the same broken decl, not redeclaring.
        if f.is_recovery_stub {
            self.recovery_stub_names.insert(f.name);
        } else {
            // G1: Detect duplicate top-level function definitions. We only report
            // a hard error when the name collides with another user-registered
            // top-level name. Collisions with builtins are handled elsewhere as
            // a shadow warning.
            if self.top_level_names.contains(&f.name) {
                self.error(
                    format!(
                        "duplicate top-level definition of '{}'; names must be unique at module scope",
                        f.name
                    ),
                    f.span,
                );
            }
            self.top_level_names.insert(f.name);
        }
        let mut param_map = HashMap::new();
        let mut param_types = Vec::new();

        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on parameter / return type annotations.
        let prev_type_span = self.current_type_anno_span.replace(f.span);
        for param in &f.params {
            let ty = match param.kind {
                ParamKind::Type => {
                    // `type a` parameter — seed `a` as a fresh type variable
                    // in `param_map` so it is in scope for the rest of the
                    // signature and any where clauses. The parameter's own
                    // compile-time type is the runtime type descriptor
                    // `TypeOf(a)`, which unifies with record / primitive
                    // descriptor globals at call sites.
                    let name = match &param.pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!("parser guarantees `type` params use an Ident pattern"),
                    };
                    let var = param_map
                        .entry(name)
                        .or_insert_with(|| self.fresh_var())
                        .clone();
                    Type::Generic(intern("TypeOf"), vec![var])
                }
                ParamKind::Data => {
                    if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &mut param_map)
                    } else {
                        self.fresh_var()
                    }
                }
            };
            param_types.push(ty);
        }

        // Binding rule: every lowercase type variable that appears in the
        // user-written return annotation must also be introduced by some
        // parameter (regular annotation or `type a`). Detected by
        // snapshotting `param_map` before resolving the return annotation —
        // any new key added afterwards is a variable that only appears in
        // the return, with no anchor.
        let pre_return_keys: std::collections::HashSet<Symbol> =
            param_map.keys().copied().collect();
        let ret_type = if let Some(te) = &f.return_type {
            let resolved = self.resolve_type_expr(te, &mut param_map);
            for (name, _) in param_map.iter() {
                if !pre_return_keys.contains(name) {
                    let n = resolve(*name);
                    self.error(
                        format!(
                            "type variable '{}' in return type is not introduced by any parameter; \
                             add a `type {}` parameter or anchor it on an existing parameter's type",
                            n, n
                        ),
                        f.span,
                    );
                }
            }
            resolved
        } else {
            self.fresh_var()
        };
        self.current_type_anno_span = prev_type_span;

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type));
        let mut scheme = self.generalize(env, &fn_type);

        // Resolve where clauses to (TyVar, trait_name) using param_map.
        // Type variables must be introduced via explicit type annotations in the signature.
        // Trait args (for parameterized traits like `a: TryInto(b)`) are
        // resolved through `param_map` and stashed in `trait_arg_bindings`
        // so descriptor method resolution can substitute them later.
        for (type_param, trait_name, trait_args) in &f.where_clauses {
            if let Some(ty) = param_map.get(type_param) {
                let resolved = self.apply(ty);
                if let Type::Var(tv) = resolved {
                    scheme.constraints.push((tv, *trait_name));
                    if !trait_args.is_empty() {
                        let resolved_args: Vec<Type> = trait_args
                            .iter()
                            .map(|te| self.resolve_type_expr(te, &mut param_map))
                            .collect();
                        self.trait_arg_bindings
                            .insert((tv, *trait_name), resolved_args);
                    }
                }
            } else {
                let first_param_name = f
                    .params
                    .first()
                    .map(|p| match &p.pattern.kind {
                        PatternKind::Ident(n) => resolve(*n),
                        _ => "_".to_string(),
                    })
                    .unwrap_or_else(|| "_".to_string());
                self.error(
                    format!(
                        "type variable '{}' in where clause is not introduced in the function signature; \
                         use an explicit type annotation, e.g.: fn {}({}: {}) where {}: {}",
                        type_param, f.name,
                        first_param_name,
                        type_param, type_param, trait_name
                    ),
                    f.span,
                );
            }
        }

        env.define(f.name, scheme);
    }

    // ── Register trait declarations ─────────────────────────────────

    fn register_trait_decl(&mut self, t: &TraitDecl) {
        // Reject redefinition of builtin trait names. The compiler has
        // already preregistered TraitInfo + auto-derived impls for these
        // names; letting a user `trait Equal { fn eq(self) -> Bool }`
        // overwrite them would produce a cascade of bogus
        // "missing method" errors when validate_trait_impls runs the
        // preregistered impls against the user's new body.
        let trait_name_str = resolve(t.name);
        if BUILTIN_TRAIT_NAMES.contains(&trait_name_str.as_str()) {
            self.error(
                format!("trait '{trait_name_str}' is a builtin trait and cannot be redefined"),
                t.span,
            );
            return;
        }

        // GAP (round 35 F6): duplicate method names in a trait
        // declaration used to silently overwrite each other in the
        // trait's `methods` Vec (first entry won for method lookup but
        // the second's signature won for any HashMap-based bookkeeping
        // like `default_method_bodies`). Emit a diagnostic per dup.
        {
            let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
            for m in &t.methods {
                if !seen.insert(m.name) {
                    self.error(
                        format!("duplicate method '{}' in trait '{}'", m.name, t.name),
                        m.span,
                    );
                }
            }
        }

        let self_var = self.fresh_var();
        // Allocate a fresh TyVar for each trait-level parameter. These
        // are in scope across every method signature — writing
        // `trait TryInto(b) { fn try_into(self) -> Result(b, Error) }`
        // makes `b` resolve to the same TyVar in the method.
        let trait_param_vars: Vec<(Symbol, Type)> =
            t.params.iter().map(|p| (*p, self.fresh_var())).collect();
        let param_var_ids: Vec<TyVar> = trait_param_vars
            .iter()
            .map(|(_, ty)| match ty {
                Type::Var(v) => *v,
                _ => unreachable!("fresh_var always returns Type::Var"),
            })
            .collect();
        let methods: Vec<(Symbol, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let mut param_map = HashMap::new();
                param_map.insert(intern("Self"), self_var.clone());
                for (name, ty) in &trait_param_vars {
                    param_map.insert(*name, ty.clone());
                }
                let mut param_types = Vec::new();
                for param in &m.params {
                    let ty = match param.kind {
                        ParamKind::Type => {
                            let name = match &param.pattern.kind {
                                PatternKind::Ident(n) => *n,
                                _ => unreachable!(
                                    "parser guarantees `type` params use an Ident pattern"
                                ),
                            };
                            let var = param_map
                                .entry(name)
                                .or_insert_with(|| self.fresh_var())
                                .clone();
                            Type::Generic(intern("TypeOf"), vec![var])
                        }
                        ParamKind::Data => {
                            if let Some(te) = &param.ty {
                                self.resolve_type_expr(te, &mut param_map)
                            } else {
                                self.fresh_var()
                            }
                        }
                    };
                    param_types.push(ty);
                }
                let ret_type = if let Some(te) = &m.return_type {
                    self.resolve_type_expr(te, &mut param_map)
                } else {
                    self.fresh_var()
                };
                (m.name, Type::Fun(param_types, Box::new(ret_type)))
            })
            .collect();

        // Collect default-bodied methods. Methods whose `is_signature_only`
        // flag is false carry a real (non-placeholder) body and are eligible
        // to be cloned into impls that omit them.
        let default_method_bodies: HashMap<Symbol, FnDecl> = t
            .methods
            .iter()
            .filter(|m| !m.is_signature_only)
            .map(|m| (m.name, (*m).clone()))
            .collect();

        // Trait-level where bounds on params. Only the (param_name,
        // trait_name) shape is kept; trait_args on the bound are not
        // yet honored (reserved for a future extension where bounds
        // can themselves reference other trait args).
        let param_where_clauses: Vec<(Symbol, Symbol)> = t
            .param_where_clauses
            .iter()
            .map(|(var, tr, _args)| (*var, *tr))
            .collect();

        let supertrait_names: Vec<Symbol> = t.supertraits.iter().map(|(n, _)| *n).collect();
        let supertrait_args: Vec<Vec<TypeExpr>> =
            t.supertraits.iter().map(|(_, a)| a.clone()).collect();

        self.traits.insert(
            t.name,
            TraitInfo {
                params: t.params.clone(),
                param_var_ids,
                supertraits: supertrait_names,
                supertrait_args,
                param_where_clauses,
                methods,
                decl_span: t.span,
                default_method_bodies,
            },
        );
    }

    // ── Register trait implementations ──────────────────────────────

    /// Convert a type name Symbol to a Type.
    fn type_from_name(name: Symbol) -> Type {
        let name_str = resolve(name);
        match name_str.as_str() {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "Bool" => Type::Bool,
            "String" => Type::String,
            _ => Type::Generic(name, vec![]),
        }
    }

    /// For every `Decl::TraitImpl` in `decls`, find missing methods that
    /// the trait provides default bodies for and clone the default
    /// FnDecls into the impl's `methods` vec. Runs between trait-decl
    /// registration and trait-impl registration so the synthesized
    /// methods participate in the normal method_table population /
    /// body-check / compile pipeline as if the user had written them
    /// inline.
    ///
    /// We intentionally mutate the AST (rather than carrying defaults
    /// out-of-band) because every downstream consumer — register_trait_impl,
    /// the pass-3 body checker loop, the compiler's emit-impl-methods
    /// loop — already iterates `ti.methods`. Cloning the default into
    /// the impl is the smallest delta that makes the existing code
    /// "just work".
    fn synthesize_default_methods(&self, decls: &mut [Decl]) {
        for decl in decls.iter_mut() {
            let Decl::TraitImpl(ti) = decl else {
                continue;
            };
            let Some(trait_info) = self.traits.get(&ti.trait_name) else {
                // Unknown trait — let validate_trait_impls / dispatch
                // surface the diagnostic; nothing to synthesize here.
                continue;
            };
            let impl_method_names: std::collections::HashSet<Symbol> =
                ti.methods.iter().map(|m| m.name).collect();
            // Walk methods in the order they appear on the trait so the
            // synthesized FnDecls land in a deterministic order.
            for (method_name, _ty) in &trait_info.methods {
                if impl_method_names.contains(method_name) {
                    continue;
                }
                if let Some(default_fn) = trait_info.default_method_bodies.get(method_name) {
                    ti.methods.push(default_fn.clone());
                }
            }
        }
    }

    fn register_trait_impl(&mut self, ti: &TraitImpl, env: &mut TypeEnv) {
        let impl_key = (ti.trait_name, ti.target_type);

        // Coherence check: reject duplicate user-defined impls.
        if self.trait_impl_set.contains(&impl_key) {
            // Allow overriding auto-derived impls.
            let first_method = ti
                .methods
                .first()
                .map(|m| m.name)
                .unwrap_or_else(|| intern("display"));
            let is_overriding_auto = self
                .method_table
                .get(&(ti.target_type, first_method))
                .map(|e| e.is_auto_derived)
                .unwrap_or(true);
            if !is_overriding_auto {
                self.error(
                    format!(
                        "duplicate implementation of trait '{}' for type '{}'",
                        ti.trait_name, ti.target_type
                    ),
                    ti.span,
                );
                return;
            }
        }

        self.trait_impl_set.insert(impl_key);
        // GAP-2: record the impl block's real span for validate_trait_impls
        // to use when reporting missing-method diagnostics.
        self.trait_impl_spans.insert(impl_key, ti.span);

        // Build the impl-level parameter map. For a parameterized target
        // like `trait X for Box(a)`, each lowercase binder in
        // `target_param_names` becomes a fresh type variable that is
        // shared across every method in the impl — so `fn get(self) -> a`
        // and `fn put(self, x: a)` in the same impl refer to the SAME
        // type variable, mirroring the fn-signature convention.
        let mut impl_param_map: HashMap<Symbol, Type> = HashMap::new();
        for &param_name in &ti.target_param_names {
            impl_param_map.insert(param_name, self.fresh_var());
        }

        // Construct the self_type. Three cases:
        //   1. Bare-target form (`trait X for Int`): target_type_args is
        //      empty. For primitive types, fall through to type_from_name.
        //      For parameterized user types, synthesize fresh-var args to
        //      match the record's / enum's arity — otherwise the receiver-
        //      unify step in dispatch_method_entry would fail with arity
        //      mismatch when the caller passes a concrete instantiation
        //      like `Box { value: 42 }` (Generic("Box", [Int])) against a
        //      zero-arg self_type (Generic("Box", [])).
        //   2. Parameterized user type (`trait X for Box(a)`): resolve
        //      each arg through impl_param_map. Arity enforced against
        //      the record's param_var_ids or the enum's declared params.
        //   3. Built-in parameterized form (`trait X for List(a)`):
        //      resolve_type_expr handles List/Map/Set/Channel/Tuple/Fn
        //      already; reuse it.
        //
        // Round-23 GAP #1: reject trait impls whose target type was never
        // declared. Previously `trait Greet for Widget { ... }` with no
        // `type Widget` anywhere silently fell through to
        // `Type::Generic("Widget", vec![])` (see type_from_name) and
        // produced no diagnostic — `silt check` reported success even
        // though the impl attached methods to a phantom type. This is
        // distinct from the round-17 `type_name_for_impl` fix (which
        // mapped Fn→Some("Fun")): here we're validating that the target
        // name refers to *something real* at all.
        //
        // The check applies only to uppercase target names. Lowercase
        // names like `trait Display for a { ... }` are the generic
        // trait-impl form — `a` is a type variable, not a declared
        // type, and must continue to type-check.
        {
            let name_str = resolve(ti.target_type);
            let first_char = name_str.chars().next().unwrap_or('A');
            let is_lowercase_tyvar = first_char.is_lowercase();
            // Both checks consult the authoritative built-in type table
            // at `crate::types::builtins`. A new built-in type added to
            // BUILTIN_TYPES is automatically recognised here.
            let is_primitive = crate::types::builtins::is_primitive(&name_str);
            let is_builtin_container = crate::types::builtins::is_container(&name_str);
            let is_user_record = self.records.contains_key(&ti.target_type);
            let is_user_enum = self.enums.contains_key(&ti.target_type);
            if !is_lowercase_tyvar
                && !is_primitive
                && !is_builtin_container
                && !is_user_record
                && !is_user_enum
            {
                self.error(
                    format!("trait impl target '{name_str}' is not a declared type"),
                    ti.span,
                );
            }
        }

        let self_type = if ti.target_type_args.is_empty() {
            let user_arity = self
                .record_param_var_ids
                .get(&ti.target_type)
                .map(|v| v.len())
                .or_else(|| self.enums.get(&ti.target_type).map(|e| e.params.len()))
                .unwrap_or(0);
            if user_arity == 0 {
                Self::type_from_name(ti.target_type)
            } else {
                let args: Vec<Type> = (0..user_arity).map(|_| self.fresh_var()).collect();
                Type::Generic(ti.target_type, args)
            }
        } else {
            // Arity check. Covers user-declared record/enum targets via
            // record_param_var_ids / self.enums, AND builtin parameterized
            // containers (List, Set, Channel, Map) whose arities are fixed
            // by the language. Without the builtin arm, `trait X for List(a, b)`
            // fell through to `_ => Type::Generic("List", [a, b])` below,
            // silently producing a phantom 2-arg List type with no diagnostic.
            let name_str_for_arity = resolve(ti.target_type);
            // Derive fixed-arity builtin entries from the authoritative
            // table. Variadic shapes (`Tuple`, `Fn`, `Fun`, `Handle`)
            // carry `arity: None` and are intentionally skipped — they
            // do not participate in this trait-impl arity check.
            let builtin_arity: Option<(usize, &'static str)> = crate::types::builtins::lookup(
                name_str_for_arity.as_str(),
            )
            .filter(|b| b.kind == crate::types::builtins::BuiltinKind::Container)
            .and_then(|b| b.arity.map(|a| (a as usize, "builtin")));
            let expected_arity = self
                .record_param_var_ids
                .get(&ti.target_type)
                .map(|v| (v.len(), "record"))
                .or_else(|| {
                    self.enums
                        .get(&ti.target_type)
                        .map(|e| (e.params.len(), "enum"))
                })
                .or(builtin_arity);
            if let Some((expected, kind)) = expected_arity
                && expected != ti.target_type_args.len()
            {
                self.error(
                    format!(
                        "type argument count mismatch for {kind} '{}' in trait impl: expected {expected}, got {}",
                        resolve(ti.target_type),
                        ti.target_type_args.len()
                    ),
                    ti.span,
                );
            }
            // Resolve through a dedicated Generic form so the head symbol
            // is preserved alongside the impl-level tyvar args.
            let resolved_args: Vec<Type> = ti
                .target_type_args
                .iter()
                .map(|arg_te| self.resolve_type_expr(arg_te, &mut impl_param_map))
                .collect();
            let name_str = resolve(ti.target_type);
            match name_str.as_str() {
                "List" if resolved_args.len() == 1 => {
                    Type::List(Box::new(resolved_args.into_iter().next().unwrap()))
                }
                "Range" if resolved_args.len() == 1 => {
                    Type::Range(Box::new(resolved_args.into_iter().next().unwrap()))
                }
                "Set" if resolved_args.len() == 1 => {
                    Type::Set(Box::new(resolved_args.into_iter().next().unwrap()))
                }
                "Channel" if resolved_args.len() == 1 => {
                    Type::Channel(Box::new(resolved_args.into_iter().next().unwrap()))
                }
                "Map" if resolved_args.len() == 2 => {
                    let mut iter = resolved_args.into_iter();
                    Type::Map(
                        Box::new(iter.next().unwrap()),
                        Box::new(iter.next().unwrap()),
                    )
                }
                _ => Type::Generic(ti.target_type, resolved_args),
            }
        };

        // Resolve impl-level where clauses (e.g. `trait X for Box(a) where
        // a: Show`) to `(TyVar, trait)` pairs against the impl_param_map.
        // These apply to every method in the impl and are appended to
        // both the method's scheme (so active_constraints in the body see
        // them during check_fn_body_with_name) and its MethodEntry (so
        // external call sites defer the obligation via pending_where).
        //
        // Multi-trait bounds (`where a: Show + Hash`) arrive pre-flattened
        // from parse_where_clauses_opt as separate (tv, trait) entries
        // sharing a type_var, so the resolution loop handles both forms
        // with a single path.
        let mut impl_level_constraints: Vec<(TyVar, Symbol)> = Vec::new();
        // Parallel structure indexed by target_param_names position, used to
        // populate self.impl_constraints below so that call-site constraint
        // resolution can recursively verify the impl's own where clauses
        // against the actual concrete type arguments at the call site.
        let mut impl_obligations_by_index: Vec<(usize, Symbol)> = Vec::new();
        for (type_param, trait_name, _trait_args) in &ti.where_clauses {
            if !self.traits.contains_key(trait_name) {
                self.error(
                    format!(
                        "unknown trait '{}' in where clause on trait impl '{} for {}'",
                        resolve(*trait_name),
                        resolve(ti.trait_name),
                        resolve(ti.target_type)
                    ),
                    ti.span,
                );
                continue;
            }
            match impl_param_map.get(type_param) {
                Some(ty) => {
                    let resolved = self.apply(ty);
                    if let Type::Var(tv) = resolved {
                        impl_level_constraints.push((tv, *trait_name));
                    }
                    // If resolved is concrete (shouldn't happen — impl_param_map
                    // only inserts fresh Var entries) treat it as a tautology.
                    if let Some(idx) = ti.target_param_names.iter().position(|n| n == type_param) {
                        impl_obligations_by_index.push((idx, *trait_name));
                    }
                }
                None => {
                    self.error(
                        format!(
                            "type variable '{}' in impl-level where clause is not declared in the target type arguments; \
                             declare it as a target parameter: `trait {} for {}({}, ...)`",
                            resolve(*type_param),
                            resolve(ti.trait_name),
                            resolve(ti.target_type),
                            resolve(*type_param)
                        ),
                        ti.span,
                    );
                }
            }
        }
        if !impl_obligations_by_index.is_empty() {
            self.impl_constraints
                .insert((ti.trait_name, ti.target_type), impl_obligations_by_index);
        }

        // GAP (round 35 F5): extraneous trait-impl methods — methods on
        // the impl whose names aren't declared in the trait — used to
        // get silently registered into the method_table. Reject each
        // method whose name is not in the trait's declared method list.
        //
        // GAP (round 35 F6): duplicate method names within a single
        // trait impl used to silently overwrite the earlier definition
        // in the method_table. Track a seen-set and reject the second
        // (and subsequent) occurrences.
        // Validate trait_args against the trait's declared parameter
        // count. `trait Foo(a, b)` must be implemented as `trait Foo(X, Y) for T`;
        // a parameterless trait must be `trait Foo for T` (no args).
        // When the count matches and the trait declared param-level
        // where bounds, verify that each supplied arg satisfies the
        // declared bounds — concrete types are checked now via
        // `verify_trait_obligation`; unresolved tyvars are added as
        // impl-level constraints so body checking sees them.
        let trait_info_clone = self.traits.get(&ti.trait_name).cloned();
        if let Some(trait_info) = &trait_info_clone {
            if ti.trait_args.len() != trait_info.params.len() {
                let expected = trait_info.params.len();
                self.error(
                    format!(
                        "trait '{}' expects {} {}, got {} in impl for '{}'",
                        resolve(ti.trait_name),
                        expected,
                        inference::plural(expected, "type argument", "type arguments"),
                        ti.trait_args.len(),
                        resolve(ti.target_type),
                    ),
                    ti.span,
                );
            } else if !ti.trait_args.is_empty() {
                // Resolve each supplied trait arg through impl_param_map
                // so lowercase names bind to the same fresh tyvars used
                // by the impl's methods. Stash for later verification by
                // `verify_trait_obligation` (closes the trait-args
                // soundness hole: `where a: TryInto(Int)` against a
                // `trait TryInto(Float) for String` impl must now reject).
                let resolved_trait_args: Vec<Type> = ti
                    .trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                    .collect();
                self.impl_trait_args
                    .insert((ti.trait_name, ti.target_type), resolved_trait_args.clone());
                if !trait_info.param_where_clauses.is_empty() {
                    for (param_name, bound_trait) in &trait_info.param_where_clauses {
                        let Some(idx) = trait_info.params.iter().position(|p| p == param_name)
                        else {
                            continue;
                        };
                        let Some(arg_ty) = resolved_trait_args.get(idx) else {
                            continue;
                        };
                        let applied = self.apply(arg_ty);
                        match &applied {
                            Type::Var(_) => {
                                // Deferred — the impl's own where clause path
                                // will propagate it; skip here.
                            }
                            _ => {
                                // Parameterless sub-bound: `trait Foo(a) where a: Display`
                                // — no args to thread.
                                self.verify_trait_obligation(
                                    *bound_trait,
                                    &[],
                                    &applied,
                                    ti.span,
                                );
                            }
                        }
                    }
                }
            }
        }

        let trait_method_names: Option<std::collections::HashSet<Symbol>> = self
            .traits
            .get(&ti.trait_name)
            .map(|info| info.methods.iter().map(|(n, _)| *n).collect());
        let mut seen_impl_methods: std::collections::HashSet<Symbol> =
            std::collections::HashSet::new();
        for method in &ti.methods {
            if !seen_impl_methods.insert(method.name) {
                self.error(
                    format!(
                        "duplicate method '{}' in trait impl '{} for {}'",
                        method.name, ti.trait_name, ti.target_type
                    ),
                    method.span,
                );
            }
            if let Some(names) = &trait_method_names
                && !names.contains(&method.name)
            {
                self.error(
                    format!(
                        "method '{}' is not declared in trait '{}'",
                        method.name, ti.trait_name
                    ),
                    method.span,
                );
            }
        }

        let self_sym = intern("self");
        for method in &ti.methods {
            // Seed the method's param_map with both the impl-level target
            // tyvars AND the Self alias, so the method signature and body
            // see `a` as a concrete TyVar and `self` / `Self` resolve to
            // the parameterized self_type.
            let mut param_map = impl_param_map.clone();
            param_map.insert(intern("Self"), self_type.clone());
            let mut param_types = Vec::new();
            for (i, param) in method.params.iter().enumerate() {
                let ty = match param.kind {
                    ParamKind::Type => {
                        let name = match &param.pattern.kind {
                            PatternKind::Ident(n) => *n,
                            _ => {
                                unreachable!("parser guarantees `type` params use an Ident pattern")
                            }
                        };
                        let var = param_map
                            .entry(name)
                            .or_insert_with(|| self.fresh_var())
                            .clone();
                        Type::Generic(intern("TypeOf"), vec![var])
                    }
                    ParamKind::Data => {
                        if let Some(te) = &param.ty {
                            self.resolve_type_expr(te, &mut param_map)
                        } else if i == 0
                            && matches!(&param.pattern.kind, PatternKind::Ident(n) if *n == self_sym)
                        {
                            // Bare `self` parameter in a trait impl: type it as the
                            // target type so field/method accesses on `self` are
                            // properly checked against the impl's target.
                            self_type.clone()
                        } else {
                            self.fresh_var()
                        }
                    }
                };
                param_types.push(ty);
            }
            let ret_type = if let Some(te) = &method.return_type {
                self.resolve_type_expr(te, &mut param_map)
            } else {
                self.fresh_var()
            };

            let fn_type = Type::Fun(param_types, Box::new(ret_type));

            // GAP (round 17 F3): method-name coherence across distinct
            // traits on the same target. If `(target, method_name)` is
            // already in the method table and came from a *different*
            // user-defined trait, registering this impl would silently
            // overwrite the earlier one and route every `.method()`
            // call to the last-registered trait. Reject with an
            // ambiguity error that names both traits.
            if let Some(existing) = self.method_table.get(&(ti.target_type, method.name))
                && !existing.is_auto_derived
                && let Some(existing_trait) = existing.trait_name
                && existing_trait != ti.trait_name
            {
                self.error(
                    format!(
                        "ambiguous method '{}' on type '{}': provided by traits {}, {}",
                        method.name, ti.target_type, existing_trait, ti.trait_name
                    ),
                    ti.span,
                );
            }

            // Collect constraints for this method:
            //   (a) every impl-level constraint, verbatim (they reference
            //       impl_param_map TyVars which are also visible to the
            //       method's fn_type because param_map was cloned from
            //       impl_param_map);
            //   (b) every method-level `where` clause, resolved through
            //       the method's param_map — which sees BOTH impl-level
            //       binders (from the clone) AND method-local type annos.
            //
            // Method-level where clauses on trait-impl methods were
            // silently ignored by prior rounds — `register_trait_impl`
            // never consulted `method.where_clauses`. The impl-level
            // follow-up folds that latent gap into the same code path.
            let mut method_constraints = impl_level_constraints.clone();
            for (type_param, trait_name, _trait_args) in &method.where_clauses {
                if !self.traits.contains_key(trait_name) {
                    self.error(
                        format!(
                            "unknown trait '{}' in where clause on '{}.{}'",
                            resolve(*trait_name),
                            resolve(ti.target_type),
                            resolve(method.name)
                        ),
                        method.span,
                    );
                    continue;
                }
                match param_map.get(type_param) {
                    Some(ty) => {
                        let resolved = self.apply(ty);
                        if let Type::Var(tv) = resolved {
                            method_constraints.push((tv, *trait_name));
                        }
                    }
                    None => {
                        // Give the user the full "declare it in the sig or
                        // target" hint — this is the same spirit as the
                        // register_fn_decl error at mod.rs:1690.
                        self.error(
                            format!(
                                "type variable '{}' in where clause on '{}.{}' is not declared in the impl target \
                                 arguments or in the method's parameter annotations",
                                resolve(*type_param),
                                resolve(ti.target_type),
                                resolve(method.name)
                            ),
                            method.span,
                        );
                    }
                }
            }

            // Populate method_table. Store BOTH the raw template type
            // AND the collected constraints so receiver-method dispatch
            // sites can instantiate both through a shared substitution
            // via instantiate_method_entry, then push the instantiated
            // constraints into pending_where_constraints for the
            // finalize-pass check.
            self.method_table.insert(
                (ti.target_type, method.name),
                MethodEntry {
                    method_type: fn_type.clone(),
                    span: ti.span,
                    is_auto_derived: false,
                    trait_name: Some(ti.trait_name),
                    method_constraints: method_constraints.clone(),
                },
            );

            // Legacy: register in TypeEnv as "TypeName.method_name".
            // Attach the same constraints to the scheme so the method
            // body's check_fn_body_with_name sees them as active.
            let key = intern(&format!("{}.{}", ti.target_type, method.name));
            let mut scheme = self.generalize(env, &fn_type);
            for (tv, trait_name) in &method_constraints {
                if !scheme.constraints.contains(&(*tv, *trait_name)) {
                    scheme.constraints.push((*tv, *trait_name));
                }
            }
            env.define(key, scheme);
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────

/// Walk two types in parallel and build a mapping from `old` tyvars to
/// `new` tyvars wherever they appear at the same structural position.
/// Used by pass-3 scheme narrowing to remap where-clause constraints
/// from their pass-2 tyvar ids (stored in the original scheme) to the
/// fresh pass-3 tyvar ids that ended up in the narrowed scheme after
/// body inference. Structurally divergent positions are skipped — the
/// caller only uses the mapping for entries whose new tyvar is still
/// free in the narrowed scheme, so spurious matches are harmless.
pub(super) fn align_tyvars(old: &Type, new: &Type) -> HashMap<TyVar, TyVar> {
    let mut map = HashMap::new();
    align_tyvars_into(old, new, &mut map);
    map
}

fn align_tyvars_into(old: &Type, new: &Type, map: &mut HashMap<TyVar, TyVar>) {
    match (old, new) {
        (Type::Var(o), Type::Var(n)) => {
            map.entry(*o).or_insert(*n);
        }
        (Type::Fun(op, or_), Type::Fun(np, nr)) => {
            if op.len() == np.len() {
                for (a, b) in op.iter().zip(np.iter()) {
                    align_tyvars_into(a, b, map);
                }
            }
            align_tyvars_into(or_, nr, map);
        }
        (Type::List(o), Type::List(n)) => align_tyvars_into(o, n, map),
        (Type::Range(o), Type::Range(n)) => align_tyvars_into(o, n, map),
        // Range is a nominal alias for List (see unify arms in
        // src/typechecker/mod.rs). Align element-wise across the
        // List/Range boundary so scheme generalization/instantiation
        // remains sound when a fn returning List(a) flows into a
        // Range-typed binder or vice versa.
        (Type::List(o), Type::Range(n)) | (Type::Range(o), Type::List(n)) => {
            align_tyvars_into(o, n, map)
        }
        (Type::Set(o), Type::Set(n)) => align_tyvars_into(o, n, map),
        (Type::Channel(o), Type::Channel(n)) => align_tyvars_into(o, n, map),
        (Type::Tuple(o), Type::Tuple(n)) if o.len() == n.len() => {
            for (a, b) in o.iter().zip(n.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        (Type::Map(ok, ov), Type::Map(nk, nv)) => {
            align_tyvars_into(ok, nk, map);
            align_tyvars_into(ov, nv, map);
        }
        (Type::Record(_, of), Type::Record(_, nf)) if of.len() == nf.len() => {
            for ((_, a), (_, b)) in of.iter().zip(nf.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        (Type::Generic(_, oa), Type::Generic(_, na)) if oa.len() == na.len() => {
            for (a, b) in oa.iter().zip(na.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        _ => {}
    }
}

/// Collect the set of variable names bound by a pattern.
pub(super) fn collect_pattern_vars(pat: &Pattern) -> Vec<Symbol> {
    match &pat.kind {
        PatternKind::Ident(name) => vec![*name],
        PatternKind::Tuple(pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        PatternKind::List(pats, rest) => {
            let mut vars: Vec<Symbol> = pats.iter().flat_map(collect_pattern_vars).collect();
            if let Some(rest_pat) = rest {
                vars.extend(collect_pattern_vars(rest_pat));
            }
            vars
        }
        PatternKind::Constructor(_, pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        PatternKind::Record { fields, .. } => {
            let mut vars: Vec<Symbol> = Vec::new();
            for (field_name, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    // Shorthand field `{ x }` binds `x`
                    vars.push(*field_name);
                }
            }
            vars
        }
        PatternKind::Or(alts) => {
            // Return vars from first alt (they should all be the same after validation)
            alts.first().map(collect_pattern_vars).unwrap_or_default()
        }
        PatternKind::Map(entries) => entries
            .iter()
            .flat_map(|(_, p)| collect_pattern_vars(p))
            .collect(),
        PatternKind::Wildcard
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(_, _)
        | PatternKind::FloatRange(_, _)
        | PatternKind::Pin(_) => vec![],
    }
}

/// Check if a type variable occurs in a type (occurs check for unification).
fn occurs_in(var: TyVar, ty: &Type) -> bool {
    match ty {
        Type::Var(v) => *v == var,
        Type::Fun(params, ret) => params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret),
        Type::List(inner) => occurs_in(var, inner),
        Type::Range(inner) => occurs_in(var, inner),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in(var, e)),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| occurs_in(var, t)),
        Type::Generic(_, args) => args.iter().any(|a| occurs_in(var, a)),
        Type::Map(k, v) => occurs_in(var, k) || occurs_in(var, v),
        Type::Set(inner) => occurs_in(var, inner),
        Type::Channel(inner) => occurs_in(var, inner),
        Type::Int
        | Type::Float
        | Type::ExtFloat
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Error
        | Type::Never => false,
    }
}

/// Register a single built-in trait declaration with a one-method
/// signature of the shape `fn <method>(a0, ..., a{arity-1}) -> <ret>`.
/// Each `ai` is a fresh type variable. All per-trait fields other than
/// the method signature are defaulted (empty params, no supertraits,
/// no default bodies, zero span) — the four built-in traits Display,
/// Compare, Equal, Hash share that shape exactly. Extracted round 61
/// to collapse four near-identical blocks that differed only in the
/// (trait_name, method_name, arity, return_ty) 4-tuple.
fn register_builtin_trait_decl(
    checker: &mut TypeChecker,
    trait_name: &str,
    method_name: &str,
    arity: usize,
    return_ty: Type,
) {
    let param_vars: Vec<Type> = (0..arity).map(|_| checker.fresh_var()).collect();
    checker.traits.insert(
        intern(trait_name),
        TraitInfo {
            params: Vec::new(),
            param_var_ids: Vec::new(),
            supertraits: Vec::new(),
            supertrait_args: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![(
                intern(method_name),
                Type::Fun(param_vars, Box::new(return_ty)),
            )],
            decl_span: Span::new(0, 0),
            default_method_bodies: HashMap::new(),
        },
    );
}

/// Register built-in trait declarations (Display/Compare/Equal/Hash)
/// and their auto-derived impls for primitives and builtin containers.
///
/// This is the single source of truth for derive policy. Both
/// `TypeChecker::check_program` and `ReplTypeContext::new` call it so
/// `silt check` and the REPL never diverge on which types implement
/// which traits.
///
/// Derive policy:
/// - `Int`, `Float`, `Bool`, `String`, `()`, `List` get all four
///   built-in traits (Equal, Compare, Hash, Display).
/// - `Tuple`, `Map`, `Set` get Equal/Hash/Display only — the VM's
///   `compare()` (src/vm/arithmetic.rs) does not support ordering for
///   these, so registering Compare would type-check code that then
///   panics at runtime.
/// - `Option`, `Result` get Equal/Hash/Display only. They wrap generic
///   parameters; the auto-derived methods are stored as polymorphic
///   templates and instantiated at each call site. Compare is
///   excluded because ordering on Variants is limited to same-name
///   variants at runtime.
pub(super) fn register_builtin_trait_impls(checker: &mut TypeChecker) {
    // ── Register built-in trait declarations ────────────────────
    // Round 61 dead-code fix: four near-identical blocks (Display,
    // Compare, Equal, Hash) collapsed to one parameterised helper.
    // Every per-block field (params/param_var_ids/supertraits/
    // supertrait_args/param_where_clauses/decl_span/default_method_bodies)
    // was already identical across the four sites — only the trait
    // name, method name, arity, and return type varied. The sibling
    // helper `register_auto_derived_impls_for` proved this shape is
    // parameterisable.
    register_builtin_trait_decl(checker, "Display", "display", 1, Type::String);
    register_builtin_trait_decl(checker, "Compare", "compare", 2, Type::Int);
    register_builtin_trait_decl(checker, "Equal", "equal", 2, Type::Bool);
    register_builtin_trait_decl(checker, "Hash", "hash", 1, Type::Int);
    // ── Error trait ─────────────────────────────────────────────
    // Phase 1 of the stdlib error redesign (see
    // `docs/proposals/stdlib-errors.md`). `trait Error: Display`
    // provides a single `message(self) -> String` method whose default
    // body delegates to `self.display()`, so impls that are happy with
    // the default don't need to write a body. `Error` is NOT
    // auto-derived — stdlib and user types must write
    // `trait Error for MyErr { ... }` explicitly.
    {
        let error_self = checker.fresh_var();
        // Synthesize the default body `self.display()` as a real
        // FnDecl so `synthesize_default_methods` can clone it into
        // impls that omit `message`. The synthesized FnDecl goes
        // through the normal method-compilation pipeline.
        let dummy_span = Span::new(0, 0);
        let self_sym = intern("self");
        let self_param = Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(self_sym), dummy_span),
            ty: None,
        };
        let self_ident = Expr::new(ExprKind::Ident(self_sym), dummy_span);
        let field_access = Expr::new(
            ExprKind::FieldAccess(Box::new(self_ident), intern("display")),
            dummy_span,
        );
        let default_body = Expr::new(
            ExprKind::Call(Box::new(field_access), Vec::new()),
            dummy_span,
        );
        let default_fn = FnDecl {
            name: intern("message"),
            params: vec![self_param],
            return_type: Some(TypeExpr::new(
                TypeExprKind::Named(intern("String")),
                dummy_span,
            )),
            where_clauses: Vec::new(),
            body: default_body,
            is_pub: true,
            span: dummy_span,
            is_recovery_stub: false,
            is_signature_only: false,
        };
        let mut default_bodies = HashMap::new();
        default_bodies.insert(intern("message"), default_fn);
        checker.traits.insert(
            intern("Error"),
            TraitInfo {
                params: Vec::new(),
                param_var_ids: Vec::new(),
                supertraits: vec![intern("Display")],
                supertrait_args: vec![Vec::new()],
                param_where_clauses: Vec::new(),
                methods: vec![(
                    intern("message"),
                    Type::Fun(vec![error_self], Box::new(Type::String)),
                )],
                decl_span: Span::new(0, 0),
                default_method_bodies: default_bodies,
            },
        );
    }

    // ── Register auto-derived impls ─────────────────────────────
    // Error is intentionally excluded from auto-derive: user code and
    // stdlib must `trait Error for XyzError { ... }` explicitly. Only
    // Equal/Compare/Hash/Display are auto-derived for the built-in
    // types below.
    let all_auto_traits: &[&str] = BUILTIN_AUTO_DERIVED_TRAIT_NAMES;
    let non_ordering_traits: &[&str] = &["Equal", "Hash", "Display"];

    // Primitives + List: all four auto-derived traits.
    // `ExtFloat` is the widened-float result of `Float / Float` (see
    // `src/typechecker/inference.rs:2326-2333`); it must auto-derive all
    // four built-in traits so that a divided Float can flow through a
    // `Display`/`Equal`/`Compare`/`Hash` trait bound without a spurious
    // "type 'ExtFloat' does not implement trait ..." rejection.
    register_auto_derived_impls_for(
        checker,
        &["Int", "Float", "ExtFloat", "Bool", "String", "()"],
        all_auto_traits,
    );
    register_auto_derived_impls_for(checker, &["List"], all_auto_traits);
    // Tuple/Map/Set: Equal/Hash/Display only.
    register_auto_derived_impls_for(checker, &["Tuple", "Map", "Set"], non_ordering_traits);
    // Option/Result: Equal/Hash/Display only (generic wrappers, stored
    // as polymorphic templates).
    register_auto_derived_impls_for(checker, &["Option", "Result"], non_ordering_traits);
}

/// Register auto-derived trait impls and method-table entries for a
/// group of types against a set of trait names. Shared helper used by
/// `register_builtin_trait_impls` and the `time` builtin module so
/// the set of derived methods stays consistent.
///
/// The four built-in trait methods are always considered. A method is
/// registered only when its parent trait appears in `trait_names`:
/// - `display` ← Display
/// - `equal`   ← Equal
/// - `compare` ← Compare
/// - `hash`    ← Hash
pub(super) fn register_auto_derived_impls_for(
    checker: &mut TypeChecker,
    type_names: &[&str],
    trait_names: &[&str],
) {
    let dummy_span = Span {
        line: 0,
        col: 0,
        offset: 0,
    };
    let has_display = trait_names.contains(&"Display");
    let has_equal = trait_names.contains(&"Equal");
    let has_compare = trait_names.contains(&"Compare");
    let has_hash = trait_names.contains(&"Hash");
    for type_name in type_names {
        for trait_name in trait_names {
            checker
                .trait_impl_set
                .insert((intern(trait_name), intern(type_name)));
        }
        // Build method entries only for traits in `trait_names`.
        let mut methods: Vec<(&str, Type)> = Vec::with_capacity(4);
        if has_display {
            methods.push((
                "display",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::String)),
            ));
        }
        if has_equal {
            methods.push((
                "equal",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ));
        }
        if has_compare {
            methods.push((
                "compare",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Int),
                ),
            ));
        }
        if has_hash {
            methods.push((
                "hash",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::Int)),
            ));
        }
        for (method_name, method_type) in &methods {
            checker.method_table.insert(
                (intern(type_name), intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                    trait_name: None,
                    method_constraints: Vec::new(),
                },
            );
        }
    }
}

/// Run the type checker on a program. Returns a list of type errors (warnings).
pub fn check(program: &mut Program) -> Vec<TypeError> {
    let mut checker = TypeChecker::new();
    checker.check_program(program);
    checker.errors
}

// ── Persistent REPL type context ───────────────────────────────────

/// Persistent type-checking context for the REPL.
///
/// Holds a `TypeChecker` and its `TypeEnv` across REPL inputs so that
/// previously defined names (variables, functions, types) remain visible
/// to subsequent type-checking passes.
pub struct ReplTypeContext {
    checker: TypeChecker,
    env: TypeEnv,
}

impl Default for ReplTypeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplTypeContext {
    /// Create a new REPL type context with builtins and built-in traits
    /// already registered.
    pub fn new() -> Self {
        let mut checker = TypeChecker::new();
        let mut env = TypeEnv::new();

        // Register builtins in the type environment
        checker.register_builtins(&mut env);

        // Register built-in traits and auto-derived impls. Shared with
        // `check_program` so the REPL and `silt check` never drift on
        // derive policy.
        register_builtin_trait_impls(&mut checker);

        Self { checker, env }
    }

    /// Type-check a REPL input (one or more declarations/expressions) against
    /// the accumulated environment.  New bindings are persisted for future inputs.
    /// Returns any type errors from this input.
    pub fn check(&mut self, program: &mut Program) -> Vec<TypeError> {
        // Clear errors from the previous input
        self.checker.errors.clear();
        // G1: REPL inputs naturally redefine names across entries; only
        // duplicates WITHIN a single input should error.
        self.checker.top_level_names.clear();

        // Process imports. Round 56 item 4: we do NOT clear
        // `self.checker.imported_modules` here — REPL sessions
        // accumulate imports across inputs, so `import list` typed in
        // one input stays in scope for subsequent inputs.
        for decl in &program.decls {
            if let Decl::Import(ImportTarget::Items(module, items), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    self.checker.imported_modules.insert(*module);
                    for item in items {
                        let qualified = intern(&format!("{module}.{item}"));
                        if let Some(scheme) = self.env.lookup(qualified).cloned() {
                            self.env.define(*item, scheme);
                        }
                    }
                } else {
                    self.checker.warning(
                        format!(
                            "unknown module '{module_str}'; imported items will not be type-checked"
                        ),
                        *span,
                    );
                }
            } else if let Decl::Import(ImportTarget::Alias(module, alias), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    self.checker.imported_modules.insert(*alias);
                    // See the parallel path in `check_program` (round 58)
                    // for the rationale — we mirror every qualified
                    // binding with the right prefix so methods registered
                    // outside `builtin_module_functions` (e.g. `list.sum`)
                    // are reachable under the alias.
                    let alias_str = resolve(*alias);
                    let prefix = format!("{module_str}.");
                    let to_alias: Vec<(Symbol, Scheme)> = self
                        .env
                        .bindings
                        .iter()
                        .filter_map(|(k, scheme)| {
                            let k_str = resolve(*k);
                            k_str.strip_prefix(&prefix).map(|suffix| {
                                (intern(&format!("{alias_str}.{suffix}")), scheme.clone())
                            })
                        })
                        .collect();
                    for (aliased, scheme) in to_alias {
                        self.env.define(aliased, scheme);
                    }
                } else {
                    self.checker.warning(
                        format!(
                            "unknown module '{module_str}'; aliased imports will not be type-checked"
                        ),
                        *span,
                    );
                }
            } else if let Decl::Import(ImportTarget::Module(module), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
                    self.checker.imported_modules.insert(*module);
                } else {
                    self.checker.warning(
                        format!(
                            "unknown module '{module_str}'; imported module will not be type-checked"
                        ),
                        *span,
                    );
                    // Minimal binding so `module.foo(...)` calls don't cascade
                    // into "undefined variable" errors downstream.
                    let placeholder = self.checker.fresh_var();
                    self.env.define(*module, Scheme::mono(placeholder));
                }
            }
        }

        // Register type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.checker.register_type_decl(td, &mut self.env);
            }
        }

        // Register trait declarations first so default-method synthesis
        // sees every TraitInfo before any TraitImpl is processed.
        for decl in &program.decls {
            if let Decl::Trait(t) = decl {
                self.checker.register_trait_decl(t);
            }
        }

        // Synthesize default method bodies into impls that omitted them.
        self.checker.synthesize_default_methods(&mut program.decls);

        // Register fn signatures and trait impls.
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.checker.register_fn_decl(f, &mut self.env);
                }
                Decl::TraitImpl(ti) => {
                    self.checker.register_trait_impl(ti, &mut self.env);
                }
                _ => {}
            }
        }

        // Process top-level let bindings
        for i in 0..program.decls.len() {
            if let Decl::Let {
                ref mut value,
                ref pattern,
                ref ty,
                span,
                ..
            } = program.decls[i]
            {
                let is_value = inference::is_syntactic_value(&value.kind);
                let val_ty = self.checker.infer_expr(value, &mut self.env);
                if let Some(te) = ty {
                    let declared = self
                        .checker
                        .resolve_type_expr(te, &mut std::collections::HashMap::new());
                    self.checker.unify(&val_ty, &declared, span);
                }
                let scheme = if is_value {
                    self.checker.generalize(&self.env, &val_ty)
                } else {
                    Scheme::mono(self.checker.apply(&val_ty))
                };
                if let PatternKind::Ident(name) = &pattern.kind {
                    // G1: duplicate top-level let binding within a single REPL input.
                    if self.checker.top_level_names.contains(name) {
                        self.checker.error(
                            format!(
                                "duplicate top-level definition of '{}'; names must be unique at module scope",
                                name
                            ),
                            span,
                        );
                    }
                    self.checker.top_level_names.insert(*name);
                    self.env.define(*name, scheme);
                } else {
                    self.checker
                        .bind_pattern(pattern, &val_ty, &mut self.env, span);
                }
            }
        }

        // Validate trait implementations
        self.checker.validate_trait_impls();

        // Check function bodies (skip recovery stubs per Option B).
        for i in 0..program.decls.len() {
            if let Decl::Fn(ref mut f) = program.decls[i]
                && !f.is_recovery_stub
            {
                self.checker.check_fn_body(f, &self.env);
            }
        }

        // Check trait impl method bodies
        for i in 0..program.decls.len() {
            if let Decl::TraitImpl(ref mut ti) = program.decls[i] {
                let target = ti.target_type;
                for j in 0..ti.methods.len() {
                    let method_name = ti.methods[j].name;
                    let key = intern(&format!("{target}.{method_name}"));
                    let constrained =
                        self.checker
                            .check_fn_body_with_name(&mut ti.methods[j], &self.env, key);
                    if let Some(ty) = constrained
                        && let Some(entry) =
                            self.checker.method_table.get_mut(&(target, method_name))
                    {
                        if !entry.method_constraints.is_empty() {
                            let remap = align_tyvars(&entry.method_type, &ty);
                            entry.method_constraints = entry
                                .method_constraints
                                .iter()
                                .filter_map(|(old_tv, trait_name)| {
                                    remap.get(old_tv).map(|&new_tv| (new_tv, *trait_name))
                                })
                                .collect();
                        }
                        entry.method_type = ty;
                    }
                }
            }
        }

        // Resolve deferred checks before reporting unresolved types.
        self.checker.finalize_deferred_checks();

        // Detect unresolved type variables and resolve remaining types
        self.checker.check_unresolved_let_types(program);
        self.checker.resolve_all_types(program);

        self.checker.errors.clone()
    }
}

/// Return a map of builtin qualified names to their type signature strings.
/// Used by the LSP to show type info in completions.
pub fn builtin_type_signatures() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut sigs = std::collections::HashMap::new();
    for (name, scheme) in &env.bindings {
        let name_str = resolve(*name);
        if name_str.contains('.') {
            let ty = checker.instantiate(scheme);
            sigs.insert(name_str, format!("{ty}"));
        }
    }
    sigs
}

/// Test-only introspection: collect the auto-derived trait-impl and
/// method registrations produced by the two init paths so the parity
/// test under `tests/trait_init_parity_tests.rs` can assert they agree.
///
/// Returns `(trait_impls, method_keys)` where:
/// - `trait_impls` is the set of `"Trait:Type"` pairs registered in
///   `trait_impl_set`.
/// - `method_keys` is the set of `"Type.method"` pairs in
///   `method_table`.
///
/// Stringifies the `Symbol` keys so test code doesn't need access to
/// the crate-private `Symbol`/`intern` types.
#[doc(hidden)]
pub fn __trait_init_fingerprint_check_program() -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    use std::collections::BTreeSet;
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    register_builtin_trait_impls(&mut checker);
    let trait_impls: BTreeSet<String> = checker
        .trait_impl_set
        .iter()
        .map(|(tr, ty)| format!("{}:{}", resolve(*tr), resolve(*ty)))
        .collect();
    let method_keys: BTreeSet<String> = checker
        .method_table
        .keys()
        .map(|(ty, m)| format!("{}.{}", resolve(*ty), resolve(*m)))
        .collect();
    (trait_impls, method_keys)
}

/// Test-only introspection: same as
/// `__trait_init_fingerprint_check_program` but runs the REPL init path
/// (`ReplTypeContext::new`).
#[doc(hidden)]
pub fn __trait_init_fingerprint_repl() -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    use std::collections::BTreeSet;
    let ctx = ReplTypeContext::new();
    let trait_impls: BTreeSet<String> = ctx
        .checker
        .trait_impl_set
        .iter()
        .map(|(tr, ty)| format!("{}:{}", resolve(*tr), resolve(*ty)))
        .collect();
    let method_keys: BTreeSet<String> = ctx
        .checker
        .method_table
        .keys()
        .map(|(ty, m)| format!("{}.{}", resolve(*ty), resolve(*m)))
        .collect();
    (trait_impls, method_keys)
}

/// Test-only introspection for the built-in trait declarations
/// (Display/Compare/Equal/Hash). Returns one tuple per registered
/// trait in the fixed order Display, Compare, Equal, Hash:
///
///   (trait_name, method_name, method_arity, return_type_string,
///    supertrait_args_count, default_method_bodies_count,
///    params_count, supertraits_count, param_where_clauses_count)
///
/// Used by `tests/typechecker_builtin_trait_registration_parity_tests.rs`
/// to lock the semantics of the round-61 dead-code collapse: the four
/// near-identical TraitInfo construction blocks were replaced with a
/// single parameterised helper, and this fingerprint proves the
/// before/after shapes are identical.
#[doc(hidden)]
pub fn __builtin_trait_registration_fingerprint()
-> Vec<(String, String, usize, String, usize, usize, usize, usize, usize)> {
    let mut checker = TypeChecker::new();
    register_builtin_trait_impls(&mut checker);
    let names = ["Display", "Compare", "Equal", "Hash"];
    let mut out = Vec::new();
    for name in names {
        let sym = intern(name);
        let info = checker
            .traits
            .get(&sym)
            .unwrap_or_else(|| panic!("built-in trait {name} not registered"));
        assert_eq!(
            info.methods.len(),
            1,
            "built-in trait {name} should have exactly one method, got {}",
            info.methods.len()
        );
        let (method_sym, method_ty) = &info.methods[0];
        let (arity, ret_str) = match method_ty {
            Type::Fun(params, ret) => (params.len(), format!("{ret:?}")),
            other => panic!("built-in trait {name} method type is not Fun: {other:?}"),
        };
        out.push((
            name.to_string(),
            resolve(*method_sym),
            arity,
            ret_str,
            info.supertrait_args.len(),
            info.default_method_bodies.len(),
            info.params.len(),
            info.supertraits.len(),
            info.param_where_clauses.len(),
        ));
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────────

/// Shared test helpers used by every submodule test suite in
/// `typechecker/`. Before the round-N dedupe, four near-identical
/// copies of `assert_no_errors` / `assert_has_error` / `check_errors`
/// lived in `mod.rs`, `inference.rs`, `exhaustiveness.rs`,
/// `resolve.rs`, and `builtins.rs` (~128 lines of duplication).
///
/// why: we picked the most-general signatures across those copies.
///   - `assert_has_error(input, expected)` — shortest param name
///     used in 3 of 4 copies; mod.rs used `expected_substring` but
///     the body is byte-identical.
///   - `check_errors` inlines `parse()` + `check()` (the mod.rs
///     copy split them into two helpers; the split had no external
///     callers, so we collapsed it).
///   - Panic messages are preserved in the dominant form
///     ("expected no type errors" / "expected error containing").
#[cfg(test)]
pub(super) mod test_helpers {
    use super::*;

    pub(super) fn check_errors(input: &str) -> Vec<TypeError> {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        check(&mut program)
    }

    pub(super) fn check_program(input: &str) -> Vec<TypeError> {
        check_errors(input)
    }

    pub(super) fn assert_no_errors(input: &str) {
        let errors = check_errors(input);
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

    pub(super) fn assert_has_error(input: &str, expected: &str) {
        let errors = check_errors(input);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod size_locks {
    //! Round 50 audit: after removing the never-read informational
    //! fields (`_name`, `_params`) from `EnumInfo`, `RecordInfo`, and
    //! `TraitInfo`, these assertions lock the struct sizes so that
    //! accidentally re-adding a purely informational field (which
    //! would bloat the typechecker HashMaps storing thousands of
    //! these per compilation) fails fast at test time.
    //!
    //! If you INTENTIONALLY add a field, update the expected size
    //! below. If the size changed because the underlying Vec/HashMap
    //! layout changed in a Rust release, that's also fine — bump the
    //! numbers once, and the lock continues to protect against
    //! accidental re-introduction of dead fields.
    use super::{EnumInfo, RecordInfo, TraitInfo};

    #[test]
    fn enum_info_size_locked() {
        assert_eq!(
            std::mem::size_of::<EnumInfo>(),
            72,
            "EnumInfo size changed — see module doc"
        );
    }

    #[test]
    fn record_info_size_locked() {
        assert_eq!(
            std::mem::size_of::<RecordInfo>(),
            24,
            "RecordInfo size changed — see module doc"
        );
    }

    #[test]
    fn trait_info_size_locked() {
        assert_eq!(
            std::mem::size_of::<TraitInfo>(),
            216,
            "TraitInfo size changed — see module doc"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;

    // ── Basic type inference ────────────────────────────────────────

    #[test]
    fn test_int_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42
  x
}
        "#,
        );
    }

    #[test]
    fn test_float_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 3.14
  x
}
        "#,
        );
    }

    #[test]
    fn test_string_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = "hello"
  x
}
        "#,
        );
    }

    #[test]
    fn test_bool_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = true
  x
}
        "#,
        );
    }

    #[test]
    fn test_arithmetic() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 + 2
  let y = x * 3
  y
}
        "#,
        );
    }

    #[test]
    fn test_comparison() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 < 2
  x
}
        "#,
        );
    }

    #[test]
    fn test_function_call() {
        assert_no_errors(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_shadowing() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
        "#,
        );
    }

    // ── List inference ──────────────────────────────────────────────

    #[test]
    fn test_list_inference() {
        assert_no_errors(
            r#"
fn main() {
  let xs = [1, 2, 3]
  xs
}
        "#,
        );
    }

    #[test]
    fn test_empty_list() {
        assert_no_errors(
            r#"
fn main() {
  let xs = []
  xs
}
        "#,
        );
    }

    // ── Tuple inference ─────────────────────────────────────────────

    #[test]
    fn test_tuple_inference() {
        assert_no_errors(
            r#"
fn main() {
  let pair = (1, "hello")
  pair
}
        "#,
        );
    }

    // ── Lambda inference ────────────────────────────────────────────

    #[test]
    fn test_lambda() {
        assert_no_errors(
            r#"
fn main() {
  let double = fn(x) { x * 2 }
  double(5)
}
        "#,
        );
    }

    // ── Enum types ──────────────────────────────────────────────────

    #[test]
    fn test_enum_type() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  area(Circle(5.0))
}
        "#,
        );
    }

    // ── Record types ────────────────────────────────────────────────

    #[test]
    fn test_record_type() {
        assert_no_errors(
            r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  u.name
}
        "#,
        );
    }

    #[test]
    fn test_record_update() {
        assert_no_errors(
            r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  let u2 = birthday(u)
  u2.age
}
        "#,
        );
    }

    // ── Match exhaustiveness ────────────────────────────────────────

    #[test]
    fn test_match_exhaustive_with_wildcard() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    _ -> "other"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#,
        );
    }

    #[test]
    fn test_match_exhaustive_all_variants() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    Rect(w, h) -> "rect"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#,
        );
    }

    #[test]
    fn test_match_non_exhaustive() {
        assert_has_error(
            r#"
type Color {
  Red,
  Green,
  Blue,
}

fn name(c) {
  match c {
    Red -> "red"
    Green -> "green"
  }
}

fn main() {
  name(Red)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_non_exhaustive_nested_option() {
        // The new Maranget algorithm catches nested patterns.
        // Matching Ok(Some(x)) and Err(e) misses Ok(None).
        assert_has_error(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_nested_option() {
        // Full coverage of nested Option inside Result.
        assert_no_errors(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> 0
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
        "#,
        );
    }

    #[test]
    fn test_match_non_exhaustive_bool_in_tuple() {
        // Tuple of bools: (true, true) and (false, false) misses mixed cases.
        assert_has_error(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both"
    (false, false) -> "neither"
  }
}
fn main() { check((true, true)) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_bool_tuple() {
        assert_no_errors(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both true"
    (true, false) -> "first true"
    (false, _) -> "first false"
  }
}
fn main() { check((true, true)) }
        "#,
        );
    }

    // ── Generic types ───────────────────────────────────────────────

    #[test]
    fn test_option_some_none() {
        assert_no_errors(
            r#"
fn main() {
  let x = Some(42)
  let y = None
  match x {
    Some(n) -> n
    None -> 0
  }
}
        "#,
        );
    }

    #[test]
    fn test_result_ok_err() {
        assert_no_errors(
            r#"
fn main() {
  let x = Ok(42)
  match x {
    Ok(n) -> n
    Err(e) -> 0
  }
}
        "#,
        );
    }

    // ── Question mark operator ──────────────────────────────────────

    #[test]
    fn test_question_mark() {
        assert_no_errors(
            r#"
fn process(x) {
  let val = Ok(x)?
  Ok(val * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── When guard (type narrowing) ─────────────────────────────────

    #[test]
    fn test_when_guard() {
        assert_no_errors(
            r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  Ok(value * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── Boolean when guard ────────────────────────────────────────────

    #[test]
    fn test_when_bool_guard() {
        assert_no_errors(
            r#"
fn check(n) {
  when n > 0 else {
    return "not positive"
  }
  "positive"
}

fn main() {
  check(5)
}
        "#,
        );
    }

    #[test]
    fn test_when_bool_mixed_with_pattern_guard() {
        assert_no_errors(
            r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  when value > 0 else {
    return Err("must be positive")
  }
  Ok(value * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── Pipe operator ───────────────────────────────────────────────

    #[test]
    fn test_pipe_operator() {
        assert_no_errors(
            r#"
import list
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
        "#,
        );
    }

    // ── String interpolation ────────────────────────────────────────

    #[test]
    fn test_string_interpolation() {
        assert_no_errors(
            r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
        "#,
        );
    }

    // ── Trait implementation ────────────────────────────────────────

    #[test]
    fn test_trait_impl() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn main() {
  let s = Circle(5.0)
  s.display()
}
        "#,
        );
    }

    // ── Map literal ─────────────────────────────────────────────────

    #[test]
    fn test_map_literal() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  m
}
        "#,
        );
    }

    // ── Single-expression function ──────────────────────────────────

    #[test]
    fn test_single_expr_fn() {
        assert_no_errors(
            r#"
fn square(x) = x * x
fn add(a, b) = a + b

fn main() {
  add(square(3), square(4))
}
        "#,
        );
    }

    // ── Integration test programs ───────────────────────────────────

    #[test]
    fn test_fizzbuzz_program() {
        assert_no_errors(
            r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  let results = [
    fizzbuzz(1),
    fizzbuzz(3),
    fizzbuzz(5),
    fizzbuzz(15),
  ]
  results
}
        "#,
        );
    }

    #[test]
    fn test_closures_and_higher_order() {
        assert_no_errors(
            r#"
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
        "#,
        );
    }

    #[test]
    fn test_error_handling_pipeline() {
        assert_no_errors(
            r#"
import list
import string
import int
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when let Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when let Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when let Ok(port) = port_result else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}

fn main() {
  match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }

  match parse_config("host=localhost") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }
}
        "#,
        );
    }

    #[test]
    fn test_match_with_guards() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  [classify(-5), classify(0), classify(42)]
}
        "#,
        );
    }

    // ── Let-polymorphism ────────────────────────────────────────────

    #[test]
    fn test_let_polymorphism() {
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  a
}
        "#,
        );
    }

    // ── Unification error ───────────────────────────────────────────

    #[test]
    fn test_type_mismatch_in_binary_op() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 + "hello"
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_bool_op_type_mismatch() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 && true
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Range ───────────────────────────────────────────────────────

    #[test]
    fn test_range_expression() {
        assert_no_errors(
            r#"
fn main() {
  let r = 1..10
  r
}
        "#,
        );
    }

    // ── Exhaustiveness: guards don't count as covering ──────────────

    #[test]
    fn test_match_guards_with_catch_all() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  classify(5)
}
        "#,
        );
    }

    // ── Severity tests ─────────────────────────────────────────────

    #[test]
    fn test_type_error_has_error_severity() {
        // A type mismatch should produce Severity::Error.
        //
        // LATENT fix (audit round 36): previously this test only asserted that
        // *some* error with Error severity existed — any unrelated diagnostic
        // with Error severity would satisfy it. Narrow the lock to the specific
        // Int/String mismatch under test: find the diagnostic whose message
        // mentions both "Int" and "String" and assert IT has Error severity.
        // Per the "test must fail on a mutated source" rule for weak-lock
        // strengthenings: if the typechecker regressed to produce the Int/String
        // mismatch as a Warning, this strengthened assertion would fail where
        // the old `any()` check would still pass due to unrelated errors.
        let errors = check_errors(
            r#"
            fn main() {
                let x: Int = "hello"
                x
            }
        "#,
        );
        assert!(!errors.is_empty());
        let mismatch = errors
            .iter()
            .find(|e| e.message.contains("Int") && e.message.contains("String"))
            .unwrap_or_else(|| {
                panic!(
                    "expected an error mentioning both Int and String, got: {:?}",
                    errors.iter().map(|e| &e.message).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            mismatch.severity,
            Severity::Error,
            "Int/String mismatch must be Error severity, got {:?} for message {:?}",
            mismatch.severity,
            mismatch.message
        );
    }

    #[test]
    fn test_valid_program_no_errors() {
        let errors = check_errors(
            r#"
            fn main() {
                let x = 42
                x + 1
            }
        "#,
        );
        let hard_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(hard_errors.is_empty());
    }

    #[test]
    fn test_trait_impl_validates_methods() {
        // Complete impl should have no errors about missing methods
        let errors = check_program(
            r#"
            trait Greet {
                fn greet(self) -> String {
                    "hello"
                }
            }
            trait Greet for User {
                fn greet(self) -> String {
                    "hi"
                }
            }
            type User { name: String }
            fn main() { 0 }
        "#,
        );
        let trait_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("missing method"))
            .collect();
        assert!(
            trait_errors.is_empty(),
            "unexpected trait errors: {:?}",
            trait_errors
        );
    }

    #[test]
    fn test_trait_impl_missing_method() {
        // Both trait methods are abstract (no body) so omitting `detail`
        // in the impl is genuinely missing — not silently filled in by a
        // default. With the default-method feature, a method with a body
        // would be synthesized into the impl rather than reported.
        let errors = check_program(
            r#"
            trait Showable {
                fn show(self) -> String
                fn detail(self) -> String
            }
            trait Showable for Item {
                fn show(self) -> String { "item" }
            }
            type Item { name: String }
            fn main() { 0 }
        "#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("missing method") && e.message.contains("detail"))
        );
    }

    #[test]
    fn test_trait_impl_unknown_trait() {
        let errors = check_program(
            r#"
            trait Nonexistent for Thing {
                fn foo(self) -> Int { 0 }
            }
            type Thing { x: Int }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("not declared")));
    }

    #[test]
    fn test_builtin_display_trait_exists() {
        // Implementing Display should not produce "trait not declared" error
        let errors = check_program(
            r#"
            type Color { Red, Blue }
            trait Display for Color {
                fn display(self) -> String {
                    match self {
                        Red -> "red"
                        Blue -> "blue"
                    }
                }
            }
            fn main() { 0 }
        "#,
        );
        let undeclared: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared"))
            .collect();
        assert!(
            undeclared.is_empty(),
            "Display should be a built-in trait: {:?}",
            undeclared
        );
    }

    #[test]
    fn test_where_unknown_trait_warning() {
        let errors = check_program(
            r#"
            fn show(x) where x: Nonexistent {
                x
            }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("Nonexistent")));
    }

    #[test]
    fn test_where_constraint_satisfied() {
        // Should produce no errors about constraints
        let errors = check_errors(
            r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            type Color { Red, Blue }
            trait Showable for Color {
                fn show(self) -> String { "color" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                display(Red)
            }
        "#,
        );
        let constraint_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("does not implement"))
            .collect();
        assert!(
            constraint_errors.is_empty(),
            "unexpected: {:?}",
            constraint_errors
        );
    }

    #[test]
    fn test_where_constraint_violated() {
        // GAP (round 17 F4): the previous test only bound `where x: Showable`
        // on `x` which never referenced a valid type variable — the
        // constraint-introduction check fired with a suggestion string
        // that happened to contain "Showable", and the test's disjunctive
        // assertion (`contains("does not implement") || contains("Showable")`)
        // matched the wrong branch. It was green against a codebase that
        // completely dropped the "does not implement" check.
        //
        // Pin the real path: declare `display` with a proper typed
        // parameter `x: a where a: Showable`, implement Showable for Int
        // only, then call `display("text")`. Int satisfies; String does
        // not. Must now produce "type 'String' does not implement trait
        // 'Showable'".
        let errors = check_errors(
            r#"
            trait Showable { fn show(self) -> String }
            trait Showable for Int { fn show(self) -> String { "int" } }
            fn display(x: a) -> String where a: Showable { x.show() }
            fn main() { display("text") }
        "#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("does not implement")
                    && e.message.contains("Showable")),
            "expected 'does not implement trait Showable', got: {errors:?}"
        );
    }

    // ── Record types with generic fields (List, Map) ───────────────

    #[test]
    fn test_record_with_list_field() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  b.name
}
        "#,
        );
    }

    #[test]
    fn test_record_with_map_field() {
        assert_no_errors(
            r#"
type Config {
  data: Map,
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  c.data
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_and_map_fields() {
        assert_no_errors(
            r#"
type Config {
  values: Map,
  errors: List,
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  c.values
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_field_access() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  b.items
}
        "#,
        );
    }

    // ── Tests for newly registered builtins ────────────────────────

    #[test]
    fn test_list_module_builtins() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [1, 2, 3]
  let ys = list.append(xs, 4)
  let zs = list.concat(xs, ys)
  let head = list.head(xs)
  let tail = list.tail(xs)
  let last = list.last(xs)
  let rev = list.reverse(xs)
  let sorted = list.sort(xs)
  let has = list.contains(xs, 2)
  let n = list.length(xs)
  let taken = list.take(xs, 2)
  let dropped = list.drop(xs, 1)
  let got = list.get(xs, 0)
  let pairs = list.enumerate(xs)
  n
}
        "#,
        );
    }

    #[test]
    fn test_string_module_builtins() {
        assert_no_errors(
            r#"
import string
fn main() {
  let s = "hello world"
  let upper = string.to_upper(s)
  let lower = string.to_lower(s)
  let n = string.length(s)
  let starts = string.starts_with(s, "hello")
  let ends = string.ends_with(s, "world")
  let chars = string.chars(s)
  let repeated = string.repeat(s, 3)
  let idx = string.index_of(s, "world")
  let sliced = string.slice(s, 0, 5)
  let replaced = string.replace(s, "world", "there")
  n
}
        "#,
        );
    }

    #[test]
    fn test_float_module_builtins() {
        assert_no_errors(
            r#"
import float
fn main() {
  let a = 3.14
  let b = 2.71
  let mn = float.min(a, b)
  let mx = float.max(a, b)
  let parsed = float.parse("3.14")
  let rounded = float.round(a)
  let ceiled = float.ceil(a)
  let floored = float.floor(a)
  let abs = float.abs(a)
  rounded
}
        "#,
        );
    }

    #[test]
    fn test_int_module_builtins() {
        assert_no_errors(
            r#"
import int
fn main() {
  let a = 5
  let b = 3
  let mn = int.min(a, b)
  let mx = int.max(a, b)
  let f = int.to_float(a)
  f
}
        "#,
        );
    }

    #[test]
    fn test_map_module_builtins() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let got = map.get(m, "a")
  let updated = map.set(m, "c", 3)
  let deleted = map.delete(m, "a")
  let ks = map.keys(m)
  let vs = map.values(m)
  let merged = map.merge(m, #{ "c": 3 })
  ks
}
        "#,
        );
    }

    #[test]
    fn test_io_module_builtins() {
        assert_no_errors(
            r#"
import io
fn main() {
  let result = io.read_file("test.txt")
  let args = io.args()
  args
}
        "#,
        );
    }

    #[test]
    fn test_option_module_builtins() {
        assert_no_errors(
            r#"
import option
fn main() {
  let opt = Some(42)
  let is_s = option.is_some(opt)
  let is_n = option.is_none(opt)
  let val = option.unwrap_or(opt, 0)
  let mapped = option.map(opt, fn(x) { x + 1 })
  let res = option.to_result(opt, "no value")
  val
}
        "#,
        );
    }

    #[test]
    fn test_result_module_builtins() {
        assert_no_errors(
            r#"
import result
fn main() {
  let r = Ok(42)
  let is_ok = result.is_ok(r)
  let is_err = result.is_err(r)
  is_ok
}
        "#,
        );
    }

    #[test]
    fn test_higher_order_builtins() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [[1, 2], [3, 4], [5]]
  let flat = list.flatten(xs)
  let zipped = list.zip([1, 2, 3], ["a", "b", "c"])
  let sorted = list.sort_by([3, 1, 2], fn(x) { x })
  flat
}
        "#,
        );
    }

    #[test]
    fn test_len_accepts_string_and_map() {
        assert_no_errors(
            r#"
import list
import string
import map
fn main() {
  let list_len = list.length([1, 2, 3])
  let str_len = string.length("hello")
  let map_len = map.length(#{ "a": 1 })
  list_len + str_len + map_len
}
        "#,
        );
    }

    #[test]
    fn test_assert_ne_builtin() {
        assert_no_errors(
            r#"
import test
fn main() {
  test.assert_ne(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_channel_new_no_type_error() {
        assert_no_errors(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.close(ch)
  ch
}
        "#,
        );
    }

    #[test]
    fn test_channel_send_mixed_types_is_error() {
        assert_has_error(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.send(ch, "hello")
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_channel_receive_constrains_element_type() {
        assert_no_errors(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let result = channel.receive(ch)
  result
}
            "#,
        );
    }

    #[test]
    fn test_task_spawn_no_type_error() {
        assert_no_errors(
            r#"
import task
fn main() {
  let h = task.spawn(fn() { 42 })
  let result = task.join(h)
  result
}
        "#,
        );
    }

    #[test]
    fn test_map_length_no_type_error() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let n = map.length(m)
  n
}
        "#,
        );
    }

    // ── Type narrowing after when/pattern match ────────────────────

    #[test]
    fn test_when_some_narrows_inner_type() {
        // After `when let Some(x) = opt`, x should have the inner type (Int)
        assert_no_errors(
            r#"
fn get_value(opt) {
  when let Some(x) = opt else {
    return 0
  }
  x + 1
}

fn main() {
  get_value(Some(42))
}
            "#,
        );
    }

    #[test]
    fn test_when_ok_narrows_inner_type() {
        // After `when let Ok(v) = result`, v should have the ok type
        assert_no_errors(
            r#"
fn process(result) {
  when let Ok(v) = result else {
    return 0
  }
  v + 10
}

fn main() {
  process(Ok(5))
}
            "#,
        );
    }

    #[test]
    fn test_when_some_used_in_arithmetic() {
        assert_no_errors(
            r#"
fn double_or_zero(opt) {
  when let Some(n) = opt else {
    return 0
  }
  n * 2
}

fn main() {
  double_or_zero(Some(21))
}
            "#,
        );
    }

    // ── Generic type inference ──────────────────────────────────────

    #[test]
    fn test_generic_identity_multiple_types() {
        // A generic function used with multiple types
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  let c = identity(true)
  a + 1
}
            "#,
        );
    }

    #[test]
    fn test_nested_generic_list_of_options() {
        // List<Option<Int>> — nested generic type
        assert_no_errors(
            r#"
fn main() {
  let xs = [Some(1), Some(2), None]
  xs
}
            "#,
        );
    }

    #[test]
    fn test_generic_function_returning_generic() {
        assert_no_errors(
            r#"
fn wrap(x) {
  Some(x)
}

fn main() {
  let a = wrap(42)
  let b = wrap("hello")
  match a {
    Some(n) -> n
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_generic_pair_function() {
        assert_no_errors(
            r#"
fn make_pair(a, b) {
  (a, b)
}

fn main() {
  let p1 = make_pair(1, "hello")
  let p2 = make_pair(true, 3.14)
  p1
}
            "#,
        );
    }

    // ── Recursive functions ─────────────────────────────────────────

    #[test]
    fn test_recursive_function() {
        assert_no_errors(
            r#"
fn factorial(n) {
  match n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

fn main() {
  factorial(5)
}
            "#,
        );
    }

    #[test]
    fn test_recursive_list_function() {
        assert_no_errors(
            r#"
import list
fn sum(xs) {
  match list.head(xs) {
    None -> 0
    Some(h) -> h + sum(list.tail(xs))
  }
}

fn main() {
  sum([1, 2, 3])
}
            "#,
        );
    }

    // ── More exhaustiveness checking ────────────────────────────────

    #[test]
    fn test_match_int_without_wildcard_non_exhaustive() {
        // Matching on Int literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
  }
}

fn main() {
  describe(2)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_string_without_wildcard_non_exhaustive() {
        // Matching on String literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
  }
}

fn main() {
  greet("carol")
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_enum_one_variant_non_exhaustive() {
        // Matching only one variant of a multi-variant enum
        assert_has_error(
            r#"
type Shape {
  Circle(Float),
  Square(Float),
  Triangle(Float, Float),
}

fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}

fn main() {
  area(Circle(5.0))
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_all_guards_non_exhaustive() {
        // Guard arms don't count toward exhaustiveness
        assert_has_error(
            r#"
fn classify(n) {
  match n {
    x when x > 0 -> "positive"
    x when x < 0 -> "negative"
    x when x == 0 -> "zero"
  }
}

fn main() {
  classify(5)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_int_with_wildcard_exhaustive() {
        // Adding a wildcard makes int matching exhaustive
        assert_no_errors(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}

fn main() {
  describe(2)
}
            "#,
        );
    }

    #[test]
    fn test_match_string_with_wildcard_exhaustive() {
        assert_no_errors(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
    _ -> "hi stranger"
  }
}

fn main() {
  greet("carol")
}
            "#,
        );
    }

    // ── Error cases ─────────────────────────────────────────────────

    #[test]
    fn test_wrong_number_of_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2, 3)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_too_few_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_access_nonexistent_record_field() {
        assert_has_error(
            r#"
type Point { x: Int, y: Int }

fn main() {
  let p = Point { x: 1, y: 2 }
  p.z
}
            "#,
            "no field",
        );
    }

    #[test]
    fn test_undefined_variable() {
        assert_has_error(
            r#"
fn main() {
  let x = 1
  y + x
}
            "#,
            "undefined variable",
        );
    }

    #[test]
    fn test_arithmetic_on_string_and_int() {
        // String + Int should produce a type mismatch
        assert_has_error(
            r#"
fn main() {
  "hello" + 42
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_boolean_and_with_non_bool() {
        assert_has_error(
            r#"
fn main() {
  let x = "hello" && true
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_int_minus_string() {
        assert_has_error(
            r#"
fn main() {
  42 - "hello"
}
            "#,
            "operator '-'",
        );
    }

    // ── Set type inference ──────────────────────────────────────────

    #[test]
    fn test_set_literal_inference() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[1, 2, 3]
  s
}
            "#,
        );
    }

    #[test]
    fn test_empty_set_literal() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[]
  s
}
            "#,
        );
    }

    #[test]
    fn test_set_of_strings() {
        assert_no_errors(
            r#"
fn main() {
  let s = #["hello", "world"]
  s
}
            "#,
        );
    }

    // ── Loop/recur ──────────────────────────────────────────────────

    #[test]
    fn test_loop_basic() {
        assert_no_errors(
            r#"
fn main() {
  loop n = 0 {
    match n > 10 {
      true -> n
      false -> loop(n + 1)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_with_accumulator() {
        assert_no_errors(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1, acc + i)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_recur_arity_mismatch() {
        // loop has 2 bindings, recur has 1 argument.
        //
        // LATENT fix (audit round 36): previously the assertion was a 2-way
        // substring OR — `contains("binding") || contains("argument")` — so
        // many unrelated diagnostics could satisfy it (e.g. any diagnostic
        // that says "unused binding" or "argument count"). The real message
        // produced by typechecker/inference.rs is
        // `loop has N binding(s), but recur supplies M argument(s)`.
        //
        // Strengthening:
        //   - AND-chain specific phrases "loop has" && "recur supplies"
        //   - require Severity::Error (GAP #163 established recur arity
        //     mismatch is an Error, not a Warning)
        //
        // Per the "test must fail on a mutated source" rule for weak-lock
        // strengthenings: if the message were reworded, or if the emitter
        // regressed to `self.warning(...)` instead of `self.error(...)`,
        // this strengthened check would fail where the old OR-substring
        // check could still pass. The current code passes both — this is a
        // correct-just-under-locked scenario, so the strengthening is valid.
        let errors = check_program(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1)
    }
  }
}
            "#,
        );
        let recur_err = errors
            .iter()
            .find(|e| e.message.contains("loop has") && e.message.contains("recur supplies"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a recur arity diagnostic containing both \"loop has\" and \
                     \"recur supplies\", got: {:?}",
                    errors.iter().map(|e| &e.message).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            recur_err.severity,
            Severity::Error,
            "recur arity mismatch must be Error severity (GAP #163), got {:?} for message {:?}",
            recur_err.severity,
            recur_err.message
        );
    }

    // ── Trait system edge cases ─────────────────────────────────────

    #[test]
    fn test_trait_impl_with_wrong_method_signature() {
        // Both trait methods are declared abstract (no body) so the impl
        // genuinely owes both. Methods with default bodies are now
        // synthesized into impls rather than reported as missing.
        let errors = check_program(
            r#"
trait Describable {
  fn describe(self) -> String
  fn summary(self) -> String
}

type Widget { label: String }

trait Describable for Widget {
  fn describe(self) -> String { "widget" }
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("missing method")),
            "expected missing method error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_trait_unknown_in_impl() {
        assert_has_error(
            r#"
type Foo { x: Int }

trait DoesNotExist for Foo {
  fn bar(self) -> Int { 0 }
}

fn main() { 0 }
            "#,
            "not declared",
        );
    }

    #[test]
    fn test_where_clause_unknown_trait() {
        let errors = check_program(
            r#"
fn do_thing(x) where x: FakeTrait {
  x
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("FakeTrait")),
            "expected unknown trait error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multiple_trait_impls_for_same_type() {
        // Implementing two different traits for the same type should be fine
        let errors = check_program(
            r#"
trait Printable {
  fn print(self) -> String { "default" }
}

trait Serializable {
  fn serialize(self) -> String { "default" }
}

type Item { name: String }

trait Printable for Item {
  fn print(self) -> String { "item" }
}

trait Serializable for Item {
  fn serialize(self) -> String { "serialized" }
}

fn main() { 0 }
            "#,
        );
        // Should not produce "not declared" or "missing method" errors
        let bad_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared") || e.message.contains("missing method"))
            .collect();
        assert!(
            bad_errors.is_empty(),
            "unexpected trait errors: {:?}",
            bad_errors
        );
    }

    // ── Ascription (as) ─────────────────────────────────────────────

    #[test]
    fn test_valid_ascription() {
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
    fn test_ascription_string() {
        assert_no_errors(
            r#"
fn main() {
  let s = "hello" as String
  s
}
            "#,
        );
    }

    #[test]
    fn test_ascription_incompatible_type() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 as String
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Import-dependent type checking ──────────────────────────────

    #[test]
    fn test_string_module_split() {
        assert_no_errors(
            r#"
import string
fn main() {
  let parts = string.split("a,b,c", ",")
  parts
}
            "#,
        );
    }

    #[test]
    fn test_list_map_and_filter() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [1, 2, 3, 4, 5]
  let doubled = list.map(xs, fn(x) { x * 2 })
  let evens = list.filter(xs, fn(x) { x > 2 })
  doubled
}
            "#,
        );
    }

    #[test]
    fn test_map_get_returns_option() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let result = map.get(m, "a")
  match result {
    Some(v) -> v
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_chained_module_calls() {
        assert_no_errors(
            r#"
import string
import list
fn main() {
  let s = "Hello World"
  let result = s
    |> string.to_lower
    |> string.split(" ")
    |> list.length
  result
}
            "#,
        );
    }

    // ── Additional edge cases ───────────────────────────────────────

    #[test]
    fn test_nested_match_exhaustive() {
        // Nested Result<Option<Int>> fully covered
        assert_no_errors(
            r#"
fn process(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> -1
    Err(_) -> -2
  }
}

fn main() {
  process(Ok(Some(42)))
}
            "#,
        );
    }

    #[test]
    fn test_enum_match_all_variants_exhaustive() {
        assert_no_errors(
            r#"
type Direction {
  North,
  South,
  East,
  West,
}

fn to_string(d) {
  match d {
    North -> "north"
    South -> "south"
    East -> "east"
    West -> "west"
  }
}

fn main() {
  to_string(North)
}
            "#,
        );
    }

    #[test]
    fn test_record_update_type_checks() {
        assert_no_errors(
            r#"
type Config {
  host: String,
  port: Int,
}

fn main() {
  let c = Config { host: "localhost", port: 8080 }
  let c2 = c.{ port: 9090 }
  c2.host
}
            "#,
        );
    }

    #[test]
    fn test_question_mark_on_non_result() {
        // Using ? on a non-Result/Option type should error
        assert_has_error(
            r#"
fn main() -> Result {
  let x = 42?
  x
}
            "#,
            "requires Result or Option",
        );
    }

    // ── Unification unit tests ─────────────────────────────────────

    #[test]
    fn test_unify_occurs_check() {
        // Unifying Var(0) with List(Var(0)) should produce an infinite type error
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        let list_of_var = Type::List(Box::new(var.clone()));
        tc.unify(&var, &list_of_var, Span::new(0, 0));
        assert!(
            !tc.errors.is_empty(),
            "occurs check should produce an error"
        );
        assert!(
            tc.errors[0].message.contains("infinite type"),
            "expected 'infinite type' error, got: {}",
            tc.errors[0].message
        );
    }

    #[test]
    fn test_unify_function_arity_mismatch() {
        // Unifying Function([Int], Int) with Function([Int, Int], Int) should error
        let mut tc = TypeChecker::new();
        let fn1 = Type::Fun(vec![Type::Int], Box::new(Type::Int));
        let fn2 = Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int));
        tc.unify(&fn1, &fn2, Span::new(0, 0));
        assert!(
            !tc.errors.is_empty(),
            "function arity mismatch should produce an error"
        );
        assert!(
            tc.errors[0].message.contains("expects") && tc.errors[0].message.contains("argument"),
            "expected arity diagnostic, got: {}",
            tc.errors[0].message
        );
    }

    #[test]
    fn test_unify_basic_var_with_int() {
        // Unifying Var(0) with Int should map Var(0) -> Int
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        tc.unify(&var, &Type::Int, Span::new(0, 0));
        assert!(tc.errors.is_empty(), "basic unification should not error");
        let resolved = tc.apply(&var);
        assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
    }

    #[test]
    fn test_unify_transitive() {
        // Unify Var(0) with Var(1), then Var(1) with String.
        // Resolving Var(0) should yield String.
        let mut tc = TypeChecker::new();
        let var0 = tc.fresh_var(); // Type::Var(0)
        let var1 = tc.fresh_var(); // Type::Var(1)
        tc.unify(&var0, &var1, Span::new(0, 0));
        tc.unify(&var1, &Type::String, Span::new(0, 0));
        assert!(
            tc.errors.is_empty(),
            "transitive unification should not error"
        );
        let resolved = tc.apply(&var0);
        assert_eq!(
            resolved,
            Type::String,
            "Var(0) should transitively resolve to String"
        );
    }

    #[test]
    fn test_unify_list() {
        // Unifying List(Var(0)) with List(Int) should resolve Var(0) to Int
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        let list_var = Type::List(Box::new(var.clone()));
        let list_int = Type::List(Box::new(Type::Int));
        tc.unify(&list_var, &list_int, Span::new(0, 0));
        assert!(tc.errors.is_empty(), "list unification should not error");
        let resolved = tc.apply(&var);
        assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
    }

    #[test]
    fn test_comparison_float_extfloat() {
        // Comparing Float with ExtFloat (e.g. result of division) should succeed
        // and produce Bool, not a unification error.
        assert_no_errors(
            r#"
fn main() {
  let x = 10.0 / 3.0
  let result = x == 1.0
  result
}
            "#,
        );
    }
}
