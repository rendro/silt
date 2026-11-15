//! Shared type definitions for Silt's type system.
//!
//! This module contains the core type representations used by the type checker,
//! interpreter, and other parts of the compiler pipeline.

pub mod builtins;
pub mod canonical;

use std::collections::HashMap;

use crate::defs::{TraitId, TypeId};
use crate::intern::{Symbol, intern};

// ── Type representation ─────────────────────────────────────────────

/// A unique identifier for type variables.
pub(crate) type TyVar = usize;

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

/// A trait is written by its name; a trait of a builtin module with the
/// module (`json.Encode`), as a program names it.
impl std::fmt::Display for TraitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some((name, Some(module))) = crate::defs::builtin_trait_of_id(self.id.0) {
            return write!(f, "{module}.{name}");
        }
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
    /// A type constructor variable applied to a type, `m(Int)`: the head
    /// is a variable (a unification variable or an annotation's) that
    /// stands for a type constructor. Once the head is a constructor the
    /// type is the full type (`Type::applied`).
    App(Box<Type>, Box<Type>),
    /// A type constructor: `head` with its first type argument open and
    /// the others `rest` (`Option(_)`, `Result(_, String)`), what a type
    /// constructor variable stands for.
    Con(Head, Vec<Type>),
}

/// The head of a type constructor ([`Type::Con`]): a named type, or one
/// of the builtin containers the type representation writes apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Head {
    Named(TypeRef),
    List,
    Map,
    Set,
    Channel,
}

/// The name of a builtin type; `None` for a type a module declares.
pub(crate) fn builtin_type_name(ty: TypeRef) -> Option<&'static str> {
    crate::defs::builtin_types()
        .get(ty.id.0.0 as usize)
        .map(|(name, _)| *name)
}

/// The builtin containers, each with its name and its number of type
/// arguments: the one place a container's name meets its variant.
const CONTAINERS: [(Head, &str, usize); 4] = [
    (Head::List, "List", 1),
    (Head::Map, "Map", 2),
    (Head::Set, "Set", 1),
    (Head::Channel, "Channel", 1),
];

impl Head {
    /// The builtin container named `name`; `None` for any other name.
    pub(crate) fn container(name: &str) -> Option<Head> {
        CONTAINERS
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(h, _, _)| *h)
    }

    /// The head of the type `ty`: the builtin container it is, else the
    /// named type.
    pub(crate) fn of(ty: TypeRef) -> Head {
        builtin_type_name(ty)
            .and_then(Head::container)
            .unwrap_or(Head::Named(ty))
    }

    /// The name of a builtin container; `None` for a named type.
    pub(crate) fn container_name(self) -> Option<&'static str> {
        CONTAINERS
            .iter()
            .find(|(h, _, _)| *h == self)
            .map(|(_, n, _)| *n)
    }

    /// The number of type arguments of a builtin container; `None` for a
    /// named type.
    pub(crate) fn container_arity(self) -> Option<usize> {
        CONTAINERS
            .iter()
            .find(|(h, _, _)| *h == self)
            .map(|(_, _, k)| *k)
    }

    /// The type this head names: a builtin container is the builtin type
    /// of its name.
    pub(crate) fn type_ref(self) -> TypeRef {
        match self {
            Head::Named(r) => r,
            container => TypeRef::builtin(container.container_name().unwrap_or_default()),
        }
    }

    /// The full type of this head at `args`: a builtin container takes
    /// exactly its number of type arguments (`None` for any other
    /// number), a named type any.
    pub(crate) fn with_args(self, args: Vec<Type>) -> Option<Type> {
        let Head::Named(r) = self else {
            if Some(args.len()) != self.container_arity() {
                return None;
            }
            let mut it = args.into_iter().map(Box::new);
            let mut next = || it.next().unwrap_or_else(|| Box::new(Type::Error));
            return Some(match self {
                Head::List => Type::List(next()),
                Head::Set => Type::Set(next()),
                Head::Channel => Type::Channel(next()),
                _ => {
                    let k = next();
                    Type::Map(k, next())
                }
            });
        };
        Some(Type::Generic(r, args))
    }

    /// The full type of the constructor `self` with the arguments `rest`
    /// after its first, applied to `arg`.
    pub fn apply(self, mut rest: Vec<Type>, arg: Type) -> Type {
        match self {
            Head::Named(r) => {
                rest.insert(0, arg);
                Type::Generic(r, rest)
            }
            Head::List => Type::List(Box::new(arg)),
            Head::Set => Type::Set(Box::new(arg)),
            Head::Channel => Type::Channel(Box::new(arg)),
            Head::Map => {
                let value = rest.pop().unwrap_or(Type::Error);
                Type::Map(Box::new(arg), Box::new(value))
            }
        }
    }
}

