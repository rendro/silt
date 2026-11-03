//! Filesystem path helpers used across several CLI subcommands:
//! recursive .silt discovery for `silt fmt`, the session of an entry
//! file and how its files are named in diagnostics, and the
//! path-relative helper that `silt add` uses when recording dependency
//! paths in `silt.toml`. (Lexical `.`/`..` normalization is NOT defined here:
//! `silt add` delegates to `silt::lockfile::normalize_path`, the
//! single definition, so the manifest form and lockfile resolution
//! can never normalize differently.)

use std::path::Path;

use silt::file_discovery::files_under;

use silt::package_graph::LockChange;
use silt::session::{Config, LockPolicy, ProjectSetup, Session};
use silt::source::FileId;

use crate::cli::package::{PackageFailure, die_on_manifest_error};

/// Recursively find all .silt files in a directory, as `silt fmt` does:
/// in the directories [`enters_package_dir`] lets the walk into.
pub(crate) fn find_silt_files(dir: &Path) -> Vec<String> {
    let wanted = |path: &Path| path.to_string_lossy().ends_with(".silt");
    let names = files_under(dir, &enters_package_dir, &wanted).into_iter();
    let mut results: Vec<String> = names
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    results.sort();
    results
}

/// Whether a walk of a package's files goes into the directory `path`
/// below its start: not into one of the skip list
/// (`file_discovery::should_skip_dir`: `target/`, `.git/`,
/// `node_modules/`, `fuzz/corpus/...`, generated or vendored files), and
/// not into one that holds a `silt.toml` of its own: another package (a
/// path dependency kept inside this one), whose files are its own.
/// `silt fmt`, `silt test` and `silt check` walk alike.
fn enters_package_dir(path: &Path) -> bool {
    !silt::file_discovery::should_skip_dir(path) && !path.join("silt.toml").is_file()
}

/// The test files under `root` (`*_test.silt`, `*.test.silt`), sorted,
/// as `silt test` and `silt check` find them, in the directories
/// [`enters_package_dir`] lets the walk into.
pub(crate) fn package_test_files(root: &Path) -> Vec<String> {
    let wanted = |path: &Path| {
        let name = path.to_string_lossy();
        name.ends_with("_test.silt") || name.ends_with(".test.silt")
    };
    let names = files_under(root, &enters_package_dir, &wanted).into_iter();
    let mut out: Vec<String> = names
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// A session for the entry file `path`, with the file opened in it. The
/// project is found from the file's directory. When the packages are
/// resolved, a rewritten `silt.lock` is announced; package errors are
/// printed and the process exits. `Err` when the file cannot be read.
pub(crate) fn open_entry(path: &str, lock: LockPolicy) -> std::io::Result<(Session, FileId)> {
    let dir = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let mut session = Session::new(Config {
        project: ProjectSetup::Discover(dir),
        lock,
        host: Vec::new(),
    });
    let file = session.open(Path::new(path))?;
    let failure = match session.packages() {
        Ok(packages) => {
            match packages.lock {
                LockChange::Updated => {
                    eprintln!("Updating silt.lock for the dependencies in silt.toml")
                }
                LockChange::Rewritten => eprintln!("Updating silt.lock"),
                LockChange::Created | LockChange::Unchanged => {}
            }
            None
        }
        Err(diagnostics) => Some(diagnostics.to_vec()),
    };
    if let Some(diagnostics) = failure {
        die_on_manifest_error(PackageFailure {
            sources: session.into_sources(),
            diagnostics,
        });
    }
    Ok((session, file))
}

/// [`open_entry`], where a file that cannot be read is reported and the
/// process exits.
pub(crate) fn open_entry_or_exit(path: &str, lock: LockPolicy) -> (Session, FileId) {
    match open_entry(path, lock) {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!(
                "error reading {path}: {}",
                silt::diagnostic::io_error_text(&e)
            );
            std::process::exit(1);
        }
    }
}

/// Every diagnostic a door shows for the entry file `file`, in the order
/// it prints them: the analysis's, then what compiling found (its errors,
/// or the compiler's warnings). `compiled` is what
/// [`Session::compile`] returned for it.
pub(crate) fn door_diagnostics(
    session: &mut Session,
    file: FileId,
    compiled: &Result<silt::session::Program, Vec<silt::diagnostic::Diagnostic>>,
) -> Vec<silt::diagnostic::Diagnostic> {
    let analysis = session.analyze(file);
    let analysed_with_errors = analysis.has_errors();
    let mut diagnostics = analysis.diagnostics.clone();
    match compiled {
        Ok(_) => {}
        // A program whose analysis has errors is not compiled: `compile`
        // hands back the analysis's errors, which are listed already.
        Err(_) if analysed_with_errors => {}
        Err(errors) => diagnostics.extend(errors.iter().cloned()),
    }
    diagnostics
}

/// How the files of a program are named in its diagnostics, static and
/// runtime alike: relative to the working directory (with `..` above
/// it), the entry file as the user typed it when that was a relative
/// path. So the header, the `-->` line and every call-stack frame agree,
/// whichever way the entry was named.
pub(crate) struct ProgramFiles<'a> {
    /// The entry file, as the user typed it.
    path: &'a str,
    sources: &'a silt::source::SourceMap,
}

impl<'a> ProgramFiles<'a> {
    pub(crate) fn new(path: &'a str, sources: &'a silt::source::SourceMap) -> Self {
        ProgramFiles { path, sources }
    }

    /// The name of the file `file`.
    fn name(&self, file: &silt::source::SourceFile) -> String {
        use silt::source::SourceName;
        match &file.path {
            SourceName::Path(p) if p == Path::new(self.path) && p.is_relative() => {
                self.path.to_string()
            }
            SourceName::Path(p) => silt::manifest::display_path(p).display().to_string(),
            other => silt::diagnostic::source_name_for_display(other).unwrap_or_default(),
        }
    }
}

impl silt::diagnostic::SourceView for ProgramFiles<'_> {
    fn locate(&self, span: silt::source::Span) -> Option<silt::diagnostic::Located> {
        let file = self.sources.get(span.file)?;
        Some(silt::diagnostic::Located {
            file: self.name(file),
            position: self.sources.position(span),
        })
    }

    /// A frame in code silt adds itself is put in the entry file.
    fn frame(&self, span: silt::source::Span) -> String {
        match self.locate(span) {
            Some(located) => {
                let p = located.position.expect("a file of the map has positions");
                format!("{}:{}:{}", located.file, p.line, p.col)
            }
            None => format!(
                "{}:<unknown location>",
                silt::manifest::display_path(Path::new(self.path)).display()
            ),
        }
    }
}
