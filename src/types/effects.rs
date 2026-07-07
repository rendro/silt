//! Effect sets attached to function schemes.
//!
//! Phase A of the effect-rows proposal: an internal data type that tracks
//! which effects a function performs. The end-to-end story (parser surface
//! syntax `!{io, fs}`, LSP hover rendering, stdlib annotation, strict
//! enforcement) lands in later phases; this module is plumbing only.
//!
//! A fixed vocabulary of five flat effects — `Io`, `Fs`, `Net`, `Time`,
//! `Random` — packed into a `u8` bitset. `EffectSet::TOP` is the
//! permissive "any effect" default used by un-annotated code during the
//! gradual rollout; `EffectSet::EMPTY` is a fully pure function. The two
//! are deliberately distinct so diagnostics can render the gradual default
//! as `!*` (a recognisable token) rather than have it collide with the
//! "all-five-set" full-effect form.
//!
//! See `docs/proposals/effect-rows.md` for the design rationale.

use std::fmt;

/// One of silt's five tracked effects.
///
/// `repr(u8)` with explicit discriminants pins the bit positions used by
/// `EffectSet`'s bitset; reordering the variants would silently change
/// every previously-stored set's meaning. The value is the bit index, not
/// the bitmask — `1 << (e as u8)` produces the bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Effect {
    /// Catch-all OS-resource effect. Co-occurs with a refinement (`Fs`,
    /// `Net`, `Time`, `Random`) for code that touches one of those
    /// specific axes.
    Io = 0,
    /// Filesystem read/write.
    Fs = 1,
    /// Network communication (TCP, HTTP, Postgres, …).
    Net = 2,
    /// Wall-clock reads.
    Time = 3,
    /// OS-entropy reads.
    Random = 4,
}

impl Effect {
    /// All five effects in canonical (alphabetic) order, used by
    /// `EffectSet::iter` and `Display` to produce deterministic output.
    /// Alphabetic was chosen over enum-declaration order so users reading
    /// `!{fs, io, net}` in a hover tip see a stable, sortable layout
    /// regardless of how the code happened to discover the effects.
    const ALL_ALPHABETIC: [Effect; 5] = [
        Effect::Fs,
        Effect::Io,
        Effect::Net,
        Effect::Random,
        Effect::Time,
    ];

