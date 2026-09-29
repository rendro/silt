//! Round-85 lock: `find_type_in_expr` in `src/lsp/ast_walk.rs` must
//! NOT inline a second copy of `visit_expr_children`'s `ExprKind::*`
//! arm enumeration. Pre-round-85 both walkers each enumerated all 23
//! child-bearing variants, and drift in either was silent — a new
//! `ExprKind` variant added to one but not the other would silently
//! be skipped by hover / find-type while find-references kept working
//! (or vice versa).
//!
//! Fix: `find_type_in_expr` delegates child enumeration to
//! `visit_expr_children`. This test grep-counts `ExprKind::` matches
//! in each function's body and asserts:
//!   (a) `find_type_in_expr` contains a call to `visit_expr_children`
//!       (proving it delegates), and
//!   (b) `find_type_in_expr`'s body contains zero `ExprKind::` arms
//!       in a `match &expr.kind` table (its only `ExprKind::` mention
//!       — if any — is incidental, not a duplicated arm list).
//!
//! The second assertion is intentionally aggressive: after the
//! refactor `find_type_in_expr` has NO `match &expr.kind` table at
//! all. If a future change reintroduces one, this lock fires.

fn read_walker_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lsp/ast_walk.rs");
    std::fs::read_to_string(path).expect("read ast_walk.rs")
}

/// Extract the body of a top-level `fn <name>(` ... matching by
/// brace-balanced scan. Returns the slice from the first `{` after the
/// `fn` keyword through its matching `}`. Naive (does not skip strings
/// / comments), but the walker file has no nested braces inside string
/// or comment content that would confuse it.
fn extract_fn_body<'a>(source: &'a str, fn_name: &str) -> &'a str {
    let needle = format!("fn {fn_name}");
    let start = source.find(&needle).unwrap_or_else(|| {
        panic!("function `fn {fn_name}` not found in ast_walk.rs");
    });
    let rest = &source[start..];
    let open = rest.find('{').expect("opening brace");
    let body_start = start + open;
    let bytes = source.as_bytes();
    let mut depth: i32 = 0;
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced braces in `fn {fn_name}`");
}

#[test]
fn find_type_in_expr_delegates_to_visit_expr_children() {
    let src = read_walker_source();
    let body = extract_fn_body(&src, "find_type_in_expr");
    assert!(
        body.contains("visit_expr_children"),
        "`find_type_in_expr` no longer delegates to `visit_expr_children`:\n{body}"
    );
}

#[test]
fn find_type_in_expr_does_not_inline_exprkind_arms() {
    // After the round-85 dedup, the function body has no `match
    // &expr.kind` table — child enumeration is fully delegated to
    // `visit_expr_children`. The `ExprKind::` count in the body is 0.
    let src = read_walker_source();
    let body = extract_fn_body(&src, "find_type_in_expr");
    let n = body.matches("ExprKind::").count();
    assert_eq!(
        n, 0,
        "`find_type_in_expr` body re-introduces inline `ExprKind::` arms ({n} matches); \
         it should delegate child enumeration to `visit_expr_children`:\n{body}"
    );
}

#[test]
fn visit_expr_children_still_owns_the_arm_table() {
    // Sanity: the single canonical arm table lives in `visit_expr_children`.
    // If a future refactor moves it elsewhere this test will need to
    // update, but until then it documents the invariant.
    let src = read_walker_source();
    let body = extract_fn_body(&src, "visit_expr_children");
    let n = body.matches("ExprKind::").count();
    assert!(
        n >= 15,
        "`visit_expr_children` lost its arm table (only {n} ExprKind:: matches); \
         the round-85 dedup expects this function to remain the single source of truth"
    );
}
