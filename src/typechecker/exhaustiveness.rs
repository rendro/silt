//! Which values a set of patterns matches: the matrix algorithm of
//! "Warnings for pattern matching" (Maranget, JFP 2007).
//!
//! The pattern checker (`infer/pattern.rs`) lowers every pattern to a
//! [`Pat`]. Two questions are asked of the lowered patterns, and both are
//! the one question [`Search::covers`] answers: a `match` is exhaustive
//! when its unguarded arms cover every value, and a pattern is irrefutable
//! when it does so alone.

use super::inference::plural;
use super::*;

/// A pattern as the search sees it: what it tests, without names, spans
/// or types.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Pat {
    /// Matches every value: `_` or a name.
    Wild,
    /// A constructor applied to the patterns of its fields.
    Ctor(CtorId, Vec<Pat>),
    /// Matches what any of the alternatives matches.
    Or(Vec<Pat>),
    /// A test for some values of a type whose values cannot be listed: a
    /// float or string literal, a float range, a pin, a map pattern. No
    /// set of such tests covers the type, so which values one takes is
    /// not recorded.
    Lit,
    /// An integer from the first to the second, both included. A literal
    /// `n` is `n..n`.
    IntRange(i64, i64),
}

impl Pat {
    /// The list pattern with the patterns `elems` for its first elements
    /// and `tail` for the list of the others.
    pub(super) fn list(elems: Vec<Pat>, tail: Pat) -> Pat {
        // A pattern nested deeper than the search goes is not built: it
        // is one test, which is what a list pattern with elements is.
        if elems.len() > MAX_DEPTH {
            return Pat::Lit;
        }
        elems
            .into_iter()
            .rev()
            .fold(tail, |tail, elem| Pat::Ctor(CtorId::Cons, vec![elem, tail]))
    }

    /// The pattern of the empty list.
    pub(super) fn nil() -> Pat {
        Pat::Ctor(CtorId::Nil, Vec::new())
    }
}

/// A constructor of a pattern.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum CtorId {
    /// The one constructor of a tuple type; of `()` with no fields.
    Tuple,
    /// The one constructor of a record type, nominal or anonymous, with
    /// the names of the fields the pattern gives, in that order. Patterns
    /// of one column may name different fields; a field a pattern leaves
    /// out takes any value.
    Record(Vec<Symbol>),
    /// A variant of the enum, by its position in the declaration.
    Variant(TypeRef, usize),
    /// `true` or `false`.
    Bool(bool),
    /// The empty list.
    Nil,
    /// A list with a first element and the list of the others.
    Cons,
}

/// The search gave up at one of its bounds: nothing was shown either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unverified;

/// How many patterns one search may look at, over all the matrices it
/// builds: about a second of work in a debug build, a tenth of that in a
/// release build. A match of tens of thousands of arms stays far under
/// it; the bound stops a pattern set that would take exponential time.
const MAX_CELLS: usize = 32_000_000;

/// How deep one search may recurse: about one level for every
/// constructor on a path through a pattern.
const MAX_DEPTH: usize = 1_000;

static WILD: Pat = Pat::Wild;

/// A row of the matrix: one pattern for each column.
type Row<'p> = Vec<&'p Pat>;

/// The variants of an enum the search counts: a variant declared a
/// second time under a name (an error of the declaration) is none of its
/// own, since no pattern can name it.
fn distinct_variants(info: &EnumInfo) -> impl Iterator<Item = (usize, &VariantInfo)> {
    let mut seen = std::collections::HashSet::new();
    info.variants
        .iter()
        .enumerate()
        .filter(move |(_, variant)| seen.insert(variant.name))
}

/// One run of the matrix algorithm.
struct Search<'a> {
    enums: &'a HashMap<TypeRef, EnumInfo>,
    /// The patterns looked at so far.
    cells: usize,
}

impl<'a> Search<'a> {
    fn new(enums: &'a HashMap<TypeRef, EnumInfo>, cells: usize) -> Self {
        Search { enums, cells }
    }

