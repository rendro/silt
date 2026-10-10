//! Shared type definitions for Silt's type system.
//!
//! This module contains the core type representations used by the type checker,
//! interpreter, and other parts of the compiler pipeline.

pub mod builtins;
pub mod canonical;

use std::collections::{BTreeMap, HashMap};

use crate::defs::{TraitId, TypeId};
use crate::intern::{Symbol, intern};

// ── Type representation ─────────────────────────────────────────────

/// A unique identifier for type variables.
pub type TyVar = usize;

/// An annotation variable inside its own declaration (see
/// [`Type::Rigid`]): the variable it is in the declaration's scheme, and
/// its name as written.
#[derive(Debug, Clone, Copy)]
pub struct RigidId {
    pub var: TyVar,
    pub name: Symbol,
}

impl PartialEq for RigidId {
    fn eq(&self, other: &Self) -> bool {
        self.var == other.var
    }
}

/// Tail of a row (record) type. Either closed (no extra fields) or
/// open with a unification variable that may bind to a record holding
/// the remaining fields. See `Type::AnonRecord`.
#[derive(Debug, Clone, PartialEq)]
pub enum RowTail {
    /// The record is closed: it has exactly the listed fields and no more.
    Closed,
    /// The record may have additional fields. The TyVar is a row variable
    /// that unification can bind to a record carrying the leftover fields.
    Var(TyVar),
    /// The record has the fields a row variable of an annotation stands
    /// for, inside the declaration that writes it: whatever they are,
    /// they are the same ones wherever the variable is written, and
    /// nothing in the declaration may assume what they are.
    Rigid(RigidId),
}

/// A record, enum or alias type, or a builtin type that has no variant of
/// its own in [`Type`] (`Option`, `time.Duration`): its definition, and
/// the name it is declared with, for display. Two refs are one type when
/// their ids are equal; two types of one name from two modules are not.
#[derive(Debug, Clone, Copy)]
pub struct TypeRef {
    pub id: TypeId,
    pub name: Symbol,
}

impl TypeRef {
    /// The builtin type `name`. Panics when no builtin type has that
    /// name.
    pub fn builtin(name: &str) -> TypeRef {
        let id = crate::defs::builtin_type_id(name)
            .unwrap_or_else(|| panic!("'{name}' is not a builtin type"));
        TypeRef {
            id,
            name: intern(name),
        }
    }

    /// Whether this is the builtin type `name`, and not a type of a
    /// module that has the same name.
    pub fn is_builtin(&self, name: &str) -> bool {
        crate::defs::builtin_type_id(name) == Some(self.id)
    }
}

#[cfg(test)]
impl TypeRef {
    /// A type of a module, named `name`, with an id no definition table
    /// hands out (unit tests that build types by hand).
    pub fn test(name: &str) -> TypeRef {
        let k = name
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
            % 100_000;
        TypeRef {
            id: TypeId(crate::defs::DefId(u32::MAX - 1 - k)),
            name: intern(name),
        }
    }
}

impl PartialEq for TypeRef {
    fn eq(&self, other: &TypeRef) -> bool {
        self.id == other.id
    }
}

impl Eq for TypeRef {}

impl std::hash::Hash for TypeRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl std::fmt::Display for TypeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// A trait, by its definition, and the name it is declared with, for
/// display. Two refs are one trait when their ids are equal.
#[derive(Debug, Clone, Copy)]
pub struct TraitKey {
    pub id: TraitId,
    pub name: Symbol,
}

impl TraitKey {
    /// The builtin trait `name`. Panics when no builtin trait has that
    /// name.
    pub fn builtin(name: &str) -> TraitKey {
        let id = crate::defs::builtin_trait_id(name)
            .unwrap_or_else(|| panic!("'{name}' is not a builtin trait"));
        TraitKey {
            id,
            name: intern(name),
        }
    }

    /// Whether this is the builtin trait `name`.
    pub fn is_builtin(&self, name: &str) -> bool {
        crate::defs::builtin_trait_id(name) == Some(self.id)
    }
}

impl PartialEq for TraitKey {
    fn eq(&self, other: &TraitKey) -> bool {
        self.id == other.id
    }
}

impl Eq for TraitKey {}

