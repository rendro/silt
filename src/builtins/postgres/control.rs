//! The statements a program may not send itself: the ones that begin or
//! end a transaction, and, outside one, the ones that work only inside
//! a transaction (savepoints).
//!
//! A statement on a pool runs on its own; `postgres.transact` begins and
//! ends its transaction itself. A `BEGIN` sent on the pool would leave
//! its connection inside a transaction nobody ends, which goes back to
//! the pool: the statements after it that get that connection answer
//! `Ok` and are never committed, or are committed by the `COMMIT` of an
//! unrelated `postgres.transact`. A `COMMIT` sent inside
//! `postgres.transact` would end its transaction early, and the
//! callback's `Err` would roll nothing back.
//!
//! Every statement of the module goes to the server as one prepared
//! statement (the extended protocol), which holds one command at most:
//! the server refuses a string with two. So the first word of the
//! command decides what it is, and a transaction can be opened or ended
//! by no other command: a `DO` block or a function cannot run
//! `BEGIN`, and a procedure's `COMMIT` cannot leave a transaction open
//! or end one it was called in. What is read here is that first word,
//! as the server reads it: after white space, comments (`--` to the end
//! of the line, `/* */`, which nest) and empty commands (`;`), in any
//! case.

/// A statement the program may not send itself, by its first words, as
/// a message names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Control {
    /// It begins or ends a transaction (`BEGIN`, `START TRANSACTION`,
    /// `COMMIT`, `END`, `ROLLBACK`, `ABORT`, `PREPARE TRANSACTION`):
    /// refused on a pool and inside `postgres.transact`.
    Ends(&'static str),
    /// It works only inside a transaction (`SAVEPOINT`, `RELEASE`,
    /// `ROLLBACK TO`): refused on a pool.
    Savepoint(&'static str),
}

/// What `sql` is, if it is a statement the program may not send
/// itself. `COMMIT PREPARED` and `ROLLBACK PREPARED`, which end a
/// prepared transaction of any session, are not: they run on a pool,
/// and the server refuses them inside a transaction.
pub(super) fn control(sql: &str) -> Option<Control> {
    let (first, rest) = word(skip(sql, true))?;
    let next = || word(skip(rest, false));
    let is =
        |w: Option<(&str, &str)>, want: &str| w.is_some_and(|(w, _)| w.eq_ignore_ascii_case(want));
    let first = first.to_ascii_lowercase();
    Some(match first.as_str() {
        "begin" => Control::Ends("BEGIN"),
        "start" if is(next(), "transaction") => Control::Ends("START TRANSACTION"),
        "end" => Control::Ends("END"),
        "abort" => Control::Ends("ABORT"),
        "commit" if is(next(), "prepared") => return None,
        "commit" => Control::Ends("COMMIT"),
        "rollback" if is(next(), "prepared") => return None,
        "rollback" => {
            // `ROLLBACK [WORK | TRANSACTION] TO [SAVEPOINT] name`
            let mut after = next();
            if is(after, "work") || is(after, "transaction") {
                after = after.and_then(|(_, rest)| word(skip(rest, false)));
            }
            match is(after, "to") {
                true => Control::Savepoint("ROLLBACK TO"),
                false => Control::Ends("ROLLBACK"),
            }
        }
        "savepoint" => Control::Savepoint("SAVEPOINT"),
        "release" => Control::Savepoint("RELEASE"),
        // `PREPARE TRANSACTION 'id'`, not `PREPARE transaction AS ...`,
        // a statement named `transaction`.
        "prepare" => match next() {
            Some((w, rest)) if w.eq_ignore_ascii_case("transaction") => {
                let rest = skip(rest, false);
                let named = rest.starts_with('(') || is(word(rest), "as");
                match named {
                    true => return None,
                    false => Control::Ends("PREPARE TRANSACTION"),
                }
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// A `COPY` of standard input or output: the copy protocol, whose data
/// the module does not send or read. (The client answers its first
/// message with "unexpected message from server" and closes the
/// connection.) A `COPY` of a file or a program the server reaches
/// runs as any other statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Copy {
    FromStdin,
    ToStdout,
}

impl Copy {
    /// The statement as a message names it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Copy::FromStdin => "COPY ... FROM STDIN",
            Copy::ToStdout => "COPY ... TO STDOUT",
        }
    }
}

/// What `sql` is, if it is a `COPY` of standard input or output: a
/// `COPY` whose words outside parentheses, strings and quoted names
/// hold `FROM STDIN` or `TO STDOUT` (the table's columns and a query
/// are in parentheses, a file's name in a string).
pub(super) fn copy_stream(sql: &str) -> Option<Copy> {
    let (first, mut rest) = word(skip(sql, true))?;
    if !first.eq_ignore_ascii_case("copy") {
        return None;
    }
    let mut depth = 0usize;
    // The word before, outside parentheses.
    let mut before: Option<&str> = None;
    loop {
        rest = skip(rest, false);
        if let Some((w, after)) = word(rest) {
            if depth == 0 {
                let after_word = |want: &str| before.is_some_and(|b| b.eq_ignore_ascii_case(want));
                if after_word("from") && w.eq_ignore_ascii_case("stdin") {
                    return Some(Copy::FromStdin);
                }
                if after_word("to") && w.eq_ignore_ascii_case("stdout") {
                    return Some(Copy::ToStdout);
                }
                before = Some(w);
            }
            rest = after;
            continue;
        }
        let mut chars = rest.chars();
        let c = chars.next()?;
        if depth == 0 {
            before = None;
        }
        rest = match c {
            '(' => {
                depth += 1;
                chars.as_str()
            }
            ')' => {
                depth = depth.saturating_sub(1);
                chars.as_str()
            }
            '\'' | '"' => after_quoted(rest, c),
            '$' => after_dollar_quote(rest),
            _ => chars.as_str(),
        };
    }
}

/// Whether `sql` leaves in the session what the reset of a connection
/// does not undo, or drops what the connection keeps for itself, so that
/// its connection is closed instead of given back: `LOAD` (a library
/// stays loaded, as `DISCARD ALL` leaves it), `DEALLOCATE` (of one of
/// the statements the client keeps prepared, by its name, the next
/// statement that needs it fails).
pub(super) fn closes_session(sql: &str) -> bool {
    word(skip(sql, true)).is_some_and(|(w, _)| {
        w.eq_ignore_ascii_case("load") || w.eq_ignore_ascii_case("deallocate")
    })
}

/// `s`, which starts with the quote `quote` (`'` of a string, `"` of a
/// name), after the quoted text: a doubled quote is one inside it. An
/// unterminated one runs to the end.
fn after_quoted(s: &str, quote: char) -> &str {
    let mut rest = &s[quote.len_utf8()..];
    loop {
        match rest.find(quote) {
            None => return "",
            Some(at) => {
                rest = &rest[at + quote.len_utf8()..];
                match rest.strip_prefix(quote) {
                    Some(more) => rest = more,
                    None => return rest,
                }
            }
        }
    }
}

/// `s`, which starts with `$`, after a dollar-quoted string (`$$...$$`,
/// `$tag$...$tag$`); after the `$` alone if it starts none (`$1`). An
/// unterminated one runs to the end.
fn after_dollar_quote(s: &str) -> &str {
    let body = &s[1..];
    let tag_len = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()))
        .unwrap_or(body.len());
    let opens = body[tag_len..].starts_with('$')
        && !body[..tag_len].starts_with(|c: char| c.is_ascii_digit());
    if !opens {
        return body;
    }
    let delimiter = &s[..tag_len + 2];
    let inside = &s[delimiter.len()..];
    match inside.find(delimiter) {
        Some(at) => &inside[at + delimiter.len()..],
        None => "",
    }
}

/// `s` after the white space and comments it starts with, and, at the
/// start of a statement (`empty`), the empty commands too. An
/// unterminated `/*` comment runs to the end.
fn skip(mut s: &str, empty: bool) -> &str {
    loop {
        let trimmed = s.trim_start_matches(|c: char| {
            matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c') || (empty && c == ';')
        });
        if let Some(line) = trimmed.strip_prefix("--") {
            s = match line.find(['\n', '\r']) {
                Some(end) => &line[end..],
                None => "",
            };
        } else if trimmed.starts_with("/*") {
            s = after_block_comment(trimmed);
        } else {
            return trimmed;
        }
    }
}

/// `s`, which starts with `/*`, after the comment: comments nest.
fn after_block_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'/', b'*') => {
                depth += 1;
                i += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return &s[i..];
                }
            }
            _ => i += 1,
        }
    }
    ""
}

