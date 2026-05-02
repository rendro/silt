//! Round-68 DOC agent locks for the associated-types & row-polymorphism
//! finding cluster.
//!
//! Two BROKEN/GAP findings are pinned here:
//!
//! - F1: `docs/language/generics.md` previously claimed "No associated
//!   types" under "What silt deliberately does not have", but associated
//!   types ship in commit `ceb11c9`. The doc has been rewritten with a
//!   proper section covering declarations on traits, bindings on impls,
//!   `Self::Item` vs `<T as Trait>::Item` projection, supertrait
//!   inheritance, and the rules around defaults / duplicates.
//!   - Source-grep lock: the old false claim must never reappear.
//!   - Walker lock: every associated-types snippet from the new
//!     section compiles and runs end-to-end via `silt run`.
//!
//! - F2: `docs/language/types.md` had zero coverage of anonymous
//!   structural records and row polymorphism, even though the lexer
//!   ships `DotDotDot` and the typechecker accepts open rows.
//!   The doc has gained an "Anonymous Records and Row Polymorphism"
//!   section.
//!   - Source-grep lock: the section's anchor headings must be
//!     present.
//!   - Walker lock: every row-polymorphism snippet compiles and runs
//!     end-to-end.

use std::path::{Path, PathBuf};
use std::process::Command;

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round68_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

fn run_ok(label: &str, src: &str) -> (String, String) {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "[{label}] silt run should succeed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

fn check_ok(label: &str, src: &str) -> (String, String) {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "[{label}] silt check should succeed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

fn check_fails_with(label: &str, src: &str, needle: &str) -> String {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "[{label}] silt check should reject the snippet; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(needle),
        "[{label}] expected stderr to contain {needle:?}; got:\n{stderr}"
    );
    stderr
}

// ────────────────────────────────────────────────────────────────────
// F1: generics.md must not claim "No associated types" any more
// ────────────────────────────────────────────────────────────────────

#[test]
fn f1_generics_md_no_longer_claims_no_associated_types() {
    let doc = include_str!("../docs/language/generics.md");
    // The old "No associated types" exclusion (with its
    // "Traits cannot declare ..." prose) must be gone — assoc types
    // ship in commit ceb11c9.
    assert!(
        !doc.contains("No associated types"),
        "generics.md must not contain the exclusion heading \
         `No associated types` — silt has them since commit ceb11c9"
    );
    assert!(
        !doc.contains("Traits cannot declare `type Item`"),
        "generics.md must not contain the false claim \
         `Traits cannot declare type Item alongside their methods`"
    );
    assert!(
        !doc.contains("avoids the complexity of\nprojection types"),
        "generics.md must not retain the prose claiming silt avoids \
         projection-type complexity"
    );
    // Positive shape: the new feature section is present, with anchor
    // text from each subsection.
    assert!(
        doc.contains("### Associated types"),
        "generics.md should have an `### Associated types` section"
    );
    assert!(
        doc.contains("type member"),
        "generics.md associated-types section should describe assoc \
         types as `type member`s on a trait"
    );
    assert!(
        doc.contains("Qualified projection: `<T as Trait>::Item`"),
        "generics.md should document qualified projection syntax"
    );
    assert!(
        doc.contains("Supertrait inheritance"),
        "generics.md should describe supertrait inheritance of assoc \
         types"
    );
}

#[test]
fn f1_assoc_type_basic_snippet_runs() {
    // `trait Stream { type Item; ... }` + impl Stream for Wrap binding
    // `type Item = Int`, with `Self::Item` projection inside the trait
    // body. Mirrors the headline doc snippet plus a `main` to exercise
    // dispatch.
    let src = "\
trait Stream {
  type Item
  fn first(self) -> Self::Item
}

type Wrap { v: Int }

trait Stream for Wrap {
  type Item = Int

  fn first(self) -> Int {
    self.v
  }
}

fn main() {
  let w = Wrap { v: 7 }
  println(w.first())
}
";
    let (stdout, _) = run_ok("f1_basic", src);
    assert!(
        stdout.trim().ends_with("7"),
        "expected the bound int 7 in stdout; got {stdout:?}"
    );
}

#[test]
fn f1_multiple_assoc_types_snippet_runs() {
    let src = "\
trait Pair {
  type First
  type Second
  fn first(self) -> Self::First
  fn second(self) -> Self::Second
}

type IntStringPair { a: Int, b: String }

trait Pair for IntStringPair {
  type First = Int
  type Second = String

  fn first(self) -> Int { self.a }
  fn second(self) -> String { self.b }
}

