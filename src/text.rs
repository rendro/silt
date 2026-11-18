//! Measures of text that more than one part of silt needs: the edit
//! distance behind every "did you mean" hint, the checker's
//! (`typechecker::suggest`) and the command line's (an unknown command
//! of `silt`); and the hiding of a URL's user and password in a text
//! that is shown, by the rule of the http client's errors (a git URL is
//! hidden by the rule of `git::display_url`).

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
#[cfg(feature = "http")]
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

/// `msg` with the user and password of every URL in it
/// ([`userinfo_places`]) replaced by `***`: what a message of the http
/// client that can quote a URL shows. `https://u:p@h/x` is shown
/// `https://***@h/x`, `https://h/users/@me` `https://***@me`.
#[cfg(all(feature = "http", any(test, feature = "test-hooks")))]
pub(crate) fn hide_url_userinfo(msg: &str) -> String {
    hide_url_userinfo_counting(msg, &mut 0)
}

/// [`hide_url_userinfo`], adding to `looked` the bytes it looked at.
#[cfg(feature = "http")]
pub(crate) fn hide_url_userinfo_counting(msg: &str, looked: &mut usize) -> String {
    let places = userinfo_places(msg, looked);
    let mut out = String::with_capacity(msg.len());
    let mut copied = 0;
    for place in places {
        out.push_str(&msg[copied..place.start]);
        out.push_str("***");
        copied = place.end;
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
    #[cfg(feature = "http")]
    use super::hide_url_userinfo;
    use super::levenshtein;

    /// The user and password of a URL in a message are `***`.
    #[cfg(feature = "http")]
    #[test]
    fn a_message_hides_the_user_and_password_of_a_url() {
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
