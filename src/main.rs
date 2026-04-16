use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use silt::bytecode::Function;
use silt::compiler::Compiler;
use silt::disassemble::disassemble_function;
use silt::errors::SourceError;
use silt::intern::{self, Symbol};
use silt::lexer::Lexer;
use silt::lockfile::{Lockfile, LockfileError};
use silt::manifest::{Manifest, ManifestError};
use silt::parser::Parser;
use silt::typechecker;
use silt::vm::Vm;

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
    Human,
    Json,
}

// ── Shared compilation pipeline ─────────────────────────────────────

/// Result of running the full compilation pipeline (lex → parse → typecheck → compile).
struct CompilePipelineResult {
    /// The original source text.
    source: String,
    /// Parse errors (may be non-empty even when compilation proceeds).
    parse_errors: Vec<SourceError>,
    /// Type errors and warnings.
    type_errors: Vec<SourceError>,
    /// Whether any hard error (parse or type) was encountered. Callers
    /// typically recompute the "real" hard-error flag after filtering
    /// suppressible warnings (see `reportable_type_errors`), so this is
    /// kept for completeness / future callers but not currently read.
    #[allow(dead_code)]
    has_hard_errors: bool,
    /// Compiled functions — `None` if hard errors prevented compilation.
    functions: Option<Vec<Function>>,
    /// Compile errors (if compilation was attempted but failed).
    compile_errors: Vec<SourceError>,
    /// Compiler warnings (empty if compilation was not attempted).
    compile_warnings: Vec<SourceError>,
}

/// Run the full compilation pipeline for `path`: read file → lex → parse (recovering)
/// → typecheck → compile. Returns all diagnostics and compiled output without printing
/// anything or exiting, so callers can decide how to present results.
///
/// - `skip_compile`: skip the compilation step (used by `check_file` which only needs diagnostics).
/// - `typecheck_on_parse_errors`: run the type checker even when there are parse errors
///   (used by `check_file` to report as many diagnostics as possible).
/// - `auto_update_lock`: when true and the file lives inside a silt
///   package, regenerate `silt.lock` if it's missing or stale before
///   compilation. Set to `false` for read-only commands like `silt
///   disasm` and `silt fmt` so they don't mutate user files.
fn run_compile_pipeline(
    path: &str,
    skip_compile: bool,
    typecheck_on_parse_errors: bool,
    auto_update_lock: bool,
) -> CompilePipelineResult {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };

    let tokens = match Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            // Lex errors are fatal for all callers. Return a result with the error
            // so that `check_file` can format it as JSON when needed.
            let source_err = SourceError::from_lex_error(&e, &source, path);
            return CompilePipelineResult {
                source,
                parse_errors: vec![source_err],
                type_errors: Vec::new(),
                has_hard_errors: true,
                functions: None,
                compile_errors: Vec::new(),
                compile_warnings: Vec::new(),
            };
        }
    };

    let (mut program, raw_parse_errors) = Parser::new(tokens).parse_program_recovering();

    let parse_errors: Vec<SourceError> = raw_parse_errors
        .iter()
        .map(|e| SourceError::from_parse_error(e, &source, path))
        .collect();
    let has_parse_errors = !parse_errors.is_empty();

    // Skip the type checker when there are parse errors, unless the caller opted in
    // (e.g. `check_file` reports as many diagnostics as possible on partial programs).
    let (type_errors, has_type_hard_errors) = if !has_parse_errors || typecheck_on_parse_errors {
        let raw_type_errors = typechecker::check(&mut program);
        let hard = raw_type_errors
            .iter()
            .any(|e| e.severity == typechecker::Severity::Error);
        let errs: Vec<SourceError> = raw_type_errors
            .iter()
            .map(|e| SourceError::from_type_error(e, &source, path))
            .collect();
        (errs, hard)
    } else {
        (Vec::new(), false)
    };

    let has_hard_errors = has_parse_errors || has_type_hard_errors;

    // If there are parse errors or compilation is not requested, skip compile.
    // Type errors do NOT block compilation — the compiler resolves modules
    // during compilation, which fixes most "undefined" errors from the type
    // checker.  The test suite already relies on this behavior.
    if has_parse_errors || skip_compile {
        return CompilePipelineResult {
            source,
            parse_errors,
            type_errors,
            has_hard_errors,
            functions: None,
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        };
    }

    // Derive the package_roots map: when `path` is inside a silt
    // package, this loads `silt.toml` and (for dep-resolving commands)
    // auto-regenerates `silt.lock` if stale before resolving the dep
    // tree. For ad-hoc scripts outside any package, falls back to a
    // synthetic local-only setup keyed off the file's parent directory.
    //
    // The `auto_update_lock` flag distinguishes mutation-allowed
    // callers (`silt run`, `silt check`, `silt test`) from read-only
    // callers (`silt disasm`, `silt fmt`). Read-only callers still
    // need a dep map; they just resolve in-memory rather than writing
    // a refreshed lockfile to disk.
    let (local_pkg, package_roots) = package_setup_for_file(path, auto_update_lock);

    // Compile.
    let mut compiler = Compiler::with_package_roots(local_pkg, package_roots);
    match compiler.compile_program(&program) {
        Ok(functions) => {
            let compile_warnings: Vec<SourceError> = compiler
                .warnings()
                .iter()
                .map(|w| SourceError::compile_warning(&w.message, w.span, &source, path))
                .collect();
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                has_hard_errors,
                functions: Some(functions),
                compile_errors: Vec::new(),
                compile_warnings,
            }
        }
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, &source, path);
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                has_hard_errors: true,
                functions: None,
                compile_errors: vec![source_err],
                compile_warnings: Vec::new(),
            }
        }
    }
}

/// Return the type-checker diagnostics that should still be reported for `result`
/// after dropping noise that the compiler will resolve.
///
/// Why: the type checker runs before module resolution (which happens during
/// compilation). When a program imports from an unknown module, the checker
/// emits an "unknown module" *warning* for that import. We want to drop that
/// warning (and ONLY that warning) from the `compile_file` path so the user
/// isn't told about an import they actually wrote correctly. All other
/// diagnostics — real type errors, other warnings — must flow through
/// untouched so they continue to abort the run.
///
/// Previously this helper did substring matching on every entry and
/// suppressed ALL type diagnostics whenever any of them mentioned "unknown
/// module", which silently masked real type errors in any file that also
/// happened to import a user module. Filtering per-entry fixes that while
/// keeping the clean UX for importers.
fn reportable_type_errors(result: &CompilePipelineResult) -> Vec<&SourceError> {
    let has_user_import_warning = result.type_errors.iter().any(is_unknown_module_warning);
    result
        .type_errors
        .iter()
        .filter(|e| !is_unknown_module_warning(e))
        // When the program imports a user module the type checker can't
        // see, every name it exports surfaces as "undefined". The
        // compiler does resolve those at link time, so we demote them
        // here and let the compile-or-runtime stage be the source of
        // truth for name resolution.
        .filter(|e| !(has_user_import_warning && is_user_import_resolvable_error(e)))
        .collect()
}

/// Returns true iff `err` is the "unknown module" warning that the type
/// checker emits for imports the compiler will later resolve. We gate on
/// both the warning severity and the message prefix so a future real type
/// error that happens to mention those words isn't swallowed.
fn is_unknown_module_warning(err: &SourceError) -> bool {
    err.is_warning
        && err.kind == silt::errors::ErrorKind::Type
        && err.message.contains("unknown module")
}

/// Returns true iff `err` is an "undefined variable" or "undefined
/// constructor" diagnostic that the compiler is likely to resolve at
/// link time (because the name comes from a user-module selective
/// import that the type checker can't see into).
///
/// The type checker only registers selective imports for builtin
/// modules; for user modules it emits an "unknown module" warning and
/// every imported name then surfaces as "undefined variable" /
/// "undefined constructor". We demote those to warnings so the run
/// proceeds; if the name truly is undefined the compiler will emit a
/// hard runtime/link error.
fn is_user_import_resolvable_error(err: &SourceError) -> bool {
    err.kind == silt::errors::ErrorKind::Type
        && !err.is_warning
        && (err.message.starts_with("undefined variable")
            || err.message.starts_with("undefined constructor")
            || err.message.starts_with("undefined type")
            || err.message.starts_with("unknown field")
            || err.message.starts_with("type ")
            || err.message.contains("does not implement"))
}

/// Print all diagnostics to stderr and exit(1) if there are hard errors.
/// Returns the compiled functions and source on success.
fn compile_file(path: &str) -> (Vec<Function>, String) {
    compile_file_with_options(path, true)
}

/// Like [`compile_file`] but lets the caller opt out of lockfile
/// auto-regeneration. `silt disasm` is the only read-only caller that
/// uses `false` here — it inspects bytecode without the side effect of
/// writing `silt.lock`.
fn compile_file_with_options(path: &str, auto_update_lock: bool) -> (Vec<Function>, String) {
    let result = run_compile_pipeline(path, false, false, auto_update_lock);

    // Filter per-entry: drop the "unknown module" warnings the compiler will
    // resolve, but keep every other type diagnostic so real errors still
    // surface. See `reportable_type_errors` for the rationale.
    let reportable = reportable_type_errors(&result);
    // A hard error is real only if it's a parse/compile error or a
    // non-suppressed type error with severity Error.
    let has_real_type_error = reportable.iter().any(|e| !e.is_warning);
    let has_parse_errors = !result.parse_errors.is_empty();
    let has_real_hard_errors = has_parse_errors || has_real_type_error;

    // F14 (audit round 17): print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    let all_errs: Vec<&SourceError> = result
        .parse_errors
        .iter()
        .chain(reportable.iter().copied())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect();
    silt::errors::eprintln_errors_with_separator(&all_errs);

    // Exit gate: abort iff a real (non-suppressed) hard error exists.
    if has_real_hard_errors {
        process::exit(1);
    }

    let functions = match result.functions {
        Some(f) => f,
        None => process::exit(1),
    };

    if functions.is_empty() {
        eprintln!("{path}: internal error: no functions compiled");
        process::exit(1);
    }

    (functions, result.source)
}

/// Render the usage text shown by `silt --help` and the no-args screen.
///
/// Subcommands gated by Cargo features are annotated inline with the
/// feature they require, and the bottom line lists which features were
/// compiled in. This lets users discover missing features BEFORE running
/// a subcommand that would otherwise fail with "The 'X' feature is not
/// enabled" only after invocation.
fn usage_text() -> String {
    // Mark feature-gated subcommands with a `[feature: X]` suffix. The
    // marker is present regardless of whether the feature is compiled in —
    // that way `silt --help` is identical across builds and the user can
    // see what a richer build would offer.
    //
    // Alignment is structural: each row is `  <signature (padded to SIG_WIDTH)>  <desc>`.
    // Widen SIG_WIDTH if a new signature exceeds it — the help-row
    // alignment test in tests/cli_test_rendering_tests.rs will fail
    // otherwise.
    const SIG_WIDTH: usize = 46;
    let line = |sig: &str, desc: &str| format!("  {sig:<SIG_WIDTH$}  {desc}\n");
    let run_desc: String = {
        let mut d = String::from("Run a program");
        if !cfg!(feature = "watch") {
            d.push_str("  [--watch requires feature: watch]");
        }
        d
    };
    let mut out = String::new();
    out.push_str("silt — a statically-typed, expression-based language\n");
    out.push('\n');
    out.push_str("Usage:\n");
    out.push_str(&line(
        "silt run [--watch] [--disassemble] <file.silt>",
        &run_desc,
    ));
    out.push_str(&line(
        "silt check [--watch] <file.silt>",
        "Type-check without running",
    ));
    out.push_str(&line("silt test [--watch] [path]", "Run test functions"));
    out.push_str(&line("silt fmt [--check] [files...]", "Format source code"));
    out.push_str(&line("silt repl", "Interactive REPL  [feature: repl]"));
    out.push_str(&line(
        "silt init",
        "Create a new silt package in this directory",
    ));
    out.push_str(&line(
        "silt lsp",
        "Start the language server  [feature: lsp]",
    ));
    out.push_str(&line(
        "silt disasm <file.silt>",
        "Show bytecode disassembly",
    ));
    out.push_str(&line(
        "silt self-update [--dry-run] [--force]",
        "Update the silt binary to the latest release",
    ));
    out.push_str(&line(
        "silt update [<dep-name>]",
        "Regenerate silt.lock for the current package's dependencies",
    ));
    out.push_str(&line(
        "silt add <name> --path <path>",
        "Add a path-based dependency to silt.toml",
    ));
    out.push_str(&line(
        "silt add <name> --git <url> [--rev|--branch|--tag <ref>]",
        "Add a git-based dependency to silt.toml",
    ));
    out.push('\n');
    out.push_str(&format!("Enabled features: {}\n", enabled_features()));
    out
}