fn main() {
  let p = IntStringPair { a: 13, b: \"ok\" }
  println(p.first())
  println(p.second())
}
";
    let (stdout, _) = run_ok("f1_multi", src);
    assert!(
        stdout.contains("13") && stdout.contains("ok"),
        "expected `13` and `ok` in stdout; got {stdout:?}"
    );
}

#[test]
fn f1_bound_on_assoc_type_snippet_typechecks() {
    let src = "\
trait Container {
  type Item: Compare
  fn first(self) -> Self::Item
}

type Box { v: Int }

trait Container for Box {
  type Item = Int
  fn first(self) -> Int { self.v }
}

fn main() {
  let b = Box { v: 5 }
  println(b.first())
}
";
    let (stdout, _) = run_ok("f1_bound", src);
    assert!(
        stdout.trim().ends_with("5"),
        "expected `5` in stdout; got {stdout:?}"
    );
}

#[test]
fn f1_qualified_projection_snippet_runs() {
    // `<T as Trait>::Item` outside the trait body, in a free-fn return
    // type.
    let src = "\
trait Producer {
  type Out
  fn produce(self) -> Self::Out
}

type Wrap { v: Int }

trait Producer for Wrap {
  type Out = Int
  fn produce(self) -> Int { self.v }
}

fn first_out(w: Wrap) -> <Wrap as Producer>::Out {
  w.produce()
}

fn main() {
  let w = Wrap { v: 100 }
  println(first_out(w))
}
";
    let (stdout, _) = run_ok("f1_qual", src);
    assert!(
        stdout.trim().ends_with("100"),
        "expected `100` in stdout; got {stdout:?}"
    );
}

#[test]
fn f1_supertrait_assoc_inheritance_snippet_runs() {
    let src = "\
trait Super {
  type Item
  fn one(self) -> Self::Item
}

trait Sub: Super {
  fn first(self) -> Self::Item
}

type Wrap { v: Int }

trait Super for Wrap {
  type Item = Int
  fn one(self) -> Int { self.v }
}

trait Sub for Wrap {
  fn first(self) -> Int { self.v + 1 }
}

fn main() {
  let w = Wrap { v: 41 }
  println(w.first())
}
";
    let (stdout, _) = run_ok("f1_super", src);
    assert!(
        stdout.trim().ends_with("42"),
        "expected `42` in stdout; got {stdout:?}"
    );
}

#[test]
fn f1_default_on_trait_decl_remains_rejected() {
    // The doc states "No defaults in v1." — lock that the parser still
    // rejects `type Item = SomeDefault` in a trait *declaration*.
    let src = "\
trait Stream {
  type Item = Int
  fn first(self) -> Self::Item
}

