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

/// What a path given to `silt run`, `check`, `test`, `fmt` or `disasm`
/// names. One rule for the five: a `.silt` file, or a directory; a
/// directory that holds a `silt.toml` is the directory of that package.
/// Any other file is no program ([`PathArg::NotSilt`]). A path that
/// names nothing is taken for a file: reading it says it is not there
/// (a misspelt name, most likely).
pub(crate) enum PathArg {
    /// A `.silt` file, or a path that names nothing.
    File(String),
    /// The directory of a package: it holds a `silt.toml`.
    Package(String),
    /// Any other directory.
    Dir(String),
    /// A file that is not a `.silt` file (`prog.txt`, `silt.toml`).
    NotSilt(String),
}

/// What the path `arg` names: see [`PathArg`].
pub(crate) fn path_arg(arg: &str) -> PathArg {
    let path = Path::new(arg);
    if path.is_dir() {
        match path.join("silt.toml").is_file() {
            true => PathArg::Package(arg.to_string()),
            false => PathArg::Dir(arg.to_string()),
        }
    } else if arg.ends_with(".silt") || !path.exists() {
        PathArg::File(arg.to_string())
    } else {
        PathArg::NotSilt(arg.to_string())
    }
}

/// The usage error of `silt <command>` given the path `arg`, which is
/// not what it takes (`takes`: "a .silt file or a package directory"):
/// exit 1.
pub(crate) fn refuse_path(command: &str, arg: &str, takes: &str) -> ! {
    eprintln!(
        "error: silt {command}: {} is not {takes}",
        silt::git::escape_for_display(arg)
    );
    std::process::exit(1);
}

/// What `silt run` and `silt disasm` take: they need one program.
pub(crate) const FILE_OR_PACKAGE: &str = "a .silt file or a package directory";

/// What `silt check`, `test` and `fmt` take: a directory that is no
/// package stands for the files under it.
pub(crate) const FILE_OR_DIRECTORY: &str = "a .silt file or a directory";

/// The usage error of `silt <command>` (`check` or `fmt`) when the walk
/// of `dir` (`shown`: how it is named) found no `.silt` file: exit 1.
/// Where the files under it are those of packages below it, which the
/// walk leaves to their packages, the message names one (a package that
/// holds no `.silt` file either is not named).
pub(crate) fn refuse_empty_walk(command: &str, dir: &Path, shown: &str) -> ! {
    let enter = |path: &Path| !silt::file_discovery::should_skip_dir(path);
    let manifest = |path: &Path| path.file_name().is_some_and(|name| name == "silt.toml");
    let silt_file = |path: &Path| path.to_string_lossy().ends_with(".silt");
    let packages: Vec<String> = files_under(dir, &enter, &manifest)
        .iter()
        .filter_map(|toml| toml.parent())
        .filter(|package| *package != dir)
        .filter(|package| !files_under(package, &enter, &silt_file).is_empty())
        .map(|package| {
            // `./pk` (`.\pk` on Windows) is shown as `pk`.
            let package = package.strip_prefix(".").unwrap_or(package);
            silt::git::escape_for_display(&package.to_string_lossy())
        })
        .collect();
    let verb = if command == "fmt" { "format" } else { command };
    let shown = silt::git::escape_for_display(shown);
    match packages.as_slice() {
        [] => eprintln!("error: silt {command}: no .silt files found in {shown}"),
        [one] => eprintln!(
            "error: silt {command}: no .silt files found in {shown}, but for those of the \
             package in {one}: name its directory to {verb} it"
        ),
        [first, ..] => eprintln!(
            "error: silt {command}: no .silt files found in {shown}, but for those of {} \
             packages, such as the one in {first}: name a package's directory to {verb} it",
            packages.len()
        ),
    }
    std::process::exit(1);
}

/// Recursively find all .silt files in a directory, as `silt fmt` does:
/// in the directories [`enters_package_dir`] lets the walk into.
pub(crate) fn find_silt_files(dir: &Path) -> Vec<String> {
    let wanted = |path: &Path| path.to_string_lossy().ends_with(".silt");
    let names = files_under(dir, &enters_package_dir, &wanted).into_iter();
    let mut results: Vec<String> = names.map(unicode_path).collect();
    results.sort();
    results
}

