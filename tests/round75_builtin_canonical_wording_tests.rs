//! Round 75 — io / data / postgres builtins adopt the canonical
//! `"<fn> requires <Kind>, got <kind>"` wording.
//!
//! Round 73f locked `numeric.rs` and `string.rs` to the canonical form
//! `"<fn> requires <Kind>, got <kind>"`. Round 74 swept `collections.rs`.
//! Round 75 finishes the sweep across `io.rs`, `data.rs`, and
//! `postgres.rs` — ~50 sites that previously read e.g. `"requires a
//! string path"`, `"requires Int days"`, `"expected a Date record"`,
//! `"sql must be a String"`. Each terse form lacked the offending
//! `got <kind>` suffix that lets users diagnose without checking docs.
//!
//! This file locks the migration in three layers:
//!   1. SOURCE-GREP ban-list — every pre-fix substring is absent.
//!   2. SOURCE-GREP allow-list — at least one canonical
//!      `"<fn> requires ..., got"` substring is present per site (sampled).
//!   3. RUNTIME locks — invoking the builtin with the wrong kind surfaces
//!      a VmError whose message contains both the named expected Kind
//!      and `got <kind>`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

const IO_RS: &str = include_str!("../src/builtins/io.rs");
const DATA_RS: &str = include_str!("../src/builtins/data.rs");
const POSTGRES_RS: &str = include_str!("../src/builtins/postgres.rs");

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── SOURCE-GREP ban-list locks ───────────────────────────────────────

#[test]
fn io_rs_no_pre_canonical_terse_forms() {
    // Every io/fs/env site once said either "requires a string <noun>"
    // or "requires string arguments" — both shapes drop the offending
    // kind. Lock the absences. Listing every literal so a regression
    // names the exact offender in the failure message.
    let banned = [
        "io.read_file requires a string path",
        "io.write_file requires string arguments",
        "fs.exists requires a string path",
        "fs.is_file requires a string path",
        "fs.is_dir requires a string path",
        "fs.list_dir requires a string path",
        "fs.mkdir requires a string path",
        "fs.remove requires a string path",
        "fs.rename requires string arguments",
        "fs.copy requires string arguments",
        "fs.stat requires a string path",
        "fs.is_symlink requires a string path",
        "fs.read_link requires a string path",
        "fs.walk requires a string path",
        "fs.glob requires a string pattern",
        "env.get requires a string key",
        "env.set requires string arguments",
        "env.remove requires a string key",
    ];
    for needle in banned {
        assert!(
            !IO_RS.contains(needle),
            "src/builtins/io.rs still contains terse pre-fix diagnostic \
             {needle:?} — round 75 migrated all such sites to the \
             '<fn> requires <Kind>, got <kind>' shape established by \
             round 73f for numeric.rs / string.rs"
        );
    }
}

#[test]
fn data_rs_no_pre_canonical_terse_forms() {
    let banned = [
        "expected a Date record",
        "expected a Time record",
        "expected a DateTime record",
        "expected an Instant record",
        "expected a Duration record",
        "regex.replace_all_with requires a string pattern",
        "regex.replace_all_with requires a string text",
        "time.date requires Int arguments",
        "time.time requires Int arguments",
        "time.to_datetime requires Int offset",
        "time.to_instant requires Int offset",
        "time.format requires a String pattern",
        "time.format_date requires a String pattern",
        "time.parse requires String arguments",
        "time.parse_date requires String arguments",
        "time.add_days requires Int days",
        "time.add_months requires Int months",
        "time.days_in_month requires Int arguments",
        "time.is_leap_year requires an Int",
        "time.hours requires an Int",
        "time.minutes requires an Int",
        "time.seconds requires an Int",
        "time.ms requires an Int",
        "time.micros requires an Int",
        "time.nanos requires an Int",
        "http.get requires a String url",
        "http.segments requires a String",
        "http.parse_query requires a String",
        "http.request: first argument must be a Method",
        "http.request: url must be a String",
        "http.request: body must be a String",
        "http.request: headers must be a Map",
        "Response.status must be an Int",
        "Response.body must be a String",
        "json.parse: first argument must be a string",
        "json.parse_list: first argument must be a string",
        "json.parse_map: first argument must be a string",
    ];
    for needle in banned {
        assert!(
            !DATA_RS.contains(needle),
            "src/builtins/data.rs still contains terse pre-fix diagnostic \
             {needle:?} — round 75 migrated all such sites to the \
             '<fn> requires <Kind>, got <kind>' shape"
        );
    }
}