impl std::hash::Hash for TraitKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl std::fmt::Display for TraitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// The core type representation used during inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    String,
    Unit,
    /// A unification variable, to be resolved during inference.
    Var(TyVar),
    /// A type variable written in an annotation (`a` in `fn f(x: a) -> a`,
    /// `Self` in a trait's default method), inside the declaration that
    /// writes it: it stands for a type the declaration does not know, so
    /// it unifies with itself only. Outside the declaration the variable
    /// is quantified in the declaration's scheme (`RigidId::var`).
    Rigid(RigidId),
    /// Function type: param types -> return type.
    Fun(Vec<Type>, Box<Type>),
    /// Homogeneous list type.
    List(Box<Type>),
    /// Inclusive integer-range type produced by `a..b`. Nominally distinct
    /// from `List(T)` so annotations like `let r: Range(Int) = 1..10`
    /// succeed, but unifies bidirectionally with `List(T)` — Range is a
    /// zero-cost alias whose runtime representation is the same `Vec<Value>`
    /// as a List. Laziness is future work (tracked in docs/language/operators.md).
    Range(Box<Type>),
    /// Tuple type (fixed length, heterogeneous).
    Tuple(Vec<Type>),
    /// A named type with its arguments, like `Result(Int, String)`: an
    /// enum, a nominal record (its fields are the checker's), a builtin
    /// type.
    Generic(TypeRef, Vec<Type>),
    /// Map type: key type -> value type.
    Map(Box<Type>, Box<Type>),
    /// Set type: element type.
    Set(Box<Type>),
    /// Channel type: element type carried through the channel.
    Channel(Box<Type>),
    /// An error type used to allow inference to continue after errors.
    Error,
    /// A bottom type for expressions that never produce a value (return, panic).
    Never,
    /// Associated-type projection: `<receiver as trait_name>::assoc_name`.
    ///
    /// Two states:
    ///   - **Concrete receiver**: the canonicaliser reduces this to the
    ///     impl's binding for `assoc_name`. The reduction is the dispatch
    ///     oracle's only behaviour for projections.
    ///   - **Abstract receiver** (still a type variable, or another
    ///     unreduced AssocProj): stays as `AssocProj` and propagates
    ///     through inference. Two abstract `AssocProj`s unify iff they
    ///     have the same receiver, trait_name, and assoc_name.
    AssocProj {
        receiver: Box<Type>,
        trait_name: TraitKey,
        assoc_name: Symbol,
    },
    /// An anonymous structural record type (row-polymorphic capable).
    /// `{name: String, age: Int}` is closed; `{name: String, ...r}` has
    /// a row-tail variable. Field order is irrelevant for equality —
    /// `BTreeMap` gives stable ordering for canonicalisation/rendering.
    AnonRecord {
        fields: BTreeMap<Symbol, Type>,
        tail: RowTail,
    },
}

impl Type {
    /// The builtin type `name` with `args` (`Option(a)`, `time.Duration`).
    pub fn builtin(name: &str, args: Vec<Type>) -> Type {
        Type::Generic(TypeRef::builtin(name), args)
    }

    /// `Option(t)`.
    pub fn option(t: Type) -> Type {
        Type::builtin("Option", vec![t])
    }

    /// `Result(t, e)`.
    pub fn result(t: Type, e: Type) -> Type {
        Type::builtin("Result", vec![t, e])
    }

    /// `TypeOf(t)`: the type of the type `t` written as a value.
    pub fn type_of(t: Type) -> Type {
        Type::builtin(crate::defs::TYPE_OF, vec![t])
    }

    /// The named type this is (a record, an enum, an alias, a builtin
    /// with arguments); `None` for any other type.
    pub fn type_ref(&self) -> Option<TypeRef> {
        match self {
            Type::Generic(r, _) => Some(*r),
            _ => None,
        }
    }

    /// Render the types of one message, a record type by its name. Each
    /// named type is written as `qualify` says, given whether another
    /// type of the message has its name (`a.Pt` and `b.Pt`); `None`
    /// writes its declared name.
    pub fn show_all(
        types: &[&Type],
        qualify: impl Fn(TypeRef, bool) -> Option<String>,
    ) -> Vec<String> {
        let mut refs = Vec::new();
        for ty in types {
            ty.collect_refs(&mut refs);
        }
        let mut names: HashMap<TypeId, String> = HashMap::new();
        for r in &refs {
            let clash = refs.iter().any(|o| o.name == r.name && o.id != r.id);
            if !names.contains_key(&r.id)
                && let Some(name) = qualify(*r, clash)
            {
                names.insert(r.id, name);
            }
        }
        types
            .iter()
            .map(|ty| {
                Shown {
                    ty,
                    names: &names,
                    brief: true,
                }
                .to_string()
            })
            .collect()
    }

