/// Module system utilities.

/// Known builtin module names whose functions are registered as `module.func`
/// in the global environment rather than loaded from files.
const BUILTIN_MODULES: &[&str] = &[
    "io", "string", "int", "float", "list", "map", "result", "option", "test", "channel", "task",
    "regex", "json",
];

/// Returns true if `name` is a builtin module (io, string, int, etc.).
pub fn is_builtin_module(name: &str) -> bool {
    BUILTIN_MODULES.contains(&name)
}
