//! Exhaustiveness checking for match expressions (Maranget-style usefulness).
//!
//! Based on "Warnings for pattern matching" (Maranget, JFP 2007).
//! A match is exhaustive iff the wildcard pattern is NOT useful after
//! all arms have been processed.

use super::*;

/// Recursion depth bound for the usefulness algorithm. Guards against
/// pathological blowups on deeply recursive variant types. When this bound
/// is hit the checker records that it could not fully verify exhaustiveness
/// via `exhaustiveness_depth_exceeded` on the `TypeChecker`, rather than
/// silently assuming the match is exhaustive.
pub(super) const MAX_EXHAUSTIVENESS_DEPTH: usize = 20;

/// A synthetic span used for patterns constructed during exhaustiveness
/// analysis. These patterns (wildcards, tuples of wildcards, witness
/// constructors) don't correspond to any user-written source — they're
/// internal to the Maranget usefulness algorithm — so giving them a
/// zero-position span is harmless. Real diagnostics for pattern errors
/// come from the user's actual patterns in match arms, which have real
/// spans attached by the parser.
fn synth_span() -> Span {
    Span::new(0, 0)
}

/// Shortcut for building synthetic patterns used by the usefulness
/// algorithm. Keeps the body of the algorithm readable.
fn synth(kind: PatternKind) -> Pattern {
    Pattern::new(kind, synth_span())
}

impl TypeChecker {
    // ── Exhaustiveness checking (Maranget-style usefulness) ──────────
    //
    // Based on "Warnings for pattern matching" (Maranget, JFP 2007).
    // A match is exhaustive iff the wildcard pattern is NOT useful after
    // all arms have been processed.

    pub(super) fn check_exhaustiveness(
        &mut self,
        arms: &[MatchArm],
        scrutinee_ty: &Type,
        span: Span,
    ) {
        // Collect patterns from arms without guards (guarded arms don't
        // guarantee coverage since the guard may be false).
        let patterns: Vec<&Pattern> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| &a.pattern)
            .collect();

        let scrutinee_ty = self.apply(scrutinee_ty);

        // Reset the depth-exceeded flag before running the usefulness
        // algorithm so we can detect whether any recursive branch bailed
        // out at the depth bound.
        self.exhaustiveness_depth_exceeded.set(false);
        let wildcard_pat = synth(PatternKind::Wildcard);
        let wildcard_useful = self.is_useful(&patterns, &wildcard_pat, &scrutinee_ty, 0);
        let depth_exceeded = self.exhaustiveness_depth_exceeded.get();
        // Clear the flag so `missing_description`'s internal `is_useful`
        // calls start from a clean slate (their truncation is benign — a
        // conservative "missing" description is still useful).
        self.exhaustiveness_depth_exceeded.set(false);

        if wildcard_useful {
            let msg = self.missing_description(&patterns, &scrutinee_ty);
            self.error(format!("non-exhaustive match: {msg}"), span);
        } else if depth_exceeded {
            // We bailed out of the usefulness search at the depth bound,
            // so the "exhaustive" verdict is not trustworthy. Surface this
            // to the user with an actionable suggestion rather than
            // silently accepting the match.
            self.warning(
                "could not verify exhaustiveness of match: pattern analysis \
                 exceeded recursion depth limit on a recursive type; \
                 consider adding a wildcard arm (`_ -> ...`) to guarantee \
                 coverage"
                    .into(),
                span,
            );
        }