    /// Whether the rows together match every row of values. The rows
    /// have one width.
    fn covers<'p>(&mut self, mut rows: Vec<Row<'p>>, depth: usize) -> Result<bool, Unverified> {
        // A row with a test that covers nothing takes part in no answer.
        rows.retain(|row| !row.iter().any(|pat| covers_nothing(pat)));
        let Some(first) = rows.first() else {
            return Ok(false);
        };
        if first.is_empty() {
            return Ok(true);
        }
        self.cells += rows.len() * first.len();
        if self.cells > MAX_CELLS || depth > MAX_DEPTH {
            return Err(Unverified);
        }
        // A row that takes every value answers alone.
        if rows
            .iter()
            .any(|row| row.iter().all(|pat| matches!(pat, Pat::Wild)))
        {
            return Ok(true);
        }
        // The column to split the values by: the first one the first row
        // tests. That row has to be looked at in any case, and rows
        // written to be read together (`(true, _, true)`,
        // `(true, _, false)`) are decided together, where splitting by
        // the columns from left to right would look at every combination
        // of the columns between them.
        let column = rows[0]
            .iter()
            .position(|pat| !matches!(pat, Pat::Wild))
            .unwrap_or(0);
        if column != 0 {
            for row in &mut rows {
                row.swap(0, column);
            }
        }

        let mut expanded: Vec<Row<'p>> = Vec::with_capacity(rows.len());
        for row in rows {
            expand_or(row, &mut expanded);
        }
        expanded.retain(|row| !covers_nothing(row[0]));
        // What the column tests: constructors, or integers.
        let firsts = || expanded.iter().map(|row| row[0]);
        let head = firsts()
            .find(|p| matches!(p, Pat::Ctor(..)))
            .or_else(|| firsts().find(|p| matches!(p, Pat::IntRange(..))));
        match head {
            Some(Pat::Ctor(id, _)) => {
                let Some(signature) = self.signature(id, &expanded) else {
                    return self.covers(default_rows(&expanded), depth + 1);
                };
                // The matrices shown to be covered. Two constructors
                // often leave the same rows (those of an or-pattern that
                // names both, and the rows that do not test the column):
                // the rows are looked at once.
                let mut covered: Vec<Vec<Row<'p>>> = Vec::new();
                for (ctor, arity) in &signature {
                    let rows = specialize(&expanded, ctor, *arity);
                    if covered.iter().any(|done| same_rows(done, &rows)) {
                        continue;
                    }
                    if !self.covers(rows.clone(), depth + 1)? {
                        return Ok(false);
                    }
                    covered.push(rows);
                }
                Ok(true)
            }
            Some(Pat::IntRange(..)) => self.covers_int_column(&expanded, depth),
            // No row tests the column after all (an or-pattern with an
            // alternative that takes every value).
            _ => self.covers(default_rows(&expanded), depth + 1),
        }
    }

    /// Every constructor of the type `id` is a constructor of, with its
    /// number of fields. `None` when the type is not known.
    fn signature(&self, id: &CtorId, rows: &[Row<'_>]) -> Option<Vec<(CtorId, usize)>> {
        Some(match id {
            CtorId::Tuple => {
                let arity = rows
                    .iter()
                    .filter_map(|row| match row[0] {
                        Pat::Ctor(CtorId::Tuple, args) => Some(args.len()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                vec![(CtorId::Tuple, arity)]
            }
            CtorId::Record(_) => {
                // The fields any row of the column names.
                let mut names: Vec<Symbol> = Vec::new();
                for row in rows {
                    if let Pat::Ctor(CtorId::Record(row_names), _) = row[0] {
                        for name in row_names {
                            if !names.contains(name) {
                                names.push(*name);
                            }
                        }
                    }
                }
                let arity = names.len();
                vec![(CtorId::Record(names), arity)]
            }
            CtorId::Variant(enum_ty, _) => distinct_variants(self.enums.get(enum_ty)?)
                .map(|(i, variant)| (CtorId::Variant(*enum_ty, i), variant.field_types.len()))
                .collect(),
            CtorId::Bool(_) => vec![(CtorId::Bool(true), 0), (CtorId::Bool(false), 0)],
            CtorId::Nil | CtorId::Cons => vec![(CtorId::Nil, 0), (CtorId::Cons, 2)],
        })
    }

    /// `covers` for rows whose first column tests integers. The ranges
    /// cut the integers into intervals no range divides. An interval no
    /// range holds is covered by the rows that do not test the column
    /// only, and they are rows of every other interval too: with such a
    /// gap they decide alone. Without one, every interval must be
    /// covered by the rows whose range holds it and those rows.
    fn covers_int_column<'p>(
        &mut self,
        rows: &[Row<'p>],
        depth: usize,
    ) -> Result<bool, Unverified> {
        let end = i128::from(i64::MAX) + 1;
        let mut ranges: Vec<(i128, i128)> = rows
            .iter()
            .filter_map(|row| match row[0] {
                Pat::IntRange(lo, hi) if lo <= hi => Some((i128::from(*lo), i128::from(*hi))),
                _ => None,
            })
            .collect();
        ranges.sort_unstable();
        // The first integer not known to be in a range.
        let mut next = i128::from(i64::MIN);
        for (lo, hi) in &ranges {
            if *lo > next {
                break;
            }
            next = next.max(hi + 1);
        }
        if next < end {
            return self.covers(default_rows(rows), depth + 1);
        }
        let mut cuts: Vec<i128> = ranges.iter().flat_map(|(lo, hi)| [*lo, hi + 1]).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let intervals: Vec<(i128, i128)> = cuts.windows(2).map(|w| (w[0], w[1] - 1)).collect();
        let holds = |row: &Row<'p>, from: i128, to: i128| match row[0] {
            Pat::IntRange(lo, hi) => i128::from(*lo) <= from && to <= i128::from(*hi),
            _ => false,
        };
        for (from, to) in intervals {
            let kept = rows
                .iter()
                .filter(|row| matches!(row[0], Pat::Wild) || holds(row, from, to))
                .map(|row| row[1..].to_vec())
                .collect();
            if !self.covers(kept, depth + 1)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Whether `pat` matches no value that could be counted on: a test whose
/// values are not recorded, an integer range without an integer, an
/// or-pattern without alternatives.
fn covers_nothing(pat: &Pat) -> bool {
    match pat {
        Pat::Lit => true,
        Pat::IntRange(lo, hi) => lo > hi,
        Pat::Or(alts) => alts.is_empty(),
        Pat::Wild | Pat::Ctor(..) => false,
    }
}

/// Whether two matrices are the same rows of the same patterns.
fn same_rows(a: &[Row<'_>], b: &[Row<'_>]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.len() == y.len() && x.iter().zip(y).all(|(p, q)| std::ptr::eq(*p, *q)))
}

/// Add `row` to `out`, as one row for each alternative of an or-pattern
/// in its first column.
fn expand_or<'p>(row: Row<'p>, out: &mut Vec<Row<'p>>) {
    match row[0] {
        Pat::Or(alts) => {
            for alt in alts {
                let mut expanded = row.clone();
                expanded[0] = alt;
                expand_or(expanded, out);
            }
        }
        _ => out.push(row),
    }
}

/// The rows that do not test their first column, without it.
fn default_rows<'p>(rows: &[Row<'p>]) -> Vec<Row<'p>> {
    rows.iter()
        .filter(|row| matches!(row[0], Pat::Wild))
        .map(|row| row[1..].to_vec())
        .collect()
}

