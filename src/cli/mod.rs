//! The `silt` command, inside the library: the binary (src/main.rs)
//! calls [`main`] and adds only the allocator. Nothing here is part of
//! what the crate promises.
//!
//! Each subcommand owns its parsing, help text, and implementation in a
//! dedicated submodule under `crate::cli`. [`main`] decodes the top-level
//! subcommand and delegates to the matching `cli::<subcmd>::dispatch`
//! function.
//!
//! Shared plumbing lives in the non-subcommand modules:
//!   - `package` — manifest/lockfile discovery.
//!   - `paths` — filesystem path helpers, and the session of an entry
//!     file (`silt run|check|disasm|test` each drive it themselves).
//!   - `help` — usage banners and the top-level `--help` text.
//!   - `features` — `cfg!(feature = ...)` list for the help footer.
//!   - `watch` — the `--watch` interceptor.

pub(crate) mod add;
pub(crate) mod check;
pub(crate) mod disasm;
pub(crate) mod features;
pub(crate) mod fmt;
pub(crate) mod help;
pub(crate) mod init;
pub(crate) mod lsp;
pub(crate) mod package;
pub(crate) mod paths;
pub(crate) mod remove;
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod self_update;
pub(crate) mod test;
pub(crate) mod update;
pub(crate) mod watch;

use std::env;
use std::process;

use help::{GLOBAL_FLAGS, usage_text};
use watch::maybe_handle_watch;

// Windows defaults to a 1 MiB main-thread stack which is too tight for
// silt's recursive walks (type canonicalization, the structural judgement,
// supertrait obligation chasing).
// Linux/macOS default to 8 MiB. Spawn the entire dispatcher on a worker
// thread with an explicit reserve so the build behaves identically
// across platforms — same trick rustc uses.
//
// The reserve is 256 MiB, not 8 MiB. The typechecker's and
// compiler's expression walkers recurse per AST level, and a
// left-leaning binop chain (`1 + 1 + ...`) nests one level per term —
// in a debug build each level costs tens of KiB of stack, so an
// 8 MiB reserve aborted (`fatal runtime error: stack overflow`) on
// chains of only ~250 terms. 256 MiB is *virtual* address space:
// pages are only committed as they are actually touched, so the
// steady-state memory cost is unchanged while deep-but-legitimate
// programs (multi-thousand-term chains, very wide records) check and run instead of aborting. All checking/compiling
// entry points (run / check / disasm / test / repl / lsp) execute on
// this thread; `--watch` re-invokes the binary as a subprocess whose
// own silt-main gets the same reserve.
//
// Expressed as 32 × the 8 MiB that Linux and macOS give a main thread;
// the golden case cli/run/main_stack_size_lock_tests__aliased_import_*
// needs at least those 8 MiB on Windows.
const SILT_STACK_SIZE: usize = 32 * 8 * 1024 * 1024;

/// The `silt` command: what the binary (src/main.rs) runs. No part of
/// what the crate promises.
pub fn main() {
    // Restore the default SIGPIPE disposition before anything
    // writes to stdout or spawns a thread. The Rust runtime sets SIGPIPE
    // to ignored, which turns writes to a closed pipe into EPIPE errors
    // that `println!` converts into a panic — so `silt run loud.silt |
    // head -2` spewed a raw "failed printing to stdout: Broken pipe"
    // panic (and `silt disasm … | head` exited 101). With SIG_DFL the
    // process dies quietly on SIGPIPE like every other unix CLI.
    // A SIGPIPE blocked by the parent (the mask is inherited across
    // exec) would turn the signal into EPIPE again: it is unblocked too.
    // `print` and `println` also end the process quietly on EPIPE.
    // SAFETY: no other thread exists yet, so changing the process's
    // signal disposition and this thread's mask races with nothing; the
    // set is a zeroed `sigset_t`, initialised by `sigemptyset` before
    // it is read, and every pointer passed is valid or null.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGPIPE);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }

    // An argument that is not Unicode is no file, flag or text silt can
    // take (`env::args` would panic on it): a usage error. The program's
    // own name is only ever shown.
    let mut args: Vec<String> = Vec::new();
    for (at, arg) in env::args_os().enumerate() {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) if at == 0 => args.push(arg.to_string_lossy().into_owned()),
            Err(arg) => {
                eprintln!("error: silt: argument {at} is not valid Unicode: {arg:?}");
                process::exit(1);
            }
        }
    }
    let join = std::thread::Builder::new()
        .name("silt-main".into())
        .stack_size(SILT_STACK_SIZE)
        .spawn(move || {
            // The process is silt's own: a panic of this thread (the
            // checker, the compiler, the VM on the program's thread) or
            // of a thread of a program's runtime is a bug in silt, and
            // ends the process where it is raised; an embedder would get
            // the panic.
            crate::scheduler::exit_on_panic();
            run_main(args)
        })
        .expect("spawning silt main worker thread");
    // A panic that still reaches the top of silt's own thread was
    // raised on a thread that is not silt's and passed on (the
    // formatter's), or was to unwind (the REPL's line editor): it ends
    // the process here, in the same way.
    // A command that returned may have left threads of a runtime
    // behind: if one of them is ending the process with the internal
    // error, that is how it ends.
    match join.join() {
        Ok(()) => crate::scheduler::exit_with_status(0),
        Err(payload) => crate::scheduler::exit_with_internal_error(&payload),
    }
}

