use super::*;

#[test]
fn test_levenshtein_basic() {
    assert_eq!(levenshtein("", ""), 0);
    assert_eq!(levenshtein("abc", ""), 3);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("abc", "abc"), 0);
    assert_eq!(levenshtein("pintln", "println"), 1);
    assert_eq!(levenshtein("lenght", "length"), 2);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
}

#[test]
fn test_suggest_similar_close_match() {
    let cands = ["println", "print", "panic"];
    assert_eq!(
        suggest_similar("pintln", cands.iter()),
        Some("println".to_string())
    );
    // `lenght` → `length` (d=2, max=6, accepts under scaled rule)
    assert_eq!(
        suggest_similar("lenght", ["length", "filter", "map"].iter()),
        Some("length".to_string())
    );
}

#[test]
fn test_suggest_similar_too_far() {
    let cands = ["println", "print", "panic"];
    // `xyzzy_completely_unrelated` is too far from anything in the
    // candidate set — don't offer a misleading hint.
    assert_eq!(
        suggest_similar("xyzzy_completely_unrelated", cands.iter()),
        None
    );
    // `xyz` → `abc` is distance 3, max 3 — over the absolute-1 cap.
    assert_eq!(suggest_similar("xyz", ["abc"].iter()), None);
    // `foo` → `Bool` is distance 2, max 4. Under the tightened
    // short-pair cap (d <= 1) this must NOT produce a hint — it
    // was the canonical low-signal suggestion the old d<=2 cap
    // surfaced. Lock: tests/lang/suggest_threshold_tests.rs.
    assert_eq!(suggest_similar("foo", ["Bool"].iter()), None);
}

#[test]
fn test_suggest_similar_exact_match_filtered() {
    // If the candidate set contains the typo itself (e.g. because
    // the caller dumped its own scope), don't suggest it back.
    assert_eq!(
        suggest_similar("foo", ["foo", "fool"].iter()),
        Some("fool".to_string())
    );
}

#[test]
fn test_suggest_similar_empty_candidates() {
    let empty: [&str; 0] = [];
    assert_eq!(suggest_similar("anything", empty.iter()), None);
}

#[test]
fn test_suggest_similar_picks_closest() {
    // Among two equally-plausible candidates, prefer the one with
    // the smaller edit distance.
    assert_eq!(
        suggest_similar("lenght", ["length", "lengthen"].iter()),
        Some("length".to_string())
    );
}

#[test]
fn test_suggest_similar_long_names_scale_threshold() {
    // A 20-char typo with 4 edits should still get a suggestion
    // under the scaled rule (d*3 <= max).
    assert_eq!(
        suggest_similar(
            "compute_totl_ammount",
            ["compute_total_amount", "render", "sort"].iter()
        ),
        Some("compute_total_amount".to_string())
    );
}