    /// Lower-case name as it will appear in source syntax (Phase B) and
    /// in diagnostics. `Display` for `Effect` delegates here.
    pub const fn name(self) -> &'static str {
        match self {
            Effect::Io => "io",
            Effect::Fs => "fs",
            Effect::Net => "net",
            Effect::Time => "time",
            Effect::Random => "random",
        }
    }

    /// Bitmask for this effect. `Effect::Io` → `0b00001`, etc.
    #[inline]
    const fn bit(self) -> u8 {
        1u8 << (self as u8)
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Bitset of effects.
///
/// `EffectSet::TOP` means "any effect" — the permissive default applied
/// to un-annotated functions during the Phase B-D gradual rollout.
/// `EffectSet::EMPTY` is a fully pure function. Both render with a
/// distinct token (`!*` vs `!{}`) so diagnostics make the
/// "no constraint declared" state visible, rather than silently
/// collapsing it onto the "all five effects" form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectSet(u8);

impl EffectSet {
    /// The pure (no effects) set. Renders as `!{}`.
    pub const EMPTY: EffectSet = EffectSet(0);

    /// Bitmask of all five effects (`io | fs | net | time | random`).
    /// Used internally by `TOP`; not exposed because callers should use
    /// `TOP` (which has a distinct Display form `!*`) for the
    /// "any effect" default.
    const ALL_BITS: u8 = 0b0001_1111;

    /// "Any effect" — the permissive default for un-annotated functions
    /// during the gradual rollout. Renders as `!*`. Internally stores
    /// the same bitset as "all five effects set" but is treated as a
    /// distinct value at the Display layer so diagnostics can show
    /// "we didn't constrain this" separately from
    /// "we proved this touches every effect".
    ///
    /// At inference time `TOP` behaves exactly as the union of all five
    /// effects: it absorbs any other set under `union`, every set is a
    /// subset of it, and `iter` yields all five. The Display
    /// distinction is the only behavioural difference.
    pub const TOP: EffectSet = EffectSet(Self::ALL_BITS);

    /// Construct a singleton set holding just `e`.
    #[inline]
    pub const fn singleton(e: Effect) -> Self {
        EffectSet(e.bit())
    }

    // ── Convenience constructors for the Phase C stdlib sweep ─────
    //
    // The stdlib registration sites build effect sets from a small
    // fixed vocabulary: pure (`!{}`), `!{io}`, `!{io, fs}`,
    // `!{io, net}`, `!{io, time}`, `!{io, random}`, plus the
    // uuid.v7 special `!{io, time, random}`. These helpers let
    // every site read at a glance ("io_fs", "io_net", …) instead of
    // chained `singleton(...).insert(...)` calls.

    /// Pure (no effects). Same as `EffectSet::EMPTY` — exposed as a
    /// fn for parity with the other `io_*` constructors so the
    /// builtin sites read uniformly.
    #[inline]
    pub const fn pure() -> Self {
        Self::EMPTY
    }

    /// Just `!{io}` — printing, env-var reads, anything that touches
    /// the OS without a more specific refinement.
    #[inline]
    pub const fn io() -> Self {
        Self::singleton(Effect::Io)
    }

    /// `!{io, fs}` — every filesystem read/write.
    #[inline]
    pub const fn io_fs() -> Self {
        Self::io().insert(Effect::Fs)
    }

    /// `!{io, net}` — every network call (TCP, HTTP, Postgres).
    #[inline]
    pub const fn io_net() -> Self {
        Self::io().insert(Effect::Net)
    }

    /// `!{io, time}` — wall-clock reads (`time.now`, `time.today`,
    /// `time.to_utc`, `time.format_now`).
    #[inline]
    pub const fn io_time() -> Self {
        Self::io().insert(Effect::Time)
    }

    /// `!{io, random}` — OS entropy reads (`math.random`,
    /// `uuid.v4`, `crypto.random_bytes`, `crypto.gen_*`).
    #[inline]
    pub const fn io_random() -> Self {
        Self::io().insert(Effect::Random)
    }

    /// `!{io, time, random}` — `uuid.v7` (timestamp + entropy).
    /// The only stdlib operation that combines two refinements.
    #[inline]
    pub const fn io_time_random() -> Self {
        Self::io_time().insert(Effect::Random)
    }

    /// `true` iff `e` is in this set.
    #[inline]
    pub const fn contains(self, e: Effect) -> bool {
        (self.0 & e.bit()) != 0
    }

    /// Return a new set with `e` added (no-op if already present).
    #[inline]
    pub const fn insert(self, e: Effect) -> Self {
        EffectSet(self.0 | e.bit())
    }

    /// Set union — used by inference to combine effects from
    /// sub-expressions.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        EffectSet(self.0 | other.0)
    }

    /// `true` iff every bit in `self` is also set in `other`.
    /// `EMPTY.is_subset(anything) == true`. `anything.is_subset(TOP) ==
    /// true` (TOP carries every bit). `TOP.is_subset(EMPTY) == false`.
    #[inline]
    pub const fn is_subset(self, other: Self) -> bool {
        (self.0 & !other.0) == 0
    }

    /// `true` iff this set holds no effects.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate the effects in alphabetic order. Stable across calls
    /// regardless of insertion order.
    pub fn iter(self) -> impl Iterator<Item = Effect> {
        Effect::ALL_ALPHABETIC
            .into_iter()
            .filter(move |e| self.contains(*e))
    }

    /// Count of distinct effects in the set.
    ///
    /// Round-75 DEAD-6 fix: gated behind `cfg(any(test, feature =
    /// "test-hooks"))` because every production caller has been
    /// retired — the only callers live in this module's own
    /// `#[cfg(test)] mod tests`. Keeping the helper available under
    /// `test-hooks` lets downstream lock tests still introspect the
    /// set's cardinality without bloating the public production API.
    #[cfg(any(test, feature = "test-hooks"))]
    #[inline]
    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }
}

impl Default for EffectSet {
    /// The default for un-annotated code is `TOP` — the gradual-rollout
    /// permissive default. A defaulted-to-EMPTY would silently mark
    /// every legacy function pure and break round-trip inference;
    /// defaulting to TOP keeps Phase A backwards-compatible.
    fn default() -> Self {
        EffectSet::TOP
    }
}