/// Single source of truth for the `silt check` usage banner line.
/// Both the `--help` path and the "no arguments given" path render
/// from this so they can't drift apart. A regression test in
/// tests/cli.rs asserts the two banners are byte-identical.
fn check_usage_banner() -> &'static str {
    "silt check [--format json] [--watch] <file.silt>"
}

/// Single source of truth for the `silt run` usage banner line.
///
/// Four code paths print this — `--help`, no-args, the watch
/// dry-validation gate, and the missing-file-after-flags fallback.
/// Keeping them all rendering from this helper is locked by
/// `tests/run_banner_consistency_tests.rs::test_silt_run_banner_consistency_all_paths`.
///
/// We deliberately keep `<file.silt>` (without optional brackets) so the
/// banner stays byte-identical across paths even though `silt run` now
/// also accepts no file argument when invoked inside a package — the
/// optional-no-arg behavior is documented separately in the help text.
fn run_usage_banner() -> &'static str {
    "silt run [--watch] [--disassemble] <file.silt>"
}

/// Comma-separated list of Cargo features compiled into this binary.
/// Shown in `silt --help` so users can tell at a glance whether the
/// optional `repl`, `lsp`, and `watch` subcommands are available.
fn enabled_features() -> String {
    let mut feats: Vec<&'static str> = Vec::new();
    if cfg!(feature = "repl") {
        feats.push("repl");
    }
    if cfg!(feature = "lsp") {
        feats.push("lsp");
    }
    if cfg!(feature = "watch") {
        feats.push("watch");
    }
    if cfg!(feature = "local-clock") {
        feats.push("local-clock");
    }
    if cfg!(feature = "http") {
        feats.push("http");
    }
    if feats.is_empty() {
        "(none)".to_string()
    } else {
        feats.join(", ")
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprint!("{}", usage_text());
        process::exit(1);
    }

    // Handle --watch / -w flag: re-invoke without the flag on file changes
    #[cfg(feature = "watch")]
    if args.iter().any(|a| a == "--watch" || a == "-w") {
        let filtered: Vec<String> = args[1..]
            .iter()
            .filter(|a| *a != "--watch" && *a != "-w")
            .cloned()
            .collect();

        // BEFORE entering the watcher, dry-validate the underlying subcommand
        // so we don't spawn a watcher for a command that's going to fail
        // immediately on every rerun. Two failure modes we catch up front:
        //
        //   1. `--help` / `-h` combined with `--watch` — the user wants
        //      help, not a watcher. Run the subcommand once (which will
        //      print help and exit 0) and return without watching.
        //
        //   2. A subcommand that requires a positional file arg is missing
        //      one — print usage to stderr and exit 1 WITHOUT entering the
        //      watch loop (which would otherwise hang silently forever,
        //      because the initial rerun prints a 1-line usage banner and
        //      the loop just sits there waiting for saves).
        //
        // Subcommands that take no file (repl, init, lsp, fmt with
        // implicit recursion, etc.) are passed through untouched.
        let wants_help = filtered.iter().any(|a| a == "--help" || a == "-h");
        if wants_help {
            // Run the subcommand once so its own help handler fires, then
            // return without entering the watch loop.
            let exe = std::env::current_exe().unwrap_or_else(|e| {
                eprintln!("error: failed to get executable path: {e}");
                process::exit(1);
            });
            let status = std::process::Command::new(&exe).args(&filtered).status();
            match status {
                Ok(s) => process::exit(s.code().unwrap_or(0)),
                Err(e) => {
                    eprintln!("error: failed to invoke subcommand for --help: {e}");
                    process::exit(1);
                }
            }
        }

        // Detect subcommands that require a positional file argument and
        // bail out up front if it's missing. We only gate on the common
        // case (first positional after the subcommand name is missing or
        // is another flag); the subcommand's own validator handles the
        // harder cases after the watcher reruns.
        //
        // Exception: `run`, `check`, and `disasm` no longer require an
        // explicit file when the cwd is inside a silt package (manifest
        // discoverable). In that case the subcommand resolves the entry
        // point to `<root>/src/main.silt`, so we let the watcher start.
        if let Some(sub) = filtered.first().map(|s| s.as_str()) {
            let requires_file = matches!(sub, "run" | "check" | "disasm");
            // `silt test` and `silt fmt` take an optional file / path, so
            // they're NOT in the list above — `silt test --watch` alone is
            // legitimate and means "watch the cwd and rerun auto-discovered
            // tests".
            if requires_file {
                // Find the first positional (non-flag) arg after the
                // subcommand name. Flags like `--format json` consume a
                // value; the simple scan below is good enough because
                // our value-taking flags all start with `--`.
                let mut has_positional = false;
                let mut i = 1;
                while i < filtered.len() {
                    let a = filtered[i].as_str();
                    if a == "--format" {
                        // Skip the flag and its value (if present).
                        i += 2;
                        continue;
                    }
                    if a.starts_with('-') {
                        i += 1;
                        continue;
                    }
                    has_positional = true;
                    break;
                }
                if !has_positional {
                    // No positional path — only allowed if we're inside a
                    // silt package (manifest reachable from cwd).
                    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
                    let in_package = matches!(find_project_root(&cwd), Ok(Some(_)));
                    if !in_package {
                        let banner = match sub {
                            "run" => format!("Usage: {}", run_usage_banner()),
                            // Keep in sync with check_usage_banner().
                            "check" => {
                                format!("Usage: {}", check_usage_banner())
                            }
                            "disasm" => "Usage: silt disasm <file.silt>".to_string(),
                            _ => unreachable!(),
                        };
                        eprintln!("{banner}");
                        process::exit(1);
                    }
                }
            }
        }

        let watch_dir = filtered
            .iter()
            .filter_map(|a| {
                let path = Path::new(a.as_str());
                if a.ends_with(".silt") {
                    let parent = path.parent().unwrap_or(Path::new("."));
                    Some(if parent.as_os_str().is_empty() {
                        Path::new(".").to_path_buf()
                    } else {
                        parent.to_path_buf()
                    })
                } else if path.is_dir() {
                    Some(path.to_path_buf())
                } else {
                    None
                }
            })
            .next()
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| ".".into()));

        silt::watch::watch_and_rerun(&watch_dir, &filtered);
        return;
    }

    #[cfg(not(feature = "watch"))]
    if args.iter().any(|a| a == "--watch" || a == "-w") {
        eprintln!("The 'watch' feature is not enabled. Rebuild with: cargo build --features watch");
        process::exit(1);
    }

    match args[1].as_str() {
        // `-v` is the lowercase long-form convention some UNIX tools accept
        // for `--version`. silt has no verbose flag, so treating `-v` as a
        // synonym for `--version` / `-V` is unambiguous here.
        "--version" | "-V" | "-v" => {
            println!("silt {}", env!("CARGO_PKG_VERSION"));
            process::exit(0);
        }
        "--help" | "-h" | "help" => {
            print!("{}", usage_text());
            process::exit(0);
        }
        "run" => {
            if args[2..].iter().any(|a| a == "--help" || a == "-h") {
                println!("Usage: {}", run_usage_banner());
                println!();
                println!("Options:");
                println!("  --watch, -w     Re-run on file changes");
                println!("  --disassemble   Show bytecode disassembly instead of running");
                println!();
                println!("Examples:");
                println!("  silt run                      (inside a package, runs src/main.silt)");
                println!("  silt run main.silt");
                println!("  silt run --watch main.silt");
                println!("  silt run --disassemble main.silt");
                process::exit(0);
            }
            let mut disasm = false;
            let mut file: Option<String> = None;
            for arg in &args[2..] {
                if arg == "--disassemble" {
                    disasm = true;
                } else if arg.starts_with('-') {
                    let suggestion = match arg.as_str() {
                        "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt run: unknown flag '{arg}'{suggestion}");
                    eprintln!("Run 'silt run --help' for usage.");
                    process::exit(1);
                } else if file.is_none() {
                    file = Some(arg.clone());
                }
            }
            // No explicit file → look for an enclosing silt package and use
            // its `src/main.silt`. If we're not inside a package, preserve
            // the legacy "missing argument" error so non-package users
            // see a familiar message.
            let file = match file {
                Some(f) => f,
                None => match resolve_package_entry_point() {
                    Ok(Some(p)) => p.to_string_lossy().into_owned(),
                    Ok(None) => {
                        eprintln!("Usage: {}", run_usage_banner());
                        process::exit(1);
                    }
                    Err(()) => process::exit(1),
                },
            };
            if disasm {
                disasm_file(&file);
            } else {
                vm_run_file(&file);
            }
        }
        "vm" => {
            // Legacy alias: `silt vm run <file>` -> same as `silt run <file>`
            match args.get(2).map(|s| s.as_str()) {
                Some("run") => {
                    let file = args.get(3).unwrap_or_else(|| {
                        eprintln!("Usage: silt vm run <file.silt>");
                        process::exit(1);
                    });
                    vm_run_file(file);
                }
                _ => {
                    eprintln!("Usage: silt vm run <file.silt>");
                    process::exit(1);
                }
            }
        }
        "disasm" => {
            if args[2..].iter().any(|a| a == "--help" || a == "-h") {
                println!("Usage: silt disasm [<file.silt>]");
                println!();
                println!("Prints the compiled bytecode disassembly for <file.silt>.");
                println!("Inside a package with no file argument, disassembles src/main.silt.");
                println!();
                println!("Example:");
                println!("  silt disasm main.silt");
                process::exit(0);
            }
            // Reject unknown flags before interpreting args as filenames.
            for arg in &args[2..] {
                if arg.starts_with('-') && arg != "--help" && arg != "-h" {
                    eprintln!("silt disasm: unknown flag '{arg}'");
                    eprintln!("Run 'silt disasm --help' for usage.");
                    process::exit(1);
                }
            }
            let path = if args.len() < 3 {
                match resolve_package_entry_point() {
                    Ok(Some(p)) => p.to_string_lossy().into_owned(),
                    Ok(None) => {
                        eprintln!("Usage: silt disasm <file.silt>");
                        process::exit(1);
                    }
                    Err(()) => process::exit(1),
                }
            } else {
                args[2].clone()
            };
            disasm_file(&path);
        }
        "test" => {
            let mut file: Option<String> = None;
            let mut filter: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--filter" {
                    if i + 1 < args.len() {
                        filter = Some(args[i + 1].clone());
                        i += 2;
                    } else {
                        eprintln!("--filter requires a pattern");
                        process::exit(1);
                    }
                } else if args[i] == "--help" || args[i] == "-h" {
                    println!("Usage: silt test [--filter <pattern>] [--watch] [file]");
                    println!();
                    println!("Options:");
                    println!("  --filter <pat>   Only run tests whose name contains <pat>");
                    println!("  --watch, -w      Re-run on file changes");
                    println!();
                    println!("Auto-discovery: when no file is given, recursively runs tests");
                    println!("from files matching *_test.silt or *.test.silt.");
                    process::exit(0);
                } else if args[i].starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match args[i].as_str() {
                        "--filters" | "-filter" | "-f" => " (did you mean --filter?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt test: unknown flag '{}'{}", args[i], suggestion);
                    eprintln!("Run 'silt test --help' for usage.");
                    process::exit(1);
                } else {
                    file = Some(args[i].clone());
                    i += 1;
                }
            }
            run_tests(file.as_deref(), filter);
        }
        "check" => {
            let mut file: Option<String> = None;
            let mut format = OutputFormat::Human;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--format" {
                    if i + 1 < args.len() && args[i + 1] == "json" {
                        format = OutputFormat::Json;
                        i += 2;
                    } else {
                        eprintln!("--format requires 'json'");
                        process::exit(1);
                    }
                } else if args[i] == "--help" || args[i] == "-h" {
                    println!("Usage: {}", check_usage_banner());
                    println!();
                    println!("Options:");
                    println!("  --format json   Emit diagnostics as JSON");
                    println!("  --watch, -w     Re-run on file changes");
                    process::exit(0);
                } else if args[i].starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match args[i].as_str() {
                        "--formats" | "-format" | "-f" => " (did you mean --format?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt check: unknown flag '{}'{}", args[i], suggestion);
                    eprintln!("Run 'silt check --help' for usage.");
                    process::exit(1);
                } else {
                    file = Some(args[i].clone());
                    i += 1;
                }
            }
            let path = match file {
                Some(p) => p,
                None => match resolve_package_entry_point() {
                    Ok(Some(p)) => p.to_string_lossy().into_owned(),
                    Ok(None) => {
                        eprintln!("Usage: {}", check_usage_banner());
                        process::exit(1);
                    }
                    Err(()) => process::exit(1),
                },
            };
            check_file(&path, format);
        }
        #[cfg(feature = "lsp")]
        "lsp" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt lsp");
                    println!();
                    println!("Start the silt language server. Communicates over stdio using the");
                    println!(
                        "Language Server Protocol — invoked automatically by editor extensions"
                    );
                    println!(
                        "(VS Code, Vim/Neovim, etc.). Not typically run directly from a terminal."
                    );
                    process::exit(0);
                }
            }
            // Reject unknown flags before starting the server.
            for arg in &args[2..] {
                if arg.starts_with('-') && arg != "--help" && arg != "-h" {
                    eprintln!("silt lsp: unknown flag '{arg}'");
                    eprintln!("Run 'silt lsp --help' for usage.");
                    process::exit(1);
                }
            }
            silt::lsp::run();
        }
        #[cfg(not(feature = "lsp"))]
        "lsp" => {
            eprintln!("The 'lsp' feature is not enabled. Rebuild with: cargo build --features lsp");
            process::exit(1);
        }
        #[cfg(feature = "repl")]
        "repl" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt repl");
                    println!();
                    println!("Start an interactive REPL session. Type :help inside for commands.");
                    process::exit(0);
                }
            }
            // Reject unknown flags before starting the REPL.
            for arg in &args[2..] {
                if arg.starts_with('-') && arg != "--help" && arg != "-h" {
                    eprintln!("silt repl: unknown flag '{arg}'");
                    eprintln!("Run 'silt repl --help' for usage.");
                    process::exit(1);
                }
            }
            silt::repl::run_repl();
        }
        #[cfg(not(feature = "repl"))]
        "repl" => {
            eprintln!(
                "The 'repl' feature is not enabled. Rebuild with: cargo build --features repl"
            );
            process::exit(1);
        }
        "fmt" => {
            let mut check_mode = false;
            let mut files: Vec<String> = Vec::new();
            for arg in &args[2..] {
                if arg == "--check" {
                    check_mode = true;
                } else if arg == "--help" || arg == "-h" {
                    println!("Usage: silt fmt [--check] [files...]");
                    println!();
                    println!("Options:");
                    println!("  --check    Check formatting without modifying files");
                    process::exit(0);
                } else if arg.starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match arg.as_str() {
                        "--checks" | "--Check" | "-check" | "-c" => " (did you mean --check?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt fmt: unknown flag '{arg}'{suggestion}");
                    eprintln!("Run 'silt fmt --help' for usage.");
                    process::exit(1);
                } else {
                    files.push(arg.clone());
                }
            }
            // If no files given (or just an explicit `.`), find all .silt files
            // in the current directory recursively. This is risky if the user
            // happens to run `silt fmt` outside a project, so we require a
            // project anchor (silt.toml, .git) OR an explicit `.` argument,
            // and always emit a loud warning + file preview when the recursion
            // is triggered implicitly.
            let explicit_dot = files.iter().any(|f| f == "." || f == "./");
            if explicit_dot {
                // Strip the `.` marker; we'll treat it as the recursive sentinel.
                files.retain(|f| f != "." && f != "./");
            }
            let implicit_recursive = files.is_empty();
            if implicit_recursive {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                // Project boundary is now defined exclusively by `silt.toml`.
                // The previous heuristic accepted `.git` as well; that is gone
                // because v0.7 makes manifest discovery the canonical answer
                // to "am I inside a silt package?".
                let has_anchor = match find_project_root(&cwd) {
                    Ok(Some(_)) => true,
                    Ok(None) => false,
                    Err(e) => die_on_manifest_error(e),
                };
                files = find_silt_files(Path::new("."));
                if files.is_empty() {
                    eprintln!("no .silt files found in current directory");
                    process::exit(1);
                }
                if !has_anchor && !explicit_dot {
                    eprintln!(
                        "silt fmt: refusing to recursively format {} — no silt.toml found in this directory or any parent",
                        cwd.display()
                    );
                    eprintln!("         pass an explicit `.` or file paths to format anyway.");
                    process::exit(1);
                }
                eprintln!(
                    "silt fmt: no files specified; recursively formatting all .silt files under {}",
                    cwd.display()
                );
                let preview = files.iter().take(5).collect::<Vec<_>>();
                for f in &preview {
                    eprintln!("  {f}");
                }
                if files.len() > preview.len() {
                    eprintln!("  ... ({} more)", files.len() - preview.len());
                }
            }
            if check_mode {
                let mut any_unformatted = false;
                for file in &files {
                    if !check_format(file) {
                        any_unformatted = true;
                    }
                }
                if any_unformatted {
                    process::exit(1);
                }
            } else {
                let mut any_failed = false;
                for file in &files {
                    if let Err(e) = format_file(file) {
                        eprintln!("{e}");
                        any_failed = true;
                    }
                }
                if any_failed {
                    process::exit(1);
                }
            }
        }
        "init" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt init");
                    println!();
                    println!("Create a new silt package in the current directory.");
                    println!("Writes silt.toml and src/main.silt; the package name is");
                    println!("derived from the directory name.");
                    process::exit(0);
                }
            }
            // Reject unknown flags before proceeding.
            for arg in &args[2..] {
                if arg.starts_with('-') && arg != "--help" && arg != "-h" {
                    eprintln!("silt init: unknown flag '{arg}'");
                    eprintln!("Run 'silt init --help' for usage.");
                    process::exit(1);
                }
            }
            init_project();
        }
        "self-update" => {
            let mut dry_run = false;
            let mut force = false;
            for arg in &args[2..] {
                match arg.as_str() {
                    "--help" | "-h" => {
                        println!("Usage: silt self-update [--dry-run] [--force]");
                        println!();
                        println!("Download the latest release binary and replace this one.");
                        println!();
                        println!("Options:");
                        println!("  --dry-run    Show the latest version without downloading");
                        println!("  --force      Reinstall even when already up to date");
                        process::exit(0);
                    }
                    "--dry-run" => dry_run = true,
                    "--force" => force = true,
                    other => {
                        let suggestion = match other {
                            "--dryrun" | "-n" => " (did you mean --dry-run?)",
                            "-f" | "--reinstall" => " (did you mean --force?)",
                            "--h" | "-help" => " (did you mean --help?)",
                            _ => "",
                        };
                        eprintln!("silt self-update: unknown flag '{other}'{suggestion}");
                        eprintln!("Run 'silt self-update --help' for usage.");
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = silt::update::run_update(silt::update::UpdateOptions { dry_run, force })
            {
                eprintln!("  error: {e}");
                process::exit(1);
            }
        }
        // `silt update` manages package dependencies in v0.7+. It also
        // keeps a back-compat redirect for legacy self-update flags so
        // scripts that ran `silt update --dry-run` against old binaries
        // get a clear pointer to `silt self-update` rather than a
        // confusing "must be run inside a package" error.
        //
        // Argument shapes:
        //  - `silt update` (no args): regenerate the lock for the
        //    current package's full dep tree.
        //  - `silt update <name>`: regenerate the lock, optionally
        //    targeting just one dep. For Phase-1 path-only deps this
        //    behaves like the no-arg form (path deps don't have
        //    versions to bump), but the API is wired up so PR-future-2
        //    can implement targeted updates without touching the
        //    dispatch.
        //  - Legacy self-update flags (`--dry-run`, `--force`,
        //    `--version=...`): print the redirect to `self-update` and
        //    exit 2. Never silently invoke either path — that would
        //    bite anyone scripting the old API.
        "update" => {
            let mut saw_self_update_flag = false;
            let mut wants_help = false;
            let mut positional: Option<String> = None;
            for arg in &args[2..] {
                match arg.as_str() {
                    "--help" | "-h" => wants_help = true,
                    "--dry-run" | "--force" => saw_self_update_flag = true,
                    other if other.starts_with("--version=") => saw_self_update_flag = true,
                    other if other.starts_with('-') => {
                        eprintln!("silt update: unknown flag '{other}'");
                        eprintln!("Run 'silt update --help' for usage.");
                        process::exit(1);
                    }
                    other if positional.is_none() => positional = Some(other.to_string()),
                    other => {
                        eprintln!("silt update: unexpected extra argument '{other}'");
                        process::exit(1);
                    }
                }
            }
            if wants_help {
                println!("Usage: silt update [<dep-name>]");
                println!();
                println!("Regenerate `silt.lock` from the current package's `silt.toml`.");
                println!("Resolves the full dependency tree, computes content checksums,");
                println!("and writes the result next to `silt.toml`.");
                println!();
                println!("Arguments:");
                println!("  <dep-name>     Update only the named dep (Phase-1 path deps");
                println!("                 are re-resolved the same way as the no-arg form;");
                println!("                 the argument exists for forward compat).");
                println!();
                println!("To update the silt binary itself, use `silt self-update` instead.");
                process::exit(0);
            }
            // Legacy redirect: keep firing on `--dry-run` / `--force` /
            // `--version=...` no matter where we are, because anyone
            // passing those flags is clearly trying to drive the old
            // self-updater.
            if saw_self_update_flag {
                eprintln!(
                    "silt update has been renamed to silt self-update; the new silt update manages package dependencies. To update the silt binary itself, use 'silt self-update'."
                );
                process::exit(2);
            }
            run_dependency_update(positional.as_deref());
        }
        // `silt add <name> --path <path>` or
        // `silt add <name> --git <url> [--rev|--branch|--tag <ref>]` —
        // append a dep to `silt.toml` and regenerate `silt.lock`.
        // Implemented in `run_add_command`; dispatch keeps the parsing
        // local to that function so the main switch stays a one-liner.
        "add" => {
            if args[2..].iter().any(|a| a == "--help" || a == "-h") {
                println!("Usage: silt add <name> --path <path>");
                println!("       silt add <name> --git <url> --rev <sha>");
                println!("       silt add <name> --git <url> --branch <name>");
                println!("       silt add <name> --git <url> --tag <name>");
                println!();
                println!("Add a dependency to the current package's silt.toml,");
                println!("then regenerate silt.lock to include the new dep.");
                println!();
                println!("Arguments:");
                println!("  <name>             The local name to import the dep as.");
                println!("                     Must be a valid silt identifier and must not");
                println!("                     collide with a builtin module.");
                println!("  --path <path>      Path to the dep's package root (the directory");
                println!("                     containing its silt.toml).");
                println!("  --git <url>        URL of a git repository hosting a silt package.");
                println!("                     Must be paired with exactly one of");
                println!("                     --rev, --branch, or --tag.");
                println!("  --rev <sha>        Pin to a specific commit SHA (7-40 hex chars).");
                println!("  --branch <name>    Track a branch; resolved to the current HEAD SHA");
                println!("                     and re-resolved on each `silt update`.");
                println!("  --tag <name>       Track a tag; resolved at lock time and");
                println!("                     re-resolved on `silt update` if the tag moves.");
                println!();
                println!("Examples:");
                println!("  silt add calc --path ../calc");
                println!("  silt add calc --git https://github.com/foo/calc --branch main");
                println!("  silt add calc --git https://github.com/foo/calc --tag v1.0.0");
                println!("  silt add calc --git https://github.com/foo/calc --rev abc1234");
                process::exit(0);
            }
            if let Err(e) = run_add_command(&args[2..]) {
                eprintln!("error: {e}");
                process::exit(1);
            }
        }
        // If the argument looks like a file, treat as `silt run <file> [flags...]`
        arg if arg.ends_with(".silt") => {
            let file = arg;
            let mut disasm = false;
            for extra in &args[2..] {
                if extra == "--help" || extra == "-h" {
                    println!("Usage: {}", run_usage_banner());
                    println!();
                    println!("Options:");
                    println!("  --watch, -w     Re-run on file changes");
                    println!("  --disassemble   Show bytecode disassembly instead of running");
                    process::exit(0);
                } else if extra == "--disassemble" {
                    disasm = true;
                } else if extra.starts_with('-') {
                    let suggestion = match extra.as_str() {
                        "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt run: unknown flag '{extra}'{suggestion}");
                    eprintln!("Run 'silt run --help' for usage.");
                    process::exit(1);
                }
            }
            if disasm {
                disasm_file(file);
            } else {
                vm_run_file(file);
            }
        }
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Run 'silt' with no arguments to see available commands.");
            process::exit(1);
        }
    }
}

