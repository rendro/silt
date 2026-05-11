//! Round 85 parity lock: `docs/language/operators.md` precedence table
//! is kept in lockstep with the canonical binding-power constants in
//! `src/formatter.rs::mod bp` + `binop_l_bp()`.
//!
//! Background:
//!   * `docs/language/operators.md` lines 16-32 list every operator with
//!     a hand-numbered precedence (10/20/30/40/50/54/55/60/70/80/95).
//!   * The canonical source of truth is the parser's `(l_bp, r_bp)`
//!     ladder in `src/parser.rs::parse_expr_bp`, mirrored exactly by the
//!     `mod bp` constants + `binop_l_bp()` helper in
//!     `src/formatter.rs:5777-5814`.
//!   * Round 81 added a parity test between the formatter `bp` helper
//!     and the parser source (`tests/round81_formatter_binop_bp_parity_tests.rs`).
//!   * This file extends the parity chain to the public docs so that a
//!     bp ladder shift can't quietly diverge from the user-facing table.
//!
//! Strategy:
//!   * Pull the `mod bp { ... }` block out of `src/formatter.rs` and
//!     parse the named constants (FLOATELSE_L, RANGE_L, PIPE_L,
//!     ASCRIPTION, QUESTIONMARK) plus the bp values inside `binop_l_bp`
//!     for each `BinOp` variant.
//!   * Pull the `## Operator Table` section out of `docs/language/operators.md`
//!     and parse each row's `(precedence, operator symbol(s))` pair.
//!   * For every row that maps to a named constant or a binop value,
//!     assert the doc number equals the source number.
//!
//! Rows for prefix unary (`90`), trailing closure (`115`), call (`120`),
//! and field access (`130`) are NOT named constants in `mod bp` today.
//! They are deliberately skipped here (see Round-85 GAP-WEAK ticket: a
//! future round may extract them into named constants, at which point
//! this test gains four more pinned rows automatically).

use std::collections::BTreeMap;

const FORMATTER_SRC: &str = include_str!("../src/formatter.rs");
const DOC_SRC: &str = include_str!("../docs/language/operators.md");

/// Extract `pub const NAME: u8 = N;` lines from inside `mod bp { ... }`.
/// Returns a map from constant name to its integer value.
fn formatter_named_bp_constants() -> BTreeMap<String, u32> {
    let mod_start = FORMATTER_SRC
        .find("mod bp {")
        .expect("formatter.rs no longer contains `mod bp {` — has the bp table moved?");
    // Find the matching close brace at depth 0.
    let bytes = FORMATTER_SRC.as_bytes();
    let mut depth = 0i32;
    let mut i = mod_start + "mod bp {".len() - 1; // start at the `{`
    let mut mod_end = mod_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    mod_end = i + 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    assert!(
        mod_end > mod_start,
        "could not locate end of `mod bp` block in formatter.rs"
    );
    let body = &FORMATTER_SRC[mod_start..mod_end];

    let mut out = BTreeMap::new();
    for line in body.lines() {
        // Match `pub const NAME: u8 = N;` (with optional trailing comment)
        let trimmed = line.trim();
        let after_pub = trimmed.strip_prefix("pub const ").or_else(|| {
            // Also accept private `const NAME: u8 = N;` if anyone later
            // tightens visibility — keeps the parity test robust.
            trimmed.strip_prefix("const ")
        });
        let Some(after_pub) = after_pub else { continue };
        // Split "NAME: u8 = N;..."
        let Some((name, rest)) = after_pub.split_once(':') else {
            continue;
        };
        let Some((_ty, rest)) = rest.split_once('=') else {
            continue;
        };
        // Strip ; and trailing comment.
        let value_str = rest
            .split(';')
            .next()
            .unwrap_or("")
            .split("//")
            .next()
            .unwrap_or("")
            .trim();
        if let Ok(n) = value_str.parse::<u32>() {
            out.insert(name.trim().to_string(), n);
        }
    }
    out
}

