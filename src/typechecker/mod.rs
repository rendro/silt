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

pub(super) use std::collections::{BTreeSet, HashMap};

pub(super) use crate::ast::*;
pub(super) use crate::intern::{Symbol, intern, resolve};
pub(super) use crate::lexer::Span;
pub(super) use crate::types::*;

pub use crate::types::{Scheme, Severity, TyVar, Type, TypeError};

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
    pub(super) _name: Symbol,
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
    pub(super) _name: Symbol,
    pub(super) _params: Vec<Symbol>,
    pub(super) fields: Vec<(Symbol, Type)>,
}

/// Information about a declared trait.
#[derive(Debug, Clone)]
pub(super) struct TraitInfo {
    pub(super) _name: Symbol,
    pub(super) methods: Vec<(Symbol, Type)>,
}

/// A registered trait method implementation (new trait system).
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
            errors: Vec::new(),
            loop_binding_types: None,
            active_constraints: HashMap::new(),
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
                    self.error(format!("infinite type: ?{v} occurs in {t}"), span);
                } else {
                    self.subst[*v] = Some(t.clone());
                }
            }

            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    self.error(
                        format!(
                            "function arity mismatch: expected {} args, got {}",
                            p1.len(),
                            p2.len()
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
                    self.error(
                        format!(
                            "tuple length mismatch: expected {}, got {}",
                            a.len(),
                            b.len()
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
                    // Unify fields by name (symmetric check: error if either
                    // side has fields missing from the other).
                    for (name, t1) in f1 {
                        if let Some((_, t2)) = f2.iter().find(|(n, _)| n == name) {
                            self.unify(t1, t2, span);
                        } else {
                            self.error(format!("record is missing field '{name}'"), span);
                        }
                    }
                    for (name, _t2) in f2 {
                        if !f1.iter().any(|(n, _)| n == name) {
                            self.error(format!("record is missing field '{name}'"), span);
                        }
                    }
                }
            }

            // Record(name, fields) is compatible with Generic(name, args)
            (Type::Record(n1, f1), Type::Generic(n2, a2)) if n1 == n2 && !a2.is_empty() => {
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
                    self.error(format!("type mismatch: expected {n2}, got {n1}"), span);
                } else if a1.len() != a2.len() {
                    self.error(
                        format!(
                            "type argument count mismatch for {n1}: expected {}, got {}",
                            a1.len(),
                            a2.len()
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
                self.error(format!("type mismatch: expected {t2}, got {t1}"), span);
            }
        }
    }

    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme by quantifying over free variables
    /// not present in the environment.
    pub(super) fn generalize(&self, env: &TypeEnv, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let env_fvs = env.free_vars(self);
        let ty_fvs = free_vars_in(&ty);
        let vars: Vec<TyVar> = ty_fvs
            .into_iter()
            .filter(|v| !env_fvs.contains(v))
            .collect();
        Scheme {
            vars,
            ty,
            constraints: vec![],
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
        let constraints = scheme
            .constraints
            .iter()
            .map(|(v, trait_name)| match mapping.get(v) {
                Some(Type::Var(new_v)) => (*new_v, *trait_name),
                _ => (*v, *trait_name),
            })
            .collect();
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

    // ── Error reporting ─────────────────────────────────────────────

    pub(super) fn error(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            severity: Severity::Error,
        });
    }

    #[allow(dead_code)]
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

        // Register built-in traits
        {
            let display_self = self.fresh_var();
            self.traits.insert(
                intern("Display"),
                TraitInfo {
                    _name: intern("Display"),
                    methods: vec![(
                        intern("display"),
                        Type::Fun(vec![display_self], Box::new(Type::String)),
                    )],
                },
            );
        }
        {
            let compare_a = self.fresh_var();
            let compare_b = self.fresh_var();
            self.traits.insert(
                intern("Compare"),
                TraitInfo {
                    _name: intern("Compare"),
                    methods: vec![(
                        intern("compare"),
                        Type::Fun(vec![compare_a, compare_b], Box::new(Type::Int)),
                    )],
                },
            );
        }
        {
            let equal_a = self.fresh_var();
            let equal_b = self.fresh_var();
            self.traits.insert(
                intern("Equal"),
                TraitInfo {
                    _name: intern("Equal"),
                    methods: vec![(
                        intern("equal"),
                        Type::Fun(vec![equal_a, equal_b], Box::new(Type::Bool)),
                    )],
                },
            );
        }
        {
            let hash_self = self.fresh_var();
            self.traits.insert(
                intern("Hash"),
                TraitInfo {
                    _name: intern("Hash"),
                    methods: vec![(
                        intern("hash"),
                        Type::Fun(vec![hash_self], Box::new(Type::Int)),
                    )],
                },
            );
        }

        // Register builtin trait implementations for primitive types.
        // These allow where clauses like `where a: Equal` to resolve
        // when `a` is Int, String, Bool, etc.
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let primitive_types = ["Int", "Float", "Bool", "String", "()"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            macro_rules! make_trait_methods {
                ($self:expr) => {
                    vec![
                        (
                            "display",
                            Type::Fun(vec![$self.fresh_var()], Box::new(Type::String)),
                        ),
                        (
                            "equal",
                            Type::Fun(
                                vec![$self.fresh_var(), $self.fresh_var()],
                                Box::new(Type::Bool),
                            ),
                        ),
                        (
                            "compare",
                            Type::Fun(
                                vec![$self.fresh_var(), $self.fresh_var()],
                                Box::new(Type::Int),
                            ),
                        ),
                        (
                            "hash",
                            Type::Fun(vec![$self.fresh_var()], Box::new(Type::Int)),
                        ),
                    ]
                };
            }
            for type_name in &primitive_types {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(self);
                for (method_name, method_type) in &trait_methods {
                    self.method_table.insert(
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
            // L5: The VM's compare() (src/vm/arithmetic.rs) only supports
            // ordering for List/Range and primitives. Tuple, Map, and Set
            // are NOT orderable at runtime, so they only auto-derive
            // Equal/Hash/Display — not Compare.
            let non_ordering_traits = ["Equal", "Hash", "Display"];
            for type_name in &["List"] {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(self);
                for (method_name, method_type) in &trait_methods {
                    self.method_table.insert(
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
            // Tuple/Map/Set: Equal, Hash, Display only (no Compare).
            for type_name in &["Tuple", "Map", "Set"] {
                for trait_name in &non_ordering_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(self);
                for (method_name, method_type) in &trait_methods {
                    // Skip "compare" for these types.
                    if *method_name == "compare" {
                        continue;
                    }
                    self.method_table.insert(
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
            // B10: Option and Result auto-derive Equal, Hash, Display.
            // (Compare is not supported because ordering on Variants is
            // limited to same-name variants at runtime.) Both types wrap
            // generic parameters, but the auto-derived methods are stored
            // as polymorphic templates and instantiated at each call site.
            for type_name in &["Option", "Result"] {
                for trait_name in &non_ordering_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(self);
                for (method_name, method_type) in &trait_methods {
                    if *method_name == "compare" {
                        continue;
                    }
                    self.method_table.insert(
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

        // Process imports: register selective/aliased import names in the type environment
        for decl in &program.decls {
            if let Decl::Import(ImportTarget::Items(module, items), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
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
                    let names = crate::module::builtin_module_functions(&module_str)
                        .into_iter()
                        .chain(crate::module::builtin_module_constants(&module_str));
                    for func in names {
                        let qualified = intern(&format!("{module}.{func}"));
                        let aliased = intern(&format!("{alias}.{func}"));
                        if let Some(scheme) = env.lookup(qualified).cloned() {
                            env.define(aliased, scheme);
                        }
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

        // First pass: register all type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.register_type_decl(td, &mut env);
            }
        }

        // Second pass: register all function signatures, trait impls, and top-level lets
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.register_fn_decl(f, &mut env);
                }
                Decl::Trait(t) => {
                    self.register_trait_decl(t);
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
                    let declared =
                        self.resolve_type_expr(te, &mut std::collections::HashMap::new());
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

        // Narrow function schemes based on body constraints, then re-check
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
                    let remap =
                        align_tyvars(&original_scheme.ty, &new_scheme.ty);
                    for (old_tv, trait_name) in &original_scheme.constraints {
                        if let Some(&new_tv) = remap.get(old_tv)
                            && new_scheme.vars.contains(&new_tv)
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
                } else {
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
                            format!(
                                "duplicate variant '{}' in enum '{}'",
                                variant.name, td.name
                            ),
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

                    self.variant_to_enum.insert(variant.name, td.name);
                }

                self.enums.insert(
                    td.name,
                    EnumInfo {
                        _name: td.name,
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
                            format!(
                                "duplicate field '{}' in record type '{}'",
                                f.name, td.name
                            ),
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
                        _name: td.name,
                        _params: td.params.clone(),
                        fields: field_types.clone(),
                    },
                );

                // Register the record type name as a value so it can be
                // passed to json.parse: `json.parse(Employee, str)`.
                // The value is a TYPE DESCRIPTOR at runtime (represented
                // as `Value::RecordDescriptor(name)`), so its type must
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
                    let args: Vec<Type> =
                        td.params.iter().map(|p| param_vars[p].clone()).collect();
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
        match te {
            TypeExpr::Named(name) => {
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
            TypeExpr::Generic(name, args) => {
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
                        let expected_arity = self
                            .record_param_var_ids
                            .get(name)
                            .map(|v| v.len())
                            .or_else(|| self.enums.get(name).map(|e| e.params.len()));
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
                        }
                        Type::Generic(*name, resolved_args)
                    }
                }
            }
            TypeExpr::Tuple(elems) => {
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
            TypeExpr::Function(params, ret) => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, param_vars))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, param_vars);
                Type::Fun(param_types, Box::new(ret_type))
            }
            TypeExpr::SelfType => {
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
            let ty = if let Some(te) = &param.ty {
                self.resolve_type_expr(te, &mut param_map)
            } else {
                self.fresh_var()
            };
            param_types.push(ty);
        }

        let ret_type = if let Some(te) = &f.return_type {
            self.resolve_type_expr(te, &mut param_map)
        } else {
            self.fresh_var()
        };
        self.current_type_anno_span = prev_type_span;

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type));
        let mut scheme = self.generalize(env, &fn_type);

        // Resolve where clauses to (TyVar, trait_name) using param_map.
        // Type variables must be introduced via explicit type annotations in the signature.
        for (type_param, trait_name) in &f.where_clauses {
            if let Some(ty) = param_map.get(type_param) {
                let resolved = self.apply(ty);
                if let Type::Var(tv) = resolved {
                    scheme.constraints.push((tv, *trait_name));
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
        let self_var = self.fresh_var();
        let methods: Vec<(Symbol, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let mut param_map = HashMap::new();
                param_map.insert(intern("Self"), self_var.clone());
                let mut param_types = Vec::new();
                for param in &m.params {
                    let ty = if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &mut param_map)
                    } else {
                        self.fresh_var()
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

        self.traits.insert(
            t.name,
            TraitInfo {
                _name: t.name,
                methods,
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
            // Arity check for user-declared record/enum targets.
            let expected_arity = self
                .record_param_var_ids
                .get(&ti.target_type)
                .map(|v| v.len())
                .or_else(|| self.enums.get(&ti.target_type).map(|e| e.params.len()));
            if let Some(expected) = expected_arity
                && expected != ti.target_type_args.len()
            {
                let kind = if self.records.contains_key(&ti.target_type) {
                    "record"
                } else {
                    "enum"
                };
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
        for (type_param, trait_name) in &ti.where_clauses {
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
                let ty = if let Some(te) = &param.ty {
                    self.resolve_type_expr(te, &mut param_map)
                } else if i == 0 && matches!(&param.pattern.kind, PatternKind::Ident(n) if *n == self_sym) {
                    // Bare `self` parameter in a trait impl: type it as the
                    // target type so field/method accesses on `self` are
                    // properly checked against the impl's target.
                    self_type.clone()
                } else {
                    self.fresh_var()
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
            if let Some(existing) =
                self.method_table.get(&(ti.target_type, method.name))
                && !existing.is_auto_derived
                && let Some(existing_trait) = existing.trait_name
                && existing_trait != ti.trait_name
            {
                self.error(
                    format!(
                        "ambiguous method '{}' on type '{}': provided by traits {}, {}",
                        method.name,
                        ti.target_type,
                        existing_trait,
                        ti.trait_name
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
            for (type_param, trait_name) in &method.where_clauses {
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
                scheme.constraints.push((*tv, *trait_name));
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
        (Type::Set(o), Type::Set(n)) => align_tyvars_into(o, n, map),
        (Type::Channel(o), Type::Channel(n)) => align_tyvars_into(o, n, map),
        (Type::Tuple(o), Type::Tuple(n)) => {
            if o.len() == n.len() {
                for (a, b) in o.iter().zip(n.iter()) {
                    align_tyvars_into(a, b, map);
                }
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

        // Register built-in traits (mirrors check_program init)
        {
            let display_self = checker.fresh_var();
            checker.traits.insert(
                intern("Display"),
                TraitInfo {
                    _name: intern("Display"),
                    methods: vec![(
                        intern("display"),
                        Type::Fun(vec![display_self], Box::new(Type::String)),
                    )],
                },
            );
        }
        {
            let compare_a = checker.fresh_var();
            let compare_b = checker.fresh_var();
            checker.traits.insert(
                intern("Compare"),
                TraitInfo {
                    _name: intern("Compare"),
                    methods: vec![(
                        intern("compare"),
                        Type::Fun(vec![compare_a, compare_b], Box::new(Type::Int)),
                    )],
                },
            );
        }
        {
            let equal_a = checker.fresh_var();
            let equal_b = checker.fresh_var();
            checker.traits.insert(
                intern("Equal"),
                TraitInfo {
                    _name: intern("Equal"),
                    methods: vec![(
                        intern("equal"),
                        Type::Fun(vec![equal_a, equal_b], Box::new(Type::Bool)),
                    )],
                },
            );
        }
        {
            let hash_self = checker.fresh_var();
            checker.traits.insert(
                intern("Hash"),
                TraitInfo {
                    _name: intern("Hash"),
                    methods: vec![(
                        intern("hash"),
                        Type::Fun(vec![hash_self], Box::new(Type::Int)),
                    )],
                },
            );
        }

        // Register builtin trait implementations for primitive types.
        // Each insert uses a `Scheme` with the free vars quantified so
        // lookup sites can instantiate and get fresh type variables.
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let primitive_types = ["Int", "Float", "Bool", "String", "()"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            macro_rules! make_trait_methods {
                ($self:expr) => {
                    vec![
                        (
                            "display",
                            Type::Fun(vec![$self.fresh_var()], Box::new(Type::String)),
                        ),
                        (
                            "equal",
                            Type::Fun(
                                vec![$self.fresh_var(), $self.fresh_var()],
                                Box::new(Type::Bool),
                            ),
                        ),
                        (
                            "compare",
                            Type::Fun(
                                vec![$self.fresh_var(), $self.fresh_var()],
                                Box::new(Type::Int),
                            ),
                        ),
                        (
                            "hash",
                            Type::Fun(vec![$self.fresh_var()], Box::new(Type::Int)),
                        ),
                    ]
                };
            }
            for type_name in &primitive_types {
                for trait_name in &all_traits {
                    checker
                        .trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(checker);
                for (method_name, method_type) in &trait_methods {
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
            // L5: See check_program — the VM only supports ordering for
            // List/Range among collection types. Tuple/Map/Set get
            // Equal/Hash/Display but not Compare.
            let non_ordering_traits = ["Equal", "Hash", "Display"];
            for type_name in &["List"] {
                for trait_name in &all_traits {
                    checker
                        .trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(checker);
                for (method_name, method_type) in &trait_methods {
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
            for type_name in &["Tuple", "Map", "Set"] {
                for trait_name in &non_ordering_traits {
                    checker
                        .trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(checker);
                for (method_name, method_type) in &trait_methods {
                    if *method_name == "compare" {
                        continue;
                    }
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
            // B10: Option and Result auto-derive Equal, Hash, Display.
            for type_name in &["Option", "Result"] {
                for trait_name in &non_ordering_traits {
                    checker
                        .trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                let trait_methods: Vec<(&str, Type)> = make_trait_methods!(checker);
                for (method_name, method_type) in &trait_methods {
                    if *method_name == "compare" {
                        continue;
                    }
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

        // Process imports
        for decl in &program.decls {
            if let Decl::Import(ImportTarget::Items(module, items), span) = decl {
                let module_str = resolve(*module);
                if crate::module::is_builtin_module(&module_str) {
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
                    let names = crate::module::builtin_module_functions(&module_str)
                        .into_iter()
                        .chain(crate::module::builtin_module_constants(&module_str));
                    for func in names {
                        let qualified = intern(&format!("{module}.{func}"));
                        let aliased = intern(&format!("{alias}.{func}"));
                        if let Some(scheme) = self.env.lookup(qualified).cloned() {
                            self.env.define(aliased, scheme);
                        }
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
                if !crate::module::is_builtin_module(&module_str) {
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

        // Register function signatures, trait impls, and top-level lets
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.checker.register_fn_decl(f, &mut self.env);
                }
                Decl::Trait(t) => {
                    self.checker.register_trait_decl(t);
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

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse(input: &str) -> Program {
        let tokens = Lexer::new(input).tokenize().expect("lexer error");
        Parser::new(tokens).parse_program().expect("parse error")
    }

    fn check_errors(input: &str) -> Vec<TypeError> {
        let mut program = parse(input);
        check(&mut program)
    }

    fn check_program(input: &str) -> Vec<TypeError> {
        check_errors(input)
    }

    fn assert_no_errors(input: &str) {
        let errors = check_program(input);
        let hard_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        if !hard_errors.is_empty() {
            panic!(
                "expected no type errors, got:\n{}",
                hard_errors
                    .iter()
                    .map(|e| format!("  {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }

    fn assert_has_error(input: &str, expected_substring: &str) {
        let errors = check_program(input);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains(expected_substring)),
            "expected an error containing '{}', got: {:?}",
            expected_substring,
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

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
  Circle(Float)
  Rect(Float, Float)
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
  Circle(Float)
  Rect(Float, Float)
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
  Circle(Float)
  Rect(Float, Float)
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
  Red
  Green
  Blue
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
  when Ok(value) = Ok(x) else {
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
  when Ok(value) = Ok(x) else {
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
  Circle(Float)
  Rect(Float, Float)
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
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when Ok(port) = port_result else {
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
        // A type mismatch should produce Severity::Error
        let errors = check_errors(
            r#"
            fn main() {
                let x: Int = "hello"
                x
            }
        "#,
        );
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.severity == Severity::Error));
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
        let errors = check_program(
            r#"
            trait Showable {
                fn show(self) -> String { "default" }
                fn detail(self) -> String { "detail" }
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
        // After `when Some(x) = opt`, x should have the inner type (Int)
        assert_no_errors(
            r#"
fn get_value(opt) {
  when Some(x) = opt else {
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
        // After `when Ok(v) = result`, v should have the ok type
        assert_no_errors(
            r#"
fn process(result) {
  when Ok(v) = result else {
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
  when Some(n) = opt else {
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
  Circle(Float)
  Square(Float)
  Triangle(Float, Float)
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
            "type mismatch",
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
        // loop has 2 bindings, recur has 1 argument
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
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("binding") || e.message.contains("argument")),
            "expected recur arity warning, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Trait system edge cases ─────────────────────────────────────

    #[test]
    fn test_trait_impl_with_wrong_method_signature() {
        // Impl that is missing one of the required methods
        let errors = check_program(
            r#"
trait Describable {
  fn describe(self) -> String { "default" }
  fn summary(self) -> String { "summary" }
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
  North
  South
  East
  West
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
            tc.errors[0].message.contains("arity mismatch"),
            "expected 'arity mismatch' error, got: {}",
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
