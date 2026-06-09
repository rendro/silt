//! Round-67 DOC agent locks for the operators / types / generics /
//! strict-effects-migration finding cluster.
//!
//! Six BROKEN/LATENT findings are pinned here:
//!
//! - F4: `docs/language/operators.md` precedence table no longer lists
//!   `xs[i]`, and the section grew an explicit no-bracket-indexing
//!   note. Walker test: the parser still rejects `xs[0]` at parse
//!   time with the documented diagnostic (so the absence-of-`xs[i]`
//!   in the table is honest).
//! - F5: the operators.md "Range" section no longer claims ranges are
//!   "materialized eagerly into a list" or that `Range` is a
//!   "zero-cost alias for `List(Int)`". Runtime walker: a billion-step
//!   range binds in O(1) memory, proving the lazy claim.
//! - F6: the operators.md ascription example no longer uses `42 as
//!   Float`. Walker: the new ascription snippet typechecks; the old
//!   form still errors out as documented.
//! - F7: `int.abs(Int::MIN)` is documented with the actual silt
//!   construction (`-9223372036854775807 - 1`) and the actual runtime
//!   error message (`integer overflow: abs(...)`). Walker runs the
//!   snippet via `silt run` and asserts the matching stderr.
//! - F8: `docs/language/generics.md` pipe example now extracts
//!   `resp.body` between `http.get(url)?` and `json.parse(Todo)`.
//!   Walker typechecks the corrected snippet.
//! - F16: `docs/strict-effects-migration.md` "After" worked example
//!   annotates BOTH `load_settings` and `main`, and the prose around
//!   it admits the iteration loop. Walker runs `silt check
//!   --strict-effects` and asserts no errors.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn read_doc(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round67_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

// ────────────────────────────────────────────────────────────────────
// F4: bracket indexing removed from operators.md table
// ────────────────────────────────────────────────────────────────────

#[test]
fn f4_operators_md_table_does_not_list_bracket_indexing() {
    let doc = read_doc("docs/language/operators.md");
    // Anchor on the literal table-row form: ``f(...)`, `xs[i]``. The
    // doc may still mention `xs[i]` in prose (e.g. the note explaining
    // that silt rejects it), so we look for the table-row pairing.
    assert!(
        !doc.contains("`f(...)`, `xs[i]`"),
        "operators.md precedence table must not pair `f(...)` with \
         `xs[i]` — silt's parser rejects bracket indexing. The row \
         should list function call only, with a separate note pointing \
         to list.get / map.get / string.slice."
    );
    // Positive shape: the doc still has the postfix-call row.
    assert!(
        doc.contains("`f(...)`"),
        "operators.md should still document `f(...)` postfix call"
    );
    // Replacement note must be present so deletion of the row alone
    // doesn't drop the explanation.
    assert!(
        doc.contains("postfix bracket indexing")
            || doc.contains("postfix indexing is not supported"),
        "operators.md should explicitly note that silt has no postfix \
         bracket indexing"
    );
}