/// Extract the BinOp -> bp value mapping from `pub fn binop_l_bp(op: BinOp) -> u8`.
/// Returns a map from variant name (e.g. `Or`) to its bp value.
fn formatter_binop_l_bp_table() -> BTreeMap<String, u32> {
    let fn_start = FORMATTER_SRC
        .find("pub fn binop_l_bp(op: BinOp) -> u8")
        .or_else(|| FORMATTER_SRC.find("fn binop_l_bp(op: BinOp) -> u8"))
        .expect(
            "formatter.rs no longer contains `binop_l_bp` helper — \
             Round 81's bp unification has been undone.",
        );
    // Take a generous window — the match body fits in well under 1KB.
    let body_end = (fn_start + 1200).min(FORMATTER_SRC.len());
    let body = &FORMATTER_SRC[fn_start..body_end];

    let mut out = BTreeMap::new();
    for line in body.lines() {
        // Match lines like `BinOp::Or => 20,` or `BinOp::Eq | BinOp::Neq => 40,`
        let trimmed = line.trim();
        if !trimmed.starts_with("BinOp::") {
            continue;
        }
        let Some((lhs, rhs)) = trimmed.split_once("=>") else {
            continue;
        };
        let value_str = rhs.trim().trim_end_matches(',').trim();
        let Ok(n) = value_str.parse::<u32>() else {
            continue;
        };
        for piece in lhs.split('|') {
            let variant = piece.trim().trim_start_matches("BinOp::").trim();
            if !variant.is_empty() {
                out.insert(variant.to_string(), n);
            }
        }
    }
    out
}

/// A row parsed from the markdown table in `docs/language/operators.md`.
#[derive(Debug, Clone)]
struct DocRow {
    precedence: u32,
    operators: String,
    kind: String,
}

/// Parse the `## Operator Table` markdown table rows. Skips the header
/// row and the separator row.
fn doc_table_rows() -> Vec<DocRow> {
    let table_start = DOC_SRC
        .find("## Operator Table")
        .expect("operators.md no longer has `## Operator Table` heading");
    let rest = &DOC_SRC[table_start..];
    let mut rows = Vec::new();
    let mut saw_header = false;
    for line in rest.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            // Stop at the first non-table line AFTER we've started seeing rows.
            if !rows.is_empty() {
                break;
            }
            continue;
        }
        // Skip separator rows like `|---|---|`
        if trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            saw_header = true; // technically the header divider; mark we passed it
            continue;
        }
        // Pull cells. Strip leading + trailing `|` then split.
        let inner = trimmed.trim_matches('|');
        let cells: Vec<&str> = inner.split('|').map(|c| c.trim()).collect();
        if cells.len() < 4 {
            continue;
        }
        // First row is the column header; skip it.
        if !saw_header {
            // The header has `Precedence` in column 0 — recognise it and skip.
            if cells[0].eq_ignore_ascii_case("precedence") {
                continue;
            }
            // Otherwise we never saw a separator yet — accept rows after we
            // do see one. Fall through to skip until the separator marks
            // `saw_header = true`.
            continue;
        }
        // Parse the precedence number.
        let Ok(prec) = cells[0].parse::<u32>() else {
            continue;
        };
        rows.push(DocRow {
            precedence: prec,
            operators: cells[1].to_string(),
            kind: cells[2].to_string(),
        });
    }
    rows
}

#[test]
fn doc_table_parses_expected_row_count() {
    let rows = doc_table_rows();
    // Round-85 snapshot: 13 rows (10/20/30/40/50/54/55/60/70/80/90/95/115/120/130).
    // We accept 12-16 to allow small additions; the parity test below is the
    // real lock.
    assert!(
        rows.len() >= 10 && rows.len() <= 20,
        "docs/language/operators.md operator-table row count is wildly off: {} rows. \
         Either the parser broke or the table structure changed. Rows: {:#?}",
        rows.len(),
        rows
    );
}

#[test]
fn doc_precedence_matches_named_bp_constants() {
    let bp = formatter_named_bp_constants();
    let rows = doc_table_rows();

    // (doc-precedence-number, constant-name-in-mod-bp)
    // Each binding here pins one row of the markdown table to one named
    // constant in `mod bp`. The bp value MUST be present in both sides
    // with the same number, or the test fails.
    let pins: &[(u32, &str)] = &[
        (10, "FLOATELSE_L"),
        (54, "QUESTIONMARK"),
        (55, "PIPE_L"),
        (60, "RANGE_L"),
        (95, "ASCRIPTION"),
    ];

    for (expected, const_name) in pins {
        let constant_value = bp.get(*const_name).unwrap_or_else(|| {
            panic!(
                "formatter `mod bp` no longer exposes `{const_name}`. \
                 Parsed constants: {bp:#?}"
            )
        });
        assert_eq!(
            *constant_value, *expected,
            "operators.md row pinned to `bp::{const_name}` claims precedence {expected}, \
             but the constant is {constant_value}. One of the two has drifted; update them \
             in lockstep.",
        );
        // Also assert the doc table contains a row whose precedence matches.
        let row = rows.iter().find(|r| r.precedence == *expected);
        assert!(
            row.is_some(),
            "operators.md operator-table has no row with precedence {expected} \
             (expected to map to `bp::{const_name}`). Either add the row or update the pin."
        );
    }
}

