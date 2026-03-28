use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::ast::{Decl, FnDecl, Program, TypeBody, TypeDecl};
use crate::env::Env;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Tracks which names are publicly exported from a module.
#[derive(Debug, Clone)]
pub struct ModuleExports {
    /// The module's evaluated environment (all declarations).
    pub env: Env,
    /// Names that are `pub` and thus importable.
    pub public_names: HashSet<String>,
    /// Maps a type name to its variant constructor names (for pub types with enums).
    pub type_variants: HashMap<String, Vec<String>>,
}

/// Loads and caches file-based modules.
pub struct ModuleLoader {
    /// Directory of the entry file; modules are resolved relative to this.
    project_root: PathBuf,
    /// Cache of already-loaded modules (by module name, e.g. "math").
    loaded: HashMap<String, ModuleExports>,
    /// Modules currently being loaded (for circular import detection).
    loading: HashSet<String>,
}

/// Result of parsing a module: (AST, public names, type->variant mapping).
pub type ParsedModule = (Program, HashSet<String>, HashMap<String, Vec<String>>);

/// Known builtin module names whose functions are registered as `module.func`
/// in the global environment rather than loaded from files.
const BUILTIN_MODULES: &[&str] = &[
    "io", "string", "int", "float", "list", "map", "result", "option", "test", "channel",
];

impl ModuleLoader {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            loaded: HashMap::new(),
            loading: HashSet::new(),
        }
    }

    /// Returns true if `name` is a builtin module (io, string, int, etc.).
    pub fn is_builtin_module(name: &str) -> bool {
        BUILTIN_MODULES.contains(&name)
    }

    /// Load a file-based module by name (e.g. "math" -> "math.silt").
    ///
    /// Returns the module's exports.  If the module has already been loaded
    /// it is returned from cache.
    pub fn load(
        &mut self,
        module_name: &str,
    ) -> Result<ModuleExports, String> {
        // Check cache first
        if let Some(exports) = self.loaded.get(module_name) {
            return Ok(exports.clone());
        }

        // Circular-import guard
        if self.loading.contains(module_name) {
            return Err(format!(
                "circular import detected: module '{module_name}' is already being loaded"
            ));
        }
        self.loading.insert(module_name.to_string());

        // Resolve file path
        let file_path = self.project_root.join(format!("{module_name}.silt"));
        let source = std::fs::read_to_string(&file_path).map_err(|e| {
            format!(
                "cannot load module '{module_name}': {}",
                e
            )
        })?;

        // Lex
        let tokens = Lexer::new(&source)
            .tokenize()
            .map_err(|e| format!("module '{module_name}': lexer error: {e}"))?;

        // Parse
        let program = Parser::new(tokens)
            .parse_program()
            .map_err(|e| format!("module '{module_name}': parse error: {e}"))?;

        // Collect pub names from declarations
        let (public_names, type_variants) = collect_public_names(&program);

        // Remove from loading set before returning
        self.loading.remove(module_name);

        let exports = ModuleExports {
            env: Env::new(), // placeholder; will be replaced after evaluation
            public_names,
            type_variants,
        };

        // Store a placeholder so recursive references don't fail
        // (the real env gets patched in by the interpreter after eval)
        self.loaded.insert(module_name.to_string(), exports);

        // We return the program so the interpreter can evaluate it.
        // The interpreter will call `finish_load` to patch in the real env.
        // For now, return what we have.
        Ok(self.loaded.get(module_name).unwrap().clone())
    }

    /// Parse a module file and return (program, public_names) without caching
    /// the environment yet.  The interpreter calls this, evaluates the program,
    /// then calls `finish_load`.
    pub fn parse_module(
        &mut self,
        module_name: &str,
    ) -> Result<ParsedModule, String> {
        // Circular-import guard
        if self.loading.contains(module_name) {
            return Err(format!(
                "circular import detected: module '{module_name}' is already being loaded"
            ));
        }

        // Check cache — if already loaded we still need to return *something*,
        // but the interpreter can skip evaluation.
        if self.loaded.contains_key(module_name) {
            // Already loaded; the caller should use `get_cached` instead.
            return Err("__already_loaded__".to_string());
        }

        self.loading.insert(module_name.to_string());

        // Resolve file path
        let file_path = self.project_root.join(format!("{module_name}.silt"));
        let source = std::fs::read_to_string(&file_path).map_err(|e| {
            self.loading.remove(module_name);
            format!("cannot load module '{module_name}': {e}")
        })?;

        // Lex
        let tokens = Lexer::new(&source).tokenize().map_err(|e| {
            self.loading.remove(module_name);
            format!("module '{module_name}': lexer error: {e}")
        })?;

        // Parse
        let program = Parser::new(tokens).parse_program().map_err(|e| {
            self.loading.remove(module_name);
            format!("module '{module_name}': parse error: {e}")
        })?;

        let (public_names, type_variants) = collect_public_names(&program);

        Ok((program, public_names, type_variants))
    }

    /// Called by the interpreter after evaluating a module's declarations.
    /// Patches the cached entry with the real environment.
    pub fn finish_load(
        &mut self,
        module_name: &str,
        env: Env,
        public_names: HashSet<String>,
        type_variants: HashMap<String, Vec<String>>,
    ) {
        self.loading.remove(module_name);
        self.loaded.insert(
            module_name.to_string(),
            ModuleExports {
                env,
                public_names,
                type_variants,
            },
        );
    }

    /// Get a cached module by name.
    pub fn get_cached(&self, module_name: &str) -> Option<&ModuleExports> {
        self.loaded.get(module_name)
    }
}

/// Walk the AST and collect names that are marked `pub`, plus variant
/// constructor mappings for pub enum types.
fn collect_public_names(program: &Program) -> (HashSet<String>, HashMap<String, Vec<String>>) {
    let mut names = HashSet::new();
    let mut type_variants: HashMap<String, Vec<String>> = HashMap::new();

    for decl in &program.decls {
        match decl {
            Decl::Fn(FnDecl { name, is_pub: true, .. }) => {
                names.insert(name.clone());
            }
            Decl::Type(TypeDecl { name, is_pub: true, body, .. }) => {
                names.insert(name.clone());
                // For enum types, also mark variant constructors as public
                if let TypeBody::Enum(variants) = body {
                    let variant_names: Vec<String> =
                        variants.iter().map(|v| v.name.clone()).collect();
                    for vn in &variant_names {
                        names.insert(vn.clone());
                    }
                    type_variants.insert(name.clone(), variant_names);
                }
            }
            Decl::Trait(t) => {
                // Trait declarations are always public (they define interfaces)
                names.insert(t.name.clone());
            }
            Decl::TraitImpl(_) => {
                // Trait impls are automatically available (no explicit export needed)
            }
            _ => {}
        }
    }
    (names, type_variants)
}