impl fmt::Display for EffectSet {
    /// Renders as one of three forms:
    /// - `!*` for `TOP` (gradual-rollout default).
    /// - `!{}` for `EMPTY` (fully pure).
    /// - `!{a, b, c}` otherwise, alphabetically ordered.
    ///
    /// The `TOP` vs "all five effects via union" distinction is
    /// preserved at the Display layer only — internally both are the
    /// same bitset.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Distinguish TOP from a hand-built all-five set by identity:
        // only the const TOP renders as `!*`. A set that happens to
        // hold all five via repeated insertions will render explicitly.
        // We approximate this by comparing the bitset; for Phase A this
        // is fine because the inference pass never explicitly inserts
        // all five effects — it either propagates TOP from a builtin or
        // unions singletons. The choice is documented so Phase B can
        // refine it if a real inference path produces "all five via
        // union" and we want to render that distinctly.
        if *self == EffectSet::TOP {
            return f.write_str("!*");
        }
        if self.is_empty() {
            return f.write_str("!{}");
        }
        f.write_str("!{")?;
        for (i, e) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{e}")?;
        }
        f.write_str("}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_contains_no_effects() {
        let s = EffectSet::EMPTY;
        assert!(!s.contains(Effect::Io));
        assert!(!s.contains(Effect::Fs));
        assert!(!s.contains(Effect::Net));
        assert!(!s.contains(Effect::Time));
        assert!(!s.contains(Effect::Random));
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn singleton_contains_only_its_effect() {
        let s = EffectSet::singleton(Effect::Io);
        assert!(s.contains(Effect::Io));
        assert!(!s.contains(Effect::Fs));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn insert_is_idempotent() {
        let s = EffectSet::singleton(Effect::Io);
        let t = s.insert(Effect::Io);
        assert_eq!(s, t);
    }

    #[test]
    fn insert_adds_a_distinct_effect() {
        let s = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
        assert!(s.contains(Effect::Io));
        assert!(s.contains(Effect::Fs));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn union_combines_two_sets() {
        let a = EffectSet::singleton(Effect::Io);
        let b = EffectSet::singleton(Effect::Fs);
        let u = a.union(b);
        assert!(u.contains(Effect::Io));
        assert!(u.contains(Effect::Fs));
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn union_with_empty_is_identity() {
        let a = EffectSet::singleton(Effect::Net);
        assert_eq!(a.union(EffectSet::EMPTY), a);
        assert_eq!(EffectSet::EMPTY.union(a), a);
    }

    #[test]
    fn union_with_top_is_top() {
        let a = EffectSet::singleton(Effect::Net);
        assert_eq!(a.union(EffectSet::TOP), EffectSet::TOP);
        assert_eq!(EffectSet::TOP.union(a), EffectSet::TOP);
    }

    #[test]
    fn is_subset_empty_is_subset_of_everything() {
        assert!(EffectSet::EMPTY.is_subset(EffectSet::EMPTY));
        assert!(EffectSet::EMPTY.is_subset(EffectSet::singleton(Effect::Io)));
        assert!(EffectSet::EMPTY.is_subset(EffectSet::TOP));
    }

    #[test]
    fn is_subset_top_only_subset_of_top() {
        assert!(EffectSet::TOP.is_subset(EffectSet::TOP));
        assert!(!EffectSet::TOP.is_subset(EffectSet::EMPTY));
        assert!(!EffectSet::TOP.is_subset(EffectSet::singleton(Effect::Io)));
    }

    #[test]
    fn is_subset_singleton_proper() {
        let io = EffectSet::singleton(Effect::Io);
        let io_fs = io.insert(Effect::Fs);
        assert!(io.is_subset(io_fs));
        assert!(!io_fs.is_subset(io));
    }

    #[test]
    fn iter_yields_alphabetic_order() {
        // Insert in non-alphabetic order; expect alphabetic out.
        let s = EffectSet::singleton(Effect::Time)
            .insert(Effect::Io)
            .insert(Effect::Fs);
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Effect::Fs, Effect::Io, Effect::Time]);
    }

    #[test]
    fn iter_on_empty_yields_nothing() {
        assert_eq!(EffectSet::EMPTY.iter().count(), 0);
    }

    #[test]
    fn iter_on_top_yields_all_five() {
        let collected: Vec<_> = EffectSet::TOP.iter().collect();
        assert_eq!(collected.len(), 5);
        assert!(collected.contains(&Effect::Io));
        assert!(collected.contains(&Effect::Fs));
        assert!(collected.contains(&Effect::Net));
        assert!(collected.contains(&Effect::Time));
        assert!(collected.contains(&Effect::Random));
    }

    /// Round-100 parity lock: the `Effect` enum has THREE independent
    /// hand-maintained mirrors that must stay in sync but were derived
    /// from nothing and locked by nothing:
    ///   1. `Effect::ALL_ALPHABETIC` (the array `EffectSet::iter` filters)
    ///      — a variant missing here is never iterated or Displayed.
    ///   2. `EffectSet::ALL_BITS` (the mask backing `TOP`) — a variant not
    ///      in the mask makes `TOP` no longer the true top, a soundness
    ///      break in `is_subset` / `union` and `--strict-effects`.
    ///   3. the parser's effect-name match (`src/parser.rs`) — covered by
    ///      a name round-trip below.
    ///
    /// The `match` over every variant has NO wildcard arm, so adding a new
    /// `Effect` variant forces this test to be updated (compile error),
    /// and the assertions then guarantee the array, the mask, and the
    /// name-set were all updated to match.
    #[test]
    fn effect_enum_mirrors_stay_in_sync() {
        // No-wildcard census: the compiler forces this list to cover every
        // variant. A new variant breaks compilation here until added.
        fn census(e: Effect) -> &'static str {
            match e {
                Effect::Io => "io",
                Effect::Fs => "fs",
                Effect::Net => "net",
                Effect::Time => "time",
                Effect::Random => "random",
            }
        }
        const ALL: [Effect; 5] = [
            Effect::Io,
            Effect::Fs,
            Effect::Net,
            Effect::Time,
            Effect::Random,
        ];
        let n = ALL.len();

        // Mirror 1: the alphabetic array must list every variant exactly once.
        assert_eq!(
            Effect::ALL_ALPHABETIC.len(),
            n,
            "Effect::ALL_ALPHABETIC must list every Effect variant"
        );
        for e in ALL {
            assert!(
                Effect::ALL_ALPHABETIC.contains(&e),
                "Effect::ALL_ALPHABETIC is missing {e:?} — it would never be \
                 iterated or Displayed"
            );
        }
        // ...and `TOP` (which filters that array) yields all of them.
        assert_eq!(EffectSet::TOP.iter().count(), n);

        // Mirror 2: the bitmask must have exactly one bit per variant.
        assert_eq!(
            EffectSet::ALL_BITS.count_ones() as usize,
            n,
            "EffectSet::ALL_BITS must set exactly one bit per Effect variant; \
             a missing bit makes TOP not the true top"
        );
        for e in ALL {
            assert!(
                EffectSet::TOP.contains(e),
                "EffectSet::TOP must contain {e:?} (ALL_BITS drift)"
            );
        }

        // Mirror 3: every variant's source name must round-trip through the
        // parser back to the singleton set for that variant, so the
        // parser's name match and its "valid effects are: …" error list
        // cannot silently drift from the enum.
        for e in ALL {
            let name = census(e);
            assert_eq!(name, e.name(), "census/name disagreement for {e:?}");
            let src = format!("fn f() -> Int !{{{name}}} = 0");
            let tokens = crate::lexer::Lexer::new(&src)
                .tokenize()
                .unwrap_or_else(|err| panic!("lex {name}: {err:?}"));
            let program = crate::parser::Parser::new(tokens)
                .parse_program()
                .unwrap_or_else(|err| panic!("parse effect name {name:?}: {err:?}"));
            let declared = program
                .decls
                .iter()
                .find_map(|d| match d {
                    crate::ast::Decl::Fn(f) => Some(f.declared_effects),
                    _ => None,
                })
                .expect("a fn decl");
            assert_eq!(
                declared,
                EffectSet::singleton(e),
                "effect name {name:?} did not parse back to a singleton {e:?} \
                 set — the parser's effect-name match drifted from the Effect \
                 enum"
            );
        }
    }

    #[test]
    fn display_empty_is_brace_pair() {
        assert_eq!(EffectSet::EMPTY.to_string(), "!{}");
    }

    #[test]
    fn display_top_is_star() {
        assert_eq!(EffectSet::TOP.to_string(), "!*");
    }

    #[test]
    fn display_singleton() {
        assert_eq!(EffectSet::singleton(Effect::Io).to_string(), "!{io}");
        assert_eq!(EffectSet::singleton(Effect::Fs).to_string(), "!{fs}");
    }

    #[test]
    fn display_two_effects_alphabetic() {
        let s = EffectSet::singleton(Effect::Io).insert(Effect::Fs);
        assert_eq!(s.to_string(), "!{fs, io}");
    }

    #[test]
    fn display_three_effects_alphabetic() {
        let s = EffectSet::singleton(Effect::Time)
            .insert(Effect::Io)
            .insert(Effect::Net);
        assert_eq!(s.to_string(), "!{io, net, time}");
    }

    #[test]
    fn default_is_top() {
        assert_eq!(EffectSet::default(), EffectSet::TOP);
    }

    #[test]
    fn effect_name_matches_display() {
        for e in [
            Effect::Io,
            Effect::Fs,
            Effect::Net,
            Effect::Time,
            Effect::Random,
        ] {
            assert_eq!(e.to_string(), e.name());
        }
    }

    #[test]
    fn effect_bit_positions_unique() {
        // Locks the repr(u8) discriminants. Reordering Effect would
        // silently change the bitmask of every persisted set.
        assert_eq!(Effect::Io as u8, 0);
        assert_eq!(Effect::Fs as u8, 1);
        assert_eq!(Effect::Net as u8, 2);
        assert_eq!(Effect::Time as u8, 3);
        assert_eq!(Effect::Random as u8, 4);
    }
}
