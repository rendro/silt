//! Filesystem path helpers used across several CLI subcommands:
//! recursive .silt discovery for `silt fmt`, the session of an entry
//! file and how its files are named in diagnostics, and the
//! path-relative helper that `silt add` uses when recording dependency
//! paths in `silt.toml`. (Lexical `.`/`..` normalization is NOT defined here:
//! `silt add` delegates to `silt::lockfile::normalize_path`, the
//! single definition, so the manifest form and lockfile resolution
//! can never normalize differently.)

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use silt::package_graph::LockChange;
use silt::session::{Config, LockPolicy, ProjectSetup, Session};
use silt::source::FileId;

use crate::cli::package::{PackageFailure, die_on_manifest_error};

/// Recursively find all .silt files in a directory, as `silt fmt` does:
/// in the directories [`enters_package_dir`] lets the walk into.
pub(crate) fn find_silt_files(dir: &Path) -> Vec<String> {
    let names = files_under(dir, &enters_package_dir).into_iter();
    let mut results: Vec<String> = names
        .map(|path| path.to_string_lossy().to_string())
        .filter(|name| name.ends_with(".silt"))
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

/// The files under `root`, in the directories `enter` lets the walk
/// into. A directory is entered once, by the first path that reaches
/// it (in the order of names): a link back to a directory the walk has
/// entered (a loop) or a second way into one is not followed, so each
/// file is named at its own depth.
fn files_under(root: &Path, enter: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    fn walk(
        dir: &Path,
        enter: &dyn Fn(&Path) -> bool,
        entered: &mut HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if !path.is_dir() {
                out.push(path);
            } else if enter(&path) && entered.insert(canonical(&path)) {
                walk(&path, enter, entered, out);
            }
        }
    }
    fn canonical(dir: &Path) -> PathBuf {
        fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
    }
    let mut entered = HashSet::from([canonical(root)]);
    let mut out = Vec::new();
    walk(root, enter, &mut entered, &mut out);
    out
}

/// The test files under `root` (`*_test.silt`, `*.test.silt`), sorted,
/// as `silt test` and `silt check` find them, in the directories
/// [`enters_package_dir`] lets the walk into.
pub(crate) fn package_test_files(root: &Path) -> Vec<String> {
    let names = files_under(root, &enters_package_dir).into_iter();
    let mut out: Vec<String> = names
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|name| name.ends_with("_test.silt") || name.ends_with(".test.silt"))
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
/// runtime alike: the entry file as the user typed it, every other file
/// (an imported module, a dependency) in the style of the path the user
/// typed (see [`display_path_for`]), so the header, the `-->` line and
/// every call-stack frame agree.
pub(crate) struct ProgramFiles<'a> {
    /// The entry file, as the user typed it.
    path: &'a str,
    sources: &'a silt::source::SourceMap,
    user_path_is_absolute: bool,
    cwd: Option<PathBuf>,
}

impl<'a> ProgramFiles<'a> {
    pub(crate) fn new(path: &'a str, sources: &'a silt::source::SourceMap) -> Self {
        ProgramFiles {
            path,
            sources,
            user_path_is_absolute: Path::new(path).is_absolute(),
            cwd: std::env::current_dir().ok(),
        }
    }

    /// The name of the file `file`.
    fn name(&self, file: &silt::source::SourceFile) -> String {
        use silt::source::SourceName;
        match &file.path {
            SourceName::Path(p) if p != Path::new(self.path) => {
                display_path_for(self.user_path_is_absolute, self.cwd.as_deref(), p)
            }
            SourceName::Path(_) => self.path.to_string(),
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
            None => format!("{}:<unknown location>", self.path),
        }
    }
}

