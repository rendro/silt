//! Trait evidence: for each trait predicate the checker solves, what
//! proves it. The compiler and the VM dispatch as they always have; in
//! a debug build the VM checks at each call of a method it finds where
//! the code runs that the impl it finds is the one the evidence names
//! (`vm::evidence`), so that a checker and a runtime that disagree are
//! found before anything is compiled from evidence.
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
    /// The impl of `tr` for the head `ty`, with the evidence of each
    /// predicate its header asks (`where a: Greet`), in order.
    Impl {
        tr: TraitId,
        ty: TypeId,
        subs: Vec<Evidence>,
    },
    /// The bound `k` of the enclosing definition `owner`.
    Param { owner: Span, k: u16 },
    /// The supertrait `to` of what `of` proves: the impl of `to` for
    /// the same type.
    Super { of: Box<Evidence>, to: TraitId },
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
    /// trait declares for the method.
    Default,
}

/// What a compiled function's code knows of evidence.
#[derive(Debug, Clone, Default)]
pub struct FnEvidence {
    /// The definition the function is, named as [`Evidence::Param`]
    /// names it, and how it numbers its bounds.
    pub owner: Option<(Span, Layout)>,
    /// The evidence each instruction passes on, by the offset just past
    /// it, in the order of the code.
    pub sites: Vec<(u32, Site)>,
}

impl FnEvidence {
    /// The evidence the instruction that ends at `at` passes on.
    pub fn site(&self, at: usize) -> Option<&Site> {
        let at = u32::try_from(at).ok()?;
        let k = self.sites.binary_search_by_key(&at, |(end, _)| *end).ok()?;
        Some(&self.sites[k].1)
    }
}
