//! Lock tests for the compile-session-scoped `Resolver` (round 65
//! follow-up on `docs/proposals/canonical-registry-scoping.md`).
//!
//! Pre-refactor the alias and associated-type-binding registries lived
//! in process-global `RwLock<HashMap>` statics — every `TypeChecker`
//! instance ever constructed in the process shared them. That created a
//! structural cross-pull contamination hazard for the LSP path
//! (multi-file pulls would see one another's aliases) and would
//! eventually bite a multi-tenant embedder.
//!
//! Post-refactor, each `Resolver` instance owns its own maps. Cross-
//! module compile invocations explicitly thread one `Resolver` through
//! every module's typecheck (so `module B importing A` still sees A's
//! aliases) while one-shot typechecks (LSP pulls, REPL inputs) get a
//! fresh `Resolver` per invocation.
//!
//! The three tests below lock the three properties of the new design:
//!
//! 1. `two_resolvers_do_not_share_aliases` — two `Resolver` instances
//!    are independent. (Direct unit test; pre-refactor this would have
//!    failed because the registry was a process-global.)
//! 2. `cross_module_compile_shares_one_resolver` — explicit cross-
//!    module threading via `check_with_package_and_imports_options_resolver`.
//!    Module A registers an alias; module B (which receives A's
//!    `Resolver`) reads it. Locks the cross-module sharing contract.
//! 3. `lsp_pull_does_not_inherit_other_files_aliases` — two independent
//!    `typechecker::check` calls (the legacy LSP entry point allocates
//!    a fresh resolver internally per call). The first registers an
//!    alias; the second references the same name without import and
//!    sees an "unknown type" diagnostic. Locks the LSP isolation goal.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use silt::compiler::Compiler;
use silt::intern::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::canonical::{AliasInfo, Resolver};
use silt::types::{Severity, Type};

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Mirror of `setup_dir` in `tests/cross_module_inference_tests.rs`,
/// kept self-contained here so this test file doesn't reach across
/// integration-test boundaries. See the canonicalize-on-macOS comment
/// in that file for why we always canonicalize the tempdir path.
fn setup_dir(files: &[(&str, &str)], main_source: &str) -> PathBuf {
    let raw_dir = std::env::temp_dir().join(format!(
        "silt_resolver_iso_{}_{}",
        std::process::id(),
        rand_u64()
    ));
    let _ = fs::remove_dir_all(&raw_dir);
    fs::create_dir_all(&raw_dir).expect("mkdir");
    let dir = raw_dir.canonicalize().expect("canonicalize tempdir");
    for (name, content) in files {
        fs::write(dir.join(name), content).expect("write module");
    }
    fs::write(dir.join("main.silt"), main_source).expect("write main");
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    dir
}

// ── 1. Resolver instances are independent ──────────────────────────

/// Two `Resolver` instances do not share alias state. Pre-refactor
/// this test could not have been written: the `register_alias` /
/// `lookup_alias` free functions read and wrote a process-global
/// `RwLock<HashMap>`, so an alias registered through one path was
/// visible through every other path. After the round-65 refactor each
/// `Resolver` owns its own map, so registering on `a` is invisible to
/// `b`.
#[test]
fn two_resolvers_do_not_share_aliases() {
    let mut a = Resolver::new();
    let b = Resolver::new();

    let name = silt::intern::intern("ResolverIso_Mass");
    a.register_alias(
        name,
        AliasInfo {
            params: vec![],
            param_var_ids: vec![],
            target: Type::Float,
        },
    );

    assert!(
        a.lookup_alias(name).is_some(),
        "alias registered on `a` must be visible on `a`"
    );
    assert!(
        b.lookup_alias(name).is_none(),
        "alias registered on `a` must NOT be visible on `b` — \
         pre-refactor this would fail because the registry was global"
    );
}

// ── 2. Cross-module compile shares one Resolver ─────────────────────

fn parse(src: &str) -> silt::ast::Program {
    let tokens = Lexer::new(src).tokenize().expect("lex");
    Parser::new(tokens).parse_program().expect("parse")
}

