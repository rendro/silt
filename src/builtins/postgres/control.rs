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
}