impl Type {
    /// `head` applied to `arg`: the full type where `head` is a type
    /// constructor (`Option(_)` at `Int` is `Option(Int)`), else the
    /// application `head(arg)` of a constructor variable.
    pub fn applied(head: Type, arg: Type) -> Type {
        match head {
            Type::Con(h, rest) => h.apply(rest, arg),
            other => Type::App(Box::new(other), Box::new(arg)),
        }
    }

    /// The type constructor of `self` and its first type argument, where
    /// it has one: `List(Int)` is `List(_)` at `Int`, `Result(Int, e)` is
    /// `Result(_, e)` at `Int`, `Map(String, Int)` is `Map(_, Int)` at
    /// `String`. A type of no type arguments, a function, a tuple, a
    /// record and a type of types (`type a`) have none.
    pub fn split_constructor(&self) -> Option<(Head, Vec<Type>, Type)> {
        match self.head_and_args()? {
            (Head::Named(r), _) if r.is_builtin(crate::defs::TYPE_OF) => None,
            (head, args) => {
                let (first, rest) = args.split_first()?;
                Some((
                    head,
                    rest.iter().map(|t| (*t).clone()).collect(),
                    (*first).clone(),
                ))
            }
        }
    }

    /// The type `ty` names at `args`: a builtin scalar given none is its
    /// own variant (`Int`, `()`), a builtin container given its number of
    /// type arguments its own (`List(a)`, `Map(k, v)`); any other is
    /// `Generic(ty, args)`.
    pub(crate) fn named(ty: TypeRef, args: Vec<Type>) -> Type {
        if args.is_empty() {
            let scalar = match builtin_type_name(ty) {
                Some("Int") => Some(Type::Int),
                Some("Float") => Some(Type::Float),
                Some("Bool") => Some(Type::Bool),
                Some("String") => Some(Type::String),
                Some("()") => Some(Type::Unit),
                _ => None,
            };
            if let Some(scalar) = scalar {
                return scalar;
            }
        }
        Type::container_or_generic(ty, args)
    }

    /// The type `ty` at `args`: a builtin container given its number of
    /// type arguments is its own variant (`List(a)`, `Map(k, v)`); any
    /// other is `Generic(ty, args)`.
    pub(crate) fn container_or_generic(ty: TypeRef, args: Vec<Type>) -> Type {
        let head = Head::of(ty);
        if head.container_arity().is_some_and(|n| n != args.len()) {
            return Type::Generic(ty, args);
        }
        head.with_args(args).unwrap_or(Type::Error)
    }