/// Module A declares `pub type Mass = Float`. Module B does
/// `import a` and uses `Mass` in a binding annotation. Driven through
/// the same compiler entry point the CLI pipeline uses
/// (`Compiler::with_package_roots` + `pre_typecheck_imports` +
/// `check_with_package_and_imports_options_resolver`), this end-to-end
/// path must thread the same `Resolver` through every module's
/// typecheck so module B's annotation resolves the alias via the
/// shared resolver to `Float`. No "unknown type 'Mass'" diagnostic
/// must surface.
///
/// Pre-refactor this contract was satisfied implicitly because the
/// alias registry was a process-global. Post-refactor it must hold
/// because the compiler now owns a session-scoped `Resolver` that it
/// threads through every per-module typecheck call.
#[test]
fn cross_module_compile_shares_one_resolver() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub type ResolverShared_Mass = Float
            "#,
        )],
        r#"
import a

fn use_mass(x: ResolverShared_Mass) -> ResolverShared_Mass = x

fn main() {
  let m: ResolverShared_Mass = 1.5
  let _ = use_mass(m)
}
"#,
    );

    let main_path = dir.join("main.silt");
    let source = fs::read_to_string(&main_path).expect("read main");
    let tokens = Lexer::new(&source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");

    let local_pkg = intern("__test__");
    let mut roots = HashMap::new();
    roots.insert(local_pkg, dir.clone());
    let mut compiler = Compiler::with_package_roots(local_pkg, roots);
    compiler.pre_typecheck_imports(&program);
    let exports = compiler.module_exports_snapshot();

    // Thread the compiler's session-shared resolver through the
    // entrypoint typecheck. Without this threading the alias body
    // registered while pre-typechecking `a.silt` would not be visible
    // here, and the body of `use_mass` could not unify.
    let resolver = compiler.take_resolver();
    let (errors, _exports, _resolver) =
        typechecker::check_with_package_and_imports_options_resolver(
            &mut program,
            Some(local_pkg),
            exports,
            false,
            Some(resolver),
        );

    let hard: Vec<_> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard.iter().all(|e| !e.message.contains("unknown type")
            && !e.message.contains("undefined type")),
        "cross-module compile must resolve `ResolverShared_Mass` via the shared resolver; \
         got hard errors: {hard:?}"
    );
}

// ── 3. LSP-style isolation: fresh resolver per pull ────────────────

/// Two consecutive `typechecker::check(...)` calls — the legacy LSP
/// entry point — must NOT share alias state. The first call registers
/// `type FooLsp_Mass = Float`; the second call uses the bare name
/// `FooLsp_Mass` without any import or declaration and must therefore
/// produce an "undefined type" diagnostic.
///
/// Pre-refactor this test would have FAILED: the second call's
/// `canonicalize` would have found the alias via the process-global
/// registry and silently treated `FooLsp_Mass` as `Float`. Post-
/// refactor each `typechecker::check` call constructs a fresh
/// `TypeChecker` (and therefore a fresh `Resolver`), so cross-pull
/// contamination is structurally impossible.
#[test]
fn lsp_pull_does_not_inherit_other_files_aliases() {
    // First "pull": file with the alias declaration.
    let mut prog_first = parse("type FooLsp_Mass = Float\nfn use_it(x: FooLsp_Mass) = x\n");
    let errs_first = typechecker::check(&mut prog_first);
    let hard_first: Vec<_> = errs_first
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard_first.is_empty(),
        "first pull (file declaring the alias) should typecheck cleanly: {hard_first:?}"
    );

    // Second "pull": different file, references `FooLsp_Mass` without
    // declaring or importing it. Post-refactor the second
    // `typechecker::check` allocates a fresh Resolver internally, so
    // the alias the first pull registered is invisible.
    let mut prog_second = parse("fn use_other(x: FooLsp_Mass) = x\n");
    let errs_second = typechecker::check(&mut prog_second);
    let saw_unknown = errs_second.iter().any(|e| {
        e.severity == Severity::Error
            && (e.message.contains("undefined type")
                || e.message.contains("unknown type")
                || e.message.contains("FooLsp_Mass"))
    });
    assert!(
        saw_unknown,
        "second pull must see 'undefined type FooLsp_Mass' (cross-pull isolation); got: {errs_second:?}"
    );
}