        // Warn if ALL arms have guards.
        if !arms.is_empty() && arms.iter().all(|a| a.guard.is_some()) {
            self.warning(
                "match may be non-exhaustive: all arms have guards".into(),
                span,
            );
        }
    }

    /// Check if `query` is useful with respect to existing patterns.
    /// Returns true if there exists a value matching `query` not matched by `matrix`.
    /// `depth` tracks recursion depth to prevent infinite expansion of recursive types.
    ///
    /// Note: when the recursion depth bound `MAX_EXHAUSTIVENESS_DEPTH` is
    /// hit we return `false` (to unwind cleanly) but also set
    /// `self.exhaustiveness_depth_exceeded`, so the caller can distinguish
    /// a real "not useful" verdict from a bailout and emit a "could not
    /// verify" diagnostic instead of silently accepting the match.
    pub(super) fn is_useful(
        &self,
        matrix: &[&Pattern],
        query: &Pattern,
        ty: &Type,
        depth: usize,
    ) -> bool {
        // Guard against infinite recursion on recursive types (e.g. type Expr { Num(Int), Add(Expr, Expr) }).
        // Beyond the bound we can't trust a "not useful" verdict, so we record
        // that fact via the depth-exceeded flag and let the caller surface a
        // "could not verify exhaustiveness" diagnostic.
        if depth > MAX_EXHAUSTIVENESS_DEPTH {
            self.exhaustiveness_depth_exceeded.set(true);
            return false;
        }

        if matrix.is_empty() {
            return true;
        }

        // Expand or-patterns in the query.
        if let PatternKind::Or(alts) = &query.kind {
            return alts
                .iter()
                .any(|alt| self.is_useful(matrix, alt, ty, depth));
        }

        // Expand or-patterns in the matrix.
        let expanded: Vec<&Pattern> = matrix.iter().flat_map(|p| Self::expand_or(p)).collect();
        let matrix = &expanded[..];

        if matches!(query.kind, PatternKind::Wildcard | PatternKind::Ident(_)) {
            // Maranget shortcut: a bare wildcard/ident row in the matrix
            // already covers every value at this column, so no wildcard
            // query can be useful. Without this, recursive variant types
            // re-enumerate constructors at every level and blow up
            // exponentially (e.g. `Add(Expr, Expr)` hits 5^d expansions).
            if matrix
                .iter()
                .any(|p| matches!(p.kind, PatternKind::Wildcard | PatternKind::Ident(_)))
            {
                return false;
            }
            return self.is_wildcard_useful(matrix, ty, depth);
        }

        self.is_constructor_useful(matrix, query, ty, depth)
    }

    fn expand_or(pat: &Pattern) -> Vec<&Pattern> {
        match &pat.kind {
            PatternKind::Or(alts) => alts.iter().flat_map(Self::expand_or).collect(),
            _ => vec![pat],
        }
    }

    /// Check if a wildcard is useful: enumerate constructors of the type
    /// and see if they're all covered.
    fn is_wildcard_useful(&self, matrix: &[&Pattern], ty: &Type, depth: usize) -> bool {
        match ty {
            Type::Bool => {
                let true_pat = synth(PatternKind::Bool(true));
                let false_pat = synth(PatternKind::Bool(false));
                self.is_useful(matrix, &true_pat, ty, depth + 1)
                    || self.is_useful(matrix, &false_pat, ty, depth + 1)
            }
            Type::Generic(name, type_args) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| synth(PatternKind::Wildcard))
                            .collect();
                        let ctor = synth(PatternKind::Constructor(variant.name, sub_pats.clone()));
                        if self.is_useful(matrix, &ctor, ty, depth + 1) {
                            return true;
                        }
                    }
                    false
                } else if let Some(rec_info) = self.records.get(name).cloned() {
                    // B1 (round 15): records surface as `Type::Generic(name, args)`
                    // at function boundaries because `resolve_type_expr` maps the
                    // user's record annotation through `TypeExpr::Generic`. When
                    // reached here we must instantiate the record's field
                    // templates (substituting the type args) and delegate to
                    // `is_record_useful`, matching the `Type::Record` arm below.
                    let fields: Vec<(Symbol, Type)> =
                        if let Some(param_var_ids) = self.record_param_var_ids.get(name).cloned() {
                            let mapping: HashMap<TyVar, Type> =
                                if type_args.len() == param_var_ids.len() {
                                    param_var_ids
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(&v, t)| (v, t.clone()))
                                        .collect()
                                } else {
                                    HashMap::new()
                                };
                            rec_info
                                .fields
                                .iter()
                                .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                                .collect()
                        } else {
                            rec_info.fields.clone()
                        };
                    self.is_record_useful(matrix, *name, &fields, depth)
                } else {
                    false
                }
            }
            Type::Tuple(elem_tys) => {
                // Single constructor: the tuple itself.
                let sub_pats: Vec<Pattern> = elem_tys
                    .iter()
                    .map(|_| synth(PatternKind::Wildcard))
                    .collect();
                let tuple_q = synth(PatternKind::Tuple(sub_pats));
                self.is_useful(matrix, &tuple_q, ty, depth + 1)
            }
            // Record types have a single constructor — decompose into field
            // columns and recurse, so we properly check sub-pattern coverage
            // (not just "some row has a record pattern").
            Type::Record(name, fields) => self.is_record_useful(matrix, *name, fields, depth),
            // Lists: enumerate constructors by length — `[]`, `[_]`,
            // `[_,_]`, ..., up to one past the longest fixed-length pattern
            // seen in the matrix. The final "open" constructor
            // `[_, _, ..., _, ..rest]` (of length max+1 with a rest pattern)
            // stands for "all lists strictly longer than max".
            Type::List(_elem_ty) => {
                let max_fixed_len = matrix
                    .iter()
                    .filter_map(|p| match &p.kind {
                        PatternKind::List(elems, None) => Some(elems.len()),
                        PatternKind::List(elems, Some(_)) => Some(elems.len()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                // Check fixed lengths 0..=max_fixed_len.
                for len in 0..=max_fixed_len {
                    let elems: Vec<Pattern> =
                        (0..len).map(|_| synth(PatternKind::Wildcard)).collect();
                    let fixed = synth(PatternKind::List(elems, None));
                    if self.is_useful(matrix, &fixed, ty, depth + 1) {
                        return true;
                    }
                }
                // Check the open "longer than max" constructor.
                let elems: Vec<Pattern> = (0..=max_fixed_len)
                    .map(|_| synth(PatternKind::Wildcard))
                    .collect();
                let open = synth(PatternKind::List(
                    elems,
                    Some(Box::new(synth(PatternKind::Wildcard))),
                ));
                self.is_useful(matrix, &open, ty, depth + 1)
            }
            // B4 (round 26): Unit has exactly one inhabitant, `()`. The
            // parser emits that inhabitant as `PatternKind::Tuple(vec![])`
            // (same shape the round-23 bind/check_pattern unification
            // accepts). Without this arm, Unit scrutinees fell through to
            // the "infinite type" case below, which only sees a
            // wildcard/ident row as covering the column — so
            // `match u { () -> ... }` on `let u: () = ()` was wrongly
            // reported non-exhaustive. Treat any row whose pattern is an
            // empty tuple OR a bare wildcard/ident as covering the unit
            // value.
            Type::Unit => !matrix.iter().any(|p| {
                matches!(p.kind, PatternKind::Wildcard | PatternKind::Ident(_))
                    || matches!(&p.kind, PatternKind::Tuple(ts) if ts.is_empty())
            }),
            // Infinite types: wildcard is useful iff no wildcard/ident in matrix.
            _ => !matrix
                .iter()
                .any(|p| matches!(p.kind, PatternKind::Wildcard | PatternKind::Ident(_))),
        }
    }

    /// Check if a specific constructor pattern is useful.
    fn is_constructor_useful(
        &self,
        matrix: &[&Pattern],
        query: &Pattern,
        ty: &Type,
        depth: usize,
    ) -> bool {
        match &query.kind {
            PatternKind::Bool(b) => {
                let specialized: Vec<&Pattern> = matrix
                    .iter()
                    .filter(|p| {
                        matches!(&p.kind, PatternKind::Bool(pb) if pb == b)
                            || matches!(p.kind, PatternKind::Wildcard | PatternKind::Ident(_))
                    })
                    .copied()
                    .collect();
                specialized.is_empty()
            }
            PatternKind::Constructor(name, sub_pats) => {
                let specialized = self.specialize_constructor(matrix, *name, sub_pats.len());
                if sub_pats.is_empty() {
                    specialized.is_empty()
                } else {
                    let sub_ty = self.sub_type_for_constructor(*name, ty);
                    let sub_query = if sub_pats.len() == 1 {
                        sub_pats[0].clone()
                    } else {
                        synth(PatternKind::Tuple(sub_pats.clone()))
                    };
                    let sub_refs: Vec<&Pattern> = specialized.iter().collect();
                    self.is_useful(&sub_refs, &sub_query, &sub_ty, depth + 1)
                }
            }
            PatternKind::Tuple(sub_pats) => {
                let arity = sub_pats.len();
                // Specialize: keep rows with matching tuple arity, extract sub-patterns.
                // Wildcards expand to N wildcards.
                let specialized = self.specialize_tuple(matrix, arity);
                let spec_refs: Vec<&Pattern> = specialized.iter().collect();
                if arity == 0 {
                    specialized.is_empty()
                } else if arity == 1 {
                    let elem_ty = match ty {
                        Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
                        _ => Type::Error,
                    };
                    // Unwrap the single element from each specialized tuple.
                    let unwrapped: Vec<Pattern> = specialized
                        .iter()
                        .map(|p| match &p.kind {
                            PatternKind::Tuple(ps) if !ps.is_empty() => ps[0].clone(),
                            _ => p.clone(),
                        })
                        .collect();
                    let unwrapped_refs: Vec<&Pattern> = unwrapped.iter().collect();
                    self.is_useful(&unwrapped_refs, &sub_pats[0], &elem_ty, depth + 1)
                } else {
                    // Multi-element tuple: decompose column-by-column on the
                    // specialized matrix.
                    self.is_tuple_useful_recursive(&spec_refs, sub_pats, ty, depth)
                }
            }
            // List patterns. Each list length is a distinct constructor:
            //   `[p1, ..., pk]` (no rest) matches lists of length exactly k.
            //   `[p1, ..., pk, ..rest]` (with rest) matches lists of
            //                                       length ≥ k.
            //
            // A query pattern is useful iff some value it matches is not
            // covered by any row in the matrix. We first filter the matrix
            // to rows that *could* cover the query's length set. If no row
            // does, the query is trivially useful. Otherwise we decompose
            // column-by-column (Maranget specialization) and recursively
            // check whether the query's sub-patterns expose an uncovered
            // value inside the filtered matrix.
            PatternKind::List(elems, rest) => {
                let q_len = elems.len();
                let q_has_rest = rest.is_some();

                // Does `row` cover any list in the query's length set?
                fn row_covers_query_length(row: &Pattern, q_len: usize, q_has_rest: bool) -> bool {
                    match &row.kind {
                        PatternKind::Wildcard | PatternKind::Ident(_) => true,
                        PatternKind::List(r_elems, r_rest) => {
                            let r_len = r_elems.len();
                            let r_has_rest = r_rest.is_some();
                            match (q_has_rest, r_has_rest) {
                                (false, false) => q_len == r_len,
                                (false, true) => q_len >= r_len,
                                (true, false) => r_len >= q_len,
                                (true, true) => true,
                            }
                        }
                        _ => false,
                    }
                }

                let shape_covers = matrix
                    .iter()
                    .any(|p| row_covers_query_length(p, q_len, q_has_rest));
                if !shape_covers {
                    return true;
                }

                // Decompose: specialize the matrix to the query's length set
                // by pulling out columns 0..q_len. For a fixed-length query
                // we check element columns as a tuple; for a rest query we
                // only check the fixed prefix (the rest is typically a
                // wildcard binding, so element-level checks there aren't
                // informative for the common case).
                let elem_ty = match ty {
                    Type::List(e) => (**e).clone(),
                    _ => Type::Error,
                };

                // Build specialized matrix: one entry per relevant row,
                // each entry is a Vec<Pattern> of length q_len (the first
                // q_len element patterns; rows with a shorter rest-pattern
                // get wildcards in the missing slots).
                let mut spec_rows: Vec<Vec<Pattern>> = Vec::new();
                for row in matrix {
                    match &row.kind {
                        PatternKind::Wildcard | PatternKind::Ident(_) => {
                            spec_rows.push(vec![synth(PatternKind::Wildcard); q_len]);
                        }
                        PatternKind::List(r_elems, r_rest) => {
                            let r_len = r_elems.len();
                            let r_has_rest = r_rest.is_some();
                            // Filter rows by whether they could cover the
                            // query's length set.
                            let keeps = match (q_has_rest, r_has_rest) {
                                (false, false) => q_len == r_len,
                                (false, true) => q_len >= r_len,
                                (true, false) => r_len >= q_len,
                                (true, true) => true,
                            };
                            if !keeps {
                                continue;
                            }
                            let mut cols = Vec::with_capacity(q_len);
                            // We iterate to q_len (not r_elems.len()) because
                            // when r_has_rest the row is shorter than the query
                            // and we pad with wildcards — so a direct iterator
                            // over r_elems wouldn't cover all columns.
                            #[allow(clippy::needless_range_loop)]
                            for i in 0..q_len {
                                if i < r_len {
                                    cols.push(r_elems[i].clone());
                                } else if r_has_rest {
                                    // Row was `[p1,..pR, ..rest]` with
                                    // r_len < q_len; positions beyond r_len
                                    // are "whatever the rest absorbs" which
                                    // is a wildcard match column-wise.
                                    cols.push(synth(PatternKind::Wildcard));
                                } else {
                                    // Impossible given `keeps`, but be safe.
                                    cols.push(synth(PatternKind::Wildcard));
                                }
                            }
                            spec_rows.push(cols);
                        }
                        _ => {}
                    }
                }

                if q_len == 0 {
                    return spec_rows.is_empty();
                }

                // Wrap in tuples and delegate to the tuple usefulness path
                // to reuse its proper column-by-column Maranget algorithm.
                let tuple_matrix: Vec<Pattern> = spec_rows
                    .iter()
                    .map(|r| synth(PatternKind::Tuple(r.clone())))
                    .collect();
                let tuple_refs: Vec<&Pattern> = tuple_matrix.iter().collect();
                let tuple_ty = Type::Tuple(vec![elem_ty; q_len]);
                let query_tuple_sub = elems.clone();
                self.is_tuple_useful_recursive(&tuple_refs, &query_tuple_sub, &tuple_ty, depth + 1)
            }
            // Record patterns: decompose into field columns and recurse.
            PatternKind::Record {
                fields: q_fields, ..
            } => {
                let resolved = self.apply(ty);
                if let Type::Record(_name, rec_fields) = &resolved {
                    // Build query columns from q_fields (fill omitted
                    // fields with wildcards).
                    let query_cols: Vec<Pattern> = rec_fields
                        .iter()
                        .map(|(fname, _)| {
                            q_fields
                                .iter()
                                .find(|(n, _)| n == fname)
                                .and_then(|(_, sp)| sp.clone())
                                .unwrap_or(synth(PatternKind::Wildcard))
                        })
                        .collect();
                    self.is_record_useful_with_query(matrix, rec_fields, &query_cols, depth)
                } else {
                    // Fall back to the old "not useful if any row matches"
                    // heuristic when the type isn't resolved.
                    let _ = q_fields;
                    !matrix.iter().any(|p| {
                        matches!(
                            p.kind,
                            PatternKind::Wildcard
                                | PatternKind::Ident(_)
                                | PatternKind::Record { .. }
                        )
                    })
                }
            }
            // Literal patterns — useful iff no wildcard covers them.
            PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Pin(_)
            | PatternKind::Map(..) => !matrix
                .iter()
                .any(|p| matches!(p.kind, PatternKind::Wildcard | PatternKind::Ident(_))),
            _ => false,
        }
    }

    /// Wildcard-query record usefulness: equivalent to checking whether a
    /// fully-wildcard record pattern exposes any uncovered value.
    fn is_record_useful(
        &self,
        matrix: &[&Pattern],
        _rec_name: Symbol,
        rec_fields: &[(Symbol, Type)],
        depth: usize,
    ) -> bool {
        let query_cols: Vec<Pattern> = rec_fields
            .iter()
            .map(|_| synth(PatternKind::Wildcard))
            .collect();
        self.is_record_useful_with_query(matrix, rec_fields, &query_cols, depth)
    }

    /// Check whether a record query is useful by decomposing into field
    /// columns (Maranget-style) and delegating to the tuple recursive
    /// algorithm. The record's fields are treated as an ordered tuple in
    /// the declared field order (from `rec_fields`); `query_cols` must be
    /// pre-aligned to that order.
    fn is_record_useful_with_query(
        &self,
        matrix: &[&Pattern],
        rec_fields: &[(Symbol, Type)],
        query_cols: &[Pattern],
        depth: usize,
    ) -> bool {
        if rec_fields.is_empty() {
            // Unit-like record — exhaustive iff the matrix already has a row.
            return !matrix.iter().any(|p| {
                matches!(
                    p.kind,
                    PatternKind::Wildcard | PatternKind::Ident(_) | PatternKind::Record { .. }
                )
            });
        }

        // Build the equivalent tuple-shaped matrix: every row gets mapped
        // into a tuple whose columns follow `rec_fields` order. Record rows
        // that omit a field are filled with a wildcard in that column.
        let mut tuple_rows: Vec<Pattern> = Vec::new();
        for row in matrix {
            match &row.kind {
                PatternKind::Wildcard | PatternKind::Ident(_) => {
                    let wilds: Vec<Pattern> = rec_fields
                        .iter()
                        .map(|_| synth(PatternKind::Wildcard))
                        .collect();
                    tuple_rows.push(synth(PatternKind::Tuple(wilds)));
                }
                PatternKind::Record {
                    fields: r_fields, ..
                } => {
                    let mut cols: Vec<Pattern> = Vec::with_capacity(rec_fields.len());
                    for (fname, _) in rec_fields {
                        let pat = r_fields
                            .iter()
                            .find(|(n, _)| n == fname)
                            .and_then(|(_, sp)| sp.clone())
                            .unwrap_or(synth(PatternKind::Wildcard));
                        cols.push(pat);
                    }
                    tuple_rows.push(synth(PatternKind::Tuple(cols)));
                }
                _ => {}
            }
        }

        let tuple_ty = Type::Tuple(rec_fields.iter().map(|(_, t)| t.clone()).collect());
        let tuple_refs: Vec<&Pattern> = tuple_rows.iter().collect();
        self.is_tuple_useful_recursive(&tuple_refs, query_cols, &tuple_ty, depth + 1)
    }

    /// Check multi-element tuple usefulness by specializing on the first column.
    /// This implements the proper Maranget column decomposition.
    fn is_tuple_useful_recursive(
        &self,
        matrix: &[&Pattern],
        sub_pats: &[Pattern],
        ty: &Type,
        depth: usize,
    ) -> bool {
        let arity = sub_pats.len();
        if arity == 0 {
            return matrix.is_empty();
        }
        if arity == 1 {
            let col_ty = match ty {
                Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
                _ => Type::Error,
            };
            let col_pats: Vec<&Pattern> = matrix
                .iter()
                .filter_map(|p| match &p.kind {
                    PatternKind::Tuple(ps) if ps.len() == 1 => Some(&ps[0]),
                    PatternKind::Wildcard | PatternKind::Ident(_) => Some(*p),
                    _ => None,
                })
                .collect();
            return self.is_useful(&col_pats, &sub_pats[0], &col_ty, depth + 1);
        }

        // Multi-column: specialize on first column, then recurse on rest.
        let first_ty = match ty {
            Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
            _ => Type::Error,
        };
        let rest_ty = match ty {
            Type::Tuple(ts) if ts.len() > 1 => Type::Tuple(ts[1..].to_vec()),
            _ => Type::Error,
        };

        // Get the constructors to check from the first column of the query.
        let query_first = &sub_pats[0];
        let query_rest = synth(PatternKind::Tuple(sub_pats[1..].to_vec()));

        // B3: when `query_first` is a wildcard against an "infinite" scalar
        // column type (Int / Float / ExtFloat / String), the legacy
        // `constructors_for_query` returned just `[Wildcard]`. That
        // specialization kept every matrix row, which made matches like
        // `(0, Red) -> _ | (_, Green) -> _ | (_, Blue) -> _` on
        // `(Int, Color)` look exhaustive: the rest-column check saw all
        // three Color variants and said "covered", never noticing that
        // `(1, Red)` has no matching arm.
        //
        // Fix: split the first column into
        //   {each literal value appearing in any matrix row's first col} ∪
        //   {a synthetic "not in matrix" witness}.
        // The witness case only keeps rows whose first column is already
        // wildcard/ident, so the Red literal row is dropped and the
        // recursive wildcard check on the rest column surfaces the
        // missing `(_, Red)` arm as non-exhaustive.
        //
        // B1 (round 26): the same hazard applies to any first-column type
        // that `constructors_for_query` cannot fully enumerate — notably
        // Records, nested Tuples, Lists, Maps, Sets and non-enum Generics.
        // For those, `constructors_for_query` falls through to a single
        // `[Wildcard]`, which once again pretends specific-value patterns
        // in the matrix cover the whole column. Treat these as the
        // "infinite" case too: the witness pass (Pass 2) drops rows whose
        // first column is a specific value, so `(Pair{a:0,b:0}, _)` no
        // longer masks the missing `(Pair{a:1,b:2}, _)` case. Pass 1 is
        // only useful for literal-dedupe reporting and stays a no-op when
        // the first column has no recognised literal shapes.
        let is_first_col_non_enumerable = Self::first_col_non_enumerable(&first_ty, self);
        let query_first_is_wild = matches!(
            query_first.kind,
            PatternKind::Wildcard | PatternKind::Ident(_)
        );

        if is_first_col_non_enumerable && query_first_is_wild {
            // Collect distinct literal constructors seen in the first
            // column of the matrix.
            let mut literal_ctors: Vec<Pattern> = Vec::new();
            for pat in matrix {
                if let PatternKind::Tuple(ps) = &pat.kind
                    && ps.len() == arity
                {
                    match ps[0].kind {
                        PatternKind::Int(_)
                        | PatternKind::Float(_)
                        | PatternKind::StringLit(..)
                        | PatternKind::Range(..)
                        | PatternKind::FloatRange(..) => {
                            let c = ps[0].clone();
                            if !literal_ctors
                                .iter()
                                .any(|x| Self::patterns_first_col_equal(x, &c))
                            {
                                literal_ctors.push(c);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Pass 1: each literal constructor.
            for ctor in &literal_ctors {
                let mut specialized_rest: Vec<Pattern> = Vec::new();
                for pat in matrix {
                    match &pat.kind {
                        PatternKind::Tuple(ps)
                            if ps.len() == arity && Self::first_col_matches(&ps[0], ctor) =>
                        {
                            specialized_rest.push(synth(PatternKind::Tuple(ps[1..].to_vec())));
                        }
                        PatternKind::Wildcard | PatternKind::Ident(_) => {
                            let wilds: Vec<Pattern> = (0..arity - 1)
                                .map(|_| synth(PatternKind::Wildcard))
                                .collect();
                            specialized_rest.push(synth(PatternKind::Tuple(wilds)));
                        }
                        _ => {}
                    }
                }
                let rest_refs: Vec<&Pattern> = specialized_rest.iter().collect();
                if self.is_useful(&rest_refs, &query_rest, &rest_ty, depth + 1) {
                    return true;
                }
            }
            // Pass 2: synthetic "not in matrix" witness — only rows whose
            // first column covers every value of the column type survive.
            // B1 (round 26): a record pattern whose fields are all
            // bindings/wildcards (e.g. `Pair{a, b}`) or a nested tuple
            // pattern of bindings (e.g. `(x, y)`) also covers every value
            // of the column, even though the outer pattern isn't itself a
            // wildcard/ident. Without this, a row like
            // `(Pair{a, b}, _) -> _` is dropped from the witness matrix
            // and the match is wrongly flagged non-exhaustive.
            let mut witness_rest: Vec<Pattern> = Vec::new();
            for pat in matrix {
                match &pat.kind {
                    PatternKind::Tuple(ps)
                        if ps.len() == arity && Self::is_fully_covering_pattern(&ps[0]) =>
                    {
                        witness_rest.push(synth(PatternKind::Tuple(ps[1..].to_vec())));
                    }
                    PatternKind::Wildcard | PatternKind::Ident(_) => {
                        let wilds: Vec<Pattern> = (0..arity - 1)
                            .map(|_| synth(PatternKind::Wildcard))
                            .collect();
                        witness_rest.push(synth(PatternKind::Tuple(wilds)));
                    }
                    _ => {}
                }
            }
            let rest_refs: Vec<&Pattern> = witness_rest.iter().collect();
            if self.is_useful(&rest_refs, &query_rest, &rest_ty, depth + 1) {
                return true;
            }
            return false;
        }

        // For each constructor that query_first could be, specialize the matrix
        // on that constructor in the first column and check if query_rest is useful.
        let first_constructors = self.constructors_for_query(query_first, &first_ty);

        for ctor in &first_constructors {
            // Specialize: keep rows whose first column matches this constructor,
            // replace with the remaining columns.
            let mut specialized_rest: Vec<Pattern> = Vec::new();
            for pat in matrix {
                match &pat.kind {
                    PatternKind::Tuple(ps)
                        if ps.len() == arity && Self::first_col_matches(&ps[0], ctor) =>
                    {
                        specialized_rest.push(synth(PatternKind::Tuple(ps[1..].to_vec())));
                    }
                    PatternKind::Wildcard | PatternKind::Ident(_) => {
                        let wilds: Vec<Pattern> = (0..arity - 1)
                            .map(|_| synth(PatternKind::Wildcard))
                            .collect();
                        specialized_rest.push(synth(PatternKind::Tuple(wilds)));
                    }
                    _ => {}
                }
            }
            let rest_refs: Vec<&Pattern> = specialized_rest.iter().collect();
            if self.is_useful(&rest_refs, &query_rest, &rest_ty, depth + 1) {
                return true;
            }
        }
        false
    }

    /// B1 helper: decide whether `pat` covers every value of its column
    /// type syntactically. Top-level wildcards/idents obviously do; so
    /// does a record pattern whose every field is a covering pattern,
    /// and a tuple pattern whose every element is covering (since
    /// records and tuples are single-constructor product types). This
    /// is intentionally conservative — it only examines the pattern's
    /// shape and doesn't try to prove coverage via reasoning about the
    /// column type. That's fine for Pass 2 of the witness-split, whose
    /// job is to identify rows that unconditionally cover the synthetic
    /// "not-in-matrix" first-column value.
    fn is_fully_covering_pattern(pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Wildcard | PatternKind::Ident(_) => true,
            PatternKind::Record { fields, .. } => fields.iter().all(|(_, sub)| match sub {
                Some(p) => Self::is_fully_covering_pattern(p),
                None => true,
            }),
            PatternKind::Tuple(ps) => ps.iter().all(Self::is_fully_covering_pattern),
            PatternKind::Or(alts) => alts.iter().any(Self::is_fully_covering_pattern),
            _ => false,
        }
    }

    /// B1 helper: decide whether the first-column type of a tuple is one
    /// where `constructors_for_query` cannot fully enumerate constructors,
    /// so the usefulness algorithm must fall back to the witness-split
    /// path. The only first-column types with a faithful enumeration are
    /// `Bool` (two cases) and `Type::Generic(name)` where `name` is a
    /// registered enum. Everything else — scalars, records, tuples,
    /// lists, maps, sets, channels, generics that map to records or are
    /// unknown, and inference artefacts — must be witness-split to avoid
    /// pretending a handful of specific-value rows cover the whole
    /// column.
    fn first_col_non_enumerable(ty: &Type, tc: &TypeChecker) -> bool {
        match ty {
            Type::Bool => false,
            Type::Generic(name, _) => !tc.enums.contains_key(name),
            _ => true,
        }
    }

    /// B3 helper: structural equality for literal-constructor patterns used
    /// when deduping distinct first-column literals from the matrix. Only
    /// meaningful for the literal variants enumerated at the call site.
    fn patterns_first_col_equal(a: &Pattern, b: &Pattern) -> bool {
        match (&a.kind, &b.kind) {
            (PatternKind::Int(x), PatternKind::Int(y)) => x == y,
            (PatternKind::Float(x), PatternKind::Float(y)) => x == y,
            (PatternKind::StringLit(x, _), PatternKind::StringLit(y, _)) => x == y,
            (PatternKind::Range(a1, b1), PatternKind::Range(a2, b2)) => a1 == a2 && b1 == b2,
            (PatternKind::FloatRange(a1, b1), PatternKind::FloatRange(a2, b2)) => {
                a1 == a2 && b1 == b2
            }
            _ => false,
        }
    }

    /// Get the set of constructors to check for a query pattern against a type.
    fn constructors_for_query(&self, query: &Pattern, ty: &Type) -> Vec<Pattern> {
        match &query.kind {
            PatternKind::Wildcard | PatternKind::Ident(_) => {
                // Need to enumerate all constructors of the type.
                match ty {
                    Type::Bool => vec![
                        synth(PatternKind::Bool(true)),
                        synth(PatternKind::Bool(false)),
                    ],
                    Type::Generic(name, _) => {
                        if let Some(info) = self.enums.get(name) {
                            info.variants
                                .iter()
                                .map(|v| {
                                    let sub_pats: Vec<Pattern> = (0..v.field_types.len())
                                        .map(|_| synth(PatternKind::Wildcard))
                                        .collect();
                                    synth(PatternKind::Constructor(v.name, sub_pats))
                                })
                                .collect()
                        } else {
                            vec![synth(PatternKind::Wildcard)]
                        }
                    }
                    _ => vec![synth(PatternKind::Wildcard)],
                }
            }
            // Specific constructor: just check itself.
            _ => vec![query.clone()],
        }
    }

    /// Check if a pattern in the first column matches a specific constructor.
    fn first_col_matches(pat: &Pattern, ctor: &Pattern) -> bool {
        match (&pat.kind, &ctor.kind) {
            // Wildcards/idents match anything.
            (PatternKind::Wildcard | PatternKind::Ident(_), _) => true,
            // A wildcard constructor means "anything" — all patterns match.
            (_, PatternKind::Wildcard | PatternKind::Ident(_)) => true,
            (PatternKind::Bool(a), PatternKind::Bool(b)) => a == b,
            (PatternKind::Constructor(a, _), PatternKind::Constructor(b, _)) => a == b,
            (PatternKind::Int(a), PatternKind::Int(b)) => a == b,
            (PatternKind::StringLit(a, _), PatternKind::StringLit(b, _)) => a == b,
            _ => false,
        }
    }

    /// Specialize the matrix for a specific enum constructor.
    fn specialize_constructor(
        &self,
        matrix: &[&Pattern],
        ctor_name: Symbol,
        arity: usize,
    ) -> Vec<Pattern> {
        let mut result = Vec::new();
        for pat in matrix {
            match &pat.kind {
                PatternKind::Constructor(name, sub_pats) if *name == ctor_name => {
                    if arity <= 1 {
                        result.push(
                            sub_pats
                                .first()
                                .cloned()
                                .unwrap_or_else(|| synth(PatternKind::Wildcard)),
                        );
                    } else {
                        result.push(synth(PatternKind::Tuple(sub_pats.clone())));
                    }
                }
                PatternKind::Wildcard | PatternKind::Ident(_) => {
                    if arity <= 1 {
                        result.push(synth(PatternKind::Wildcard));
                    } else {
                        let wilds = (0..arity).map(|_| synth(PatternKind::Wildcard)).collect();
                        result.push(synth(PatternKind::Tuple(wilds)));
                    }
                }
                _ => {}
            }
        }
        result
    }

    /// Specialize the matrix for a tuple constructor with the given arity.
    fn specialize_tuple(&self, matrix: &[&Pattern], arity: usize) -> Vec<Pattern> {
        let mut result = Vec::new();
        for pat in matrix {
            match &pat.kind {
                PatternKind::Tuple(sub_pats) if sub_pats.len() == arity => {
                    result.push(synth(PatternKind::Tuple(sub_pats.clone())));
                }
                PatternKind::Wildcard | PatternKind::Ident(_) => {
                    let wilds = (0..arity).map(|_| synth(PatternKind::Wildcard)).collect();
                    result.push(synth(PatternKind::Tuple(wilds)));
                }
                _ => {}
            }
        }
        result
    }

    /// Get the sub-type for a constructor's fields.
    fn sub_type_for_constructor(&self, ctor_name: Symbol, parent_ty: &Type) -> Type {
        if let Some(enum_name) = self.variant_to_enum.get(&ctor_name)
            && let Some(enum_info) = self.enums.get(enum_name)
            && let Some(variant) = enum_info.variants.iter().find(|v| v.name == ctor_name)
        {
            if variant.field_types.len() == 1 {
                if let Type::Generic(_, type_args) = parent_ty {
                    return substitute_enum_params(
                        &variant.field_types[0],
                        &enum_info.param_var_ids,
                        type_args,
                    );
                }
                return variant.field_types[0].clone();
            } else if variant.field_types.len() > 1 {
                let field_types: Vec<Type> = if let Type::Generic(_, type_args) = parent_ty {
                    variant
                        .field_types
                        .iter()
                        .map(|ft| substitute_enum_params(ft, &enum_info.param_var_ids, type_args))
                        .collect()
                } else {
                    variant.field_types.clone()
                };
                return Type::Tuple(field_types);
            }
        }
        Type::Error
    }

    /// Generate a human-readable description of what's missing.
    fn missing_description(&self, patterns: &[&Pattern], ty: &Type) -> std::string::String {
        match ty {
            Type::Bool => {
                let has_true = patterns.iter().any(|p| Self::covers_bool(p, true));
                let has_false = patterns.iter().any(|p| Self::covers_bool(p, false));
                let mut missing = Vec::new();
                if !has_true {
                    missing.push("true");
                }
                if !has_false {
                    missing.push("false");
                }
                if missing.is_empty() {
                    "not all patterns are covered".into()
                } else {
                    format!("missing {}", missing.join(", "))
                }
            }
            Type::Generic(name, type_args) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    let mut missing = Vec::new();
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| synth(PatternKind::Wildcard))
                            .collect();
                        let ctor = synth(PatternKind::Constructor(variant.name, sub_pats));
                        if self.is_useful(patterns, &ctor, ty, 0) {
                            missing.push(format!("{}", variant.name));
                        }
                    }
                    if missing.is_empty() {
                        "not all patterns are covered".into()
                    } else {
                        let word = if missing.len() == 1 {
                            "variant"
                        } else {
                            "variants"
                        };
                        format!("missing {} {}", word, missing.join(", "))
                    }
                } else if let Some(rec_info) = self.records.get(name).cloned() {
                    // B1 (round 15): mirror the enum branch for records
                    // reached via `Type::Generic` at fn boundaries.
                    let fields: Vec<(Symbol, Type)> =
                        if let Some(param_var_ids) = self.record_param_var_ids.get(name).cloned() {
                            let mapping: HashMap<TyVar, Type> =
                                if type_args.len() == param_var_ids.len() {
                                    param_var_ids
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(&v, t)| (v, t.clone()))
                                        .collect()
                                } else {
                                    HashMap::new()
                                };
                            rec_info
                                .fields
                                .iter()
                                .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                                .collect()
                        } else {
                            rec_info.fields.clone()
                        };
                    let record_ty = Type::Record(*name, fields);
                    self.missing_description(patterns, &record_ty)
                } else {
                    "not all patterns are covered".into()
                }
            }
            _ => "not all patterns are covered".into(),
        }
    }

    fn covers_bool(pat: &Pattern, val: bool) -> bool {
        match &pat.kind {
            PatternKind::Bool(b) => *b == val,
            PatternKind::Wildcard | PatternKind::Ident(_) => true,
            PatternKind::Or(alts) => alts.iter().any(|a| Self::covers_bool(a, val)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn assert_no_errors(input: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
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

    fn assert_has_error(input: &str, expected: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Or-pattern exhaustiveness ───────────────────────────────────

    #[test]
    fn test_or_pattern_exhaustive() {
        assert_no_errors(
            r#"
type Color { Red, Green, Blue }
fn describe(c) {
  match c {
    Red | Green -> "warm-ish"
    Blue -> "cool"
  }
}
fn main() { describe(Red) }
        "#,
        );
    }

    #[test]
    fn test_or_pattern_non_exhaustive() {
        assert_has_error(
            r#"
type Color { Red, Green, Blue }
fn describe(c) {
  match c {
    Red | Green -> "warm-ish"
  }
}
fn main() { describe(Red) }
        "#,
            "non-exhaustive",
        );
    }

    // ── Nested constructor exhaustiveness ────────────────────────────

    #[test]
    fn test_nested_option_exhaustive() {
        assert_no_errors(
            r#"
fn process(x) {
  match x {
    Some(Some(v)) -> v
    Some(None) -> 0
    None -> 0
  }
}
fn main() { process(Some(Some(1))) }
        "#,
        );
    }

    #[test]
    fn test_nested_option_missing_inner() {
        assert_has_error(
            r#"
fn process(x) {
  match x {
    Some(Some(v)) -> v
    None -> 0
  }
}
fn main() { process(Some(Some(1))) }
        "#,
            "non-exhaustive",
        );
    }

    // ── Tuple exhaustiveness ────────────────────────────────────────

    #[test]
    fn test_tuple_pair_exhaustive() {
        assert_no_errors(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> 1
    (true, false) -> 2
    (false, true) -> 3
    (false, false) -> 4
  }
}
fn main() { check((true, false)) }
        "#,
        );
    }

    #[test]
    fn test_tuple_pair_missing_case() {
        assert_has_error(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> 1
    (true, false) -> 2
    (false, true) -> 3
  }
}
fn main() { check((true, false)) }
        "#,
            "non-exhaustive",
        );
    }

    // ── List pattern exhaustiveness ─────────────────────────────────

    #[test]
    fn test_list_with_wildcard_exhaustive() {
        assert_no_errors(
            r#"
fn head(xs) {
  match xs {
    [] -> 0
    [x, ..rest] -> x
  }
}
fn main() { head([1, 2, 3]) }
        "#,
        );
    }

    #[test]
    fn test_list_missing_empty_case() {
        assert_has_error(
            r#"
fn head(xs) {
  match xs {
    [x, ..rest] -> x
  }
}
fn main() { head([1, 2, 3]) }
        "#,
            "non-exhaustive",
        );
    }

    // ── Empty match ─────────────────────────────────────────────────

    #[test]
    fn test_empty_match_non_exhaustive() {
        assert_has_error(
            r#"
fn process(x) {
  match x {
  }
}
fn main() { process(1) }
        "#,
            "non-exhaustive",
        );
    }

    // ── All guards non-exhaustive ───────────────────────────────────

    #[test]
    fn test_all_arms_guarded() {
        assert_has_error(
            r#"
fn check(x) {
  match x {
    n when n > 0 -> "positive"
    n when n < 0 -> "negative"
  }
}
fn main() { check(1) }
        "#,
            "non-exhaustive",
        );
    }

    // ── Wildcard covers everything ──────────────────────────────────

    #[test]
    fn test_single_wildcard_exhaustive() {
        assert_no_errors(
            r#"
fn id(x) {
  match x {
    _ -> x
  }
}
fn main() { id(42) }
        "#,
        );
    }

    // ── Bool or-pattern exhaustiveness ──────────────────────────────

    #[test]
    fn test_bool_or_pattern_covers() {
        assert_no_errors(
            r#"
fn check(b) {
  match b {
    true | false -> "done"
  }
}
fn main() { check(true) }
        "#,
        );
    }

    // ── Multi-field constructor exhaustiveness ──────────────────────

    #[test]
    fn test_multi_field_variant_exhaustive() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}
fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
  }
}
fn main() { area(Circle(1.0)) }
        "#,
        );
    }

    #[test]
    fn test_multi_field_variant_missing() {
        assert_has_error(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}
fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}
fn main() { area(Circle(1.0)) }
        "#,
            "non-exhaustive",
        );
    }

    // ── Recursive variant match certifies in polynomial time ────────
    //
    // Regression for a doubly-exponential blowup bug. On a recursive
    // enum like `Expr { Leaf(Int), Pair(Expr, Expr) }`, the usefulness
    // algorithm used to re-enumerate every variant at every level of
    // the recursion — `Pair`'s two `Expr` sub-columns each triggered a
    // fresh round of constructor enumeration, and the work grew as
    // `k^d` until `MAX_EXHAUSTIVENESS_DEPTH` tripped. The match was
    // then reported as "could not verify exhaustiveness" (and before
    // that, silently accepted).
    //
    // The fix is a standard Maranget shortcut: if any row in the matrix
    // at the current column is a bare wildcard/ident, it already covers
    // every value at that column, so no wildcard query can be useful.
    // This collapses `Pair(_, _)`-style arms to O(1) work per column
    // instead of `k^d`. This test locks in that the shortcut fires:
    // the match is certified exhaustive with no depth-limit warning and
    // no spurious diagnostics.
    #[test]
    fn test_recursive_variant_match_certifies_without_depth_bailout() {
        use super::MAX_EXHAUSTIVENESS_DEPTH;
        use crate::intern::intern;
        use crate::lexer::Span;

        let mut tc = TypeChecker::new();

        // Register a recursive enum `Expr { Leaf(Int), Pair(Expr, Expr) }`.
        // (Constructed directly because writing a depth-20+ nested pattern
        // in source would be unwieldy and fragile.)
        let expr_name = intern("ExhaustivenessDepthExpr");
        let leaf_name = intern("ExhaustivenessDepthLeaf");
        let pair_name = intern("ExhaustivenessDepthPair");
        let expr_ty = Type::Generic(expr_name, vec![]);

        tc.enums.insert(
            expr_name,
            EnumInfo {
                _name: expr_name,
                params: vec![],
                param_var_ids: vec![],
                variants: vec![
                    VariantInfo {
                        name: leaf_name,
                        field_types: vec![Type::Int],
                    },
                    VariantInfo {
                        name: pair_name,
                        field_types: vec![expr_ty.clone(), expr_ty.clone()],
                    },
                ],
            },
        );
        tc.variant_to_enum.insert(leaf_name, expr_name);
        tc.variant_to_enum.insert(pair_name, expr_name);

        // Build a two-arm match that IS logically exhaustive — every
        // `Expr` is either a `Leaf` or a `Pair`. Pre-fix, the Maranget
        // algorithm re-enumerated all variants at every level as it
        // recursed into `Pair`'s two `Expr` columns, hit the depth
        // bound, and raised "could not verify". With the wildcard-row
        // shortcut the algorithm certifies this cleanly and fast.
        let span = Span::new(1, 1);
        let body = Expr::new(crate::ast::ExprKind::Int(0), span);
        let wild = || Pattern::new(PatternKind::Wildcard, span);
        let arms = vec![
            MatchArm {
                pattern: Pattern::new(PatternKind::Constructor(leaf_name, vec![wild()]), span),
                guard: None,
                body: body.clone(),
            },
            MatchArm {
                pattern: Pattern::new(
                    PatternKind::Constructor(pair_name, vec![wild(), wild()]),
                    span,
                ),
                guard: None,
                body: body.clone(),
            },
        ];
        // Silence unused-warning — `MAX_EXHAUSTIVENESS_DEPTH` is imported
        // as a documentation anchor for this test.
        let _ = MAX_EXHAUSTIVENESS_DEPTH;

        tc.check_exhaustiveness(&arms, &expr_ty, span);

        // Post-fix expectation: the match is certified exhaustive with
        // no "could not verify" warning, no "non-exhaustive" error, and
        // no depth-bailout flag set.
        assert!(
            tc.errors.is_empty(),
            "expected no diagnostics, got: {:?}",
            tc.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(
            !tc.exhaustiveness_depth_exceeded.get(),
            "depth bound should not be hit on a simple recursive variant match",
        );
    }
}