fn main() { () }
";
    let stderr = check_fails_with("f1_default", src, "default");
    assert!(
        stderr.contains("default"),
        "expected the default-rejection diagnostic; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// F2: types.md must document anonymous records & row polymorphism
// ────────────────────────────────────────────────────────────────────

#[test]
fn f2_types_md_documents_row_polymorphism() {
    let doc = include_str!("../docs/language/types.md");
    // The new section must be anchored.
    assert!(
        doc.contains("## Anonymous Records and Row Polymorphism"),
        "types.md should have a top-level section `Anonymous Records \
         and Row Polymorphism`"
    );
    // Each documented mechanism must have an anchor heading.
    assert!(
        doc.contains("### Open rows: `...r`"),
        "types.md should document open rows under `### Open rows: ...r`"
    );
    assert!(
        doc.contains("### Closed rows reject extra fields"),
        "types.md should call out that closed rows reject extra fields"
    );
    assert!(
        doc.contains("### Record extension: `{...p, ...}`"),
        "types.md should document record extension via the spread form"
    );
    assert!(
        doc.contains("### Pattern destructuring with rest"),
        "types.md should cross-reference pattern destructuring on \
         records with rest"
    );
    // The row-spread token must appear in the documented forms.
    assert!(
        doc.contains("{name: String, ...r}"),
        "types.md should show the open-row form `{{name: String, ...r}}`"
    );
    assert!(
        doc.contains("{...p, age: 30}"),
        "types.md should show the record-extension form `{{...p, age: 30}}`"
    );
}

#[test]
fn f2_anon_record_literal_snippet_runs() {
    let src = "\
fn main() {
  let alice = { name: \"Alice\", age: 30 }
  let bob   = { name: \"Bob\",   age: 25 }
  println(alice.name)
  println(bob.name)
}
";
    let (stdout, _) = run_ok("f2_lit", src);
    assert!(
        stdout.contains("Alice") && stdout.contains("Bob"),
        "expected both names in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_full_name_closed_record_snippet_runs() {
    // The closed record form `{first: String, last: String}` from the
    // section's headline annotation example.
    let src = "\
fn full_name(p: {first: String, last: String}) -> String =
  p.first + \" \" + p.last

fn main() {
  println(full_name({first: \"Alice\", last: \"Smith\"}))
}
";
    let (stdout, _) = run_ok("f2_full_name", src);
    assert!(
        stdout.trim().ends_with("Alice Smith"),
        "expected `Alice Smith` in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_open_row_first_name_snippet_runs() {
    let src = "\
fn first_name(p: {name: String, ...r}) -> String = p.name

fn main() {
  println(first_name({name: \"Alice\", age: 30}))
  println(first_name({name: \"Bob\"}))
}
";
    let (stdout, _) = run_ok("f2_first_name", src);
    assert!(
        stdout.contains("Alice") && stdout.contains("Bob"),
        "expected both names in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_thread_row_var_through_return_snippet_runs() {
    let src = "\
fn id_name(p: {name: String, ...r}) -> {name: String, ...r} = p

fn main() {
  let q = id_name({name: \"Alice\", age: 30})
  println(q.age)
}
";
    let (stdout, _) = run_ok("f2_thread_row", src);
    assert!(
        stdout.trim().ends_with("30"),
        "expected `30` in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_inferred_row_constraint_snippet_runs() {
    // `fn show_name(p) = p.name` infers an open row carrying `name`.
    let src = "\
fn show_name(p) = p.name

fn main() {
  println(show_name({name: \"A\", age: 30}))
}
";
    let (stdout, _) = run_ok("f2_inferred", src);
    assert!(
        stdout.trim().ends_with("A"),
        "expected `A` in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_nominal_widens_into_open_row_snippet_runs() {
    let src = "\
type Person { name: String, age: Int }

fn name(p: {name: String, ...r}) -> String = p.name

fn main() {
  println(name(Person { name: \"Bob\", age: 42 }))
}
";
    let (stdout, _) = run_ok("f2_nominal_widen", src);
    assert!(
        stdout.trim().ends_with("Bob"),
        "expected `Bob` in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_closed_record_rejects_extra_field() {
    // From the "Closed rows reject extra fields" subsection — the
    // doc's negative example must really fail.
    let src = "\
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x

fn main() {
  let _ = ident({a: 1, b: \"hi\", c: 9})
  ()
}
";
    let stderr = check_fails_with("f2_closed_extra", src, "unexpected field");
    assert!(
        stderr.contains("'c'") || stderr.contains(": c"),
        "expected the diagnostic to name the offending field `c`; got:\n{stderr}"
    );
}

#[test]
fn f2_closed_record_rejects_missing_field() {
    let src = "\
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x

fn main() {
  let _ = ident({a: 1})
  ()
}
";
    let stderr = check_fails_with("f2_closed_missing", src, "missing required field");
    assert!(
        stderr.contains("b"),
        "expected the diagnostic to name the missing field `b`; got:\n{stderr}"
    );
}

#[test]
fn f2_closed_record_ident_typechecks() {
    // Positive shape from the Closed-rows subsection: matching the
    // closed type exactly typechecks.
    let src = "\
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x

fn main() {
  let _ = ident({a: 1, b: \"hi\"})
  ()
}
";
    check_ok("f2_closed_ok", src);
}

#[test]
fn f2_extend_op_snippet_runs() {
    let src = "\
fn main() {
  let p = {name: \"Alice\"}
  let q = {...p, age: 30}
  println(q.name)
  println(q.age)
}
";
    let (stdout, _) = run_ok("f2_extend", src);
    assert!(
        stdout.contains("Alice") && stdout.contains("30"),
        "expected `Alice` and `30` in stdout; got {stdout:?}"
    );
}

#[test]
fn f2_extend_op_rejects_overwrite() {
    // The doc says: "Trying to redefine an existing field —
    // `{...p, name: \"Bob\"}` — is a compile-time error."
    let src = "\
fn main() {
  let p = {name: \"Alice\"}
  let q = {...p, name: \"Bob\"}
  let _ = q
  ()
}
";
    check_fails_with("f2_extend_dup", src, "existing field");
}

#[test]
fn f2_match_record_pattern_snippet_runs() {
    // The doc shows a pattern that names a single field; the cross-ref
    // to pattern-matching.md covers `..rest` itself.
    let src = "\
fn main() {
  let p = {name: \"A\", age: 30}
  match p {
    {name: nm} -> println(nm)
  }
}
";
    let (stdout, _) = run_ok("f2_match", src);
    assert!(
        stdout.trim().ends_with("A"),
        "expected `A` in stdout; got {stdout:?}"
    );
}