    /// The name of the builtin type `self` is when the type representation
    /// writes it as a variant of its own: a scalar (`Int`, `()`), a
    /// container (`List`, `Map`), a tuple (`Tuple`) or a function (`Fn`).
    /// `None` for any other type.
    pub(crate) fn builtin_name(&self) -> Option<&'static str> {
        Some(match self {
            Type::Int => "Int",
            Type::Float => "Float",
            Type::Bool => "Bool",
            Type::String => "String",
            Type::Unit => "()",
            Type::Tuple(_) => "Tuple",
            Type::Fun(..) => "Fn",
            Type::List(_) | Type::Map(..) | Type::Set(_) | Type::Channel(_) => {
                return self.container_head()?.container_name();
            }
            Type::Var(_)
            | Type::Rigid(_)
            | Type::Generic(..)
            | Type::Error
            | Type::Never
            | Type::App(..)
            | Type::Con(..) => return None,
        })
    }

    /// The builtin container `self` is; `None` for any other type.
    pub(crate) fn container_head(&self) -> Option<Head> {
        match self {
            Type::List(_) => Some(Head::List),
            Type::Map(..) => Some(Head::Map),
            Type::Set(_) => Some(Head::Set),
            Type::Channel(_) => Some(Head::Channel),
            _ => None,
        }
    }

    /// The head of `self` and its type arguments: `List(Int)` is `List`
    /// at `[Int]`, `Map(k, v)` is `Map` at `[k, v]`, a named type is
    /// itself at its arguments (none for `Bytes`). `None` for any other
    /// type.
    pub(crate) fn head_and_args(&self) -> Option<(Head, Vec<&Type>)> {
        match self {
            Type::List(t) => Some((Head::List, vec![&**t])),
            Type::Set(t) => Some((Head::Set, vec![&**t])),
            Type::Channel(t) => Some((Head::Channel, vec![&**t])),
            Type::Map(k, v) => Some((Head::Map, vec![&**k, &**v])),
            Type::Generic(r, args) => Some((Head::Named(*r), args.iter().collect())),
            _ => None,
        }
    }

    /// The builtin type `name` with `args` (`Option(a)`, `time.Duration`).
    pub fn builtin(name: &str, args: Vec<Type>) -> Type {
        Type::Generic(TypeRef::builtin(name), args)
    }

    /// `Option(t)`.
    pub fn option(t: Type) -> Type {
        Type::builtin("Option", vec![t])
    }

    /// `Result(t, e)`.
    #[cfg(any(test, feature = "test-hooks"))]
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

    /// Whether `self` holds the type of what was already reported as an
    /// error (`Type::Error`, an unknown type's).
    pub fn holds_error(&self) -> bool {
        match self {
            Type::Error => true,
            Type::Generic(_, ts) | Type::Tuple(ts) => ts.iter().any(Type::holds_error),
            Type::Fun(params, ret) => params.iter().any(Type::holds_error) || ret.holds_error(),
            Type::List(t) | Type::Set(t) | Type::Channel(t) => t.holds_error(),
            Type::Map(k, v) => k.holds_error() || v.holds_error(),
            Type::App(head, arg) => head.holds_error() || arg.holds_error(),
            Type::Con(_, rest) => rest.iter().any(Type::holds_error),
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Var(_)
            | Type::Rigid(_)
            | Type::Never => false,
        }
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
            Type::List(t) | Type::Set(t) | Type::Channel(t) => t.collect_refs(out),
            Type::Map(k, v) => {
                k.collect_refs(out);
                v.collect_refs(out);
            }
            Type::Tuple(ts) => {
                for t in ts {
                    t.collect_refs(out);
                }
            }
            Type::App(head, arg) => {
                head.collect_refs(out);
                arg.collect_refs(out);
            }
            Type::Con(head, rest) => {
                if let Head::Named(r) = head {
                    out.push(*r);
                }
                for t in rest {
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

    /// Write the named type `ty` with `args`, written by `args` itself:
    /// its name, the arguments, and the part of a name that follows them
    /// (after [`AFTER_ARGUMENTS`]: `Box(Int) of package inner`).
    fn named(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        ty: &TypeRef,
        args: impl FnOnce(&mut std::fmt::Formatter<'_>) -> std::fmt::Result,
    ) -> std::fmt::Result {
        let name = self.name(ty);
        let (head, after) = name.split_once(AFTER_ARGUMENTS).unwrap_or((&name, ""));
        f.write_str(head)?;
        args(f)?;
        f.write_str(after)
    }
}

/// In a name [`Type::show_all`] is given for a type, where the part
/// that is written after the type's arguments begins: a description of
/// the type's place (`InErr of package inner`) follows them.
pub(crate) const AFTER_ARGUMENTS: char = '\u{1}';

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
                self.named(f, name, |f| {
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
                })
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
            // A constructor variable applied is written as written:
            // `m(Int)`.
            Type::App(head, arg) => write!(f, "{}({})", self.of(head), self.of(arg)),
            // A type constructor, with `_` for its open first argument:
            // `Option(_)`, `Result(_, String)`, as an impl header writes
            // it.
            Type::Con(head, rest) => {
                let args = |f: &mut std::fmt::Formatter<'_>| {
                    write!(f, "(_")?;
                    for t in rest {
                        write!(f, ", {}", self.of(t))?;
                    }
                    write!(f, ")")
                };
                match head {
                    Head::Named(r) => self.named(f, r, args),
                    container => {
                        write!(f, "{}", container.container_name().unwrap_or_default())?;
                        args(f)
                    }
                }
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
        /// For `json.Decode` only: what asks it, which a refusal names
        /// (`json.parse cannot decode ...`, with a note naming the
        /// function that passes the type on).
        asker: Option<Asker>,
    },
    /// `subject` is a type the builtin of `check` takes where its
    /// signature cannot say so: one the decoder decodes, a connection,
    /// a statement's parameters. `passer`: the function or method that
    /// passes the type on to the builtin, which a use names in a note;
    /// none for the builtin's own.
    Builtin {
        check: BuiltinCheck,
        subject: Type,
        passer: Option<Symbol>,
    },
    /// `subject` is given to the annotation variable `given` of the
    /// signature of the function or method `passer` (`type a`, `db: a`):
    /// what its body asks of `given` for the builtins it gives it to
    /// (`Pred::Builtin`, `Pred::Via`), it asks of `subject`. Which that
    /// is is known once the bodies of its module are checked. `written`
    /// is the alias the use names the type by, if it names it by one.
    /// `body`: the method body the variable is of, for the variables
    /// an impl's or a trait's methods share (`Self`, an impl's
    /// parameters); none for a function's own. `group`: for a predicate
    /// a definition took from a use in its body, that use (the types one
    /// use gives belong together, `GivenUse::use_id`).
    Given {
        subject: Type,
        given: TyVar,
        written: Option<Symbol>,
        passer: Symbol,
        body: Option<Symbol>,
        group: Option<usize>,
    },
    /// A call of the method `method` of the trait `tr` on a value of the
    /// type `receiver`, of the type `method_ty` there: what the method
    /// of the receiver's impl (or the trait's default) asks of the types
    /// the call gives it (its `Pred::Given`), the call asks. Decided when
    /// the receiver's type is known; a receiver that is an annotation
    /// variable passes it on to the uses of its function. `at`: where the
    /// call gave the method its type argument, when a definition took the
    /// call into its scheme with that type known (`{ l -> go(l, Blob) }`).
    Via {
        tr: TraitKey,
        method: Symbol,
        receiver: Type,
        method_ty: Type,
        passer: Option<Symbol>,
        at: Option<crate::source::Span>,
    },
}

/// What a `json.Decode` predicate comes from (`Pred::Trait::asker`):
/// what asks it, and the function or method whose scheme it is of,
/// which passes the type on (a note names it); none for a decoder's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Asker {
    pub by: Asked,
    pub passer: Option<Symbol>,
}