/// Sanitize a directory name into a valid silt identifier:
/// - lowercase
/// - replace any non-`[a-z0-9_]` character with `_`
/// - prefix with `_` if the first character is a digit
///
/// Returns `None` if the result is empty (e.g. dirname was just punctuation
/// that all collapsed to underscores trimmed away — empty/blank input).
fn sanitize_package_name(dirname: &str) -> Option<String> {
    let lowered = dirname.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // Reject if empty (dirname was empty to begin with).
    if out.is_empty() {
        return None;
    }
    // If the first character is a digit, prefix with underscore so the
    // result still satisfies `[a-z_][a-z0-9_]*`.
    if out
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    Some(out)
}

fn init_project() {
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: failed to determine current directory: {e}");
        process::exit(1);
    });

    let manifest_path = cwd.join("silt.toml");
    if manifest_path.exists() {
        eprintln!("silt.toml already exists at {}", manifest_path.display());
        process::exit(1);
    }
    let main_path = cwd.join("src").join("main.silt");
    if main_path.exists() {
        eprintln!("src/main.silt already exists at {}", main_path.display());
        process::exit(1);
    }

    let dirname = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let package_name = match sanitize_package_name(dirname) {
        Some(name) => name,
        None => {
            eprintln!(
                "error: cannot derive a package name from the current directory `{}`",
                cwd.display()
            );
            eprintln!(
                "       the directory needs a name that contains at least one lowercase letter,"
            );
            eprintln!("       digit, or underscore (or rename it to a valid silt identifier).");
            process::exit(1);
        }
    };

    let manifest_contents = format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n");
    let main_contents = "fn main() {\n  println(\"hello, silt!\")\n}\n";

    // Create src/ first so a failure there doesn't leave a stray manifest.
    if let Err(e) = fs::create_dir_all(main_path.parent().unwrap()) {
        eprintln!(
            "error: failed to create directory {}: {e}",
            main_path.parent().unwrap().display()
        );
        process::exit(1);
    }
    if let Err(e) = fs::write(&main_path, main_contents) {
        eprintln!("error writing {}: {e}", main_path.display());
        process::exit(1);
    }
    if let Err(e) = fs::write(&manifest_path, manifest_contents) {
        eprintln!("error writing {}: {e}", manifest_path.display());
        // Best-effort cleanup so a partial init doesn't leave a stray
        // src/main.silt without a manifest pinning it to a package.
        let _ = fs::remove_file(&main_path);
        process::exit(1);
    }

    println!("created silt package `{package_name}`:");
    println!("  {}", manifest_path.display());
    println!("  {}", main_path.display());
    println!();
    println!("  run:   silt run");
    println!("  test:  silt test");
}

