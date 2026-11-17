//! Measures of text that more than one part of silt needs: the edit
//! distance behind every "did you mean" hint, the checker's
//! (`typechecker::suggest`) and the command line's (an unknown command
//! of `silt`); and the hiding of a URL's user and password in a text
//! that is shown, which the http client's errors and the package code's
//! (git dependencies) share.

/// `msg` with the user and password of every URL in it replaced by
/// `***`: whatever stands after a `//`, of any scheme or of none
/// (`https://u:p@h`, `ftp://u:p@h`, `//u:p@h`), up to the LAST `@`
/// before the end of the authority (a `/`, `?`, `#`, a space, a tab or a
/// line feed), so a password with a raw `@` in it is hidden whole.
/// `scheme://@host` stays as it is. One pass over the text.
pub(crate) fn hide_url_userinfo(msg: &str) -> String {
    hide_url_userinfo_counting(msg, &mut 0)
}

/// `line` with each character of the user and password of a URL in it
/// (what [`hide_url_userinfo`] hides) replaced by a `*`: for a line of a
/// file shown under a caret, whose columns must stay where they are
/// (`git = "https://*****:******@host/r.git"`).
pub(crate) fn mask_url_userinfo(line: &str) -> String {
    let hidden = hide_url_userinfo(line);
    if hidden == line {
        return hidden;
    }
    // The places of `line` that `hide_url_userinfo` left out: the same
    // scan, keeping the length.
    let mut out = String::with_capacity(line.len());
    let (mut copied, mut at) = (0, 0);
    while let Some(found) = line[at..].find("//") {
        let host_start = at + found + 2;
        let authority = &line[host_start..];
        let end = authority
            .find(['/', '?', '#', ' ', '\t', '\n'])
            .unwrap_or(authority.len());
        at = match authority[..end].rfind('@') {
            Some(last @ 1..) => {
                out.push_str(&line[copied..host_start]);
                out.extend(authority[..last].chars().map(|_| '*'));
                copied = host_start + last;
                copied
            }
            _ => host_start,
        };
    }
    out.push_str(&line[copied..]);
    out
}

/// [`hide_url_userinfo`], adding to `looked` the bytes it looked at
/// (what a test asks: no more than a few times the text's length).
pub(crate) fn hide_url_userinfo_counting(msg: &str, looked: &mut usize) -> String {
    let mut out = String::with_capacity(msg.len());
    // What of `msg` is in `out`, and where the search for an authority
    // goes on: never back, so each byte is looked at a bounded number
    // of times.
    let (mut copied, mut at) = (0, 0);
    while let Some(found) = msg[at..].find("//") {
        *looked += found + 2;
        let host_start = at + found + 2;
        let authority = &msg[host_start..];
        let end = authority
            .find(['/', '?', '#', ' ', '\t', '\n'])
            .unwrap_or(authority.len());
        *looked += 2 * end;
        at = match authority[..end].rfind('@') {
            Some(last @ 1..) => {
                out.push_str(&msg[copied..host_start]);
                out.push_str("***");
                copied = host_start + last;
                copied
            }
            _ => host_start,
        };
    }
    out.push_str(&msg[copied..]);
    out
}

/// The Levenshtein edit distance of `a` and `b`, by characters: the
/// fewest insertions, removals and replacements that make one the
/// other. O(n*m) time and O(m) extra space.
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
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
    use super::{hide_url_userinfo, levenshtein, mask_url_userinfo};

    /// A line under a caret keeps its columns: each character of the user
    /// and the password is a `*`; the rest of the line is as it was.
    #[test]
    fn a_masked_line_keeps_its_columns() {
        for (line, masked) in [
            (
                r#"r = { git = "https://MU:MP@h/r.git", branch = "main" }"#,
                r#"r = { git = "https://*****@h/r.git", branch = "main" }"#,
            ),
            ("a //MU@h b //c@d", "a //**@h b //*@d"),
            ("git = \"ssh://git@h/r\"", "git = \"ssh://***@h/r\""),
            ("no url here", "no url here"),
            ("http://@h/", "http://@h/"),
        ] {
            assert_eq!(mask_url_userinfo(line), masked, "{line}");
            assert_eq!(masked.chars().count(), line.chars().count(), "{line}");
        }
        assert_eq!(
            hide_url_userinfo("x https://MU:MP@h/r y"),
            "x https://***@h/r y"
        );
    }

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