/// What asks `json.Decode` of a type: the decoder whose signature asks
/// it, or the declared bound of a function or method on its annotation
/// variable, which asks what its body asks of that variable (known once
/// the body is checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Asked {
    Builtin(Symbol),
    Bound(TyVar),
    /// A declared bound whose body gives its variable to no decoder, as
    /// another module reads it.
    Unknown,
    /// A declared bound whose body passes its variable on to the decoder
    /// (the first) through the functions and methods of the second, as
    /// another module reads it: of each route, the modules of other
    /// modules' definitions on it and the names of its links, one after
    /// the other, in one symbol (`Asked::routes`).
    Through(Symbol, Symbol),
}

/// A route of `Asked::Through`: the modules of the definitions of other
/// modules on it, and the functions and methods it passes the type
/// through.
pub(crate) type ThroughRoute = (Vec<crate::defs::ModuleId>, Vec<Symbol>);

impl Asked {
    /// The routes of `Through`.
    pub fn routes(routes: Symbol) -> Vec<ThroughRoute> {
        (crate::intern::resolve(routes).split('|'))
            .map(|route| {
                let (owners, links) = route.split_once(';').unwrap_or(("", route));
                let owners = (owners.split(','))
                    .filter_map(|n| n.parse().ok())
                    .map(crate::defs::ModuleId)
                    .collect();
                let links = (links.split(' '))
                    .filter(|name| !name.is_empty())
                    .map(crate::intern::intern)
                    .collect();
                (owners, links)
            })
            .collect()
    }

