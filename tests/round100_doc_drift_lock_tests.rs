//! Round-100 doc-drift lock: `docs/getting-started.md` demonstrated
//! shadowing with two BARE TOP-LEVEL `let` statements:
//!
//! ```silt
//! let x = 42
//! let x = x + 1   -- shadows, x is now 43
//! ```
//!
//! That snippet does NOT compile: top-level names must be unique at module
//! scope (`duplicate top-level definition of 'x'`). Shadowing is legal
//! only inside a function/block body. So the documented "x is now 43" was
//! false for the code as written — the snippet errors before it runs. The
//! drift escaped the doc-walker because the fragment is not a complete
//! program.
//!
//! Fix: the example is wrapped in `fn main() { ... }` so it actually
//! compiles and the claim holds. These source-grep locks fail the moment
//! the bare top-level form returns.

const GETTING_STARTED: &str = include_str!("../docs/getting-started.md");

#[test]
fn shadowing_example_is_not_shown_at_top_level() {
    // The bare top-level sequence (newline immediately before `let x = 42`,
    // i.e. no indentation) does not compile and must not reappear.
    assert!(
        !GETTING_STARTED.contains("\nlet x = 42\nlet x = x + 1"),
        "docs/getting-started.md regressed: shadowing is shown with bare \
         top-level `let` statements, which do not compile (top-level names \
         must be unique at module scope). Wrap the example in a function \
         body where shadowing is actually legal."
    );
}

#[test]
fn shadowing_example_is_shown_in_block_scope() {
    // The valid, compiling form: shadowing inside a function body.
    assert!(
        GETTING_STARTED.contains("  let x = x + 1   -- shadows, x is now 43"),
        "docs/getting-started.md should demonstrate shadowing inside a \
         function/block body (indented `let x = x + 1`), where it is legal."
    );
}