fn format_file(path: &str) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|e| format!("error reading {path}: {e}"))?;
    let formatted =
        silt::formatter::format(&source).map_err(|e| render_fmt_error(&e, &source, path))?;
    fs::write(path, formatted).map_err(|e| format!("error writing {path}: {e}"))?;
    Ok(())
}

/// Render a formatter lex/parse failure as a structured `SourceError` with
/// the source-line snippet and caret. Without this, `silt fmt` would
/// surface the bare `ParseError::Display` string (just `[line:col] msg`)
/// and users would lose the context they get from `silt run` /
/// `silt check` on the same file.
fn render_fmt_error(err: &silt::formatter::FmtError, source: &str, path: &str) -> String {
    match err {
        silt::formatter::FmtError::Lex(e) => {
            format!("{}", SourceError::from_lex_error(e, source, path))
        }
        silt::formatter::FmtError::Parse(e) => {
            format!("{}", SourceError::from_parse_error(e, source, path))
        }
    }
}

/// Check if a file is already formatted. Returns true if it is, false otherwise.
/// Prints a message for files that would be changed.
fn check_format(path: &str) -> bool {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return false;
        }
    };
    match silt::formatter::format(&source) {
        Ok(formatted) => {
            if source == formatted {
                true
            } else {
                eprintln!("{path}: not formatted");
                false
            }
        }
        Err(e) => {
            eprintln!("{}", render_fmt_error(&e, &source, path));
            false
        }
    }
}

/// Return true if `e` is the "program has no main function" runtime error.
///
/// AUDIT-NOTE: this hint is keyed on a stringly-typed error; a proper fix
/// would introduce a typed error variant. Tests pinning this live in
/// tests/cli.rs. The matcher is intentionally more permissive than a single
/// exact-string compare so a future cosmetic tweak to the producing
/// `format!` in src/vm/execute.rs doesn't silently break the "silt test"
/// nudge.
fn is_missing_main_error(e: &silt::vm::VmError) -> bool {
    let msg = &e.message;
    msg.starts_with("undefined global: ") && msg.contains("main")
}

/// Heuristic: does this source look like a test-only file?
///
/// Returns true if the source defines any `fn test_...` function OR contains
/// a top-level `test.` call (e.g. `test.assert_eq(...)`). Used by `silt run`
/// to suggest `silt test` when there's no `main()`.
///
/// Conservative: we scan whole lines that start (after trimming whitespace)
/// with `fn test_`, `fn skip_test_`, or `test.` so commented-out code and
/// string literals containing those substrings don't trigger a false positive.
fn looks_like_test_file(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("fn test_")
            || t.starts_with("fn skip_test_")
            || t.starts_with("pub fn test_")
            || t.starts_with("pub fn skip_test_")
            || t.starts_with("test.")
        {
            return true;
        }
    }
    false
}

/// Walk up from `start` looking for the nearest `silt.toml`. Returns the
/// project root directory and the loaded `Manifest` if found, or `None`
/// if no manifest is reachable before the filesystem root.
///
/// Replaces the heuristic `project_anchor()` which looked for `silt.toml`
/// OR `.git`. With first-class manifest support, only `silt.toml` matters
/// for project boundaries.
pub fn find_project_root(start: &Path) -> Result<Option<(PathBuf, Manifest)>, ManifestError> {
    match Manifest::find(start) {
        Some(dir) => {
            let manifest = Manifest::load(&dir.join("silt.toml"))?;
            Ok(Some((dir, manifest)))
        }
        None => Ok(None),
    }
}

/// Print a manifest error to stderr and exit. Used by callers that need
/// the manifest to proceed (e.g. `silt run` resolving the entry point).
fn die_on_manifest_error(err: ManifestError) -> ! {
    eprintln!("error: {err}");
    process::exit(1);
}

/// Synthetic package name used when compiling a `.silt` file outside any
/// silt package (REPL-style invocations, ad-hoc scripts, the
/// `silt run script.silt` legacy path). Matches what
/// `Compiler::with_project_root` used internally pre-PR-4 so any
/// downstream code keying on the local package name keeps working.
const ANONYMOUS_LOCAL_PACKAGE: &str = "__local__";

/// Derive the package_roots map and local-package symbol the compiler
/// needs to resolve `import` statements from `path`.
///
/// Two modes:
///   - `path` lives inside a silt package (manifest reachable above its
///     parent): we resolve the dep tree from `silt.lock`, optionally
///     auto-regenerating the lock if it's missing or stale (controlled
///     by `auto_update_lock`). The local package is registered under
///     its real name from `silt.toml`; deps are registered under the
///     names from their respective manifests.
///   - No manifest reachable: we synthesise a single-root setup under
///     [`ANONYMOUS_LOCAL_PACKAGE`] mapped to the file's parent
///     directory. This preserves the legacy "ad-hoc script" behavior
///     where `import foo` resolves to a sibling `foo.silt`.
///
/// `auto_update_lock = false` is what `silt fmt` and `silt disasm` use:
/// they should never mutate the lockfile (read-only operations); if
/// the lock is missing or stale they just resolve from the existing
/// (possibly empty) lockfile, which is fine because the local package
/// always loads regardless and missing deps surface naturally as
/// import errors.
///
/// Manifest or lockfile errors are fatal — they're rendered to stderr
/// and the process exits with code 1. Run/check/test paths can't
/// proceed without a coherent dep graph.
fn package_setup_for_file(
    path: &str,
    auto_update_lock: bool,
) -> (Symbol, HashMap<Symbol, PathBuf>) {
    let file_parent = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    match find_project_root(&file_parent) {
        Ok(Some((root, manifest))) => {
            let lockfile_path = root.join("silt.lock");
            let lockfile = if auto_update_lock {
                ensure_fresh_lockfile(&manifest, &lockfile_path)
            } else {
                load_or_resolve_lockfile(&manifest, &lockfile_path)
            };
            let package_roots = lockfile.package_roots(&manifest);
            (manifest.package.name, package_roots)
        }
        Ok(None) => fallback_package_setup(&file_parent),
        Err(e) => die_on_manifest_error(e),
    }
}

/// Construct the no-package fallback: synthetic local package name
/// mapped to `dir` so legacy ad-hoc scripts continue to resolve
/// `import foo` against sibling files.
fn fallback_package_setup(dir: &Path) -> (Symbol, HashMap<Symbol, PathBuf>) {
    let local = intern::intern(ANONYMOUS_LOCAL_PACKAGE);
    let mut roots = HashMap::new();
    roots.insert(local, dir.to_path_buf());
    (local, roots)
}

/// Auto-update path: regenerate `silt.lock` if it's missing or stale
/// relative to `manifest`. Prints a single notice line to stderr when
/// a regeneration happens so the user knows the file changed.
///
/// Any lockfile error (resolve, parse, write) is fatal. We deliberately
/// don't fall back silently — a half-resolved lockfile is worse than
/// no lockfile because it would let imports succeed against stale
/// content checksums.
fn ensure_fresh_lockfile(manifest: &Manifest, lockfile_path: &Path) -> Lockfile {
    let existing = match Lockfile::load(lockfile_path) {
        Ok(lock) => Some(lock),
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => die_on_lockfile_error(e),
    };
    let needs_refresh = match &existing {
        None => true,
        Some(lock) => !lock.matches_manifest(manifest),
    };
    if !needs_refresh {
        return existing.expect("checked above");
    }
    if existing.is_some() {
        eprintln!("Updating silt.lock for new dependencies in silt.toml");
    }
    let fresh = match Lockfile::resolve(manifest) {
        Ok(l) => l,
        Err(e) => die_on_lockfile_error(e),
    };
    if let Err(e) = fresh.write(lockfile_path) {
        die_on_lockfile_error(e);
    }
    fresh
}

