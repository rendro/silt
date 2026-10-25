//! Trait evidence: for each trait predicate the checker solves, what
//! proves it. For a trait a program declares, the compiler turns it into
//! the dictionaries the code passes on (`crate::bytecode::dict`); a
//! builtin trait is found by the value where the code runs.
//!
//! A definition with bounds (a function, a `let` the checker
//! generalises, a method of an impl or a trait's default method) has a
//! list of them, in the order of its scheme's trait predicates. What a
//! body says of the bounds of a definition it is in is a
//! [`Evidence::Param`]: the bound `k` of the definition `owner`, named
//! by the span of its body (a function's or a method's) or of its value
//! (a `let`'s), as the compiler names the function it compiles from it.

use crate::defs::{TraitId, TypeId};
use crate::source::Span;

/// The definition an impl's evidence of its supertraits names
/// ([`Evidence::Param`]): the impl's header, whose bounds are the
/// predicates it asks, in order.
pub const IMPL_HEADER: Span = Span::BUILTIN;

/// What proves that a type implements a trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// The impl of `tr` for the head `ty` at the trait arguments of the
    /// heads `args`, with the evidence of each predicate its header asks
    /// (`where a: Greet`), in order.
    Impl {
        tr: TraitId,
        ty: TypeId,
        args: crate::defs::ArgHeads,
        subs: Vec<Evidence>,
    },
    /// The bound `k` of the enclosing definition `owner`.
    Param { owner: Span, k: u16 },
    /// The supertrait `to` of what `of` proves, the `index`th of the
    /// supertraits its trait declares: the impl of `to` for the same
    /// type.
    Super {
        of: Box<Evidence>,
        to: TraitId,
        index: u16,
    },
    /// The trait holds by the type's structure (Equal, Compare, Hash;
    /// Display and the json traits where no impl is written).
    Structural { tr: TraitId },
    /// An impl the runtime provides for a builtin type: the impl of
    /// `tr` for `ty` that no program writes (`Error` of a builtin
    /// module's error enum), answered by the type's row of the builtin
    /// registry.
    Native { tr: TraitId, ty: TypeId },
    /// The checker accepted the predicate with nothing to show for it.
    Missing,
    /// Not known yet: a REPL input left the predicate to an input after
    /// it, which decides its type ([`Late`]).
    Pending(Late),
}

/// A predicate a REPL input left to a later one: the input (a module of
/// the session) and the predicate's place among its evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Late {
    pub cell: crate::session::ModuleId,
    pub id: u32,
}

/// What an impl's declaration proves of its type, from the predicates
/// of its header (`IMPL_HEADER`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImplEvidence {
    /// That the type has each supertrait of the trait.
    pub supers: Vec<(TraitId, Evidence)>,
    /// That the impl's trait arguments meet the `where` clauses the trait
    /// declares on its parameters (`trait Pr(a) where a: Greet`), each in
    /// the order the trait declares them.
    pub wheres: Vec<Evidence>,
}

/// The evidence a place in the code passes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    /// A use of a definition with bounds: the evidence of each.
    Ref(Vec<Evidence>),
    /// A call of a method, or the method taken as a value: the evidence
    /// that the receiver's type has the trait, and of each bound the
    /// trait declares for the method.
    Method { recv: Evidence, own: Vec<Evidence> },
}

/// How a compiled body numbers the bounds of the definition it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// A function or a generalised `let`: its scheme's trait predicates.
    Scheme,
    /// The method of an impl: the predicates of the impl's header, then
    /// the bounds the trait declares for the method.
    ImplMethod,
    /// A trait's default method: `Self: Trait`, then the bounds the
    /// trait declares for the method, then the `where` clauses the trait
    /// declares on its parameters.
    Default,
}

/// The dictionaries a definition the compiler compiles takes, before
/// its written parameters (`Program::dict_owners`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub layout: Layout,
    /// For a method of an impl: the predicates of the impl's header that
    /// pass a dictionary (reached through the impl's own dictionary).
    pub header: u16,
    /// For a function or a `let`: its bounds; for a method of an impl or
    /// a trait's default: the bounds the trait declares for the method
    /// (after the dictionary of the impl, the method's first).
    pub own: u16,
}