/// The rows that match a value built with `ctor` in the first column,
/// with that column replaced by the `arity` columns of the constructor's
/// fields. The rows have no or-pattern in the first column.
fn specialize<'p>(rows: &[Row<'p>], ctor: &CtorId, arity: usize) -> Vec<Row<'p>> {
    let mut out = Vec::new();
    for row in rows {
        let mut fields: Row<'p> = match row[0] {
            Pat::Wild => vec![&WILD; arity],
            Pat::Ctor(CtorId::Record(names), args) => {
                let CtorId::Record(all) = ctor else {
                    continue;
                };
                all.iter()
                    .map(|name| {
                        names
                            .iter()
                            .position(|n| n == name)
                            .and_then(|i| args.get(i))
                            .unwrap_or(&WILD)
                    })
                    .collect()
            }
            Pat::Ctor(id, args) if id == ctor => {
                (0..arity).map(|i| args.get(i).unwrap_or(&WILD)).collect()
            }
            _ => continue,
        };
        fields.extend_from_slice(&row[1..]);
        out.push(fields);
    }
    out
}

impl TypeChecker {
    /// Whether `pat` matches every value of its type: whether a `match`
    /// with it as the only arm is exhaustive.
    #[cfg(test)]
    pub(super) fn irrefutable(&self, pat: &Pat) -> Result<bool, Unverified> {
        self.cover_together(&[pat], &mut 0)
    }