#[test]
fn f4_silt_parser_rejects_bracket_indexing() {
    let dir = scratch_dir("f4_index");
    let src = "fn main() {\n  let xs = [1, 2, 3]\n  let r = xs[0]\n  ()\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "silt check should reject `xs[0]`; got success.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("postfix indexing is not supported"),
        "silt should emit the documented `postfix indexing is not supported` error; got:\n{stderr}"
    );
    // Parity with the user-facing replacement note in operators.md.
    assert!(
        stderr.contains("list.get") && stderr.contains("string.slice"),
        "the parser's diagnostic must still suggest `list.get` and `string.slice`; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F5: Range is documented as lazy, not "materialized eagerly"
// ────────────────────────────────────────────────────────────────────

#[test]
fn f5_operators_md_no_longer_claims_range_is_eager_list_alias() {
    let doc = read_doc("docs/language/operators.md");
    assert!(
        !doc.contains("materialized eagerly into a list"),
        "operators.md must not claim ranges are `materialized eagerly into a list`; \
         Value::Range is its own runtime variant"
    );
    assert!(
        !doc.contains("zero-cost alias for `List(Int)`"),
        "operators.md must not claim Range is a `zero-cost alias for List(Int)`; \
         Value::Range is a distinct variant"
    );
    // Positive shape: the lazy story is now stated.
    assert!(
        doc.contains("Ranges are lazy"),
        "operators.md should state `Ranges are lazy` (matching loops-and-pipes.md)"
    );
}

#[test]
fn f5_huge_range_does_not_oom() {
    // O(1)-memory smoke: a billion-step range binds without
    // expanding into a List(Int). If the value were eagerly
    // materialised, this would allocate 8 GB and crash.
    let dir = scratch_dir("f5_range");
    let src = "fn main() {\n  let _r = 1..1000000000\n  println(\"created range\")\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "binding a 1..1_000_000_000 range must succeed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("created range"),
        "lazy range program should print `created range`; got stdout:\n{stdout}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F6: `as` ascription example no longer uses `42 as Float`
// ────────────────────────────────────────────────────────────────────

#[test]
fn f6_operators_md_no_longer_shows_42_as_float() {
    let doc = read_doc("docs/language/operators.md");
    // Anchor on the broken example shape: `let n = 42 as Float`. The
    // doc may still mention `42 as Float` in prose to call it out as
    // rejected by the typechecker, but the worked example must be
    // gone.
    assert!(
        !doc.contains("let n = 42 as Float"),
        "operators.md must not show `let n = 42 as Float` — silt rejects \
         it with `type mismatch: expected Float, got Int`. Use \
         `int.to_float(42)` for the conversion or pick an ascription \
         that actually typechecks."
    );
    // Positive shape: the new ascription example must be present and
    // typecheck.
    assert!(
        doc.contains("[] as List(Int)") && doc.contains("List(List(Int))"),
        "operators.md should still show ascription anchored to a polymorphic \
         list literal (e.g. `[] as List(Int)` and `List(List(Int))`)"
    );
}

#[test]
fn f6_corrected_ascription_examples_typecheck() {
    let dir = scratch_dir("f6_as");
    let src =
        "fn main() {\n  let xs = [] as List(Int)\n  let rows = [[]] as List(List(Int))\n  ()\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ascription examples should typecheck; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn f6_old_form_still_rejected() {
    // Confirm the doc isn't just out-of-date because the language
    // changed: `42 as Float` continues to be rejected, so the
    // replacement is the right call.
    let dir = scratch_dir("f6_oldform");
    let src = "fn main() {\n  let n = 42 as Float\n  ()\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "silt should still reject `42 as Float`"
    );
    assert!(
        stderr.contains("type mismatch") && stderr.contains("Float") && stderr.contains("Int"),
        "old `42 as Float` should still produce the documented type-mismatch diagnostic; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F7: int.abs(Int::MIN) example uses an actually-runnable construction
// ────────────────────────────────────────────────────────────────────

#[test]
fn f7_types_md_uses_runnable_int_min_construction() {
    let doc = read_doc("docs/language/types.md");
    // The doc must use the unary-minus construction since the literal
    // 9223372036854775808 is rejected at lex time.
    assert!(
        doc.contains("-9223372036854775807 - 1"),
        "types.md should construct Int::MIN as `-9223372036854775807 - 1` \
         (the literal 9_223_372_036_854_775_808 is rejected at lex time)"
    );
    // The error message must match what the runtime actually emits.
    assert!(
        doc.contains("integer overflow: abs"),
        "types.md should match the runtime error message \
         `integer overflow: abs(...)`"
    );
}

#[test]
fn f7_design_decisions_md_uses_runnable_int_min_construction() {
    let doc = read_doc("docs/language/design-decisions.md");
    assert!(
        doc.contains("-9223372036854775807 - 1"),
        "design-decisions.md should construct Int::MIN as `-9223372036854775807 - 1`"
    );
    assert!(
        doc.contains("integer overflow: abs"),
        "design-decisions.md should match the runtime error message \
         `integer overflow: abs(...)`"
    );
}

#[test]
fn f7_int_abs_int_min_actually_overflows_at_runtime() {
    let dir = scratch_dir("f7_intabs");
    let src = "import int\n\nfn main() {\n  let min = -9223372036854775807 - 1\n  let _y = int.abs(min)\n  ()\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "int.abs(Int::MIN) must fail at runtime; got success.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("integer overflow: abs(-9223372036854775808)"),
        "int.abs(Int::MIN) should emit the documented \
         `integer overflow: abs(-9223372036854775808)` runtime error; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F8: generics.md pipe example extracts resp.body before json.parse
// ────────────────────────────────────────────────────────────────────

#[test]
fn f8_generics_md_pipe_example_extracts_response_body() {
    let doc = read_doc("docs/language/generics.md");
    // The fixed example must include the resp.body extraction step.
    assert!(
        doc.contains("resp.body"),
        "generics.md pipe example should extract `resp.body` between \
         http.get and json.parse"
    );
    // The fixed example must keep the `let resp = http.get(url)?` form
    // — the broken version piped Response straight into json.parse.
    assert!(
        doc.contains("let resp = http.get(url)?"),
        "generics.md pipe example should bind `resp` from `http.get(url)?` \
         before extracting the body"
    );
}

#[test]
fn f8_corrected_pipe_snippet_typechecks() {
    let dir = scratch_dir("f8_pipe");
    // Wrap the doc snippet in a fn whose error type matches HttpError
    // so resp.body |> json.parse(Todo) |> result.map_ok(process)
    // typechecks as standalone code (the doc snippet itself is a
    // fragment).
    let src = "import http\n\
        import json\n\
        import result\n\
        \n\
        type Todo {\n\
        \x20\x20id: Int,\n\
        \x20\x20title: String,\n\
        }\n\
        \n\
        fn process(t: Todo) -> Todo = t\n\
        \n\
        fn fetch_todo(url: String) -> Result(Todo, HttpError) {\n\
        \x20\x20let resp = http.get(url)?\n\
        \x20\x20let _parsed = resp.body\n\
        \x20\x20\x20\x20|> json.parse(Todo)\n\
        \x20\x20\x20\x20|> result.map_ok(process)\n\
        \x20\x20Ok(Todo { id: 1, title: \"\" })\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20()\n\
        }\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "corrected http→body→json.parse pipeline should typecheck; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn f8_old_pipe_shape_is_genuinely_broken() {
    // Lock that the original (broken) form really does fail typecheck
    // — confirms we changed the doc for the right reason.
    let dir = scratch_dir("f8_old");
    let src = "import http\n\
        import json\n\
        import result\n\
        \n\
        type Todo {\n\
        \x20\x20id: Int,\n\
        \x20\x20title: String,\n\
        }\n\
        \n\
        fn process(t: Todo) -> Todo = t\n\
        \n\
        fn fetch_todo(url: String) -> Result(Todo, HttpError) {\n\
        \x20\x20let _x = http.get(url)?\n\
        \x20\x20\x20\x20|> json.parse(Todo)\n\
        \x20\x20\x20\x20|> result.map_ok(process)\n\
        \x20\x20Ok(Todo { id: 1, title: \"\" })\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20()\n\
        }\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "piping Response straight into json.parse(Todo) should fail \
         typecheck; got success.\nstderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F16: strict-effects-migration "After" example actually goes clean
// ────────────────────────────────────────────────────────────────────

#[test]
fn f16_migration_after_example_annotates_both_load_settings_and_main() {
    let doc = read_doc("docs/strict-effects-migration.md");
    // Both annotations must be present.
    assert!(
        doc.contains("fn load_settings(path: String) -> Result(String, IoError) !{fs, io}"),
        "migration After example should annotate `load_settings` with `!{{fs, io}}`"
    );
    assert!(
        doc.contains("fn main() !{fs, io}"),
        "migration After example should annotate `main()` with `!{{fs, io}}` \
         too — partial annotation still fails strict-effects"
    );
    // The over-promising prose must be gone.
    assert!(
        !doc.contains("(paste the annotation, re-run, clean)"),
        "the misleading `(paste the annotation, re-run, clean)` prose must be \
         replaced with a description of the actual iterative loop"
    );
}

#[test]
fn f16_migration_after_example_passes_strict_effects_in_one_pass() {
    let dir = scratch_dir("f16_strict");
    // Mirror the doc's "After" example verbatim.
    let src = "import io\n\
        \n\
        fn load_settings(path: String) -> Result(String, IoError) !{fs, io} =\n\
        \x20\x20io.read_file(path)\n\
        \n\
        fn main() !{fs, io} {\n\
        \x20\x20let _settings = load_settings(\"config.toml\")\n\
        \x20\x20()\n\
        }\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("silt check --strict-effects");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "the migration `After` example must typecheck under \
         `silt check --strict-effects` in one pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // No error[type]: lines should appear.
    assert!(
        !stderr.contains("error[type]:"),
        "--strict-effects on the After example should produce zero \
         type errors; got stderr:\n{stderr}"
    );
}
