//! `silt disasm [<file>]` — show bytecode disassembly without running.

use std::process;

use silt::disassemble::disassemble_function;

use crate::cli::help::disasm_usage_banner;
use crate::cli::package::resolve_package_entry_point;
use crate::cli::pipeline::compile_file_with_options;

/// Dispatch `silt disasm [<file>]`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
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
                eprintln!("Usage: {}", disasm_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        }
    } else {
        args[2].clone()
    };
    disasm_file(&path);
}

/// Disassemble a file's bytecode without running it.
pub(crate) fn disasm_file(path: &str) {
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
