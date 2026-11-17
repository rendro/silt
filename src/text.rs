//! Measures of text that more than one part of silt needs: the edit
//! distance behind every "did you mean" hint, the checker's
//! (`typechecker::suggest`) and the command line's (an unknown command
//! of `silt`); and the hiding of a URL's user and password in a text
//! that is shown, which the http client's errors and the package code's
//! (git dependencies) share.

/// The places of `msg` that hold the user and password of a URL, the
/// one rule of what is hidden: whatever stands after a `//`, of any
/// scheme or of none (`https://u:p@h`, `ftp://u:p@h`, `//u:p@h`), up to
/// the LAST `@` of that URL (it ends at a space, a tab or a line feed).
/// The user and password a client sends are before the last `@` of the
/// authority; a password with a raw `/`, `?` or `#` in it ends the
/// authority there, and the rest of it, with its `@`, is in what reads
/// as the path or the query (`http://user:1234/ss@host/`), which no shape
/// tells from an `@` of a path. So the text before any `@` of a URL is
/// hidden, the host too at times (`https://h/users/@me`): more than a
/// password, never less. `scheme://@host` has none. One pass over the
/// text; `looked` gains the bytes looked at (what a test asks: no more
/// than a few times the text's length).
fn userinfo_places(msg: &str, looked: &mut usize) -> Vec<std::ops::Range<usize>> {
    let mut places = Vec::new();
    // Where the search for a URL goes on: never back, so each byte is
    // looked at a bounded number of times.
    let mut at = 0;
    while let Some(found) = msg[at..].find("//") {
        *looked += found + 2;
        let host_start = at + found + 2;
        let url = &msg[host_start..];
        let end = url.find([' ', '\t', '\n']).unwrap_or(url.len());
        *looked += 2 * end;
        at = match url[..end].rfind('@') {
            Some(last @ 1..) => {
                places.push(host_start..host_start + last);
                host_start + last
            }
            // (`//@host`: nothing before its one `@`.)
            Some(_) => host_start,
            // No `@` in the rest of this URL: no `//` in it has one.
            None => host_start + end,
        };
    }
    places
}

/// `msg` with each place of [`userinfo_places`] given by `hidden`.
fn with_places_hidden(
    msg: &str,
    places: &[std::ops::Range<usize>],
    hidden: impl Fn(&str) -> String,
) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut copied = 0;
    for place in places {
        out.push_str(&msg[copied..place.start]);
        out.push_str(&hidden(&msg[place.clone()]));
        copied = place.end;
    }
    out.push_str(&msg[copied..]);
    out
}

/// `msg` with the user and password of every URL in it
/// ([`userinfo_places`]) replaced by `***`: what every message of silt
/// that can quote a URL shows (the http client's errors, the package
/// code's: git dependencies, git's command lines and output).
/// `https://u:p@h/x` is shown `https://***@h/x`, `https://h/users/@me`
/// `https://***@me`.
pub(crate) fn hide_url_userinfo(msg: &str) -> String {
    hide_url_userinfo_counting(msg, &mut 0)
}

/// `line` with each character of the user and password of a URL in it
/// (the places [`hide_url_userinfo`] hides) replaced by a `*`: for a line
/// of a file shown under a caret, whose columns must stay where they are
/// (`git = "https://*****:******@host/r.git"`).
pub(crate) fn mask_url_userinfo(line: &str) -> String {
    let places = userinfo_places(line, &mut 0);
    with_places_hidden(line, &places, |place| place.chars().map(|_| '*').collect())
}

/// [`hide_url_userinfo`], adding to `looked` the bytes it looked at.
pub(crate) fn hide_url_userinfo_counting(msg: &str, looked: &mut usize) -> String {
    let places = userinfo_places(msg, looked);
    with_places_hidden(msg, &places, |_| "***".to_string())
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
            // A password with a raw `/`: to the URL's last `@`.
            (
                "git = \"https://MU:M/P@h/r\"",
                "git = \"https://******@h/r\"",
            ),
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
        assert_eq!(
            hide_url_userinfo("x https://MU:1/MP@h/r y"),
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
