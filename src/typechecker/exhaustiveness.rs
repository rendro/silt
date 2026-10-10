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

/// A value no row matches, as far as the search tells values apart.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Witness {
    /// Any value: nothing more had to be said of it.
    Any,
    /// A value built with the constructor, of these fields.
    Ctor(CtorId, Vec<Witness>),
    /// The integer.
    Int(i64),
}

impl Witness {
    /// Whether the value is told apart from others of its type: a tuple
    /// or a record of parts nothing is said of is any value of its type.
    fn says_something(&self) -> bool {
        match self {
            Witness::Any => false,
            Witness::Int(_) => true,
            Witness::Ctor(CtorId::Tuple | CtorId::Record(_), fields) => {
                fields.iter().any(Witness::says_something)
            }
            Witness::Ctor(..) => true,
        }
    }
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

    /// Whether the rows together match every row of `width` values. The
    /// rows have that width.
    fn covers(
        &mut self,
        rows: Vec<Row<'_>>,
        width: usize,
        depth: usize,
    ) -> Result<bool, Unverified> {
        Ok(self.uncovered(rows, width, depth)?.is_none())
    }

    /// A row of `width` values that none of the rows matches, or `None`
    /// when the rows together match every one. The rows have that
    /// width. This is the one search: `covers` asks it whether there is
    /// such a row, a diagnostic shows the row.
    fn uncovered<'p>(
        &mut self,
        mut rows: Vec<Row<'p>>,
        width: usize,
        depth: usize,
    ) -> Result<Option<Vec<Witness>>, Unverified> {
        // A row with a test that covers nothing takes part in no answer.
        rows.retain(|row| !row.iter().any(|pat| covers_nothing(pat)));
        if rows.is_empty() {
            return Ok(Some(vec![Witness::Any; width]));
        }
        if width == 0 {
            return Ok(None);
        }
        self.cells += rows.len() * width;
        if self.cells > MAX_CELLS || depth > MAX_DEPTH {
            return Err(Unverified);
        }
        // A row that takes every value answers alone.
        if rows
            .iter()
            .any(|row| row.iter().all(|pat| matches!(pat, Pat::Wild)))
        {
            return Ok(None);
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
        // Of a value the rows left do not test, nothing is said.
        let untested = |search: &mut Self| {
            let rest = search.uncovered(default_rows(&expanded), width - 1, depth + 1)?;
            Ok(rest.map(|rest| with_first(Witness::Any, rest)))
        };
        let found = match head {
            Some(Pat::Ctor(id, _)) => match self.signature(id, &expanded) {
                None => untested(self)?,
                Some(signature) => {
                    // The matrices shown to be covered. Two constructors
                    // often leave the same rows (those of an or-pattern
                    // that names both, and the rows that do not test the
                    // column): the rows are looked at once.
                    let mut covered: Vec<Vec<Row<'p>>> = Vec::new();
                    let mut found = None;
                    for (ctor, arity) in &signature {
                        let rows = specialize(&expanded, ctor, *arity);
                        if covered.iter().any(|done| same_rows(done, &rows)) {
                            continue;
                        }
                        let fields = self.uncovered(rows.clone(), arity + width - 1, depth + 1)?;
                        if let Some(mut fields) = fields {
                            let rest = fields.split_off(*arity);
                            found = Some(with_first(Witness::Ctor(ctor.clone(), fields), rest));
                            break;
                        }
                        covered.push(rows);
                    }
                    found
                }
            },
            Some(Pat::IntRange(..)) => self.uncovered_int_column(&expanded, width, depth)?,
            // No row tests the column after all (an or-pattern with an
            // alternative that takes every value).
            _ => untested(self)?,
        };
        Ok(found.map(|mut row| {
            row.swap(0, column);
            row
        }))
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

    /// `uncovered` for rows whose first column tests integers. The
    /// ranges cut the integers into intervals no range divides. An
    /// interval no range holds is covered by the rows that do not test
    /// the column only, and they are rows of every other interval too:
    /// with such a gap they decide alone. Without one, every interval
    /// must be covered by the rows whose range holds it and those rows.
    fn uncovered_int_column<'p>(
        &mut self,
        rows: &[Row<'p>],
        width: usize,
        depth: usize,
    ) -> Result<Option<Vec<Witness>>, Unverified> {
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
            let rest = self.uncovered(default_rows(rows), width - 1, depth + 1)?;
            return Ok(rest.map(|rest| with_first(Witness::Int(outside(&ranges)), rest)));
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
            if let Some(rest) = self.uncovered(kept, width - 1, depth + 1)? {
                // The integer of the interval nearest to zero.
                let shown = 0.clamp(from, to) as i64;
                return Ok(Some(with_first(Witness::Int(shown), rest)));
            }
        }
        Ok(None)
    }
}

/// `first` and then `rest`.
fn with_first(first: Witness, rest: Vec<Witness>) -> Vec<Witness> {
    let mut row = Vec::with_capacity(rest.len() + 1);
    row.push(first);
    row.extend(rest);
    row
}