fn run_main(args: Vec<String>) {
    if args.len() < 2 {
        eprint!("{}", usage_text());
        process::exit(1);
    }

    // Handle --watch / -w flag: re-invoke without the flag on file changes.
    // Returns true when the watch interceptor ran (either looping or
    // surfacing a dry-validation error); returns false when we should
    // continue with normal dispatch.
    if maybe_handle_watch(&args) {
        return;
    }

    match args[1].as_str() {
        // `-v` is the lowercase long-form convention some UNIX tools accept
        // for `--version`. silt has no verbose flag, so treating `-v` as a
        // synonym for `--version` / `-V` is unambiguous here.
        "--version" | "-V" | "-v" => {
            println!("silt {}", crate::VERSION);
            process::exit(0);
        }
        "--help" | "-h" => {
            print!("{}", usage_text());
            process::exit(0);
        }
        // `silt help` with no extra args mirrors `silt --help`. With a
        // trailing token (`silt help fmt`) it drills into the
        // per-subcommand help by re-dispatching as `silt <cmd> --help`.
        // This matches the convention from cargo / git / rustup so users
        // who type `silt help <X>` reflexively get what they expect
        // instead of the global banner.
        "help" => {
            if args.len() >= 3 {
                let sub = args[2].clone();
                let mut forwarded: Vec<String> = Vec::with_capacity(3 + args.len() - 3);
                forwarded.push(args[0].clone());
                forwarded.push(sub);
                forwarded.push("--help".to_string());
                // Pass through any extra trailing tokens — most
                // subcommands ignore them after `--help`, but keeping
                // them lets future per-subcommand `help <topic>`
                // routing work without another touch here.
                for extra in &args[3..] {
                    forwarded.push(extra.clone());
                }
                run_main(forwarded);
                return;
            }
            print!("{}", usage_text());
            process::exit(0);
        }
        name => match COMMANDS.iter().find(|(command, _)| *command == name) {
            Some((_, dispatch)) => dispatch(&args),
            // A file is run by `silt run`, with what follows it.
            None if name.ends_with(".silt") => help::file_is_no_command(name, &args[1..]),
            None => {
                eprintln!(
                    "error: silt: unknown command '{}'",
                    crate::git::escape_for_display(name)
                );
                if let Some(suggestion) = suggest_subcommand(name) {
                    eprintln!("  Did you mean '{suggestion}'?");
                }
                eprintln!("Run 'silt' with no arguments to see available commands.");
                process::exit(1);
            }
        },
    }
}

/// The commands of `silt`, each with what runs it: the one list of them.
/// `run_main` dispatches by it, the "did you mean" hint of an unknown
/// command searches it, and the usage text has a row for each of them
/// and for no other (a test of `help` holds that). The order is the
/// hint's: of two commands as close to a typo, the first is named.
pub(crate) const COMMANDS: &[(&str, fn(&[String]))] = &[
    ("run", run::dispatch),
    ("disasm", disasm::dispatch),
    ("test", test::dispatch),
    ("check", check::dispatch),
    ("lsp", lsp::dispatch),
    ("repl", repl::dispatch),
    ("fmt", fmt::dispatch),
    ("init", init::dispatch),
    ("self-update", self_update::dispatch),
    // Regenerates the lock of the package's dependencies.
    ("update", update::dispatch),
    // `silt add <name> --path <path>` or `silt add <name> --git <url>
    // (--rev|--branch|--tag) <ref>`: a dependency into `silt.toml`, and
    // `silt.lock` regenerated.
    ("add", add::dispatch),
    // A dependency out of `silt.toml`, and `silt.lock` regenerated.
    ("remove", remove::dispatch),
];

/// Find the closest valid token to `typo` within edit distance 2.
/// Returns `None` if no candidate is within that distance — in which
/// case the caller should suppress the suggestion line entirely rather
/// than offer a wildly unrelated command.
///
/// If `typo` starts with `-` we search the recognized global flag
/// list (`--version`, `--help`, …); otherwise we search the
/// subcommand list. Mixing the two pools would let `silt run` get
/// suggested for `silt -ru` (edit distance 2 to `--run`-shaped flags),
/// which is more confusing than helpful.
fn suggest_subcommand(typo: &str) -> Option<&'static str> {
    let commands: Vec<&'static str> = COMMANDS.iter().map(|(name, _)| *name).collect();
    let pool: &[&'static str] = if typo.starts_with('-') {
        GLOBAL_FLAGS
    } else {
        &commands
    };
    let mut best: Option<(&'static str, usize)> = None;
    for &candidate in pool {
        let d = crate::text::levenshtein(typo, candidate);
        if d <= 2 {
            match best {
                Some((_, bd)) if d >= bd => {}
                _ => best = Some((candidate, d)),
            }
        }
    }
    best.map(|(name, _)| name)
}

/// The usage error of a flag given twice to `silt <command>`: exit 1.
pub(crate) fn once_only(command: &str, flag: &str) -> ! {
    let command = match command {
        "" => "silt".to_string(),
        command => format!("silt {command}"),
    };
    eprintln!("error: {command}: {flag} was specified more than once");
    std::process::exit(1);
}

/// The first line of the usage error of `silt <command>` given no file
/// outside a package, where there is no `src/main.silt` to take.
pub(crate) fn no_file(command: &str) {
    eprintln!(
        "error: silt {command}: no file given, and no silt.toml found in this directory or any parent"
    );
}

/// `silt <command>` refuses to start where the environment sets the
/// runtime's settings (`SILT_IO_TIMEOUT`, `SILT_TIME_SLICE`) to a value
/// silt cannot read: exit 1, as for a bad flag.
pub(crate) fn refuse_bad_settings(command: &str) {
    if let Some(error) = crate::scheduler::settings_error() {
        eprintln!("error: silt {command}: {error}");
        std::process::exit(1);
    }
}