/// Read-only path: load `silt.lock` if it exists, otherwise resolve
/// from the manifest in-memory without writing. Used by `silt fmt`
/// and `silt disasm`, which shouldn't touch the lockfile.
fn load_or_resolve_lockfile(manifest: &Manifest, lockfile_path: &Path) -> Lockfile {
    match Lockfile::load(lockfile_path) {
        Ok(lock) => lock,
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => {
            // No lockfile on disk, but we still need *some* dep map for
            // the compiler. Resolve in-memory; if that fails the user
            // gets a clear error (and can run `silt update` to write a
            // real lock and see the same diagnostic).
            match Lockfile::resolve(manifest) {
                Ok(l) => l,
                Err(e) => die_on_lockfile_error(e),
            }
        }
        Err(e) => die_on_lockfile_error(e),
    }
}

fn die_on_lockfile_error(err: LockfileError) -> ! {
    eprintln!("error: {err}");
    process::exit(1);
}

/// Implementation of `silt update [<dep-name>]`.
///
/// Walks up from cwd to find a `silt.toml`, resolves the full dep tree
/// fresh from disk, computes checksums, and writes `silt.lock` next to
/// the manifest. Always rewrites the entire lock — even when a single
/// dep was named — so other entries refresh in tandem. (For Phase-1
/// path deps this is fine; PR-future-2 will need a richer policy when
/// version arithmetic enters the picture.)
///
/// Outside any package (no `silt.toml` reachable) we print a fixed
/// error message and exit 1 — this is the canonical "must be run
/// inside a silt package" diagnostic that tests pin to.
fn run_dependency_update(target: Option<&str>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (root, manifest) = match find_project_root(&cwd) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!(
                "silt update must be run inside a silt package (no silt.toml found in this directory or any parent)"
            );
            process::exit(1);
        }
        Err(e) => die_on_manifest_error(e),
    };

    if let Some(name) = target {
        // Validate that the named dep actually exists in the manifest
        // before doing the work. Otherwise a typo silently rewrites the
        // lock with the existing dep set and the user is left wondering
        // why their requested update didn't happen.
        let known = manifest
            .dependencies
            .keys()
            .any(|sym| intern::resolve(*sym) == name);
        if !known {
            eprintln!(
                "silt update: dependency `{name}` is not declared in {}",
                manifest.manifest_path.display()
            );
            process::exit(1);
        }
    }

    let lockfile = match Lockfile::resolve(&manifest) {
        Ok(l) => l,
        Err(e) => die_on_lockfile_error(e),
    };
    let lockfile_path = root.join("silt.lock");
    if let Err(e) = lockfile.write(&lockfile_path) {
        die_on_lockfile_error(e);
    }

    // Count of pinned (non-root) packages. Quiet single-line summary
    // matches the tone of `cargo update`'s default output.
    let dep_count = lockfile
        .packages
        .iter()
        .filter(|p| !matches!(p.source, silt::lockfile::LockedSource::Local))
        .count();
    if dep_count == 1 {
        eprintln!("Locked 1 dependency.");
    } else {
        eprintln!("Locked {dep_count} dependencies.");
    }
}

/// Source kind selected on the `silt add` command line. Mirrors the two
/// arms of `Dependency` in `src/manifest.rs`; we keep this local enum
/// so the parser's "exactly one source flag" invariant lives close to
/// the parser itself.
enum AddSource {
    Path(String),
    Git {
        url: String,
        ref_spec: silt::git::GitRef,
    },
}