    /// Whether `pats`, each a pattern for the same value, match every
    /// value of its type between them. `cells` is the number of patterns
    /// the searches of the caller have looked at, this one included when
    /// it returns: searches that share it share one bound.
    pub(super) fn cover_together(
        &self,
        pats: &[&Pat],
        cells: &mut usize,
    ) -> Result<bool, Unverified> {
        let mut search = Search::new(&self.tables.enums, *cells);
        let answer = search.covers(pats.iter().map(|pat| vec![*pat]).collect(), 0);
        *cells = search.cells;
        answer
    }

    /// Whether a value built with the constructor `id` is every value of
    /// its type: a tuple, a record, the one variant of an enum.
    pub(super) fn stands_alone(&self, id: &CtorId) -> bool {
        match id {
            CtorId::Tuple | CtorId::Record(_) => true,
            CtorId::Variant(enum_ty, _) => self
                .tables
                .enums
                .get(enum_ty)
                .is_some_and(|info| distinct_variants(info).count() == 1),
            CtorId::Bool(_) | CtorId::Nil | CtorId::Cons => false,
        }
    }

    /// Whether `rows`, each a pattern for the same value, cover every
    /// value built with `ctor`, which has `arity` fields.
    fn covers_ctor(&self, rows: &[&Pat], ctor: &CtorId, arity: usize) -> Result<bool, Unverified> {
        let mut expanded = Vec::new();
        for row in rows {
            expand_or(vec![*row], &mut expanded);
        }
        Search::new(&self.tables.enums, 0).covers(specialize(&expanded, ctor, arity), 0)
    }

    /// Report a `match` whose arms leave a value of the scrutinee's type
    /// unmatched. `pats` are the lowered patterns of `arms`, one each.
    pub(super) fn check_exhaustiveness(
        &mut self,
        arms: &[MatchArm],
        pats: &[Pat],
        scrutinee_ty: &Type,
        span: Span,
    ) {
        // An arm with a guard may not be taken: it covers nothing.
        let rows: Vec<&Pat> = arms
            .iter()
            .zip(pats)
            .filter(|(arm, _)| arm.guard.is_none())
            .map(|(_, pat)| pat)
            .collect();

        // The type's aliases expanded, so that what is missing is
        // described by the variants or fields of the type itself.
        let scrutinee_ty = self.apply(scrutinee_ty);
        let scrutinee_ty =
            crate::types::canonical::canonicalize(&self.tables.resolver, &scrutinee_ty);

        // A type without values needs no arm: `match x { }` on an enum
        // without variants. The search takes every type to have a value,
        // so it is not asked.
        if arms.is_empty() && self.is_uninhabited(&scrutinee_ty) {
            return;
        }

        match self.cover_together(&rows, &mut 0) {
            Ok(true) => {}
            Ok(false) => {
                let msg = self.missing_description(&rows, &scrutinee_ty);
                self.error(
                    Code::NonExhaustive,
                    format!("non-exhaustive match: {msg}"),
                    span,
                );
            }
            Err(Unverified) => {
                self.error(
                    Code::NonExhaustive,
                    "could not verify that the match is exhaustive: its patterns are too \
                     large to analyse; add a wildcard arm (`_ -> ...`)",
                    span,
                );
            }
        }

        // Warn if ALL arms have guards.
        if !arms.is_empty() && arms.iter().all(|a| a.guard.is_some()) {
            self.warning(
                Code::NonExhaustive,
                "match may be non-exhaustive: all arms have guards",
                span,
            );
        }
    }

