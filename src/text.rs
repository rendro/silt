//! Measures of text that more than one part of silt needs: the edit
//! distance behind every "did you mean" hint, the checker's
//! (`typechecker::suggest`) and the command line's (an unknown command
//! of `silt`).

/// The Levenshtein edit distance of `a` and `b`, by characters: the
/// fewest insertions, removals and replacements that make one the
/// other. O(n*m) time and O(m) extra space.
pub fn levenshtein(a: &str, b: &str) -> usize {
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

#[cfg(test)]
mod tests {
    use super::levenshtein;

    #[test]
    fn the_distance_counts_the_edits() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("pintln", "println"), 1);
        assert_eq!(levenshtein("lenght", "length"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        // By characters, not bytes.
        assert_eq!(levenshtein("é", "e"), 1);
        // The command line's typos.
        assert_eq!(levenshtein("chek", "check"), 1);
        assert_eq!(levenshtein("--verison", "--version"), 2);
    }
}