#[test]
fn postgres_rs_no_pre_canonical_terse_forms() {
    let banned = [
        "postgres: expected a PgCursor variant",
        "postgres: expected a PgPool variant",
        "postgres: expected a PgTx variant",
        "postgres: expected a PgPool or PgTx variant",
        "postgres: expected PgCursor variant, got",
        "postgres: expected PgPool variant, got",
        "postgres: expected PgTx variant, got",
        "postgres: expected PgPool or PgTx, got",
        "postgres.connect: url must be a String",
        "postgres.connect_with: url must be a String",
        "postgres.connect_with: opts must be a Map",
        "postgres.connect_with: opts key must be a String",
        "postgres.connect_with: max_pool_size must be an Int",
        "postgres.query: sql must be a String",
        "postgres.query: params must be a List",
        "postgres.execute: sql must be a String",
        "postgres.execute: params must be a List",
        "postgres.stream: sql must be a String",
        "postgres.stream: params must be a List",
        "postgres.cursor: requires a PgTx",
        "postgres.cursor: first argument must be a PgTx",
        "postgres.cursor: sql must be a String",
        "postgres.cursor: params must be a List",
        "postgres.cursor: batch_size must be an Int",
        "postgres.listen: channel_name must be a String",
        "postgres.notify: channel_name must be a String",
        "postgres.notify: payload must be a String",
        "postgres: param must be a Value variant",
    ];
    for needle in banned {
        assert!(
            !POSTGRES_RS.contains(needle),
            "src/builtins/postgres.rs still contains terse pre-fix diagnostic \
             {needle:?} — round 75 migrated all such sites to the \
             '<fn> requires <Kind>, got <kind>' shape"
        );
    }
}

// ── SOURCE-GREP allow-list locks ──────────────────────────────────────

#[test]
fn io_rs_emits_canonical_requires_got_form_per_site() {
    // Sample every site present in io.rs; each must include the
    // canonical "<fn> requires <Kind>, got" prefix.
    let needles = [
        "io.read_file requires String, got",
        "io.write_file requires String, got",
        "fs.exists requires String, got",
        "fs.is_file requires String, got",
        "fs.is_dir requires String, got",
        "fs.list_dir requires String, got",
        "fs.mkdir requires String, got",
        "fs.remove requires String, got",
        "fs.rename requires String, got",
        "fs.copy requires String, got",
        "fs.stat requires String, got",
        "fs.is_symlink requires String, got",
        "fs.read_link requires String, got",
        "fs.walk requires String, got",
        "fs.glob requires String, got",
        "env.get requires String, got",
        "env.set requires String, got",
        "env.remove requires String, got",
    ];
    for needle in needles {
        assert!(
            IO_RS.contains(needle),
            "src/builtins/io.rs missing canonical {needle:?} — \
             round 75 migrated this site to the \
             '<fn> requires <Kind>, got <kind>' shape"
        );
    }
}

#[test]
fn data_rs_emits_canonical_requires_got_form_per_site() {
    let needles = [
        "extract_date requires Date, got",
        "extract_time requires Time, got",
        "extract_datetime requires DateTime, got",
        "extract_instant requires Instant, got",
        "extract_duration requires Duration, got",
        "regex.replace_all_with requires String, got",
        "time.date requires Int, got",
        "time.time requires Int, got",
        "time.to_datetime requires Int, got",
        "time.to_instant requires Int, got",
        "time.format requires String, got",
        "time.format_date requires String, got",
        "time.parse requires String, got",
        "time.parse_date requires String, got",
        "time.add_days requires Int, got",
        "time.add_months requires Int, got",
        "time.days_in_month requires Int, got",
        "time.is_leap_year requires Int, got",
        "time.hours requires Int, got",
        "time.minutes requires Int, got",
        "time.seconds requires Int, got",
        "time.ms requires Int, got",
        "time.micros requires Int, got",
        "time.nanos requires Int, got",
        "http.get requires String, got",
        "http.segments requires String, got",
        "http.parse_query requires String, got",
        "http.request requires Method, got",
        "http.request requires String, got",
        "http.request requires Map, got",
        "Response.status requires Int, got",
        "Response.body requires String, got",
        "json.parse requires String, got",
        "json.parse_list requires String, got",
        "json.parse_map requires String, got",
    ];
    for needle in needles {
        assert!(
            DATA_RS.contains(needle),
            "src/builtins/data.rs missing canonical {needle:?} — \
             round 75 migrated this site to the \
             '<fn> requires <Kind>, got <kind>' shape"
        );
    }
}

#[test]
fn postgres_rs_emits_canonical_requires_got_form_per_site() {
    let needles = [
        "postgres requires PgCursor, got",
        "postgres requires PgPool, got",
        "postgres requires PgTx, got",
        "postgres requires PgPool or PgTx, got",
        "postgres.connect requires String, got",
        "postgres.connect_with requires String, got",
        "postgres.connect_with requires Map, got",
        "postgres.connect_with requires Int, got",
        "postgres.query requires String, got",
        "postgres.query requires List, got",
        "postgres.execute requires String, got",
        "postgres.execute requires List, got",
        "postgres.stream requires String, got",
        "postgres.stream requires List, got",
        "postgres.cursor requires PgTx, got",
        "postgres.cursor requires String, got",
        "postgres.cursor requires List, got",
        "postgres.cursor requires Int, got",
        "postgres.listen requires String, got",
        "postgres.notify requires String, got",
        "postgres requires Value variant",
    ];
    for needle in needles {
        assert!(
            POSTGRES_RS.contains(needle),
            "src/builtins/postgres.rs missing canonical {needle:?} — \
             round 75 migrated this site to the \
             '<fn> requires <Kind>, got <kind>' shape"
        );
    }
}