/// An integer in none of the `ranges` (sorted, and leaving one out): the
/// one nearest to zero, a positive one before a negative one.
fn outside(ranges: &[(i128, i128)]) -> i64 {
    let mut up: i128 = 0;
    for (lo, hi) in ranges {
        if *lo <= up && up <= *hi {
            up = hi + 1;
        }
    }
    if let Ok(up) = i64::try_from(up) {
        return up;
    }
    let mut down: i128 = -1;
    for (lo, hi) in ranges.iter().rev() {
        if *lo <= down && down <= *hi {
            down = lo - 1;
        }
    }
    down as i64
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
        let answer = search.covers(pats.iter().map(|pat| vec![*pat]).collect(), 1, 0);
        *cells = search.cells;
        answer
    }

    /// A value none of `pats`, each a pattern for the same value,
    /// matches; `None` when there is none, or none was found within the
    /// search's bounds.
    fn uncovered_by(&self, pats: &[&Pat]) -> Option<Witness> {
        let rows = pats.iter().map(|pat| vec![*pat]).collect();
        let found = Search::new(&self.tables.enums, 0).uncovered(rows, 1, 0);
        found.ok().flatten()?.pop()
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

    /// The fields of a value built with `ctor`, which has `arity` fields,
    /// that none of `rows` matches, each a pattern for the same value;
    /// `None` when they cover every such value (or the search gave up).
    /// With it, whether any row matches a value built with `ctor` at all.
    fn uncovered_of_ctor(
        &self,
        rows: &[&Pat],
        ctor: &CtorId,
        arity: usize,
    ) -> Option<(Vec<Witness>, bool)> {
        let mut expanded = Vec::new();
        for row in rows {
            expand_or(vec![*row], &mut expanded);
        }
        let rows = specialize(&expanded, ctor, arity);
        let matched = !rows.is_empty();
        let found = Search::new(&self.tables.enums, 0).uncovered(rows, arity, 0);
        Some((found.ok().flatten()?, matched))
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
    /// its type.
    pub(super) fn pattern_constructor_enum(
        &self,
        pattern: &Pattern,
    ) -> Option<(TypeRef, &EnumInfo)> {
        if !matches!(pattern.kind, PatternKind::Constructor { .. }) {
            return None;
        }
        let enum_ty = self.res_variant_enum(pattern.res)?;
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
    /// leave out, for the diagnostic: the variants of an enum no row
    /// names, the values of a `Bool`, and otherwise a value no row
    /// matches (`Some(None)`, `(true, false)`, `[_, _, .._]`, `2`,
    /// `R { p: false, .. }`).
    fn missing_description(&self, rows: &[&Pat], ty: &Type) -> std::string::String {
        const UNSPECIFIC: &str = "not all patterns are covered";
        match ty {
            Type::Bool => {
                let values: Vec<&str> = [(true, "true"), (false, "false")]
                    .into_iter()
                    .filter(|(value, _)| {
                        self.uncovered_of_ctor(rows, &CtorId::Bool(*value), 0)
                            .is_some()
                    })
                    .map(|(_, name)| name)
                    .collect();
                if values.is_empty() {
                    UNSPECIFIC.into()
                } else {
                    format!("missing {}", values.join(", "))
                }
            }
            Type::Generic(name, _) => {
                if let Some(enum_info) = self.tables.enums.get(name) {
                    // A variant no row names is missing. Of a variant
                    // some rows name, a value they leave out is shown;
                    // when the search tells no such value apart (the
                    // rows test strings, say), that it is covered in
                    // part.
                    let (mut missing, mut left_out, mut in_part) =
                        (Vec::new(), Vec::new(), Vec::new());
                    for (i, variant) in distinct_variants(enum_info) {
                        let ctor = CtorId::Variant(*name, i);
                        let arity = variant.field_types.len();
                        let Some((fields, matched)) = self.uncovered_of_ctor(rows, &ctor, arity)
                        else {
                            continue;
                        };
                        let told_apart = fields.iter().any(Witness::says_something);
                        let shown =
                            format!("`{}`", self.show_witness(&Witness::Ctor(ctor, fields), ty));
                        match (matched, told_apart) {
                            (false, _) => missing.push(variant.name.to_string()),
                            (true, true) => left_out.push(shown),
                            (true, false) => in_part.push(shown),
                        }
                    }
                    let mut parts: Vec<std::string::String> = Vec::new();
                    if !missing.is_empty() {
                        parts.push(format!(
                            "missing {} {}",
                            plural(missing.len(), "variant", "variants"),
                            missing.join(", ")
                        ));
                    }
                    if !left_out.is_empty() {
                        parts.push(format!(
                            "{} {} not covered",
                            left_out.join(", "),
                            plural(left_out.len(), "is", "are")
                        ));
                    }
                    if !in_part.is_empty() {
                        parts.push(format!(
                            "{} {} covered only in part",
                            in_part.join(", "),
                            plural(in_part.len(), "is", "are")
                        ));
                    }
                    if parts.is_empty() {
                        UNSPECIFIC.into()
                    } else {
                        parts.join("; ")
                    }
                } else if self.tables.records.contains_key(name) {
                    // A value of the record no row matches, whole; where
                    // none can be written (the rows test strings, say),
                    // that the record is not covered.
                    match self.uncovered_by(rows) {
                        Some(value) if value.says_something() => {
                            format!("`{}` is not covered", self.show_witness(&value, ty))
                        }
                        _ => {
                            let rec_name = self.show_type(&Type::Generic(*name, Vec::new()));
                            format!("not all patterns of {rec_name} are covered")
                        }
                    }
                } else {
                    UNSPECIFIC.into()
                }
            }
            _ => match self.uncovered_by(rows) {
                Some(value) if value.says_something() => {
                    format!("`{}` is not covered", self.show_witness(&value, ty))
                }
                _ => UNSPECIFIC.into(),
            },
        }
    }

    /// The value `value` of type `ty` as a pattern would write it. A
    /// part nothing is said of is `_`; a list of which only the first
    /// elements are said ends in `.._`; a declared record that leaves
    /// fields out ends in `..`, an anonymous one names the fields it
    /// tests.
    fn show_witness(&self, value: &Witness, ty: &Type) -> std::string::String {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let (ctor, fields) = match value {
            Witness::Any => return "_".into(),
            Witness::Int(n) => return n.to_string(),
            Witness::Ctor(ctor, fields) => (ctor, fields),
        };
        let unknown = Type::Error;
        match ctor {
            CtorId::Bool(value) => value.to_string(),
            CtorId::Tuple => {
                let types: &[Type] = match &ty {
                    Type::Tuple(types) => types,
                    _ => &[],
                };
                let shown: Vec<_> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| self.show_witness(field, types.get(i).unwrap_or(&unknown)))
                    .collect();
                format!("({})", shown.join(", "))
            }
            CtorId::Nil | CtorId::Cons => {
                let elem = match &ty {
                    Type::List(elem) => elem,
                    _ => &unknown,
                };
                let mut shown: Vec<std::string::String> = Vec::new();
                let mut rest = value;
                loop {
                    match rest {
                        Witness::Ctor(CtorId::Cons, parts) if parts.len() == 2 => {
                            shown.push(self.show_witness(&parts[0], elem));
                            rest = &parts[1];
                        }
                        Witness::Ctor(CtorId::Nil, _) => break,
                        _ => {
                            shown.push(".._".into());
                            break;
                        }
                    }
                }
                format!("[{}]", shown.join(", "))
            }
            CtorId::Variant(enum_ty, i) => {
                let Some(info) = self.tables.enums.get(enum_ty) else {
                    return "_".into();
                };
                let Some(variant) = info.variants.get(*i) else {
                    return "_".into();
                };
                if fields.is_empty() {
                    return variant.name.to_string();
                }
                // The payloads' types at the type's arguments, so that a
                // record in `Option(R)` is shown as an `R`.
                let mapping: HashMap<TyVar, Type> = match &ty {
                    Type::Generic(_, args) if args.len() == info.param_var_ids.len() => info
                        .param_var_ids
                        .iter()
                        .copied()
                        .zip(args.iter().cloned())
                        .collect(),
                    _ => HashMap::new(),
                };
                let shown: Vec<_> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| {
                        let field_ty = variant
                            .field_types
                            .get(i)
                            .map_or(unknown.clone(), |t| substitute_vars(t, &mapping));
                        self.show_witness(field, &field_ty)
                    })
                    .collect();
                format!("{}({})", variant.name, shown.join(", "))
            }
            CtorId::Record(names) => {
                // A declared record keeps its name, and says `..` when
                // it leaves fields out; an anonymous record pattern
                // names the fields it tests and no others.
                let declared: Option<(std::string::String, Vec<(Symbol, Type)>)> = match &ty {
                    Type::Generic(name, args) => self.tables.records.get(name).map(|info| {
                        let mapping: HashMap<TyVar, Type> = self
                            .tables
                            .record_param_var_ids
                            .get(name)
                            .filter(|ids| ids.len() == args.len())
                            .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                            .unwrap_or_default();
                        let fields = info
                            .fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect();
                        (self.show_type(&Type::Generic(*name, Vec::new())), fields)
                    }),
                    _ => None,
                };
                let anon_fields = match &ty {
                    Type::AnonRecord { fields, .. } => Some(fields),
                    _ => None,
                };
                let mut shown: Vec<std::string::String> = names
                    .iter()
                    .zip(fields)
                    .filter(|(_, field)| field.says_something())
                    .map(|(name, field)| {
                        let field_ty = declared
                            .as_ref()
                            .and_then(|(_, fields)| fields.iter().find(|(n, _)| n == name))
                            .map(|(_, t)| t)
                            .or_else(|| anon_fields.and_then(|fields| fields.get(name)))
                            .unwrap_or(&unknown);
                        format!("{name}: {}", self.show_witness(field, field_ty))
                    })
                    .collect();
                match declared {
                    Some((type_name, all)) => {
                        if shown.len() < all.len() {
                            shown.push("..".into());
                        }
                        format!("{type_name} {{ {} }}", shown.join(", "))
                    }
                    None => format!("{{ {} }}", shown.join(", ")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
