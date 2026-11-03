//! `silt disasm [<file>]` — show bytecode disassembly without running.

use std::io::Write;
use std::process;

use silt::diagnostic::Diagnostic;
use silt::disassemble::disassemble_function;
use silt::intern::intern;
use silt::session::{ENTRY_POINT, Entry, LockPolicy};

use crate::cli::help::{asks_for_help, disasm_usage_banner};
use crate::cli::package::resolve_package_entry_point;
use crate::cli::paths::{ProgramFiles, door_diagnostics, open_entry_or_exit};

/// Dispatch `silt disasm [<file>]`.
pub(crate) fn dispatch(args: &[String]) {
    if asks_for_help(&args[2..]) {
        println!("Usage: {}", disasm_usage_banner());
        println!();
        println!("Prints the compiled bytecode disassembly for <file.silt>.");
        println!("Inside a package with no file argument, disassembles src/main.silt.");
        println!();
        println!("Options:");
        println!("  --watch, -w     Re-run on file changes");
        println!();
        println!("Example:");
        println!("  silt disasm main.silt");
        process::exit(0);
    }
    // Reject unknown flags before interpreting args as filenames, and
    // every positional after the first.
    let mut positional: Option<String> = None;
    for arg in &args[2..] {
        if arg == "--" {
            // `silt disasm` runs no program, so it takes no program
            // arguments: `--` is an argument too many.
            eprintln!("error: silt disasm: unexpected extra argument '--'");
            eprintln!("Run 'silt disasm --help' for usage.");
            process::exit(1);
        } else if arg.starts_with('-') && arg != "--help" && arg != "-h" {
            eprintln!("error: silt disasm: unknown flag '{arg}'");
            eprintln!("Run 'silt disasm --help' for usage.");
            process::exit(1);
        } else if !arg.starts_with('-') {
            if positional.is_none() {
                positional = Some(arg.clone());
            } else {
                eprintln!("error: silt disasm: unexpected extra argument '{arg}'");
                eprintln!("Run 'silt disasm --help' for usage.");
                process::exit(1);
            }
        }
    }
    let path = match positional {
        Some(p) => p,
        None => match resolve_package_entry_point() {
            Ok(Some(p)) => p.to_string_lossy().into_owned(),
            Ok(None) => {
                crate::cli::no_file("disasm");
                eprintln!("Usage: {}", disasm_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    disasm_file(&path);
}

/// Disassemble a file's bytecode without running it: the program that
/// starts at `main` when the file binds one, otherwise its declarations.
fn disasm_file(path: &str) {
    silt::intern::reset();
    // Read-only command — never writes `silt.lock`. A lock that no
    // longer matches the manifest is a package error ("silt.lock is out
    // of date", exit 1); `silt update` rewrites it.
    let (mut session, file) = open_entry_or_exit(path, LockPolicy::ReadOnly);
    session.analyze(file);
    let binds_main = session
        .module_analysis(session.module_of(file))
        .is_some_and(|analysis| analysis.top_level.contains_key(&intern(ENTRY_POINT)));
    let target = if binds_main { Entry::Main } else { Entry::Cell };
    let compiled = session.compile(file, target);
    let diagnostics = door_diagnostics(&mut session, file, &compiled);
    silt::diagnostic::eprint_all(&ProgramFiles::new(path, session.sources()), &diagnostics);
    let program = match compiled {
        Ok(program) if !diagnostics.iter().any(Diagnostic::is_error) => program,
        _ => process::exit(1),
    };

    // Print disassembly of each function. A closed pipe (`silt disasm
    // x.silt | head`) ends the process quietly, as a death by SIGPIPE.
    let mut out = std::io::stdout().lock();
    for func in &program.functions {
        let text = format!("{}\n", disassemble_function(func, &program.globals));
        if let Err(e) = out.write_all(text.as_bytes()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                process::exit(141);
            }
            eprintln!(
                "error: silt disasm: cannot write to stdout: {}",
                silt::diagnostic::io_error_text(&e)
            );
            process::exit(1);
        }
    }
}