    /// `routes` as `Through` holds them.
    pub fn joined(routes: &[ThroughRoute]) -> Symbol {
        let routes: Vec<String> = (routes.iter())
            .map(|(owners, links)| {
                let owners: Vec<String> = owners.iter().map(|m| m.0.to_string()).collect();
                let links: Vec<String> = links.iter().map(|l| crate::intern::resolve(*l)).collect();
                format!("{};{}", owners.join(","), links.join(" "))
            })
            .collect();
        crate::intern::intern(&routes.join("|"))
    }
}

impl Asker {
    /// The asker of a declared bound `var: tr`: the bound itself, for
    /// `json.Decode`; none for any other trait.
    pub fn of_bound(tr: TraitKey, var: TyVar) -> Option<Asker> {
        tr.is_builtin("Decode").then_some(Asker {
            by: Asked::Bound(var),
            passer: None,
        })
    }
}

/// What a builtin asks of the type of one of its arguments beyond its
/// signature (`Pred::Builtin`); the builtin is named as the registry
/// names it (`json.parse`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BuiltinCheck {
    /// The type argument of a decoder: `decoding` decodes it. `written`
    /// is the alias the call names it by, if it names it by one.
    Decodes {
        builtin: Symbol,
        decoding: crate::builtins::registry::decoding::Decoding,
        written: Option<Symbol>,
    },
    /// The connection of a postgres function: a `PgPool` or a `PgTx`.
    Connection { builtin: Symbol },
    /// The parameters of a postgres statement: a tuple, or `()`.
    Params { builtin: Symbol },
    /// The value an encoder other than json's (`toml.stringify`) reads
    /// by its structure.
    Encodes { builtin: Symbol },
}

impl BuiltinCheck {
    /// The builtin that asks it.
    pub fn builtin(self) -> Symbol {
        match self {
            BuiltinCheck::Decodes { builtin, .. }
            | BuiltinCheck::Connection { builtin }
            | BuiltinCheck::Params { builtin }
            | BuiltinCheck::Encodes { builtin } => builtin,
        }
    }

    /// The check without the name a call gives the type.
    pub fn unwritten(self) -> BuiltinCheck {
        match self {
            BuiltinCheck::Decodes {
                builtin, decoding, ..
            } => BuiltinCheck::Decodes {
                builtin,
                decoding,
                written: None,
            },
            other => other,
        }
    }
}

impl Pred {
    /// `var` implements the trait `tr` at `args`: a `where` bound.
    pub fn bound(var: TyVar, tr: TraitKey, args: Vec<Type>) -> Pred {
        Pred::Trait {
            tr,
            args,
            subject: Type::Var(var),
            asker: Asker::of_bound(tr, var),
        }
    }

    /// The predicate with its type variables replaced as `mapping` says.
    pub fn substitute(&self, mapping: &HashMap<TyVar, Type>) -> Pred {
        match self {
            Pred::Trait {
                tr,
                args,
                subject,
                asker,
            } => Pred::Trait {
                tr: *tr,
                args: args.iter().map(|t| substitute_vars(t, mapping)).collect(),
                subject: substitute_vars(subject, mapping),
                asker: *asker,
            },
            Pred::Builtin {
                check,
                subject,
                passer,
            } => Pred::Builtin {
                check: *check,
                subject: substitute_vars(subject, mapping),
                passer: *passer,
            },
            Pred::Given {
                subject,
                given,
                written,
                passer,
                body,
                group,
            } => Pred::Given {
                subject: substitute_vars(subject, mapping),
                given: *given,
                written: *written,
                passer: *passer,
                body: *body,
                group: *group,
            },
            Pred::Via {
                tr,
                method,
                receiver,
                method_ty,
                passer,
                at,
            } => Pred::Via {
                tr: *tr,
                method: *method,
                receiver: substitute_vars(receiver, mapping),
                method_ty: substitute_vars(method_ty, mapping),
                passer: *passer,
                at: *at,
            },
        }
    }