/// Implementation of `silt add <name> --path <path>` and
/// `silt add <name> --git <url> [--rev|--branch|--tag <ref>]`.
///
/// Edits the current package's `silt.toml` in place to add a new
/// dependency entry, then regenerates `silt.lock` so the next compile
/// picks up the new dep. Uses `toml_edit` so user formatting
/// (comments, blank lines, key ordering) is preserved.
///
/// Validation order: argument shape → name → URL/path well-formedness
/// → (git only) `verify_reachable` → (git only) `resolve_ref` → manifest
/// write → lockfile regen. Both path and git deps now flow through the
/// same lockfile-regen step (git deps fetch into `<silt-cache>/git/...`
/// and pin the resolved SHA in `silt.lock`).
///
/// Errors are returned rather than printed so the caller can wrap them
/// in the dispatch's standard "error: ..." prefix and exit code.
fn run_add_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // ── Argument parsing ──────────────────────────────────────────────
    //
    // Positional name + one source flag (`--path <p>` OR `--git <url>`
    // with exactly one of `--rev` / `--branch` / `--tag`); reject
    // anything else so typos like `--paths` surface immediately rather
    // than being silently swallowed.
    let mut name: Option<String> = None;
    let mut path_arg: Option<String> = None;
    let mut git_arg: Option<String> = None;
    let mut rev_arg: Option<String> = None;
    let mut branch_arg: Option<String> = None;
    let mut tag_arg: Option<String> = None;
    let mut i = 0;
    // Helper: capture a flag's value, supporting both `--flag VALUE`
    // and `--flag=VALUE` forms, and complaining on duplicates.
    fn take_flag_value(
        args: &[String],
        i: &mut usize,
        slot: &mut Option<String>,
        flag: &str,
    ) -> Result<(), String> {
        if slot.is_some() {
            return Err(format!("{flag} was specified more than once"));
        }
        if *i + 1 >= args.len() {
            return Err(format!("{flag} requires a value"));
        }
        *slot = Some(args[*i + 1].clone());
        *i += 2;
        Ok(())
    }
    while i < args.len() {
        let arg = &args[i];
        if arg == "--path" {
            take_flag_value(args, &mut i, &mut path_arg, "--path")?;
        } else if let Some(rest) = arg.strip_prefix("--path=") {
            if path_arg.is_some() {
                return Err("--path was specified more than once".into());
            }
            path_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--git" {
            take_flag_value(args, &mut i, &mut git_arg, "--git")?;
        } else if let Some(rest) = arg.strip_prefix("--git=") {
            if git_arg.is_some() {
                return Err("--git was specified more than once".into());
            }
            git_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--rev" {
            take_flag_value(args, &mut i, &mut rev_arg, "--rev")?;
        } else if let Some(rest) = arg.strip_prefix("--rev=") {
            if rev_arg.is_some() {
                return Err("--rev was specified more than once".into());
            }
            rev_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--branch" {
            take_flag_value(args, &mut i, &mut branch_arg, "--branch")?;
        } else if let Some(rest) = arg.strip_prefix("--branch=") {
            if branch_arg.is_some() {
                return Err("--branch was specified more than once".into());
            }
            branch_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--tag" {
            take_flag_value(args, &mut i, &mut tag_arg, "--tag")?;
        } else if let Some(rest) = arg.strip_prefix("--tag=") {
            if tag_arg.is_some() {
                return Err("--tag was specified more than once".into());
            }
            tag_arg = Some(rest.to_string());
            i += 1;
        } else if arg.starts_with('-') {
            return Err(format!("silt add: unknown flag '{arg}'").into());
        } else if name.is_none() {
            name = Some(arg.clone());
            i += 1;
        } else {
            return Err(format!("silt add: unexpected extra argument '{arg}'").into());
        }
    }
    let name = name.ok_or("silt add: missing required <name> argument")?;

    // ── Source selection (path vs git) ────────────────────────────────
    //
    // Mutually exclusive: zero source flags or both at once is a usage
    // error. For `--git` we additionally require exactly one ref form.
    let source = match (path_arg.is_some(), git_arg.is_some()) {
        (true, true) => {
            return Err("silt add: --path and --git are mutually exclusive; pick one".into());
        }
        (false, false) => {
            return Err(
                "silt add: missing source flag; use --path <path> or --git <url> \
                 [--rev|--branch|--tag <ref>]"
                    .into(),
            );
        }
        (true, false) => {
            // Bare ref flags without --git make no sense — surface a
            // dedicated error rather than silently ignoring them.
            for (val, flag) in [
                (&rev_arg, "--rev"),
                (&branch_arg, "--branch"),
                (&tag_arg, "--tag"),
            ] {
                if val.is_some() {
                    return Err(format!(
                        "silt add: {flag} requires --git (it has no meaning with --path)"
                    )
                    .into());
                }
            }
            AddSource::Path(path_arg.expect("checked above"))
        }
        (false, true) => {
            let url = git_arg.expect("checked above");
            // Tally the ref forms so the multiple-vs-missing diagnostics
            // can be tailored.
            let mut chosen: Vec<(&str, String)> = Vec::new();
            if let Some(v) = rev_arg {
                chosen.push(("rev", v));
            }
            if let Some(v) = branch_arg {
                chosen.push(("branch", v));
            }
            if let Some(v) = tag_arg {
                chosen.push(("tag", v));
            }
            let ref_spec = match chosen.len() {
                0 => {
                    return Err(
                        "silt add: --git requires exactly one of --rev, --branch, or --tag".into(),
                    );
                }
                1 => {
                    let (kind, value) = chosen.into_iter().next().unwrap();
                    match kind {
                        "rev" => silt::git::GitRef::Rev(value),
                        "branch" => silt::git::GitRef::Branch(value),
                        "tag" => silt::git::GitRef::Tag(value),
                        _ => unreachable!("kinds restricted above"),
                    }
                }
                _ => {
                    let mentioned: Vec<&str> = chosen.iter().map(|(k, _)| *k).collect();
                    return Err(format!(
                        "silt add: --git takes exactly one ref form, but multiple were given: {}",
                        mentioned
                            .iter()
                            .map(|k| format!("--{k}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                    .into());
                }
            };
            AddSource::Git { url, ref_spec }
        }
    };

    // ── Manifest discovery ─────────────────────────────────────────────
    let cwd = std::env::current_dir().map_err(|e| format!("failed to determine cwd: {e}"))?;
    let (root, manifest) = match find_project_root(&cwd)? {
        Some(pair) => pair,
        None => return Err("silt add must be run inside a silt package".into()),
    };

    // ── Name validation ────────────────────────────────────────────────
    //
    // Identifier rules first (cheap, deterministic), then collisions:
    // a name that's both invalid AND a builtin should report invalid
    // (the user's bigger problem).
    if !silt::manifest::is_silt_identifier(&name) {
        return Err(format!(
            "silt add: invalid dependency name `{name}`: \
             must match silt identifier rules `[a-z_][a-z0-9_]*`"
        )
        .into());
    }
    if silt::module::is_builtin_module(&name) {
        return Err(format!(
            "silt add: dependency name `{name}` collides with builtin module `{name}`; \
             pick a different name"
        )
        .into());
    }
    let already_present = manifest
        .dependencies
        .keys()
        .any(|sym| intern::resolve(*sym) == name);
    if already_present {
        return Err(format!("silt add: dependency '{name}' is already declared").into());
    }

    // ── Source validation + the rendered TOML inline-table ────────────
    //
    // Path deps validate filesystem state; git deps do shape checks +
    // an `ls-remote HEAD` reachability ping + a ref-existence check
    // before we mutate anything on disk.
    let (success_summary, inline) = match source {
        AddSource::Path(path_arg) => {
            // Resolve the user-provided path against cwd (so `silt add
            // foo --path ../foo` works regardless of where in the
            // package tree they're sitting), then verify the
            // destination is actually a silt package. Both checks
            // deliberately use `is_file` / `is_dir` rather than
            // `exists()` so a stray symlink doesn't trip a misleading
            // error.
            let user_path = PathBuf::from(&path_arg);
            let absolute_dep_path = if user_path.is_absolute() {
                user_path.clone()
            } else {
                cwd.join(&user_path)
            };
            let absolute_dep_path = normalize_path(&absolute_dep_path);
            if !absolute_dep_path.exists() {
                return Err(format!(
                    "silt add: path does not exist: {}",
                    absolute_dep_path.display()
                )
                .into());
            }
            let dep_manifest = absolute_dep_path.join("silt.toml");
            if !dep_manifest.is_file() {
                return Err(format!(
                    "silt add: path is not a silt package (no silt.toml found): {}",
                    absolute_dep_path.display()
                )
                .into());
            }

            // We always store the path relative-to-manifest-dir when
            // possible; this keeps freshly-checked-out workspaces
            // portable across machines. If the dep lives outside the
            // manifest's tree (e.g. an absolute path under /opt) we
            // fall back to the absolute form because there's no clean
            // relative form to write.
            let stored_path = relative_from(&root, &absolute_dep_path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| absolute_dep_path.display().to_string());
            let mut inline = toml_edit::InlineTable::new();
            inline.insert(
                "path",
                toml_edit::value(stored_path.clone()).into_value().unwrap(),
            );
            (
                format!("Added dependency '{name}' (path = \"{stored_path}\")"),
                inline,
            )
        }
        AddSource::Git { url, ref_spec } => {
            // Cheap shape check first — this lets us reject obviously
            // malformed input ("not a url") without paying for an
            // `ls-remote` roundtrip.
            if !looks_like_git_url(&url) {
                return Err(format!(
                    "silt add: --git URL `{url}` doesn't look like a git URL \
                     (expected http(s)://, git://, ssh://, file://, or user@host:path)"
                )
                .into());
            }
            // Shape-validate Rev locally so a malformed SHA fails before
            // any network traffic. `verify_reachable` would catch this
            // eventually but the diagnostic is friendlier here, and we
            // also avoid a wasted roundtrip.
            if let silt::git::GitRef::Rev(sha) = &ref_spec
                && !silt::git::is_valid_sha_shape(sha)
            {
                return Err(format!(
                    "silt add: --rev `{sha}` is not a valid commit SHA shape \
                     (expected 7-40 hexadecimal characters)"
                )
                .into());
            }

            // Reachability ping: catches typos and private-repo-no-auth
            // before we mutate anything. We surface git's stderr in the
            // error path (via Display on GitError::CommandFailed) so
            // users see the real diagnostic, e.g. "Repository not
            // found" or "Permission denied (publickey)".
            silt::git::verify_reachable(&url)
                .map_err(|e| format!("silt add: cannot reach `{url}`: {e}"))?;

            // Ref existence: rejects `--branch nonexistent_xyz` etc.
            // For Rev specs this is a no-op (offline shape check).
            silt::git::resolve_ref(&url, &ref_spec).map_err(|e| {
                format!(
                    "silt add: cannot resolve {} `{}` in `{url}`: {e}",
                    ref_spec.kind(),
                    ref_spec.as_ref_string()
                )
            })?;

            // Render the inline table. Key order is fixed (`git` first,
            // then the ref form) so manifests stay diffable across
            // different runs and machines.
            let mut inline = toml_edit::InlineTable::new();
            inline.insert("git", toml_edit::value(url.clone()).into_value().unwrap());
            let ref_value = ref_spec.as_ref_string().to_string();
            inline.insert(
                ref_spec.kind(),
                toml_edit::value(ref_value.clone()).into_value().unwrap(),
            );
            (
                format!(
                    "Added dependency '{name}' (git = \"{url}\", {} = \"{}\")",
                    ref_spec.kind(),
                    ref_value
                ),
                inline,
            )
        }
    };

    // ── Manifest mutation via toml_edit ────────────────────────────────
    //
    // toml_edit preserves formatting, comments, and key ordering
    // verbatim — required so a user who's hand-formatted their
    // silt.toml doesn't lose that work the first time they run `silt
    // add`. We only insert the new entry; everything else stays as-is.
    let manifest_path = root.join("silt.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = manifest_text
        .parse()
        .map_err(|e| format!("failed to parse {}: {e}", manifest_path.display()))?;

    // Ensure a `[dependencies]` table exists. If it's missing entirely
    // we create one as an explicit table (so it renders as the
    // header-style `[dependencies]` users expect, not as an inline
    // `dependencies = {}` blob).
    if doc.get("dependencies").is_none() {
        doc["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let deps = doc["dependencies"]
        .as_table_mut()
        .ok_or("silt.toml has a [dependencies] entry that isn't a table")?;

    deps.insert(
        &name,
        toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)),
    );

    fs::write(&manifest_path, doc.to_string())
        .map_err(|e| format!("failed to write {}: {e}", manifest_path.display()))?;

    // ── Lockfile regeneration ──────────────────────────────────────────
    //
    // Re-load the just-written manifest and resolve the lockfile from
    // it. We deliberately don't reuse the `manifest` we loaded earlier
    // — toml_edit just rewrote the file, and any future validation
    // tightening should run against the on-disk form, not a stale
    // in-memory copy.
    let updated = Manifest::load(&manifest_path)
        .map_err(|e| format!("manifest re-validation failed after edit: {e}"))?;

    println!("{success_summary}");

    let lockfile =
        Lockfile::resolve(&updated).map_err(|e| format!("failed to resolve dependencies: {e}"))?;
    let lock_path = root.join("silt.lock");
    lockfile
        .write(&lock_path)
        .map_err(|e| format!("failed to write {}: {e}", lock_path.display()))?;

    Ok(())
}

/// Cheap regex-free shape check for git URLs. We deliberately keep the
/// rule loose — the actual `git ls-remote` will fail with a precise
/// diagnostic for transport-level errors. This is just here to reject
/// obvious non-URLs ("not a url", "/usr/local") and surface a friendlier
/// error than `git`'s "fatal: '/usr/local' does not appear to be a git
/// repository".
///
/// Accepts:
///   - `http://...`, `https://...`
///   - `git://...`
///   - `ssh://...`
///   - `file://...`
///   - `user@host:path` (the SCP-style git URL form: an `@` followed by a
///     `:` somewhere later, with no whitespace anywhere)
fn looks_like_git_url(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    if s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git://")
        || s.starts_with("ssh://")
        // `file://` is the canonical local-bare-repo URL form; git
        // clone accepts it natively. Used by hermetic test fixtures
        // and occasionally by users sharing repos via a local mount.
        || s.starts_with("file://")
    {
        // Must have *something* after the scheme.
        return s.split("://").nth(1).is_some_and(|rest| !rest.is_empty());
    }
    // SCP-style: `user@host:path`. Require both `@` and a `:` *after* the
    // `@` so a stray colon-prefix doesn't pass.
    if let Some(at_pos) = s.find('@')
        && let Some(colon_pos) = s[at_pos..].find(':')
    {
        // user@host:something
        let after_colon = &s[at_pos + colon_pos + 1..];
        if !after_colon.is_empty() {
            return true;
        }
    }
    false
}

/// Lexically normalize a path: collapse `.` and `..` components without
/// touching the filesystem. Lockfile resolution does this internally
/// for dep paths; we apply the same normalization here so the manifest
/// records (and the success-line prints) the user-recognizable form.
fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Express `target` as a path relative to `base`, using `..` segments
/// where necessary. Returns `None` only when the inputs differ in
/// rootedness (one absolute, one relative) — there's no sensible
/// relative form in that case and the caller falls back to absolute.
///
/// Rolling our own keeps us off the `pathdiff` crate; the logic is
/// 20 lines and the v0.7 manifest only needs ASCII-cleanly-named
/// paths anyway.
fn relative_from(base: &Path, target: &Path) -> Option<PathBuf> {
    if base.is_absolute() != target.is_absolute() {
        return None;
    }
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    // Find the longest common prefix.
    let mut shared = 0;
    while shared < base_components.len()
        && shared < target_components.len()
        && base_components[shared] == target_components[shared]
    {
        shared += 1;
    }
    let mut result = PathBuf::new();
    for _ in shared..base_components.len() {
        result.push("..");
    }
    for comp in &target_components[shared..] {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        // base == target — express that as `.` rather than the empty
        // string so toml_edit emits a syntactically valid path.
        result.push(".");
    }
    Some(result)
}

/// Resolve the package entry point (`<root>/src/main.silt`) for the current
/// directory.
///
/// Returns:
/// - `Ok(Some(path))` — we are inside a package and `src/main.silt` exists.
/// - `Ok(None)` — there is no enclosing package (no `silt.toml` in any parent).
/// - `Err(())` — entry point check failed and we already wrote a diagnostic.
///   The caller should propagate the failure as a non-zero exit.
fn resolve_package_entry_point() -> Result<Option<PathBuf>, ()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (root, _manifest) = match find_project_root(&cwd) {
        Ok(Some(pair)) => pair,
        Ok(None) => return Ok(None),
        Err(e) => die_on_manifest_error(e),
    };
    let entry = root.join("src").join("main.silt");
    if !entry.is_file() {
        eprintln!(
            "package has no entry point — expected `src/main.silt` at {}",
            entry.display()
        );
        return Err(());
    }
    Ok(Some(entry))
}

/// Recursively find all .silt files in a directory.
fn find_silt_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_silt_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with(".silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

/// Build a map from bare top-level function name → (file_path, source text)
/// for every module file that `main_path` transitively imports.
///
/// We scan `main_source` (and each imported module's source) for
/// `import <name>` statements, resolve them relative to the main file's
/// project root, and record each top-level `fn <name>` / `pub fn <name>`
/// we find in the resulting module file.
///
/// This is a *best-effort* mapping used solely to improve runtime-error
/// rendering when an error propagates out of an imported module. Name
/// collisions are handled by *exclusion*, not by winner-takes-all:
///
///   1. If a function name is ALSO defined at the top level of the main
///      source file, it is excluded from the map. The renderer then falls
///      back to the main source — which is correct, because the VM's
///      innermost frame name cannot distinguish `main::foo` from
///      `mod::foo`, and the main file is the safer guess.
///   2. If a function name appears in MORE THAN ONE imported module, it
///      is likewise excluded — we have no way to pick the right module.
///
/// In both cases a map miss causes the renderer to fall back to the main
/// source, which is the safe default: at worst the rendered snippet
/// points at main's line N, which is typically close to the call site
/// that invoked the module function.
///
/// See E1 in the audit for the original gap (runtime errors from module
/// code rendered against the main file), and the follow-up collision
/// case (`test_module_runtime_error_with_name_collision_renders_correct_file`)
/// which motivated the exclusion strategy here.
fn collect_module_function_sources(
    main_path: &str,
    main_source: &str,
) -> std::collections::HashMap<String, (PathBuf, String)> {
    use std::collections::{HashMap, HashSet};

    let mut out: HashMap<String, (PathBuf, String)> = HashMap::new();
    let project_root: PathBuf = Path::new(main_path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(main_path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Names defined at the top level of the main source file. Any module
    // function sharing one of these names is ambiguous w.r.t. the VM's
    // bare-name call frame, so we exclude it from the map and let the
    // renderer fall back to the main source.
    let main_fn_names: HashSet<String> = extract_top_level_fn_names(main_source)
        .into_iter()
        .collect();

    // First pass: walk the import graph, recording every (fn_name,
    // module_file, module_source) tuple we encounter. We can't decide
    // inclusion until we've seen the full graph — a name that appears in
    // one module might also appear in another, in which case it must be
    // excluded from the final map.
    let mut candidates: Vec<(String, PathBuf, String)> = Vec::new();
    let mut name_module_count: HashMap<String, usize> = HashMap::new();

    // BFS from main source: scan import statements, load each module file,
    // repeat for transitive imports.
    let mut queue: Vec<(String, String)> = vec![(main_path.to_string(), main_source.to_string())];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(main_path.to_string());

    while let Some((_cur_path, cur_source)) = queue.pop() {
        for import_name in extract_imports(&cur_source) {
            // Skip builtin modules — they're not file-backed.
            if silt::module::is_builtin_module(&import_name) {
                continue;
            }
            let file_path = project_root.join(format!("{import_name}.silt"));
            let file_key = file_path.display().to_string();
            if !seen.insert(file_key.clone()) {
                continue;
            }
            let Ok(mod_source) = fs::read_to_string(&file_path) else {
                continue;
            };
            // Per-module dedupe: a function name appearing twice in the
            // SAME file still counts as a single module for collision
            // purposes.
            let mut local_names: HashSet<String> = HashSet::new();
            for fn_name in extract_top_level_fn_names(&mod_source) {
                if local_names.insert(fn_name.clone()) {
                    *name_module_count.entry(fn_name.clone()).or_insert(0) += 1;
                    candidates.push((fn_name, file_path.clone(), mod_source.clone()));
                }
            }
            // Register the synthetic module-init frame name so that
            // top-level errors (e.g. `pub let x = 1 / 0`) can be
            // resolved to the module's source file.
            let init_key = format!("<module:{import_name}>");
            out.insert(init_key, (file_path.clone(), mod_source.clone()));

            queue.push((file_key, mod_source));
        }
    }

    // Second pass: build the final map, excluding any name that either
    // collides with main or is defined in more than one module.
    for (fn_name, file_path, mod_source) in candidates {
        if main_fn_names.contains(&fn_name) {
            continue;
        }
        if name_module_count.get(&fn_name).copied().unwrap_or(0) > 1 {
            continue;
        }
        // At this point the name is unique to a single module and not
        // shadowed by the main file, so recording it is unambiguous.
        out.entry(fn_name).or_insert((file_path, mod_source));
    }
    out
}

/// Extract the bare module names referenced by `import <name>` statements
/// in `source`. Supports both `import foo` and `import foo.{ Bar, baz }`
/// forms — we just need the module name, not the item list.
fn extract_imports(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        // Module name runs to the first `.`, whitespace, `{`, or `as`.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// Extract the names of top-level `fn <name>` (optionally `pub fn`)
/// declarations in `source`. This is a purely textual scan — we only
/// need it to correlate a runtime frame's function name with a module
/// file, so missing an edge case (e.g. an `fn` inside a multi-line
/// comment) just means falling back to the main file for rendering.
fn extract_top_level_fn_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let rest = match line.strip_prefix("pub fn ") {
            Some(r) => r,
            None => match line.strip_prefix("fn ") {
                Some(r) => r,
                None => continue,
            },
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// Run a file using the bytecode VM (default path).
fn vm_run_file(path: &str) {
    silt::intern::reset();
    let (functions, source) = compile_file(path);

    // Build a name → (module_file, source) map so runtime errors from
    // imported modules are rendered against the correct file.  See
    // `collect_module_function_sources` for the rationale.
    let module_sources = collect_module_function_sources(path, &source);

    let Some(script) = functions.into_iter().next() else {
        eprintln!("{path}: internal error: empty function list");
        process::exit(1);
    };
    let script = Arc::new(script);

    // Run via VM
    let mut vm = Vm::new();
    if let Err(e) = vm.run(script) {
        if let Some(span) = e.span {
            // F13 (audit round 17) + G1 (audit round 21): normalize
            // frame and error-header paths so they all use the same
            // style the user typed on the command line.  Moved above
            // the SourceError construction so the `-->` line also
            // benefits from normalization, not just the call-stack
            // frames.
            //
            // Lock: tests/cli_test_rendering_tests.rs
            // `test_cross_module_call_stack_uses_consistent_path_style`
            // `test_run_module_error_paths_consistently_normalized`.
            let user_path_is_absolute = Path::new(path).is_absolute();
            let cwd = std::env::current_dir().ok();
            let normalize_path = |candidate: &Path| -> String {
                if user_path_is_absolute {
                    if candidate.is_absolute() {
                        candidate.display().to_string()
                    } else if let Some(ref cwd) = cwd {
                        cwd.join(candidate).display().to_string()
                    } else {
                        candidate.display().to_string()
                    }
                } else {
                    if let Some(ref cwd) = cwd {
                        match candidate.strip_prefix(cwd) {
                            Ok(rel) => rel.display().to_string(),
                            Err(_) => candidate.display().to_string(),
                        }
                    } else {
                        candidate.display().to_string()
                    }
                }
            };

            // Determine which source text & file path to render against.
            // Prefer the innermost non-synthetic frame's function name,
            // falling back to the main file when the frame isn't from an
            // imported module.
            let innermost_fn_name: Option<&str> = e
                .call_stack
                .iter()
                .find(|(n, _)| !n.starts_with('<') || n.starts_with("<module:"))
                .map(|(n, _)| n.as_str());
            let (err_source, err_path): (&str, String) =
                match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                    Some((module_path, module_source)) => {
                        (module_source.as_str(), normalize_path(module_path))
                    }
                    None => (source.as_str(), normalize_path(Path::new(path))),
                };
            let source_err = SourceError::runtime_at(&e.message, span, err_source, &err_path);
            eprintln!("{source_err}");
            // Print call stack if there are user frames beyond the error site.
            // Drop synthetic entry-point frames (<script>, <call:...>) by name
            // rather than by span — a zero-spanned frame inside an otherwise
            // good stack shouldn't cause the whole stack to be discarded.
            // Keep <module:...> frames for module-aware path resolution.
            let meaningful: Vec<_> = e
                .call_stack
                .iter()
                .filter(|(name, _)| !name.starts_with('<') || name.starts_with("<module:"))
                .collect();
            // Only show the stack if it adds information beyond the error
            // site the user already sees above. A single-frame "stack"
            // would just restate that location, which is noisy.
            let any_real_span = meaningful.iter().any(|(_, s)| s.line > 0);
            if meaningful.len() >= 2 && any_real_span {
                eprintln!("\ncall stack:");
                let head = 10;
                let tail = 5;
                let print_frame = |name: &str, frame_span: &silt::lexer::Span| {
                    // Each frame uses its own function's source file for
                    // file labels — this matters when the call crosses a
                    // module boundary.
                    let frame_path: String = match module_sources.get(name) {
                        Some((p, _)) => normalize_path(p),
                        None => normalize_path(Path::new(path)),
                    };
                    if frame_span.line > 0 {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, frame_path, frame_span.line, frame_span.col
                        );
                    } else {
                        eprintln!("  -> {name}  at {frame_path}:<unknown location>");
                    }
                };
                if meaningful.len() <= head + tail {
                    for (name, frame_span) in &meaningful {
                        print_frame(name, frame_span);
                    }
                } else {
                    for (name, frame_span) in &meaningful[..head] {
                        print_frame(name, frame_span);
                    }
                    let omitted = meaningful.len() - head - tail;
                    eprintln!("  ... ({omitted} more frames)");
                    for (name, frame_span) in &meaningful[meaningful.len() - tail..] {
                        print_frame(name, frame_span);
                    }
                }
            }
        } else if is_missing_main_error(&e) {
            // Round-24 B-fix: wrap the missing-main message in a real
            // SourceError so it renders with the canonical
            // `error[compile]:` header consistent with every other
            // file-level diagnostic. Previously this was a plain
            // `eprintln!` with no header / no `-->` locator — the only
            // diagnostic in the codebase that broke the rustc-style
            // shape. Lock: tests/empty_program_diagnostic_tests.rs.
            //
            // We use Span::new(0, 0) because there's no source location
            // for "the file has no main()" — the Display impl omits the
            // `-->` line when span.line == 0 but still emits the header.
            //
            // Detect test-only files so we can nudge the user toward
            // `silt test` instead of the generic "add a main()" error.
            // The body line below the header is rendered as a `= note:`
            // continuation, matching the multi-line message convention.
            let msg = if looks_like_test_file(&source) {
                format!(
                    "program has no main() function\nThis looks like a test file — run it with 'silt test {path}' instead."
                )
            } else {
                "program has no main() function\nadd one as the entry point".to_string()
            };
            let source_err =
                SourceError::compile_error_at(msg, silt::lexer::Span::new(0, 0), &source, path);
            eprintln!("{source_err}");
        } else {
            eprintln!("{path}: {e}");
        }
        process::exit(1);
    }
}

/// Disassemble a file's bytecode without running it.
fn disasm_file(path: &str) {
    silt::intern::reset();
    // Read-only command — never mutates `silt.lock`. If the lock is
    // stale or missing we resolve in-memory and continue; the user
    // can still get a useful disassembly without a lockfile write.
    let (functions, _source) = compile_file_with_options(path, false);

    // Print disassembly of each function
    for func in &functions {
        print!("{}", disassemble_function(func));
        println!();
    }
}

fn check_file(path: &str, format: OutputFormat) {
    silt::intern::reset();
    // `silt check` must match `silt run` diagnostics exactly, minus
    // execution. That means (a) running the compile step so the compiler
    // surfaces real module-resolution errors, and (b) filtering out the
    // type checker's "unknown module" warnings — which the compiler
    // resolves later — so we don't cry wolf on every valid file-backed
    // import. Previously this path skipped compile entirely AND emitted
    // every warning, which produced spurious "unknown module" warnings
    // on programs that `silt run` handles cleanly.
    let result = run_compile_pipeline(path, false, true, true);

    // Filter per-entry: drop the "unknown module" warnings the compiler
    // will resolve, but keep every other diagnostic so real errors still
    // surface. See `reportable_type_errors` for the rationale.
    let reportable_types = reportable_type_errors(&result);
    let mut errors: Vec<&SourceError> = result
        .parse_errors
        .iter()
        .chain(reportable_types.iter().copied())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect();

    // Round-24 B-fix: if compilation succeeded but the program defines no
    // `main` function AND the file doesn't look like a library module
    // (`pub fn ...`) or a test file (`fn test_...`), surface the same
    // canonical missing-main diagnostic that `silt run` emits — exit 1
    // with `error[compile]: program has no main() function`. Without
    // this, an empty / no-main "script" file would pass `silt check`
    // cleanly and then fail at `silt run`, which is off-spec.
    //
    // We deliberately exclude library modules (identified by any
    // `pub fn`) and test files (identified by `fn test_*` / `test.*`)
    // because those files legitimately never define `main` and are
    // consumed by importers / by `silt test` respectively. The
    // `silt run` path still flags both with its own nudge — `check`
    // is the "does this file compile standalone" answer, and neither
    // a library nor a test file should be invoked standalone.
    //
    // Lock: tests/empty_program_diagnostic_tests.rs and
    // tests/examples_check.rs (every_example_type_checks_and_has_no_warnings).
    let missing_main_err: Option<SourceError> = if errors.is_empty()
        && result.functions.is_some()
        && !program_has_main(&result.source)
        && !looks_like_library_module(&result.source)
        && !looks_like_test_file(&result.source)
    {
        let msg = "program has no main() function\nadd one as the entry point".to_string();
        Some(SourceError::compile_error_at(
            msg,
            silt::lexer::Span::new(0, 0),
            &result.source,
            path,
        ))
    } else {
        None
    };
    if let Some(ref err) = missing_main_err {
        errors.push(err);
    }

    if format == OutputFormat::Json {
        print_json_errors(&errors);
    } else {
        // F14 (audit round 17): separate diagnostics with blank lines.
        silt::errors::eprintln_errors_with_separator(&errors);
    }

    // A hard error is real only if it's a parse/compile error or a
    // non-suppressed type error with severity Error — same gate as
    // `compile_file`. We deliberately do NOT rely on
    // `result.has_hard_errors`, which counts the suppressed warnings'
    // peers but we re-check here for clarity.
    let has_real_type_error = reportable_types.iter().any(|e| !e.is_warning);
    let has_real_hard_errors = !result.parse_errors.is_empty()
        || !result.compile_errors.is_empty()
        || has_real_type_error
        || missing_main_err.is_some();
    if has_real_hard_errors {
        process::exit(1);
    }
}

/// Conservative text scan: does `source` look like a library module
/// (has at least one `pub fn ...` definition)?  Used by `silt check` to
/// suppress the missing-main diagnostic on files that are intended to
/// be imported rather than run directly.
fn looks_like_library_module(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("pub fn ") {
            return true;
        }
    }
    false
}

/// Conservative text scan for whether `source` defines a top-level `main`
/// function. We match lines whose trimmed prefix is `fn main(` / `fn main `
/// / `fn main{` or the `pub fn` variants. Must be conservative — a false
/// positive here would suppress the missing-main diagnostic for a program
/// that actually needs it.
fn program_has_main(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        let rest = if let Some(r) = t.strip_prefix("pub fn ") {
            r
        } else if let Some(r) = t.strip_prefix("fn ") {
            r
        } else {
            continue;
        };
        // Match `main` followed by a non-identifier character.
        if let Some(after) = rest.strip_prefix("main") {
            match after.chars().next() {
                Some(c) if !(c.is_alphanumeric() || c == '_') => return true,
                None => return true,
                _ => {}
            }
        }
    }
    false
}

fn print_json_errors(errors: &[&SourceError]) {
    let json_errors: Vec<serde_json::Value> = errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file.as_deref().unwrap_or("<unknown>"),
                "line": e.span.line,
                "col": e.span.col,
                "message": e.message.lines().next().unwrap_or(&e.message),
                "severity": if e.is_warning { "warning" } else { "error" },
                "kind": e.kind.to_string(),
            })
        })
        .collect();
    match serde_json::to_string(&json_errors) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("internal error: failed to serialize diagnostics: {e}");
            process::exit(1);
        }
    }
}

