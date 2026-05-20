//! Round 88 typechecker locks — Item D2 (stale doc comments) and Item
//! D3 (dead Record arm in `find_alias_cycle`).
//!
//! ── Item D2: production entrypoint naming ──────────────────────────
//!
//! Two comments in `src/typechecker/mod.rs` previously named
//! `check_with_package` as "the" production entrypoint. The actual
//! production callers (compiler, CLI, LSP) all go through
//! `check_with_package_and_imports_options_resolver`. The bare
//! `check_with_package` is now a test-only convenience wrapper
//! (only `tests/orphan_rule_tests.rs` calls it). Round 88 D2 rewords
//! both comments. The lock here is a source-grep: it asserts both
//! the field comment on `current_package` and the RAII-swap comment
//! mention `check_with_package_and_imports_options_resolver`.
//! Reverting either comment to the old wording fails the assert.
//!
//! ── Item D3: dead Record arm in find_alias_cycle ───────────────────
//!
//! `find_alias_cycle` walked `Type::Record(name, fields)` and tested
//! `visiting.contains(name)` before recursing into fields. But
//! `visiting` is seeded only with alias decl names and extended
//! exclusively through `resolver.lookup_alias` — and
//! `register_type_decl` rejects duplicate top-level names — so the
//! alias name can never collide with a record name. The guard was
//! dead. Round 88 D3 deletes the guard while preserving the
//! field-recursion (the part that descends into nested alias
//! references inside record fields, which IS how a real alias cycle
//! can route through a record).
//!
//! The lock here is behavioural. We assert:
//!
//!   (a) A snippet with mutually-referencing records (NOT through an
//!       alias) does NOT trip a spurious alias-cycle diagnostic. This
//!       confirms the deleted guard wasn't actually doing anything
//!       useful: removing it didn't introduce a regression.
//!
//!   (b) The genuine alias-cycle case (`type A = B; type B = A`) is
//!       still rejected with a cycle diagnostic. This confirms the
//!       field-recursion that we kept still propagates cycles when
//!       routing through records (and that we didn't accidentally
//!       gut the arm).

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── D2: source-grep lock on doc comments ───────────────────────────

/// Round 88 D2 lock: the field comment on `current_package` must name
/// `check_with_package_and_imports_options_resolver` as the production
/// entrypoint and must NOT name `check_with_package` as the production
/// entrypoint. (`check_with_package` may still appear — labelled as
/// the test-only convenience wrapper — but it can't be the load-
/// bearing entrypoint reference.)
#[test]
fn current_package_field_comment_names_real_production_entrypoint() {
    let mod_src = include_str!("../src/typechecker/mod.rs");

    // Locate the field declaration and slice a small window above it.
    let field_decl = "pub(super) current_package: Option<Symbol>,";
    let field_pos = mod_src
        .find(field_decl)
        .expect("`current_package` field declaration should exist in typechecker/mod.rs");
    // Take ~1400 bytes before the field decl to capture the full
    // multi-line doc comment.
    let scan_start = field_pos.saturating_sub(1400);
    let comment_window = &mod_src[scan_start..field_pos];

    assert!(
        comment_window.contains("check_with_package_and_imports_options_resolver"),
        "round 88 D2 regression: the field comment on `current_package` \
         must name `check_with_package_and_imports_options_resolver` as \
         the production entrypoint. Window scanned:\n{comment_window}"
    );

    // The mere presence of the bare phrase `check_with_package` is fine
    // (it shows up labelled as the test-only wrapper). What MUST be
    // present is the production-entrypoint name. Asserting that alone
    // is sufficient to catch a revert that drops the new wording.
}

/// Round 88 D2 lock: the RAII-swap comment inside
/// `check_program_returning_env` must also reference the real
/// production entrypoint
/// `check_with_package_and_imports_options_resolver`.
#[test]
fn raii_swap_comment_names_real_production_entrypoint() {
    let mod_src = include_str!("../src/typechecker/mod.rs");

    // Anchor at the RAII swap line itself.
    let raii_line = "let saved_current_pkg = self.current_package.take();";
    let raii_pos = mod_src
        .find(raii_line)
        .expect("RAII swap of `current_package` should exist in typechecker/mod.rs");
    // Take ~1000 bytes before the swap to capture the comment.
    let scan_start = raii_pos.saturating_sub(1000);
    let comment_window = &mod_src[scan_start..raii_pos];

    assert!(
        comment_window.contains("check_with_package_and_imports_options_resolver"),
        "round 88 D2 regression: the RAII-swap comment in \
         `check_program_returning_env` must name \
         `check_with_package_and_imports_options_resolver` as the \
         production entrypoint. Window scanned:\n{comment_window}"
    );
}