/// Render an error/frame path in the same style the user typed on the
/// command line (audit rounds 17/21 policy, F13 + G1/G2):
///
/// - user typed an **absolute** path → render `candidate` absolute,
///   joining relative candidates onto `cwd` when available;
/// - user typed a **relative** path → render `candidate` relative by
///   stripping `cwd` when it is a prefix, otherwise fall back to the
///   candidate as-is (e.g. `strip_prefix` failure under a symlinked
///   cwd);
/// - no `cwd` available (rare: deleted working dir) → candidate as-is.
///
/// This body used to be a byte-identical 21-line
/// closure duplicated between `silt run` (src/cli/run.rs) and
/// `silt test` (src/cli/test.rs) error rendering — an
/// acknowledged-mirror drift hazard. Hoisted here so both subcommands
/// share one implementation.
///
/// Locks: unit tests below (`display_path_for_*`), source-grep lock
/// tests/meta/round101_display_path_helper_lock_tests.rs, plus the
/// behavioral rendering locks in tests/cli/cli_test_rendering_tests.rs
/// (`test_run_module_error_paths_consistently_normalized`,
/// `test_test_setup_error_paths_normalized`,
/// `test_cross_module_call_stack_uses_consistent_path_style`).
pub(crate) fn display_path_for(
    user_path_is_absolute: bool,
    cwd: Option<&Path>,
    candidate: &Path,
) -> String {
    if user_path_is_absolute {
        if candidate.is_absolute() {
            without_verbatim_prefix(candidate)
        } else if let Some(cwd) = cwd {
            without_verbatim_prefix(&cwd.join(candidate))
        } else {
            without_verbatim_prefix(candidate)
        }
    } else if let Some(cwd) = cwd {
        if let Ok(rel) = candidate.strip_prefix(cwd) {
            return rel.display().to_string();
        }
        // Module paths are canonicalized upstream; on Windows that gives
        // the extended-length form (`\\?\C:\...`) while `cwd` is `C:\...`,
        // so the literal strip misses. Canonicalizing both sides makes
        // them comparable; any failure falls through to the raw path.
        if let (Ok(candidate_canon), Ok(cwd_canon)) =
            (std::fs::canonicalize(candidate), std::fs::canonicalize(cwd))
            && let Ok(rel) = candidate_canon.strip_prefix(&cwd_canon)
        {
            return rel.display().to_string();
        }
        without_verbatim_prefix(candidate)
    } else {
        without_verbatim_prefix(candidate)
    }
}

/// `path` for display, without the Windows extended-length prefix
/// `\\?\` that `canonicalize` adds; unchanged elsewhere.
fn without_verbatim_prefix(path: &Path) -> String {
    silt::source::without_verbatim_prefix(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Direct behavioral locks for the shared path-display
    // policy that `silt run` / `silt test` error rendering delegate to.
    // These pin the exact semantics the previously-duplicated closures
    // implemented, so a future edit to the helper that changes any of
    // the three branches trips here without needing a full CLI run.

    /// Relative user path + cwd is a prefix of the candidate → the
    /// cwd prefix is stripped and the path renders relative.
    #[test]
    fn display_path_for_relative_user_path_strips_cwd_prefix() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("/work/proj/src/helper.silt");
        assert_eq!(
            display_path_for(false, Some(cwd), candidate),
            "src/helper.silt"
        );
    }

    /// Relative user path but strip_prefix fails (candidate outside
    /// cwd, e.g. symlinked working directory) → candidate rendered
    /// as-is rather than mangled.
    #[test]
    fn display_path_for_relative_user_path_strip_prefix_failure_falls_back() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("/elsewhere/helper.silt");
        assert_eq!(
            display_path_for(false, Some(cwd), candidate),
            "/elsewhere/helper.silt"
        );
    }

    /// Absolute user path + relative candidate → candidate is joined
    /// onto cwd so everything renders absolute, matching what the user
    /// typed.
    #[test]
    fn display_path_for_absolute_user_path_absolutizes_relative_candidate() {
        let cwd = Path::new("/work/proj");
        let candidate = Path::new("src/helper.silt");
        // `join` inserts the platform separator between cwd and the
        // candidate; the candidate's own `/` separators are preserved
        // verbatim by Display.
        let expected = format!("/work/proj{}src/helper.silt", std::path::MAIN_SEPARATOR);
        assert_eq!(display_path_for(true, Some(cwd), candidate), expected);
    }

    /// Absolute user path + already-absolute candidate → unchanged.
    #[test]
    fn display_path_for_absolute_user_path_keeps_absolute_candidate() {
        let candidate = Path::new("/work/proj/src/helper.silt");
        assert_eq!(
            display_path_for(true, Some(Path::new("/work/proj")), candidate),
            "/work/proj/src/helper.silt"
        );
    }

    /// No cwd available (deleted working directory) → candidate
    /// rendered as-is in both user-path styles.
    #[test]
    fn display_path_for_no_cwd_renders_candidate_as_is() {
        let candidate = Path::new("src/helper.silt");
        assert_eq!(display_path_for(false, None, candidate), "src/helper.silt");
        assert_eq!(display_path_for(true, None, candidate), "src/helper.silt");
    }
}