    /// The type the predicate is about.
    pub fn subject(&self) -> &Type {
        match self {
            Pred::Trait { subject, .. }
            | Pred::Builtin { subject, .. }
            | Pred::Given { subject, .. } => subject,
            Pred::Via { receiver, .. } => receiver,
        }
    }

    /// Whether the predicate names the type variable `var`, in its
    /// subject or in the other types it names.
    pub fn mentions(&self, var: TyVar) -> bool {
        std::iter::once(self.subject())
            .chain(self.args())
            .any(|ty| free_vars_in(ty).contains(&var))
    }

    /// The types the predicate names besides its subject.
    pub fn args(&self) -> &[Type] {
        match self {
            Pred::Trait { args, .. } => args,
            Pred::Via { method_ty, .. } => std::slice::from_ref(method_ty),
            Pred::Builtin { .. } | Pred::Given { .. } => &[],
        }
    }
}

/// A type scheme represents a polymorphic type: forall vars . preds => ty
/// The `vars` are the universally quantified type variables.
/// The `preds` are what each use owes for them (from `where` clauses, or
/// inferred from what the definition's body uses).
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<TyVar>,
    pub preds: Vec<Pred>,
    pub ty: Type,
}

impl Scheme {
    pub fn mono(ty: Type) -> Self {
        Scheme {
            vars: Vec::new(),
            preds: Vec::new(),
            ty,
        }
    }

    /// The name of each type variable the scheme quantifies, as `:type`
    /// and the errors write it: a letter, in the order the variables
    /// first appear in its type (`Fn(a, Fn(a) -> b) -> b`); a type
    /// constructor variable `m`, `n`, ... (`Fn(m(Int)) -> m(Int)`).
    pub fn letters(&self) -> HashMap<TyVar, Symbol> {
        let heads = constructor_heads(&self.ty);
        let mut constructors = ["m", "n", "o", "p", "q", "r"].into_iter();
        let mut others = (b'a'..=b'z').map(|letter| char::from(letter).to_string());
        free_vars_in(&self.ty)
            .into_iter()
            .filter(|var| self.vars.contains(var))
            .map(|var| {
                let name = match heads.contains(&var) {
                    true => constructors.next().map(String::from),
                    false => others.next(),
                }
                .unwrap_or_else(|| "_".to_string());
                (var, intern(&name))
            })
            .collect()
    }
}

// ── Type errors ─────────────────────────────────────────────────────

// ── Free functions on types ─────────────────────────────────────────

