//! Formatting a program does not change the order its modules are
//! initialised in.
//!
//! `silt fmt` sorts a file's imports, and a file's imports are compiled
//! in that same order whatever order they are written in
//! (`silt::ast::import_order` is the one function for both). So for
//! every directory case of the corpus that has more than one `.silt`
//! file, in a copy of it, `silt disasm` of the entry file gives the same
//! sequence of `== <module:...> ==` headers before and after `silt fmt`
//! of all its files.
//!
//! What the sequence is: the order in which the module initialisers are
//! compiled. That fixes the order they run in, because an initialiser
//! is called where it is compiled; the disassembler lists a function
//! before the functions nested in it, so the sequence is one walk of
//! the tree of initialisers and not the run order itself. Two equal
//! sequences mean the same order. The run order itself is held by the
//! golden cases `lang/modules/init_order__three_imports_written_*`,
//! whose modules print while they are initialised: six orders of the
//! import lines, one output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The `.silt` files under `dir`, and the directories that hold a
/// `main.silt`.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, entries: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk(&path, files, entries);
        } else if path.extension().is_some_and(|ext| ext == "silt") {
            if path.file_name().is_some_and(|name| name == "main.silt") {
                entries.push(path.clone());
            }
            files.push(path);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the case").flatten() {
        let (path, target) = (entry.path(), to.join(entry.file_name()));
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy a file of the case");
        }
    }
}

/// The module headers of `silt disasm entry` in `root`, in order; `None`
/// if the program does not compile.
fn module_headers(root: &Path, entry: &Path) -> Option<Vec<String>> {
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("disasm")
        .arg(entry)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run silt disasm");
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Some(
        listing
            .lines()
            .filter(|line| line.starts_with("== <module:"))
            .map(str::to_string)
            .collect(),
    )
}

/// The import lines of the `.silt` files under `root`, file by file.
fn import_lines(root: &Path) -> Vec<Vec<String>> {
    let (mut files, mut entries) = (Vec::new(), Vec::new());
    walk(root, &mut files, &mut entries);
    files
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let imports = text.lines().filter(|line| line.starts_with("import "));
            imports.map(str::to_string).collect()
        })
        .collect()
}

#[test]
fn formatting_a_package_keeps_the_order_of_its_module_initialisers() {
    let scratch = std::env::temp_dir().join(format!("silt_init_order_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let (mut files, mut entries) = (Vec::new(), Vec::new());
    for corpus in ["tests/golden", "examples", "tests/lsp"] {
        walk(&repo().join(corpus), &mut files, &mut entries);
    }
    // (cases with modules, of those: cases whose imports `silt fmt` moved)
    let (mut with_modules, mut moved) = (0, 0);
    let mut failures = Vec::new();
    for (n, main) in entries.iter().enumerate() {
        let dir = main.parent().expect("a file is in a directory");
        // A package's entry is `src/main.silt` beside its `silt.toml`.
        let package = dir.parent().filter(|package| {
            dir.file_name().is_some_and(|name| name == "src") && package.join("silt.toml").exists()
        });
        let (root, entry) = match package {
            Some(package) => (package, Path::new("src").join("main.silt")),
            None => (dir, PathBuf::from("main.silt")),
        };
        let (mut of_case, mut nested) = (Vec::new(), Vec::new());
        walk(root, &mut of_case, &mut nested);
        if of_case.len() < 2 {
            continue;
        }
        let copy = scratch.join(n.to_string());
        copy_dir(root, &copy);
        let Some(before) = module_headers(&copy, &entry) else {
            continue;
        };
        let imports_before = import_lines(&copy);
        // (A file that does not parse is left as it is, and so is the
        // order of its imports.)
        let _ = Command::new(env!("CARGO_BIN_EXE_silt"))
            .args(["fmt", "."])
            .current_dir(&copy)
            .env("NO_COLOR", "1")
            .output()
            .expect("run silt fmt");
        let after = module_headers(&copy, &entry);
        if !before.is_empty() {
            with_modules += 1;
            moved += usize::from(import_lines(&copy) != imports_before);
        }
        if after.as_ref() != Some(&before) {
            failures.push(format!(
                "{}: before `silt fmt` {before:?}, after it {after:?}",
                main.strip_prefix(repo()).unwrap_or(main).display()
            ));
        }
        let _ = std::fs::remove_dir_all(&copy);
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!("{with_modules} programs with modules, {moved} whose import lines `silt fmt` moved");
    assert!(
        failures.is_empty(),
        "`silt fmt` changed the order of module initialisers in {} program(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    // The sweep is not empty: the corpus has programs of several
    // modules, and some whose import lines the formatter moves.
    assert!(
        with_modules >= 100 && moved >= 20,
        "{with_modules} programs with modules, {moved} whose import lines `silt fmt` moved"
    );
}