    /// The enum that owns the variant a constructor pattern names, with
    /// its type (see `pattern_variant_enum`).
    pub(super) fn pattern_constructor_enum(
        &self,
        pattern: &Pattern,
    ) -> Option<(TypeRef, &EnumInfo)> {
        let PatternKind::Constructor { qualifier, .. } = &pattern.kind else {
            return None;
        };
        let enum_ty = self.pattern_variant_enum(pattern.res, qualifier)?;
        let info = self.tables.enums.get(&enum_ty)?;
        Some((enum_ty, info))
    }

    /// Whether `ty` has no value. Recognised: an enum without variants,
    /// and `Never`. A composite type with such a part (a tuple, a record)
    /// is not looked into: taking a type with values for an empty one
    /// would accept a match that misses them.
    pub(super) fn is_uninhabited(&self, ty: &Type) -> bool {
        match ty {
            Type::Generic(name, _) => self
                .tables
                .enums
                .get(name)
                .is_some_and(|info| info.variants.is_empty()),
            Type::Never => true,
            _ => false,
        }
    }

    /// What the rows of a non-exhaustive match on a value of type `ty`
    /// leave out, for the diagnostic: the variants of an enum, the values
    /// of a `Bool`, or the first field of a record whose own patterns
    /// leave something out.
    fn missing_description(&self, rows: &[&Pat], ty: &Type) -> std::string::String {
        const UNSPECIFIC: &str = "not all patterns are covered";
        let missing =
            |ctor: CtorId, arity: usize| matches!(self.covers_ctor(rows, &ctor, arity), Ok(false));
        match ty {
            Type::Bool => {
                let values: Vec<&str> = [(true, "true"), (false, "false")]
                    .into_iter()
                    .filter(|(value, _)| missing(CtorId::Bool(*value), 0))
                    .map(|(_, name)| name)
                    .collect();
                if values.is_empty() {
                    UNSPECIFIC.into()
                } else {
                    format!("missing {}", values.join(", "))
                }
            }
            Type::Generic(name, type_args) => {
                if let Some(enum_info) = self.tables.enums.get(name) {
                    let variants: Vec<std::string::String> = distinct_variants(enum_info)
                        .filter(|(i, variant)| {
                            missing(CtorId::Variant(*name, *i), variant.field_types.len())
                        })
                        .map(|(_, variant)| variant.name.to_string())
                        .collect();
                    if variants.is_empty() {
                        UNSPECIFIC.into()
                    } else {
                        format!(
                            "missing {} {}",
                            plural(variants.len(), "variant", "variants"),
                            variants.join(", ")
                        )
                    }
                } else if let Some(rec_info) = self.tables.records.get(name) {
                    // A record type as a signature names it.
                    let mapping: HashMap<TyVar, Type> = self
                        .tables
                        .record_param_var_ids
                        .get(name)
                        .filter(|ids| ids.len() == type_args.len())
                        .map(|ids| ids.iter().copied().zip(type_args.iter().cloned()).collect())
                        .unwrap_or_default();
                    let fields: Vec<(Symbol, Type)> = rec_info
                        .fields
                        .iter()
                        .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                        .collect();
                    self.missing_description(rows, &Type::Record(*name, fields))
                } else {
                    UNSPECIFIC.into()
                }
            }
            Type::Record(rec_name, rec_fields) => {
                let mut expanded = Vec::new();
                for row in rows {
                    expand_or(vec![*row], &mut expanded);
                }
                for (fname, fty) in rec_fields {
                    // The patterns the rows give this field, each field
                    // looked at alone.
                    let column: Vec<&Pat> = specialize(&expanded, &CtorId::Record(vec![*fname]), 1)
                        .into_iter()
                        .map(|row| row[0])
                        .collect();
                    let child = self.missing_description(&column, fty);
                    if child != UNSPECIFIC {
                        return format!("in {rec_name}.{fname}: {child}");
                    }
                }
                format!("not all patterns of {rec_name} are covered")
            }
            _ => UNSPECIFIC.into(),
        }
    }
}

#[cfg(test)]
mod tests;