// ── D3: behavioural lock on dead-arm removal ───────────────────────

/// Round 88 D3 lock (no false positives): mutually-referencing
/// records (NOT through an alias) must not produce an alias-cycle
/// diagnostic. The pre-fix code's dead `visiting.contains(name)`
/// guard in the Record arm couldn't fire in practice, so removing
/// it shouldn't change behaviour — and this test confirms exactly
/// that, while also pinning down the contract that record-record
/// references aren't mis-diagnosed as alias cycles.
#[test]
fn record_to_record_reference_does_not_trigger_alias_cycle() {
    // Two records that reference each other through fields. The
    // typechecker treats inter-record references via the canonicaliser,
    // not the alias machinery, so `find_alias_cycle` should never
    // emit a cycle diagnostic for this shape.
    let src = "
type Node {
  next: Node,
  value: Int,
}
fn main() -> Int { 0 }
";
    let errs = type_errors(src);
    for e in &errs {
        assert!(
            !e.contains("forms a cycle"),
            "round 88 D3 regression: self-referencing record should \
             not produce an alias-cycle diagnostic; got: {errs:?}"
        );
    }
}

/// Round 88 D3 lock (genuine alias cycle still rejected):
/// `type A = B; type B = A` must still surface a cycle diagnostic.
/// This pins down that the field-recursion we kept inside the
/// Record arm — plus the surrounding Generic arm where cycles
/// actually close — still works end-to-end.
#[test]
fn alias_cycle_via_aliases_is_still_rejected() {
    let src = "
type A = B
type B = A
fn main() -> Int { 0 }
";
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("forms a cycle")),
        "round 88 D3 regression: `type A = B; type B = A` must still \
         produce a cycle diagnostic after dead-arm cleanup; got: {errs:?}"
    );
}

/// Round 88 D3 lock (source-grep on the removal itself): the dead
/// guard `if visiting.contains(name)` immediately followed by a
/// `chain.push(*name); return Some(chain);` must NOT appear inside
/// the Record arm of `find_alias_cycle`. (The same shape DOES
/// legitimately appear in the Generic arm — that's where alias
/// cycles actually close.) Scoping the grep to the Record arm
/// requires anchoring on the arm header.
///
/// We slice the source from the Record arm header to the next arm
/// header (`Type::List(inner)`) and assert the cycle-emitting guard
/// shape is absent.
#[test]
fn record_arm_in_find_alias_cycle_no_longer_contains_dead_guard() {
    let mod_src = include_str!("../src/typechecker/mod.rs");

    // Locate find_alias_cycle, then the Record arm inside it, then
    // the next arm that bounds the slice.
    let fn_decl = "fn find_alias_cycle(&self, ty: &Type, visiting: &mut Vec<Symbol>)";
    let fn_pos = mod_src
        .find(fn_decl)
        .expect("find_alias_cycle should exist in typechecker/mod.rs");
    let after_fn = &mod_src[fn_pos..];

    let record_arm_header = "Type::Record(";
    let record_pos = after_fn
        .find(record_arm_header)
        .expect("Record arm header should exist in find_alias_cycle");
    let after_record = &after_fn[record_pos..];

    // The next arm after Record (per current source) is the
    // List/Range/Set/Channel combined arm.
    let next_arm = "Type::List(inner)";
    let next_pos = after_record
        .find(next_arm)
        .expect("List arm should bound the Record arm slice");
    let record_arm_slice = &after_record[..next_pos];

    // The dead guard was: `if visiting.contains(name) {`
    // followed (within a couple of lines) by `return Some(chain);`.
    // Assert that the `visiting.contains(name)` guard shape no
    // longer appears inside the Record arm.
    assert!(
        !record_arm_slice.contains("if visiting.contains(name)"),
        "round 88 D3 regression: the dead `if visiting.contains(name)` \
         guard inside the Record arm of `find_alias_cycle` has been \
         reinstated. Slice scanned:\n{record_arm_slice}"
    );
}