    /// Every named type `self` mentions.
    pub fn collect_refs(&self, out: &mut Vec<TypeRef>) {
        match self {
            Type::Generic(r, args) => {
                out.push(*r);
                for t in args {
                    t.collect_refs(out);
                }
            }
            Type::Fun(params, ret) => {
                for t in params {
                    t.collect_refs(out);
                }
                ret.collect_refs(out);
            }
            Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => t.collect_refs(out),
            Type::Map(k, v) => {
                k.collect_refs(out);
                v.collect_refs(out);
            }
            Type::Tuple(ts) => {
                for t in ts {
                    t.collect_refs(out);
                }
            }
            Type::AssocProj { receiver, .. } => receiver.collect_refs(out),
            Type::AnonRecord { fields, .. } => {
                for t in fields.values() {
                    t.collect_refs(out);
                }
            }
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Var(_)
            | Type::Rigid(_)
            | Type::Error
            | Type::Never => {}
        }
    }

    /// Whether this is the builtin type `name` (with any arguments).
    pub fn is_builtin(&self, name: &str) -> bool {
        self.type_ref().is_some_and(|r| r.is_builtin(name))
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            Shown {
                ty: self,
                names: &HashMap::new(),
                brief: false,
            }
        )
    }
}

/// A type rendered with some of its named types written otherwise than
/// by their declared names (see [`Type::show_apart`]).
struct Shown<'a> {
    ty: &'a Type,
    names: &'a HashMap<TypeId, String>,
    /// A record type is written by its name only.
    brief: bool,
}

impl Shown<'_> {
    fn of<'b>(&'b self, ty: &'b Type) -> Shown<'b> {
        Shown {
            ty,
            names: self.names,
            brief: self.brief,
        }
    }

    fn name(&self, ty: &TypeRef) -> String {
        self.names
            .get(&ty.id)
            .cloned()
            .unwrap_or_else(|| ty.name.to_string())
    }
}

impl std::fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.ty {
            Type::Int => write!(f, "Int"),
            Type::Float => write!(f, "Float"),
            Type::Bool => write!(f, "Bool"),
            Type::String => write!(f, "String"),
            Type::Unit => write!(f, "()"),
            // Type variables have no user-facing name at this point in
            // inference — rendering `?17` leaks an internal id. The
            // underscore matches silt's own "I don't care about this
            // type" convention in patterns and reads as "unknown type"
            // in diagnostics.
            Type::Var(_) => write!(f, "_"),
            Type::Rigid(r) => write!(f, "{}", r.name),
            Type::Fun(params, ret) => {
                // Match the parser's surface syntax `Fn(A, B) -> C` so
                // diagnostics render fn types in the same form users
                // wrote in annotations. Without the `Fn` prefix, the
                // render `(A, B) -> C` visually collides with silt's
                // tuple-type syntax `(A, B)`.
                write!(f, "Fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.of(p))?;
                }
                write!(f, ") -> {}", self.of(ret))
            }
            Type::List(inner) => write!(f, "List({})", self.of(inner)),
            Type::Range(inner) => write!(f, "Range({})", self.of(inner)),
            Type::Tuple(elems) => {
                write!(f, "(")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.of(e))?;
                }
                write!(f, ")")
            }
            Type::Generic(name, args) => {
                // `TypeOf(a)` is the internal lowering of a `type a`
                // parameter. Render it as `type a` so diagnostics use the
                // surface syntax the user wrote — never leak `TypeOf`.
                if name.is_builtin(crate::defs::TYPE_OF) && args.len() == 1 {
                    return write!(f, "type {}", self.of(&args[0]));
                }
                write!(f, "{}", self.name(name))?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.of(a))?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Type::Map(k, v) => write!(f, "Map({}, {})", self.of(k), self.of(v)),
            Type::Set(inner) => write!(f, "Set({})", self.of(inner)),
            Type::Channel(inner) => write!(f, "Channel({})", self.of(inner)),
            // A type that reached `Type::Error` already triggered a
            // prior diagnostic; rendering `<error>` on cascading
            // messages reads as double-reporting. An empty placeholder
            // (`_`) keeps downstream messages readable without
            // suggesting a second, distinct failure.
            Type::Error => write!(f, "_"),
            Type::Never => write!(f, "Never"),
            Type::AssocProj {
                receiver,
                trait_name,
                assoc_name,
            } => {
                // Render the qualified form `<recv as Trait>::Name` for
                // diagnostics so the receiver/trait/assoc-name triple is
                // unambiguous regardless of context.
                write!(f, "<{} as {trait_name}>::{assoc_name}", self.of(receiver))
            }
            Type::AnonRecord { fields, tail } => {
                write!(f, "{{")?;
                let mut first = true;
                for (n, t) in fields.iter() {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{n}: {}", self.of(t))?;
                }
                if !matches!(tail, RowTail::Closed) {
                    if !first {
                        write!(f, ", ")?;
                    }
                    // A row variable is written `...`: "more fields
                    // possible" (its id is internal). An annotation's
                    // row variable, inside its declaration, has its
                    // name.
                    match tail {
                        RowTail::Rigid(r) => write!(f, "...{}", r.name)?,
                        _ => write!(f, "...")?,
                    }
                }
                write!(f, "}}")
            }
        }
    }
}