fn find_test_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_test_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with("_test.silt") || name.ends_with(".test.silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

fn run_tests(file: Option<&str>, filter: Option<String>) {
    silt::intern::reset();
    let paths: Vec<String> = if let Some(f) = file {
        let p = Path::new(f);
        if p.is_dir() {
            // silt test dir/ — find all test files in directory recursively
            find_test_files(p)
        } else {
            // silt test file.silt — single file
            vec![f.to_string()]
        }
    } else {
        // silt test — find all test files in current directory recursively
        find_test_files(Path::new("."))
    };

    if paths.is_empty() {
        println!("no test files found");
        return;
    }

    // When a filter is provided, skip files that can't possibly contain matching tests.
    // We do a quick text scan for `fn test_` / `fn skip_test_` names rather than a full parse.
    let paths: Vec<String> = if let Some(ref filter) = filter {
        paths
            .into_iter()
            .filter(|path| {
                let source = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => return true, // keep the file so the error is reported later
                };
                // Scan for function names like `fn test_...` or `fn skip_test_...`
                // (including `pub fn` variants).
                for line in source.lines() {
                    let trimmed = line.trim_start();
                    let rest = if let Some(r) = trimmed.strip_prefix("pub fn ") {
                        Some(r)
                    } else {
                        trimmed.strip_prefix("fn ")
                    };
                    if let Some(rest) = rest {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if (name.starts_with("test_") || name.starts_with("skip_test_"))
                            && name.contains(filter.as_str())
                        {
                            return true;
                        }
                    }
                }
                false
            })
            .collect()
    } else {
        paths
    };

    if paths.is_empty() {
        println!("no matching test files found");
        return;
    }

    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    // Count files that failed to lex / parse / type-check / compile.
    // These are tracked separately from the per-test failure counter so
    // that `X tests: Y passed, Z failed` still reflects what actually
    // ran. Previously a single file compile error was booked as one
    // "failed test", which was misleading — that file may have contained
    // dozens of tests we couldn't even count.
    let mut file_errors: usize = 0;

    for path in &paths {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{path}: failed to read — {e}");
                file_errors += 1;
                continue;
            }
        };

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                let source_err = SourceError::from_lex_error(&e, &source, path.as_str());
                eprintln!("{path}: failed to compile — {source_err}");
                file_errors += 1;
                continue;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
        if !parse_errors.is_empty() {
            eprintln!("{path}: failed to compile — parse errors:");
            for (i, e) in parse_errors.iter().enumerate() {
                if i > 0 {
                    eprintln!();
                }
                let source_err = SourceError::from_parse_error(e, &source, path.as_str());
                eprintln!("{source_err}");
            }
            file_errors += 1;
            continue;
        }

        // Type-check before compiling so type errors fail the test.
        // Drop "unknown module" warnings for imports the compiler resolves
        // later (see `reportable_type_errors` / `is_unknown_module_warning`):
        // every test file that imports a sibling module would otherwise
        // flood test output with a spurious warning even on clean runs.
        // Matches `silt run`'s behavior exactly. Real missing modules are
        // still caught by the compiler's own "cannot load module" error
        // in the block below.
        let type_errors = typechecker::check(&mut program);
        let mut has_type_error = false;
        let mut printed_type_errors: usize = 0;
        for te in &type_errors {
            let source_err = SourceError::from_type_error(te, &source, path);
            if is_unknown_module_warning(&source_err) {
                continue;
            }
            if printed_type_errors > 0 {
                eprintln!();
            }
            eprintln!("{source_err}");
            printed_type_errors += 1;
            if te.severity == typechecker::Severity::Error {
                has_type_error = true;
            }
        }
        if has_type_error {
            eprintln!("{path}: failed to compile — type errors (see above)");
            file_errors += 1;
            continue;
        }

        // Compile all declarations (without calling main).  Package
        // setup mirrors `silt run`: prefer the nearest enclosing
        // `silt.toml` so cross-file `import foo` resolves consistently
        // regardless of which file `silt test` was pointed at, and
        // auto-update the lockfile when the manifest has new deps.
        let (local_pkg, package_roots) = package_setup_for_file(path.as_str(), true);
        let mut compiler = Compiler::with_package_roots(local_pkg, package_roots);
        let functions = match compiler.compile_declarations(&program) {
            Ok(f) => f,
            Err(e) => {
                let source_err = SourceError::from_compile_error(&e, &source, path);
                eprintln!("{path}: failed to compile — {source_err}");
                file_errors += 1;
                continue;
            }
        };

        // Run the setup script to register all globals in the VM
        let Some(first) = functions.into_iter().next() else {
            eprintln!("{path}: internal error: no functions compiled");
            file_errors += 1;
            continue;
        };
        // Build module_sources BEFORE running the script so setup errors
        // from imported modules can render against the correct source file.
        let module_sources = collect_module_function_sources(path, &source);

        // G2 (audit round 21): normalize frame and error-header paths
        // for both setup errors and per-test errors.  Moved above the
        // vm.run() call so setup-error rendering can also benefit.
        //
        // Lock: tests/cli_test_rendering_tests.rs
        // `test_test_setup_error_paths_normalized`.
        let user_path_is_absolute = Path::new(path.as_str()).is_absolute();
        let cwd = std::env::current_dir().ok();
        let normalize_path = |candidate: &Path| -> String {
            if user_path_is_absolute {
                if candidate.is_absolute() {
                    candidate.display().to_string()
                } else if let Some(ref cwd) = cwd {
                    cwd.join(candidate).display().to_string()
                } else {
                    candidate.display().to_string()
                }
            } else {
                if let Some(ref cwd) = cwd {
                    match candidate.strip_prefix(cwd) {
                        Ok(rel) => rel.display().to_string(),
                        Err(_) => candidate.display().to_string(),
                    }
                } else {
                    candidate.display().to_string()
                }
            }
        };

        let script = Arc::new(first);
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            if let Some(span) = e.span {
                // Find the innermost frame that identifies a source file:
                // either a user function or a <module:X> init frame.
                let innermost_fn_name: Option<&str> = e
                    .call_stack
                    .iter()
                    .find(|(n, _)| !n.starts_with('<') || n.starts_with("<module:"))
                    .map(|(n, _)| n.as_str());
                let (err_source, err_path): (&str, String) =
                    match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                        Some((module_path, module_source)) => {
                            (module_source.as_str(), normalize_path(module_path))
                        }
                        None => (source.as_str(), normalize_path(Path::new(path))),
                    };
                let source_err = SourceError::runtime_at(&e.message, span, err_source, &err_path);
                eprintln!("{path}: setup error:");
                eprintln!("{source_err}");
                let stack_lines =
                    silt::vm::error::render_call_stack(&e.call_stack, |frame_name, frame_span| {
                        let frame_path: String = match module_sources.get(frame_name) {
                            Some((p, _)) => normalize_path(p),
                            None => normalize_path(Path::new(path)),
                        };
                        if frame_span.line > 0 {
                            format!("{}:{}:{}", frame_path, frame_span.line, frame_span.col)
                        } else {
                            format!("{frame_path}:<unknown location>")
                        }
                    });
                if !stack_lines.is_empty() {
                    eprintln!("\ncall stack:");
                    for line in stack_lines {
                        eprintln!("{line}");
                    }
                }
            } else {
                eprintln!("{path}: setup error: {e}");
            }
            file_errors += 1;
            continue;
        }

        // Run each test function
        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                let name = silt::intern::resolve(f.name);
                if name.starts_with("skip_test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    eprintln!("  SKIP {path}::{name}");
                    skipped += 1;
                    continue;
                }
                if name.starts_with("test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    let caller = silt::bytecode::call_global_script(&name);
                    match vm.run(Arc::new(caller)) {
                        Ok(_) => {
                            eprintln!("  PASS {path}::{name}");
                            passed += 1;
                        }
                        Err(e) => {
                            eprintln!("  FAIL {path}::{name}");
                            if let Some(span) = e.span {
                                // Determine which source text & file path
                                // to render against, mirroring `silt run`.
                                let innermost_fn_name: Option<&str> = e
                                    .call_stack
                                    .iter()
                                    .find(|(n, _)| !n.starts_with('<') || n.starts_with("<module:"))
                                    .map(|(n, _)| n.as_str());
                                let (err_source, err_path): (&str, String) =
                                    match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                                        Some((module_path, module_source)) => {
                                            (module_source.as_str(), normalize_path(module_path))
                                        }
                                        None => (source.as_str(), path.to_string()),
                                    };
                                let source_err = SourceError::runtime_at(
                                    &e.message, span, err_source, &err_path,
                                );
                                // Indent every line of the formatted error
                                // so multi-line SourceErrors stay aligned
                                // with the FAIL header.
                                let formatted = format!("{source_err}");
                                for line in formatted.lines() {
                                    eprintln!("    {line}");
                                }
                                // Mirror `silt run`: render a call stack
                                // when the error crosses ≥2 meaningful
                                // frames. Without this, a test that fails
                                // deep inside a helper chain only prints
                                // the innermost site, leaving the user
                                // without any trail back to the test
                                // function that invoked it.
                                let stack_lines = silt::vm::error::render_call_stack(
                                    &e.call_stack,
                                    |frame_name, frame_span| {
                                        // Use module path if the frame
                                        // belongs to an imported module,
                                        // then normalize to match user's
                                        // path style (relative/absolute).
                                        let frame_path: String =
                                            match module_sources.get(frame_name) {
                                                Some((p, _)) => normalize_path(p),
                                                None => path.to_string(),
                                            };
                                        if frame_span.line > 0 {
                                            format!(
                                                "{}:{}:{}",
                                                frame_path, frame_span.line, frame_span.col
                                            )
                                        } else {
                                            format!("{frame_path}:<unknown location>")
                                        }
                                    },
                                );
                                if !stack_lines.is_empty() {
                                    eprintln!("\n    call stack:");
                                    for line in stack_lines {
                                        eprintln!("    {line}");
                                    }
                                }
                            } else {
                                eprintln!("    Error: {e}");
                            }
                            failed += 1;
                        }
                    }
                }
            }
        }
    }

    let test_word = if total == 1 { "test" } else { "tests" };
    if file_errors > 0 {
        eprintln!(
            "\n{total} {test_word}: {passed} passed, {failed} failed, {skipped} skipped ({file_errors} file{} failed to compile)",
            if file_errors == 1 { "" } else { "s" }
        );
    } else {
        eprintln!("\n{total} {test_word}: {passed} passed, {failed} failed, {skipped} skipped");
    }
    if total == 0 && file_errors == 0 {
        eprintln!(
            "hint: test functions must be named 'fn test_*'; test files should end with '_test.silt'"
        );
    }
    if failed > 0 || file_errors > 0 {
        process::exit(1);
    }
}