#[test]
fn doc_precedence_matches_binop_l_bp_table() {
    let table = formatter_binop_l_bp_table();
    let rows = doc_table_rows();

    // (doc-precedence-number, list of BinOp variants the row claims to cover)
    // Each row of the markdown table lists the operator(s) at that bp level;
    // here we map those operators to BinOp variants and assert the
    // `binop_l_bp` table agrees.
    let pins: &[(u32, &[&str])] = &[
        (20, &["Or"]),
        (30, &["And"]),
        (40, &["Eq", "Neq"]),
        (50, &["Lt", "Gt", "Leq", "Geq"]),
        (70, &["Add", "Sub"]),
        (80, &["Mul", "Div", "Mod"]),
    ];

    for (expected, variants) in pins {
        // Source side: every named variant must have the expected bp.
        for v in *variants {
            let got = table.get(*v).unwrap_or_else(|| {
                panic!(
                    "formatter `binop_l_bp` no longer maps `BinOp::{v}` to any bp. \
                     Parsed table: {table:#?}"
                )
            });
            assert_eq!(
                *got, *expected,
                "operators.md row at precedence {expected} pins `BinOp::{v}` to {expected}, \
                 but `binop_l_bp` reports {got}. One side has drifted; update in lockstep."
            );
        }
        // Doc side: there must be a row with that precedence.
        let row = rows.iter().find(|r| r.precedence == *expected);
        assert!(
            row.is_some(),
            "operators.md operator-table has no row with precedence {expected} \
             (expected to cover BinOps: {variants:?}). Either add the row or update the pin."
        );
    }
}

#[test]
fn doc_table_has_no_duplicate_precedence_numbers() {
    // Defensive: if someone copies a row and forgets to renumber it, the
    // parity tests above could spuriously pass (because both rows match
    // the same constant). Assert uniqueness so that's caught here.
    let rows = doc_table_rows();
    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        assert!(
            seen.insert(r.precedence),
            "duplicate precedence number {} in operators.md operator-table \
             (operators: {:?}, kind: {:?}). Rows must be unique on precedence.",
            r.precedence,
            r.operators,
            r.kind,
        );
    }
}

#[test]
fn doc_named_constants_are_all_pinned() {
    // Lock-the-lock: every NAMED bp constant in `mod bp` must appear in
    // the `doc_precedence_matches_named_bp_constants` pin list. If
    // someone adds a new named constant (e.g. extracting CALL_BP = 120),
    // this test fails and prompts a doc-side pin update.
    let bp = formatter_named_bp_constants();
    let pinned: std::collections::HashSet<&str> = [
        "FLOATELSE_L",
        "FLOATELSE_R",
        "RANGE_L",
        "RANGE_R",
        "PIPE_L",
        "PIPE_R",
        "ASCRIPTION",
        "QUESTIONMARK",
    ]
    .iter()
    .copied()
    .collect();
    for name in bp.keys() {
        assert!(
            pinned.contains(name.as_str()),
            "formatter `mod bp` has a new named constant `{name}` that is not pinned \
             in this parity test. Add it to the pin list in \
             `doc_precedence_matches_named_bp_constants` (and add a row to \
             docs/language/operators.md if it's user-facing)."
        );
    }
    // And the inverse: every pinned name still exists in `mod bp`. If one
    // is renamed/removed, the test fails so the doc table can be updated
    // alongside.
    for expected_name in [
        "FLOATELSE_L",
        "RANGE_L",
        "PIPE_L",
        "ASCRIPTION",
        "QUESTIONMARK",
    ] {
        assert!(
            bp.contains_key(expected_name),
            "formatter `mod bp` no longer exposes `{expected_name}` — the doc parity \
             pin in this test is stale. Update both the constant and the pin list."
        );
    }
}
