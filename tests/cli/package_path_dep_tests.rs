//! API contract of `Compiler::with_package_roots`, the compiler entry
//! point that receives the resolved set of packages (local + transitive
//! deps) and the symbol naming the local package.
//!
//! The cross-package import behaviour (path deps, transitive deps,
//! cycles, missing or entry-point-less deps, privacy, local sub-modules)
//! is covered by the golden directory cases under
//! `tests/golden/cli/packages/`, which run real packages through the CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use silt::compiler::Compiler;
use silt::intern::{self, Symbol};

/// A tiny direct test that `with_package_roots` panics on a
/// programming error (local symbol not in the map). Locks the API
/// contract so the CLI can rely on it.
#[test]
#[should_panic(expected = "local_package symbol must appear in package_roots")]
fn test_with_package_roots_requires_local_in_map() {
    let mut roots: HashMap<Symbol, PathBuf> = HashMap::new();
    roots.insert(intern::intern("other"), Path::new("/tmp").to_path_buf());
    let _ = Compiler::with_package_roots(intern::intern("missing"), roots);
}