/// The path of a file a walk found, as text. A file whose path is not
/// Unicode (its own name, or a directory's on the way) cannot be named to
/// read it, nor is its name a module name: as for such an argument, a
/// usage error (exit 1).
pub(crate) fn unicode_path(path: std::path::PathBuf) -> String {
    match path.into_os_string().into_string() {
        Ok(path) => path,
        Err(path) => {
            eprintln!(
                "error: silt: the path {path:?} is not valid Unicode: rename the file or \
                 directory whose name is not"
            );
            std::process::exit(1);
        }
    }
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

/// The test files under `root` (`*_test.silt`), sorted,
/// as `silt test` and `silt check` find them, in the directories
/// [`enters_package_dir`] lets the walk into.
pub(crate) fn package_test_files(root: &Path) -> Vec<String> {
    let wanted = |path: &Path| {
        let name = path.to_string_lossy();
        name.ends_with("_test.silt")
    };
    let names = files_under(root, &enters_package_dir, &wanted).into_iter();
    let mut out: Vec<String> = names.map(unicode_path).collect();
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
    Ok(with_packages(session, file))
}

/// How a program read from stdin is named: `silt check -` and `silt fmt -`.
pub(crate) const STDIN_NAME: &str = "<stdin>";

/// [`open_entry`] for the program read from stdin, named [`STDIN_NAME`]:
/// a file of the working directory, whose project is found from there.
/// `Err` when stdin cannot be read (or is not UTF-8).
pub(crate) fn open_stdin(lock: LockPolicy) -> std::io::Result<(Session, FileId)> {
    let text = std::io::read_to_string(std::io::stdin())?;
    let mut session = Session::new(Config {
        project: ProjectSetup::Discover(std::env::current_dir().unwrap_or_else(|_| ".".into())),
        lock,
        host: Vec::new(),
    });
    let file = session.set_overlay(Path::new(STDIN_NAME), text);
    Ok(with_packages(session, file))
}

/// The session of an entry file `file` once its packages are resolved:
/// a rewritten `silt.lock` is announced; package errors are printed and
/// the process exits.
fn with_packages(mut session: Session, file: FileId) -> (Session, FileId) {
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
    (session, file)
}

/// [`open_entry`], where a file that cannot be read is reported and the
/// process exits.
pub(crate) fn open_entry_or_exit(path: &str, lock: LockPolicy) -> (Session, FileId) {
    match open_entry(path, lock) {
        Ok(opened) => opened,
        Err(e) => {
            // Windows refuses to open a directory as "access denied":
            // say what it is, as other systems do.
            let why = if std::path::Path::new(path).is_dir() {
                "it is a directory".to_string()
            } else {
                silt::diagnostic::io_error_text(&e)
            };
            eprintln!("error reading {path}: {why}");
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

/// `paths` with each file once: two paths of one file (two spellings,
/// or a link and what it points to) are the first of them.
pub(crate) fn once_each(mut paths: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| seen.insert(silt::source::canonical_path(Path::new(p))));
    paths
}

/// The files of a source map named by their canonical paths: what a
/// diagnostic is rendered with to tell whether it was shown already,
/// whichever spelling of a file's path the program it was found in was
/// given.
pub(crate) struct CanonicalFiles<'a>(pub(crate) &'a silt::source::SourceMap);

impl silt::diagnostic::SourceView for CanonicalFiles<'_> {
    fn locate(&self, span: silt::source::Span) -> Option<silt::diagnostic::Located> {
        use silt::source::SourceName;
        let file = self.0.get(span.file)?;
        let name = match &file.path {
            SourceName::Path(p) => silt::source::canonical_path(p).display().to_string(),
            other => silt::diagnostic::source_name_for_display(other).unwrap_or_default(),
        };
        Some(silt::diagnostic::Located {
            file: name,
            position: self.0.position(span),
        })
    }
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