/// The variables of `ty` applied as type constructors (`m` of `m(Int)`),
/// each once, in the order they first appear.
pub(crate) fn constructor_heads(ty: &Type) -> Vec<TyVar> {
    fn walk(ty: &Type, out: &mut Vec<TyVar>) {
        match ty {
            Type::App(head, arg) => {
                if let Type::Var(v) = &**head
                    && !out.contains(v)
                {
                    out.push(*v);
                }
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

/// Collect free type variables in a type: each once, in the order they
/// first appear.
pub(crate) fn free_vars_in(ty: &Type) -> Vec<TyVar> {
    fn walk(ty: &Type, out: &mut Distinct<TyVar>) {
        match ty {
            Type::Var(v) => {
                out.push(*v);
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
            Type::App(head, arg) => {
                walk(head, out);
                walk(arg, out);
            }
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Rigid(_)
            | Type::Error
            | Type::Never => {}
        }
    }
    let mut out = Distinct::default();
    walk(ty, &mut out);
    out.items
}

/// Items each kept once, in the order first pushed. Past a few, a set
/// says which are kept: a type of many variables is not compared with
/// itself pair by pair.
pub(crate) struct Distinct<T> {
    pub items: Vec<T>,
    seen: Option<std::collections::HashSet<T>>,
}

impl<T> Default for Distinct<T> {
    fn default() -> Self {
        Distinct {
            items: Vec::new(),
            seen: None,
        }
    }
}

impl<T: Copy + Eq + std::hash::Hash> Distinct<T> {
    /// The few that are compared one by one.
    const FEW: usize = 32;

    /// Keep `item` unless it is kept already; whether it was new.
    pub(crate) fn push(&mut self, item: T) -> bool {
        let new = match &mut self.seen {
            Some(seen) => seen.insert(item),
            None => !self.items.contains(&item),
        };
        if new {
            self.items.push(item);
            if self.seen.is_none() && self.items.len() > Self::FEW {
                self.seen = Some(self.items.iter().copied().collect());
            }
        }
        new
    }

    /// Whether `item` is kept.
    pub(crate) fn contains(&self, item: &T) -> bool {
        match &self.seen {
            Some(seen) => seen.contains(item),
            None => self.items.contains(item),
        }
    }
}

impl<T: Copy + Eq + std::hash::Hash> FromIterator<T> for Distinct<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut out = Distinct::default();
        items.into_iter().for_each(|item| {
            out.push(item);
        });
        out
    }
}

/// The type `ty` with each annotation variable of `rigid` written as
/// the rigid variable it is inside its declaration.
pub(crate) fn rigidify(ty: &Type, rigid: &[RigidId]) -> Type {
    if rigid.is_empty() {
        return ty.clone();
    }
    let mapping: HashMap<TyVar, Type> = rigid.iter().map(|r| (r.var, Type::Rigid(*r))).collect();
    substitute_vars(ty, &mapping)
}

/// The type `ty` with each rigid variable written as the variable a
/// scheme quantifies, and those variables, in the order they appear.
pub(crate) fn unrigidify(ty: &Type) -> (Type, Vec<TyVar>) {
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
pub(crate) fn release_rigid(ty: &Type, own: &[RigidId]) -> (Type, Vec<TyVar>) {
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
pub(crate) fn map_rigid(ty: &Type, f: &mut impl FnMut(RigidId) -> Type) -> Type {
    match ty {
        Type::Rigid(r) => f(*r),
        Type::Fun(params, ret) => {
            let params = params.iter().map(|p| map_rigid(p, f)).collect();
            Type::Fun(params, Box::new(map_rigid(ret, f)))
        }
        Type::List(inner) => Type::List(Box::new(map_rigid(inner, f))),
        Type::Set(inner) => Type::Set(Box::new(map_rigid(inner, f))),
        Type::Channel(inner) => Type::Channel(Box::new(map_rigid(inner, f))),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| map_rigid(e, f)).collect()),
        Type::Generic(name, args) => {
            Type::Generic(*name, args.iter().map(|a| map_rigid(a, f)).collect())
        }
        Type::Map(k, v) => Type::Map(Box::new(map_rigid(k, f)), Box::new(map_rigid(v, f))),
        Type::App(head, arg) => Type::applied(map_rigid(head, f), map_rigid(arg, f)),
        Type::Con(head, rest) => Type::Con(*head, rest.iter().map(|t| map_rigid(t, f)).collect()),
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

/// `ty` with each named type `r` replaced by `f(r)`: the same type with
/// another name to show (`regex.Regex` for the builtin `Regex` where the
/// bare name means a program's type).
#[cfg(feature = "lsp")]
pub(crate) fn map_refs(ty: &Type, f: &impl Fn(TypeRef) -> TypeRef) -> Type {
    let each = |ts: &[Type]| ts.iter().map(|t| map_refs(t, f)).collect::<Vec<_>>();
    let boxed = |t: &Type| Box::new(map_refs(t, f));
    match ty {
        Type::Generic(r, args) => Type::Generic(f(*r), each(args)),
        Type::Fun(params, ret) => Type::Fun(each(params), boxed(ret)),
        Type::List(inner) => Type::List(boxed(inner)),
        Type::Set(inner) => Type::Set(boxed(inner)),
        Type::Channel(inner) => Type::Channel(boxed(inner)),
        Type::Tuple(elems) => Type::Tuple(each(elems)),
        Type::Map(k, v) => Type::Map(boxed(k), boxed(v)),
        Type::App(head, arg) => Type::App(boxed(head), boxed(arg)),
        Type::Con(head, rest) => {
            let head = match head {
                Head::Named(r) => Head::Named(f(*r)),
                other => *other,
            };
            Type::Con(head, each(rest))
        }
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => ty.clone(),
    }
}

/// Whether `at` is the type `general` with a type for each of its
/// variables, which `stands` then holds. (Where the two differ in
/// more than that the answer is no: the caller takes it for unknown.)
pub(crate) fn instance_of(general: &Type, at: &Type, stands: &mut HashMap<TyVar, Type>) -> bool {
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
        | (Type::Set(g), Type::Set(a))
        | (Type::Channel(g), Type::Channel(a)) => instance_of(g, a, stands),
        (Type::Tuple(g), Type::Tuple(a)) => all(g, a),
        (Type::Generic(gn, g), Type::Generic(an, a)) => gn == an && all(g, a),
        (Type::Map(gk, gv), Type::Map(ak, av)) => {
            instance_of(gk, ak, stands) && instance_of(gv, av, stands)
        }
        (Type::App(gh, ga), Type::App(ah, aa)) => {
            instance_of(gh, ah, stands) && instance_of(ga, aa, stands)
        }
        (Type::Con(gh, g), Type::Con(ah, a)) => gh == ah && all(g, a),
        _ => general == at,
    }
}

/// Substitute type variables according to a mapping.
pub(crate) fn substitute_vars(ty: &Type, mapping: &HashMap<TyVar, Type>) -> Type {
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
        Type::App(head, arg) => Type::applied(
            substitute_vars(head, mapping),
            substitute_vars(arg, mapping),
        ),
        Type::Con(head, rest) => Type::Con(
            *head,
            rest.iter().map(|t| substitute_vars(t, mapping)).collect(),
        ),
        _ => ty.clone(),
    }
}

/// Substitute enum type parameters with concrete type arguments.
/// This is used when we know e.g. Result(Int, String) and want to
/// resolve the type of a variant's field.
pub(crate) fn substitute_enum_params(
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
        _ => field_ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The table of builtin containers ─────────────────────────────

    #[test]
    fn the_container_table_agrees_with_the_builtin_types() {
        for (head, name, arity) in CONTAINERS {
            let entry = builtins::lookup(name).expect("a builtin type");
            assert_eq!(entry.arity, Some(arity as u32), "{name}");
            assert!(crate::module::BUILTIN_GENERIC_CONTAINER_NAMES.contains(&name));
            assert_eq!(Head::of(TypeRef::builtin(name)), head);
            assert_eq!(head.type_ref(), TypeRef::builtin(name));
            // A container at its arity is its own variant, and reads back
            // as its head at those arguments.
            let args: Vec<Type> = (0..arity).map(|k| Type::Var(k)).collect();
            let ty = Type::named(TypeRef::builtin(name), args.clone());
            assert_eq!(ty.container_head(), Some(head));
            assert_eq!(ty.builtin_name(), Some(name));
            let (back, back_args) = ty.head_and_args().expect("a head");
            assert_eq!(back, head);
            assert_eq!(back_args, args.iter().collect::<Vec<_>>());
            // At another number of arguments it is the named type.
            let odd = Type::named(TypeRef::builtin(name), vec![Type::Int; arity + 1]);
            assert!(matches!(odd, Type::Generic(..)), "{name}");
        }
        for scalar in ["Int", "Float", "Bool", "String", "()"] {
            let ty = Type::named(TypeRef::builtin(scalar), Vec::new());
            assert_eq!(ty.builtin_name(), Some(scalar));
        }
    }

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