// ── Type scheme (polymorphic type) ──────────────────────────────────

/// What a scheme asks of the types its variables stand for: each use of
/// the scheme owes its predicates, at the types the use gives the
/// variables.
#[derive(Debug, Clone, PartialEq)]
pub enum Pred {
    /// `subject` implements the trait `tr`, at the trait arguments
    /// `args` (`where a: TryInto(Int)`: `[Int]`; none for a trait without
    /// parameters).
    Trait {
        tr: TraitKey,
        args: Vec<Type>,
        subject: Type,
    },
    /// The row `row` (a row variable, as a type) is not the rest of a
    /// declared record: the definition spreads a record over the row, or
    /// binds the rest of one, and what that makes is an anonymous
    /// record. With `given`, the row variable of a function's signature,
    /// only if the body of that function does so to that variable
    /// (which is known once the body is checked).
    Anon { row: Type, given: Option<TyVar> },
    /// The row `row` (a row variable, as a type) has no field `field`:
    /// the definition extends a record over the row with the field
    /// (`{...p, age: 30}`), whether or not what it makes reaches the
    /// definition's type.
    Lacks { row: Type, field: Symbol },
}

impl Pred {
    /// `var` implements the trait `tr` at `args`: a `where` bound.
    pub fn bound(var: TyVar, tr: TraitKey, args: Vec<Type>) -> Pred {
        Pred::Trait {
            tr,
            args,
            subject: Type::Var(var),
        }
    }

    /// The predicate with its type variables replaced as `mapping` says.
    pub fn substitute(&self, mapping: &HashMap<TyVar, Type>) -> Pred {
        match self {
            Pred::Trait { tr, args, subject } => Pred::Trait {
                tr: *tr,
                args: args.iter().map(|t| substitute_vars(t, mapping)).collect(),
                subject: substitute_vars(subject, mapping),
            },
            Pred::Anon { row, given } => Pred::Anon {
                row: substitute_vars(row, mapping),
                given: *given,
            },
            Pred::Lacks { row, field } => Pred::Lacks {
                row: substitute_vars(row, mapping),
                field: *field,
            },
        }
    }

    /// The type the predicate is about.
    pub fn subject(&self) -> &Type {
        match self {
            Pred::Trait { subject, .. } => subject,
            Pred::Anon { row, .. } | Pred::Lacks { row, .. } => row,
        }
    }

    /// The types the predicate names besides its subject.
    pub fn args(&self) -> &[Type] {
        match self {
            Pred::Trait { args, .. } => args,
            Pred::Anon { .. } | Pred::Lacks { .. } => &[],
        }
    }
}

/// A type scheme represents a polymorphic type: forall vars . preds => ty
/// The `vars` are the universally quantified type variables.
/// The `preds` are what each use owes for them (from `where` clauses, or
/// inferred from what the definition's body uses).
///
/// `optional_last_param` is part of a function's signature: when `true`,
/// a call may leave out the function's last parameter. Only the builtins
/// whose implementation accepts the shorter call declare it
/// (`test.assert`, `test.assert_eq`, `test.assert_ne`, `float.to_string`,
/// `channel.new`); every other function requires exactly as many
/// arguments as it has parameters. The fact belongs to the named
/// function, not to its type: a scheme made from a type alone
/// (`generalize`, `mono`) never declares it, so a function value bound
/// with `let` or passed as an argument is called with its full arity.
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<TyVar>,
    pub preds: Vec<Pred>,
    pub ty: Type,
    pub optional_last_param: bool,
}