/// The word `s` starts with, as the server reads a keyword, and the rest.
fn word(s: &str) -> Option<(&str, &str)> {
    let starts = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
    let goes_on = |c: char| starts(c) || c.is_ascii_digit() || c == '$';
    if !s.starts_with(starts) {
        return None;
    }
    let end = s.find(|c: char| !goes_on(c)).unwrap_or(s.len());
    Some(s.split_at(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statements_that_begin_or_end_a_transaction() {
        for (sql, name) in [
            ("BEGIN", "BEGIN"),
            ("begin", "BEGIN"),
            ("BeGiN WORK", "BEGIN"),
            ("BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE", "BEGIN"),
            ("BEGIN;", "BEGIN"),
            ("START TRANSACTION", "START TRANSACTION"),
            ("start transaction read only", "START TRANSACTION"),
            ("COMMIT", "COMMIT"),
            ("COMMIT AND CHAIN", "COMMIT"),
            ("commit work", "COMMIT"),
            ("END", "END"),
            ("end transaction", "END"),
            ("ROLLBACK", "ROLLBACK"),
            ("ROLLBACK AND NO CHAIN", "ROLLBACK"),
            ("rollback work", "ROLLBACK"),
            ("ABORT", "ABORT"),
            ("PREPARE TRANSACTION 'x'", "PREPARE TRANSACTION"),
            ("prepare transaction $$x$$", "PREPARE TRANSACTION"),
        ] {
            assert_eq!(control(sql), Some(Control::Ends(name)), "{sql}");
        }
    }

    #[test]
    fn the_statements_that_work_only_inside_a_transaction() {
        for (sql, name) in [
            ("SAVEPOINT a", "SAVEPOINT"),
            ("RELEASE a", "RELEASE"),
            ("RELEASE SAVEPOINT a", "RELEASE"),
            ("ROLLBACK TO a", "ROLLBACK TO"),
            ("ROLLBACK TO SAVEPOINT a", "ROLLBACK TO"),
            ("rollback work to savepoint a", "ROLLBACK TO"),
            ("ROLLBACK TRANSACTION TO a", "ROLLBACK TO"),
            ("ROLLBACK /* x */ TO a", "ROLLBACK TO"),
        ] {
            assert_eq!(control(sql), Some(Control::Savepoint(name)), "{sql}");
        }
    }

    /// White space, comments and empty commands before the first word
    /// do not hide it.
    #[test]
    fn what_comes_before_the_first_word_does_not_hide_it() {
        for sql in [
            "  \t\n BEGIN",
            "\r\n\x0b\x0cBEGIN",
            "-- a comment\nBEGIN",
            "-- a comment\rBEGIN",
            "--\n--\nBEGIN",
            "/* a comment */ BEGIN",
            "/* nested /* comments */ still */BEGIN",
            "/**/BEGIN",
            ";BEGIN",
            " ; ; BEGIN",
            "/* x */ ; -- y\n ;begin",
        ] {
            assert_eq!(control(sql), Some(Control::Ends("BEGIN")), "{sql:?}");
        }
    }

    #[test]
    fn other_statements_are_sent() {
        for sql in [
            "SELECT 1",
            "INSERT INTO begin_log VALUES (1)",
            "SELECT 'BEGIN'",
            "\"BEGIN\"",
            "beginning",
            "BEGIN_X",
            "begin$x",
            "DO $$BEGIN PERFORM 1; END$$",
            "CREATE FUNCTION f() RETURNS int BEGIN ATOMIC SELECT 1; END",
            "COMMIT PREPARED 'x'",
            "commit /* */ prepared 'x'",
            "ROLLBACK PREPARED 'x'",
            "PREPARE transaction AS SELECT 1",
            "PREPARE transaction (int) AS SELECT $1",
            "PREPARE p AS SELECT 1",
            "SET statement_timeout = 1",
            "-- BEGIN\nSELECT 1",
            "/* BEGIN */ SELECT 1",
            "/* unterminated BEGIN",
            "-- BEGIN",
            "",
            ";",
            "   ",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "(SELECT 1)",
            "rollbacks",
            "START",
            "start something",
            "ÄBEGIN",
        ] {
            assert_eq!(control(sql), None, "{sql:?}");
        }
    }

    /// A `COPY` of standard input or output is found in each way the
    /// server reads one, and only then.
    #[test]
    fn a_copy_of_standard_input_or_output_is_found() {
        for (sql, copy) in [
            ("COPY t FROM STDIN", Copy::FromStdin),
            ("copy t from stdin with (format csv)", Copy::FromStdin),
            ("COPY t (a, b) FROM STDIN", Copy::FromStdin),
            ("COPY public.t FROM STDIN CSV HEADER", Copy::FromStdin),
            ("COPY \"t x\" FROM /* c */ STDIN", Copy::FromStdin),
            ("-- c\nCOPY t FROM\nSTDIN", Copy::FromStdin),
            ("COPY t TO STDOUT", Copy::ToStdout),
            (
                "COPY (SELECT 'FROM STDIN', ')' FROM t) TO STDOUT",
                Copy::ToStdout,
            ),
            (
                "COPY (SELECT $$ ) TO STDOUT $$) TO stdout (FORMAT binary)",
                Copy::ToStdout,
            ),
            ("COPY (SELECT $a$x$a$, $1) TO STDOUT", Copy::ToStdout),
        ] {
            assert_eq!(copy_stream(sql), Some(copy), "{sql:?}");
        }
        for sql in [
            "COPY t FROM '/tmp/x.csv'",
            "COPY t FROM 'stdin'",
            "COPY t TO '/tmp/stdout'",
            "COPY t FROM PROGRAM 'cat stdin'",
            "COPY (SELECT 1 FROM stdin) TO '/tmp/x'",
            "COPY stdin FROM '/tmp/x'",
            "COPY t (\"from\", stdin) FROM '/x'",
            "SELECT 'COPY t FROM STDIN'",
            "-- COPY t FROM STDIN\nSELECT 1",
            "COPYING t FROM STDIN",
            "COPY t FROM",
            "COPY (SELECT ')",
            "COPY $$",
            "",
        ] {
            assert_eq!(copy_stream(sql), None, "{sql:?}");
        }
    }

    /// `LOAD` and `DEALLOCATE` close their connection; nothing else does
    /// by its first word.
    #[test]
    fn load_and_deallocate_close_their_session() {
        for sql in [
            "LOAD 'auto_explain'",
            "load '$libdir/plugins/x'",
            "DEALLOCATE s3",
            "deallocate prepare s3",
            "DEALLOCATE ALL",
            "/* c */ DEALLOCATE s0",
        ] {
            assert!(closes_session(sql), "{sql:?}");
        }
        for sql in [
            "SELECT 'LOAD'",
            "PREPARE p AS SELECT 1",
            "loading",
            "-- LOAD 'x'\nSELECT 1",
            "DISCARD PLANS",
            "",
        ] {
            assert!(!closes_session(sql), "{sql:?}");
        }
    }
}