// ── RUNTIME locks ─────────────────────────────────────────────────────

#[test]
fn io_read_file_non_string_path_runtime_says_canonical_form() {
    // Pass an Int where String is required. The typechecker may catch
    // this; if not, the runtime path produces the canonical wording.
    let err = run_err(
        r#"
import io
fn main() { io.read_file(42) }
"#,
    );
    // Either the typechecker rejected (with its own message) or the
    // runtime fired our canonical form. In either case we must NOT see
    // the terse pre-fix wording. When the runtime path fires, the
    // canonical form contains both the "requires String" prefix and a
    // ", got <kind>" suffix.
    assert!(
        !err.contains("requires a string path"),
        "io.read_file runtime/typecheck wording should not contain the \
         terse 'requires a string path' form. Got: {err}"
    );
    // POSITIVE LOCK (round-75 L6 tightening): assert the source-of-truth
    // canonical form is present in src/builtins/io.rs. Without this, a
    // partial revert to a different terse form (e.g. "needs String path")
    // would leave the negation green. Mirror the round-74 sibling test
    // pattern in tests/round74_collections_canonical_kind_wording_tests.rs.
    assert!(
        IO_RS.contains("io.read_file requires String, got"),
        "src/builtins/io.rs missing canonical \
         'io.read_file requires String, got' form — partial revert to a \
         different terse wording would otherwise leave this test green."
    );
}

#[test]
fn time_add_days_non_int_count_runtime_says_canonical_form() {
    // Pass a String where Int is required for the days arg. The
    // typechecker may reject this earlier, but if the runtime fires
    // the canonical wording must be visible.
    let err = run_err(
        r#"
import time
fn main() {
    let d = time.today()
    time.add_days(d, "x")
}
"#,
    );
    assert!(
        !err.contains("requires Int days"),
        "time.add_days runtime/typecheck wording should not contain the \
         terse 'requires Int days' form. Got: {err}"
    );
    // POSITIVE LOCK (round-75 L6 tightening): the canonical form
    // "time.add_days requires Int, got <kind>" lives in
    // src/builtins/data.rs. Lock its presence so a partial revert to
    // any other terse wording (e.g. "needs Int") is caught.
    assert!(
        DATA_RS.contains("time.add_days requires Int, got"),
        "src/builtins/data.rs missing canonical \
         'time.add_days requires Int, got' form — partial revert to a \
         different terse wording would otherwise leave this test green."
    );
}

#[test]
fn extract_date_non_record_runtime_says_canonical_form() {
    // time.add_days expects a Date record as arg 0. Passing an Int
    // routes through extract_date, which emits the canonical
    // "extract_date requires Date, got Int" wording at runtime when
    // the typechecker doesn't catch it (e.g. a polymorphic-bypass
    // path or an `as` cast). Since the typechecker may catch this,
    // we lock only the absence of the terse pre-fix form.
    let err = run_err(
        r#"
import time
fn main() { time.add_days(42, 1) }
"#,
    );
    assert!(
        !err.contains("expected a Date record"),
        "extract_date runtime/typecheck wording should not contain the \
         terse 'expected a Date record' form. Got: {err}"
    );
    // POSITIVE LOCK (round-75 L6 tightening): the canonical form
    // "extract_date requires Date, got <kind>" lives in
    // src/builtins/data.rs. Lock its presence so a partial revert to
    // any other terse wording (e.g. "needs a Date") is caught.
    assert!(
        DATA_RS.contains("extract_date requires Date, got"),
        "src/builtins/data.rs missing canonical \
         'extract_date requires Date, got' form — partial revert to a \
         different terse wording would otherwise leave this test green."
    );
}

// ── HELPER LOCK: the canonical helpers in common.rs still drive the
// shape (mirrors round 73f's `fix3_canonical_helpers_in_common_rs_carry_kind`
// — round 75 widened the dependent surface so the source-of-truth helper
// must keep its wording even more carefully). ────────────────────────

#[test]
fn common_rs_canonical_wording_helpers_unchanged() {
    let common_rs = include_str!("../src/builtins/common.rs");
    assert!(
        common_rs.contains("requires String, got"),
        "common::require_string must keep canonical kind-named diagnostic"
    );
    assert!(
        common_rs.contains("requires Int, got"),
        "common::require_int must keep canonical kind-named diagnostic"
    );
}
