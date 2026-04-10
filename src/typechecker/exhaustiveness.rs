//! Exhaustiveness checking for match expressions (Maranget-style usefulness).
//!
//! Based on "Warnings for pattern matching" (Maranget, JFP 2007).
//! A match is exhaustive iff the wildcard pattern is NOT useful after
//! all arms have been processed.

use super::*;

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

        if self.is_useful(&patterns, &Pattern::Wildcard, &scrutinee_ty, 0) {
            let msg = self.missing_description(&patterns, &scrutinee_ty);
            self.error(format!("non-exhaustive match: {msg}"), span);
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
    fn is_useful(&self, matrix: &[&Pattern], query: &Pattern, ty: &Type, depth: usize) -> bool {
        // Guard against infinite recursion on recursive types (e.g. type Expr { Num(Int), Add(Expr, Expr) }).
        // Beyond a reasonable depth, conservatively assume exhaustive (not useful).
        const MAX_EXHAUSTIVENESS_DEPTH: usize = 20;
        if depth > MAX_EXHAUSTIVENESS_DEPTH {
            return false;
        }

        if matrix.is_empty() {
            return true;
        }

        // Expand or-patterns in the query.
        if let Pattern::Or(alts) = query {
            return alts
                .iter()
                .any(|alt| self.is_useful(matrix, alt, ty, depth));
        }

        // Expand or-patterns in the matrix.
        let expanded: Vec<&Pattern> = matrix.iter().flat_map(|p| Self::expand_or(p)).collect();
        let matrix = &expanded[..];

        if matches!(query, Pattern::Wildcard | Pattern::Ident(_)) {
            return self.is_wildcard_useful(matrix, ty, depth);
        }

        self.is_constructor_useful(matrix, query, ty, depth)
    }

    fn expand_or(pat: &Pattern) -> Vec<&Pattern> {
        match pat {
            Pattern::Or(alts) => alts.iter().flat_map(Self::expand_or).collect(),
            _ => vec![pat],
        }
    }

    /// Check if a wildcard is useful: enumerate constructors of the type
    /// and see if they're all covered.
    fn is_wildcard_useful(&self, matrix: &[&Pattern], ty: &Type, depth: usize) -> bool {
        match ty {
            Type::Bool => {
                let true_pat = Pattern::Bool(true);
                let false_pat = Pattern::Bool(false);
                self.is_useful(matrix, &true_pat, ty, depth + 1)
                    || self.is_useful(matrix, &false_pat, ty, depth + 1)
            }
            Type::Generic(name, _) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| Pattern::Wildcard)
                            .collect();
                        let ctor = Pattern::Constructor(variant.name, sub_pats.clone());
                        if self.is_useful(matrix, &ctor, ty, depth + 1) {
                            return true;
                        }
                    }
                    false
                } else {
                    false
                }
            }
            Type::Tuple(elem_tys) => {
                // Single constructor: the tuple itself.
                let sub_pats: Vec<Pattern> = elem_tys.iter().map(|_| Pattern::Wildcard).collect();
                let tuple_q = Pattern::Tuple(sub_pats);
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
                    .filter_map(|p| match p {
                        Pattern::List(elems, None) => Some(elems.len()),
                        Pattern::List(elems, Some(_)) => Some(elems.len()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                // Check fixed lengths 0..=max_fixed_len.
                for len in 0..=max_fixed_len {
                    let elems: Vec<Pattern> = (0..len).map(|_| Pattern::Wildcard).collect();
                    let fixed = Pattern::List(elems, None);
                    if self.is_useful(matrix, &fixed, ty, depth + 1) {
                        return true;
                    }
                }
                // Check the open "longer than max" constructor.
                let elems: Vec<Pattern> = (0..=max_fixed_len).map(|_| Pattern::Wildcard).collect();
                let open = Pattern::List(elems, Some(Box::new(Pattern::Wildcard)));
                self.is_useful(matrix, &open, ty, depth + 1)
            }
            // Infinite types: wildcard is useful iff no wildcard/ident in matrix.
            _ => !matrix
                .iter()
                .any(|p| matches!(p, Pattern::Wildcard | Pattern::Ident(_))),
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
        match query {
            Pattern::Bool(b) => {
                let specialized: Vec<&Pattern> = matrix
                    .iter()
                    .filter(|p| {
                        matches!(p, Pattern::Bool(pb) if pb == b)
                            || matches!(p, Pattern::Wildcard | Pattern::Ident(_))
                    })
                    .copied()
                    .collect();
                specialized.is_empty()
            }
            Pattern::Constructor(name, sub_pats) => {
                let specialized = self.specialize_constructor(matrix, *name, sub_pats.len());
                if sub_pats.is_empty() {
                    specialized.is_empty()
                } else {
                    let sub_ty = self.sub_type_for_constructor(*name, ty);
                    let sub_query = if sub_pats.len() == 1 {
                        sub_pats[0].clone()
                    } else {
                        Pattern::Tuple(sub_pats.clone())
                    };
                    let sub_refs: Vec<&Pattern> = specialized.iter().collect();
                    self.is_useful(&sub_refs, &sub_query, &sub_ty, depth + 1)
                }
            }
            Pattern::Tuple(sub_pats) => {
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
                        .map(|p| match p {
                            Pattern::Tuple(ps) if !ps.is_empty() => ps[0].clone(),
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
            Pattern::List(elems, rest) => {
                let q_len = elems.len();
                let q_has_rest = rest.is_some();

                // Does `row` cover any list in the query's length set?
                fn row_covers_query_length(row: &Pattern, q_len: usize, q_has_rest: bool) -> bool {
                    match row {
                        Pattern::Wildcard | Pattern::Ident(_) => true,
                        Pattern::List(r_elems, r_rest) => {
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
                    match row {
                        Pattern::Wildcard | Pattern::Ident(_) => {
                            spec_rows.push(vec![Pattern::Wildcard; q_len]);
                        }
                        Pattern::List(r_elems, r_rest) => {
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
                                    cols.push(Pattern::Wildcard);
                                } else {
                                    // Impossible given `keeps`, but be safe.
                                    cols.push(Pattern::Wildcard);
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
                    .map(|r| Pattern::Tuple(r.clone()))
                    .collect();
                let tuple_refs: Vec<&Pattern> = tuple_matrix.iter().collect();
                let tuple_ty = Type::Tuple(vec![elem_ty; q_len]);
                let query_tuple_sub = elems.clone();
                self.is_tuple_useful_recursive(&tuple_refs, &query_tuple_sub, &tuple_ty, depth + 1)
            }
            // Record patterns: decompose into field columns and recurse.
            Pattern::Record {
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
                                .unwrap_or(Pattern::Wildcard)
                        })
                        .collect();
                    self.is_record_useful_with_query(matrix, rec_fields, &query_cols, depth)
                } else {
                    // Fall back to the old "not useful if any row matches"
                    // heuristic when the type isn't resolved.
                    let _ = q_fields;
                    !matrix.iter().any(|p| {
                        matches!(
                            p,
                            Pattern::Wildcard | Pattern::Ident(_) | Pattern::Record { .. }
                        )
                    })
                }
            }
            // Literal patterns — useful iff no wildcard covers them.
            Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::StringLit(..)
            | Pattern::Range(..)
            | Pattern::FloatRange(..)
            | Pattern::Pin(_)
            | Pattern::Map(..) => !matrix
                .iter()
                .any(|p| matches!(p, Pattern::Wildcard | Pattern::Ident(_))),
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
        let query_cols: Vec<Pattern> = rec_fields.iter().map(|_| Pattern::Wildcard).collect();
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
                    p,
                    Pattern::Wildcard | Pattern::Ident(_) | Pattern::Record { .. }
                )
            });
        }

        // Build the equivalent tuple-shaped matrix: every row gets mapped
        // into a tuple whose columns follow `rec_fields` order. Record rows
        // that omit a field are filled with a wildcard in that column.
        let mut tuple_rows: Vec<Pattern> = Vec::new();
        for row in matrix {
            match row {
                Pattern::Wildcard | Pattern::Ident(_) => {
                    let wilds: Vec<Pattern> =
                        rec_fields.iter().map(|_| Pattern::Wildcard).collect();
                    tuple_rows.push(Pattern::Tuple(wilds));
                }
                Pattern::Record {
                    fields: r_fields, ..
                } => {
                    let mut cols: Vec<Pattern> = Vec::with_capacity(rec_fields.len());
                    for (fname, _) in rec_fields {
                        let pat = r_fields
                            .iter()
                            .find(|(n, _)| n == fname)
                            .and_then(|(_, sp)| sp.clone())
                            .unwrap_or(Pattern::Wildcard);
                        cols.push(pat);
                    }
                    tuple_rows.push(Pattern::Tuple(cols));
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
                .filter_map(|p| match p {
                    Pattern::Tuple(ps) if ps.len() == 1 => Some(&ps[0]),
                    Pattern::Wildcard | Pattern::Ident(_) => Some(*p),
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
        let query_rest = Pattern::Tuple(sub_pats[1..].to_vec());

        // For each constructor that query_first could be, specialize the matrix
        // on that constructor in the first column and check if query_rest is useful.
        let first_constructors = self.constructors_for_query(query_first, &first_ty);

        for ctor in &first_constructors {
            // Specialize: keep rows whose first column matches this constructor,
            // replace with the remaining columns.
            let mut specialized_rest: Vec<Pattern> = Vec::new();
            for pat in matrix {
                match pat {
                    Pattern::Tuple(ps) if ps.len() == arity => {
                        if Self::first_col_matches(&ps[0], ctor) {
                            specialized_rest.push(Pattern::Tuple(ps[1..].to_vec()));
                        }
                    }
                    Pattern::Wildcard | Pattern::Ident(_) => {
                        let wilds: Vec<Pattern> =
                            (0..arity - 1).map(|_| Pattern::Wildcard).collect();
                        specialized_rest.push(Pattern::Tuple(wilds));
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

    /// Get the set of constructors to check for a query pattern against a type.
    fn constructors_for_query(&self, query: &Pattern, ty: &Type) -> Vec<Pattern> {
        match query {
            Pattern::Wildcard | Pattern::Ident(_) => {
                // Need to enumerate all constructors of the type.
                match ty {
                    Type::Bool => vec![Pattern::Bool(true), Pattern::Bool(false)],
                    Type::Generic(name, _) => {
                        if let Some(info) = self.enums.get(name) {
                            info.variants
                                .iter()
                                .map(|v| {
                                    let sub_pats: Vec<Pattern> = (0..v.field_types.len())
                                        .map(|_| Pattern::Wildcard)
                                        .collect();
                                    Pattern::Constructor(v.name, sub_pats)
                                })
                                .collect()
                        } else {
                            vec![Pattern::Wildcard]
                        }
                    }
                    _ => vec![Pattern::Wildcard],
                }
            }
            // Specific constructor: just check itself.
            _ => vec![query.clone()],
        }
    }

    /// Check if a pattern in the first column matches a specific constructor.
    fn first_col_matches(pat: &Pattern, ctor: &Pattern) -> bool {
        match (pat, ctor) {
            // Wildcards/idents match anything.
            (Pattern::Wildcard | Pattern::Ident(_), _) => true,
            // A wildcard constructor means "anything" — all patterns match.
            (_, Pattern::Wildcard | Pattern::Ident(_)) => true,
            (Pattern::Bool(a), Pattern::Bool(b)) => a == b,
            (Pattern::Constructor(a, _), Pattern::Constructor(b, _)) => a == b,
            (Pattern::Int(a), Pattern::Int(b)) => a == b,
            (Pattern::StringLit(a, _), Pattern::StringLit(b, _)) => a == b,
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
            match pat {
                Pattern::Constructor(name, sub_pats) if *name == ctor_name => {
                    if arity <= 1 {
                        result.push(sub_pats.first().cloned().unwrap_or(Pattern::Wildcard));
                    } else {
                        result.push(Pattern::Tuple(sub_pats.clone()));
                    }
                }
                Pattern::Wildcard | Pattern::Ident(_) => {
                    if arity <= 1 {
                        result.push(Pattern::Wildcard);
                    } else {
                        let wilds = (0..arity).map(|_| Pattern::Wildcard).collect();
                        result.push(Pattern::Tuple(wilds));
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
            match pat {
                Pattern::Tuple(sub_pats) if sub_pats.len() == arity => {
                    result.push(Pattern::Tuple(sub_pats.clone()));
                }
                Pattern::Wildcard | Pattern::Ident(_) => {
                    let wilds = (0..arity).map(|_| Pattern::Wildcard).collect();
                    result.push(Pattern::Tuple(wilds));
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
            Type::Generic(name, _) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    let mut missing = Vec::new();
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| Pattern::Wildcard)
                            .collect();
                        let ctor = Pattern::Constructor(variant.name, sub_pats);
                        if self.is_useful(patterns, &ctor, ty, 0) {
                            missing.push(format!("{}", variant.name));
                        }
                    }
                    if missing.is_empty() {
                        "not all patterns are covered".into()
                    } else {
                        format!("missing variant(s) {}", missing.join(", "))
                    }
                } else {
                    "not all patterns are covered".into()
                }
            }
            _ => "not all patterns are covered".into(),
        }
    }

    fn covers_bool(pat: &Pattern, val: bool) -> bool {
        match pat {
            Pattern::Bool(b) => *b == val,
            Pattern::Wildcard | Pattern::Ident(_) => true,
            Pattern::Or(alts) => alts.iter().any(|a| Self::covers_bool(a, val)),
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
}
