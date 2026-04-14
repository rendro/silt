//! "did you mean ...?" suggestion helper for type-checker diagnostics.
//!
//! Closes round-17 deferred finding #4: when the type checker emits
//! `undefined variable '<typo>'` or `unknown function '<typo>' on module
//! '<mod>'`, it should append a `did you mean \`<candidate>\`?` hint when
//! a close-enough identifier exists in scope. This module owns the
//! string-distance math and the threshold policy so call sites stay
//! one-line.
//!
//! Pure stdlib — no external deps.

/// Levenshtein edit distance between two strings, O(n*m) time and O(m)
/// extra space.
pub(super) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Return the closest candidate to `typo` from `candidates`, or `None`
/// if no candidate is close enough.
///
/// Threshold policy:
/// - Exact matches to `typo` are filtered out (never suggest yourself).
/// - For a pair (typo, candidate), define `max = typo.len().max(cand.len())`.
/// - Short pairs (`max <= 5`) accept Levenshtein distance `<= 1`.
///   Round-24 tightened this from `<= 2` after the audit surfaced
///   low-signal hits like `foo` → `Bool` (d=2). Genuine 1-edit typos
///   on very short names still pass (`fo` → `for`, `namee` → `name`).
///   Two-edit typos on slightly longer names remain covered by the
///   scaled rule below: `pintln` → `println` (d=1, max=7, 1*3 <= 7),
///   `lenght` → `length` (d=2, max=6, 2*3 <= 6).
/// - Longer pairs accept `d * 3 <= max`, i.e. up to ~33% of the longer
///   string may change. Scales with length so 20-character names don't
///   get stuck at the absolute-1 threshold.
///
/// Among accepted candidates the smallest edit distance wins; ties break
/// lexicographically to keep the hint deterministic.
pub(super) fn suggest_similar<I, S>(typo: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut best: Option<(usize, String)> = None;
    for c in candidates {
        let c = c.as_ref();
        if c == typo {
            // Never suggest the typo itself — this can happen when the
            // caller sweeps its own env and the name is present under
            // a different scope (e.g. a shadowed fn param).
            continue;
        }
        let d = levenshtein(typo, c);
        let max = typo.chars().count().max(c.chars().count());
        let accept = if max <= 5 {
            d <= 1
        } else {
            d.saturating_mul(3) <= max
        };
        if !accept {
            continue;
        }
        match &best {
            Some((bd, bc)) if *bd < d => {}
            Some((bd, bc)) if *bd == d && bc.as_str() < c => {}
            _ => best = Some((d, c.to_string())),
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
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
        // surfaced. Lock: tests/suggest_threshold_tests.rs.
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
}