impl Scheme {
    pub fn mono(ty: Type) -> Self {
        Scheme {
            vars: Vec::new(),
            preds: Vec::new(),
            ty,
            optional_last_param: false,
        }
    }

    /// Declare that a call may leave out this function's last parameter.
    /// See the `optional_last_param` note on `Scheme`.
    pub fn with_optional_last_param(mut self) -> Self {
        self.optional_last_param = true;
        self
    }
}

// ── Type errors ─────────────────────────────────────────────────────

// ── Free functions on types ─────────────────────────────────────────

/// Collect free type variables in a type.
pub fn free_vars_in(ty: &Type) -> Vec<TyVar> {
    match ty {
        Type::Var(v) => vec![*v],
        Type::Fun(params, ret) => {
            let mut fvs = Vec::new();
            for p in params {
                for v in free_vars_in(p) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            for v in free_vars_in(ret) {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
            fvs
        }
        Type::List(inner) => free_vars_in(inner),
        Type::Range(inner) => free_vars_in(inner),
        Type::Tuple(elems) => {
            let mut fvs = Vec::new();
            for e in elems {
                for v in free_vars_in(e) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            fvs
        }
        Type::Generic(_, args) => {
            let mut fvs = Vec::new();
            for a in args {
                for v in free_vars_in(a) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            fvs
        }
        Type::Map(k, v) => {
            let mut fvs = free_vars_in(k);
            for fv in free_vars_in(v) {
                if !fvs.contains(&fv) {
                    fvs.push(fv);
                }
            }
            fvs
        }
        Type::Set(inner) => free_vars_in(inner),
        Type::Channel(inner) => free_vars_in(inner),
        Type::AssocProj { receiver, .. } => free_vars_in(receiver),
        Type::AnonRecord { fields, tail } => {
            let mut fvs = Vec::new();
            for t in fields.values() {
                for v in free_vars_in(t) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            if let RowTail::Var(v) = tail
                && !fvs.contains(v)
            {
                fvs.push(*v);
            }
            fvs
        }
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => Vec::new(),
    }
}

/// The type `ty` with each annotation variable of `rigid` written as
/// the rigid variable it is inside its declaration.
pub fn rigidify(ty: &Type, rigid: &[RigidId]) -> Type {
    if rigid.is_empty() {
        return ty.clone();
    }
    let mapping: HashMap<TyVar, Type> = rigid.iter().map(|r| (r.var, Type::Rigid(*r))).collect();
    substitute_vars(ty, &mapping)
}

/// The type `ty` with each rigid variable written as the variable a
/// scheme quantifies, and those variables, in the order they appear.
pub fn unrigidify(ty: &Type) -> (Type, Vec<TyVar>) {
    let mut vars = Vec::new();
    let ty = map_rigid(ty, &mut |r| {
        if !vars.contains(&r.var) {
            vars.push(r.var);
        }
        Type::Var(r.var)
    });
    (ty, vars)
}

/// The type `ty` with each of the rigid variables `own` written as the
/// variable a scheme quantifies, and those of them it mentions.
pub fn release_rigid(ty: &Type, own: &[RigidId]) -> (Type, Vec<TyVar>) {
    let mut vars = Vec::new();
    let ty = map_rigid(ty, &mut |r| {
        if !own.contains(&r) {
            return Type::Rigid(r);
        }
        if !vars.contains(&r.var) {
            vars.push(r.var);
        }
        Type::Var(r.var)
    });
    (ty, vars)
}

/// `ty` with each rigid variable replaced by what `f` gives for it.
pub fn map_rigid(ty: &Type, f: &mut impl FnMut(RigidId) -> Type) -> Type {
    match ty {
        Type::Rigid(r) => f(*r),
        Type::Fun(params, ret) => {
            let params = params.iter().map(|p| map_rigid(p, f)).collect();
            Type::Fun(params, Box::new(map_rigid(ret, f)))
        }
        Type::List(inner) => Type::List(Box::new(map_rigid(inner, f))),
        Type::Range(inner) => Type::Range(Box::new(map_rigid(inner, f))),
        Type::Set(inner) => Type::Set(Box::new(map_rigid(inner, f))),
        Type::Channel(inner) => Type::Channel(Box::new(map_rigid(inner, f))),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| map_rigid(e, f)).collect()),
        Type::Generic(name, args) => {
            Type::Generic(*name, args.iter().map(|a| map_rigid(a, f)).collect())
        }
        Type::Map(k, v) => Type::Map(Box::new(map_rigid(k, f)), Box::new(map_rigid(v, f))),
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => Type::AssocProj {
            receiver: Box::new(map_rigid(receiver, f)),
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        Type::AnonRecord { fields, tail } => Type::AnonRecord {
            fields: fields.iter().map(|(n, t)| (*n, map_rigid(t, f))).collect(),
            tail: match tail {
                RowTail::Rigid(r) => match f(*r) {
                    Type::Var(v) => RowTail::Var(v),
                    Type::Rigid(other) => RowTail::Rigid(other),
                    _ => tail.clone(),
                },
                _ => tail.clone(),
            },
        },
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Error
        | Type::Never => ty.clone(),
    }
}

/// Whether `at` is the type `general` with a type for each of its
/// variables, which `stands` then holds. (Where the two differ in
/// more than that the answer is no: the caller takes it for unknown.)
pub fn instance_of(general: &Type, at: &Type, stands: &mut HashMap<TyVar, Type>) -> bool {
    let mut all = |general: &[Type], at: &[Type]| {
        general.len() == at.len()
            && general
                .iter()
                .zip(at)
                .all(|(g, a)| instance_of(g, a, stands))
    };
    match (general, at) {
        (Type::Var(v), _) => match stands.get(v) {
            Some(known) => known == at,
            None => {
                stands.insert(*v, at.clone());
                true
            }
        },
        (Type::Fun(gp, gr), Type::Fun(ap, ar)) => {
            all(gp, ap) && all(std::slice::from_ref(&**gr), std::slice::from_ref(&**ar))
        }
        (Type::List(g), Type::List(a))
        | (Type::Range(g), Type::Range(a))
        | (Type::Set(g), Type::Set(a))
        | (Type::Channel(g), Type::Channel(a)) => instance_of(g, a, stands),
        (Type::Tuple(g), Type::Tuple(a)) => all(g, a),
        (Type::Generic(gn, g), Type::Generic(an, a)) => gn == an && all(g, a),
        (Type::Map(gk, gv), Type::Map(ak, av)) => {
            instance_of(gk, ak, stands) && instance_of(gv, av, stands)
        }
        (
            Type::AnonRecord {
                fields: g,
                tail: RowTail::Closed,
            },
            Type::AnonRecord {
                fields: a,
                tail: RowTail::Closed,
            },
        ) => {
            g.len() == a.len()
                && g.iter()
                    .zip(a)
                    .all(|((gn, g), (an, a))| gn == an && instance_of(g, a, stands))
        }
        _ => general == at,
    }
}

/// Substitute type variables according to a mapping.
pub fn substitute_vars(ty: &Type, mapping: &HashMap<TyVar, Type>) -> Type {
    match ty {
        Type::Var(v) => {
            if let Some(replacement) = mapping.get(v) {
                replacement.clone()
            } else {
                ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params.iter().map(|p| substitute_vars(p, mapping)).collect();
            let ret = Box::new(substitute_vars(ret, mapping));
            Type::Fun(params, ret)
        }
        Type::List(inner) => Type::List(Box::new(substitute_vars(inner, mapping))),
        Type::Range(inner) => Type::Range(Box::new(substitute_vars(inner, mapping))),
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| substitute_vars(e, mapping)).collect())
        }
        Type::Generic(name, args) => {
            let args = args.iter().map(|a| substitute_vars(a, mapping)).collect();
            Type::Generic(*name, args)
        }
        Type::Map(k, v) => Type::Map(
            Box::new(substitute_vars(k, mapping)),
            Box::new(substitute_vars(v, mapping)),
        ),
        Type::Set(inner) => Type::Set(Box::new(substitute_vars(inner, mapping))),
        Type::Channel(inner) => Type::Channel(Box::new(substitute_vars(inner, mapping))),
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => Type::AssocProj {
            receiver: Box::new(substitute_vars(receiver, mapping)),
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        Type::AnonRecord { fields, tail } => {
            let new_fields: BTreeMap<Symbol, Type> = fields
                .iter()
                .map(|(n, t)| (*n, substitute_vars(t, mapping)))
                .collect();
            let new_tail = match tail {
                RowTail::Closed => RowTail::Closed,
                RowTail::Var(v) => match mapping.get(v) {
                    // If the row var resolved to a record, merge its
                    // fields and propagate its tail.
                    Some(Type::AnonRecord {
                        fields: extra_fields,
                        tail: extra_tail,
                    }) => {
                        let mut merged = new_fields.clone();
                        for (n, t) in extra_fields.iter() {
                            // Round 76 LATENT T3: align with the
                            // companion `apply` site (typechecker/
                            // mod.rs ~line 922) which uses
                            // `or_insert` (existing wins). The
                            // unifier's invariant guarantees
                            // overlapping fields have already been
                            // pairwise-unified before any tail
                            // binding fires (`unify_anon_anon` runs
                            // the common-key pass at line ~1017
                            // before binding the tail), so the
                            // existing field is canonical and the
                            // substituted record's same-name field
                            // would only re-introduce a stale type
                            // pre-merge. Pre-fix this site used
                            // `merged.insert` (substitution wins),
                            // diverging from `apply` and producing
                            // different results for any caller that
                            // exercised overlap. Aligning on
                            // `or_insert` matches the unifier
                            // invariant.
                            merged
                                .entry(*n)
                                .or_insert_with(|| substitute_vars(t, mapping));
                        }
                        return Type::AnonRecord {
                            fields: merged,
                            tail: extra_tail.clone(),
                        };
                    }
                    // Generic instantiation maps row tail var `v` to
                    // a fresh `Type::Var(w)`; carry the freshening
                    // through by re-tailing on `w`. Without this we
                    // silently kept `RowTail::Var(*v)`, leaving the
                    // unifier with a bound row var (`v`) that the
                    // caller had already instantiated away — which
                    // tripped the `apply` chain and surfaced as
                    // missing-binding crashes.
                    Some(Type::Var(w)) => RowTail::Var(*w),
                    // Inside its declaration an annotation's row
                    // variable is rigid.
                    Some(Type::Rigid(r)) => RowTail::Rigid(*r),
                    // The row is a nominal record: the type is that
                    // record's (see `TypeChecker::apply`).
                    Some(other) => return other.clone(),
                    None => RowTail::Var(*v),
                },
                RowTail::Rigid(r) => RowTail::Rigid(*r),
            };
            Type::AnonRecord {
                fields: new_fields,
                tail: new_tail,
            }
        }
        _ => ty.clone(),
    }
}

/// Substitute enum type parameters with concrete type arguments.
/// This is used when we know e.g. Result(Int, String) and want to
/// resolve the type of a variant's field.
pub fn substitute_enum_params(
    field_ty: &Type,
    param_var_ids: &[TyVar],
    type_args: &[Type],
) -> Type {
    match field_ty {
        Type::Var(v) => {
            // Find which parameter position this TyVar corresponds to
            if let Some(pos) = param_var_ids.iter().position(|id| id == v) {
                if pos < type_args.len() {
                    type_args[pos].clone()
                } else {
                    field_ty.clone()
                }
            } else {
                field_ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params
                .iter()
                .map(|p| substitute_enum_params(p, param_var_ids, type_args))
                .collect();
            let ret = Box::new(substitute_enum_params(ret, param_var_ids, type_args));
            Type::Fun(params, ret)
        }
        Type::List(inner) => Type::List(Box::new(substitute_enum_params(
            inner,
            param_var_ids,
            type_args,
        ))),
        Type::Range(inner) => Type::Range(Box::new(substitute_enum_params(
            inner,
            param_var_ids,
            type_args,
        ))),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| substitute_enum_params(e, param_var_ids, type_args))
                .collect(),
        ),
        Type::Generic(name, args) => {
            let args = args
                .iter()
                .map(|a| substitute_enum_params(a, param_var_ids, type_args))
                .collect();
            Type::Generic(*name, args)
        }
        Type::Channel(inner) => Type::Channel(Box::new(substitute_enum_params(
            inner,
            param_var_ids,
            type_args,
        ))),
        Type::Map(k, v) => Type::Map(
            Box::new(substitute_enum_params(k, param_var_ids, type_args)),
            Box::new(substitute_enum_params(v, param_var_ids, type_args)),
        ),
        Type::Set(t) => Type::Set(Box::new(substitute_enum_params(
            t,
            param_var_ids,
            type_args,
        ))),
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => Type::AssocProj {
            receiver: Box::new(substitute_enum_params(receiver, param_var_ids, type_args)),
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        Type::AnonRecord { fields, tail } => Type::AnonRecord {
            fields: fields
                .iter()
                .map(|(n, t)| (*n, substitute_enum_params(t, param_var_ids, type_args)))
                .collect(),
            tail: tail.clone(),
        },
        _ => field_ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Regression: substitute_enum_params recurses into Map/Set ───────
    // Locks in 3a4edd6 B3: prior to the fix these branches fell through
    // to `_ => field_ty.clone()`, so a variant field typed
    // `Map(String, a)` (or `Set(a)`) carrying the enum's parameter
    // variable was returned unchanged, leaking the enum's internal
    // TyVar into downstream inference instead of being replaced with
    // the concrete instantiation.

    #[test]
    fn substitute_enum_params_recurses_into_map_value() {
        // Simulate `type Box(a) { Carry(Map(String, a)) }` instantiated
        // as `Box(Int)`: enum param var is TyVar 0, type_args is [Int].
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Map(Box::new(Type::String), Box::new(Type::Var(0)));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(Box::new(Type::String), Box::new(Type::Int)),
            "Map value type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_recurses_into_map_key() {
        // A pathological but legal shape: `Map(a, Int)`.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::String];
        let field = Type::Map(Box::new(Type::Var(0)), Box::new(Type::Int));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(Box::new(Type::String), Box::new(Type::Int)),
            "Map key type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_recurses_into_set() {
        // Simulate `type Bag(a) { Contents(Set(a)) }` as `Bag(Int)`.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Set(Box::new(Type::Var(0)));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Set(Box::new(Type::Int)),
            "Set element type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_handles_nested_map_of_set() {
        // Map(String, Set(a)) — catches a regression where only the
        // outermost container is substituted.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Map(
            Box::new(Type::String),
            Box::new(Type::Set(Box::new(Type::Var(0)))),
        );
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(
                Box::new(Type::String),
                Box::new(Type::Set(Box::new(Type::Int))),
            ),
            "nested Set inside Map must be substituted"
        );
    }

    // ── Regression: Type::Fun Display matches parser `Fn(...)` surface ──
    // The parser at src/parser.rs:2116 reads function-type annotations as
    // `Fn(A, B) -> C`. Without the `Fn` prefix in Display, diagnostics
    // render fn types as `(A, B) -> C`, which visually collides with
    // silt's tuple-type syntax `(A, B)` and doesn't match anything a
    // user could write in an annotation.

    #[test]
    fn display_fun_uses_fn_prefix_multi_arg() {
        let ty = Type::Fun(vec![Type::Int, Type::String], Box::new(Type::Int));
        assert_eq!(format!("{ty}"), "Fn(Int, String) -> Int");
    }

    #[test]
    fn display_fun_uses_fn_prefix_single_arg() {
        let ty = Type::Fun(vec![Type::Int], Box::new(Type::Bool));
        assert_eq!(format!("{ty}"), "Fn(Int) -> Bool");
    }

    #[test]
    fn display_fun_distinguishes_tuple_arg_from_multi_arg() {
        // `Fn((Int, String)) -> Int` is a 1-arg fn taking a tuple.
        // `Fn(Int, String) -> Int` is a 2-arg fn. These must render
        // distinctly so diagnostics don't conflate arity with tupling.
        let tuple_arg = Type::Fun(
            vec![Type::Tuple(vec![Type::Int, Type::String])],
            Box::new(Type::Int),
        );
        let two_arg = Type::Fun(vec![Type::Int, Type::String], Box::new(Type::Int));
        assert_eq!(format!("{tuple_arg}"), "Fn((Int, String)) -> Int");
        assert_eq!(format!("{two_arg}"), "Fn(Int, String) -> Int");
        assert_ne!(format!("{tuple_arg}"), format!("{two_arg}"));
    }

    #[test]
    fn display_fun_zero_arg() {
        let ty = Type::Fun(vec![], Box::new(Type::Unit));
        assert_eq!(format!("{ty}"), "Fn() -> ()");
    }
}
