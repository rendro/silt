//! Round 91 lock: watch mode must not write a raw ANSI clear-screen
//! escape to stderr unconditionally.
//!
//! Background: `src/watch.rs` previously emitted `eprint!("\x1B[2J\x1B[H")`
//! at three (re)run sites with no TTY guard. Running
//! `silt run app.silt --watch 2> watch.log` wrote the literal escape
//! bytes (`^[[2J^[[H`) into the redirected log on every run. The rest of
//! silt gates terminal control on `std::io::IsTerminal::is_terminal(&stderr())`
//! (see `src/errors.rs`). The fix extracts a `clear_screen_seq()` helper
//! that returns `""` when stderr is not a terminal (or `NO_COLOR` is set),
//! and routes every site through `eprint!("{}", clear_screen_seq())`.
//!
//! This is a SOURCE-GREP lock: it reads the watch.rs source at compile
//! time and asserts the unguarded escape is gone and the helper is wired
//! in. It fails before the fix and passes after.

const WATCH_SRC: &str = include_str!("../src/watch.rs");

/// The raw escape must never appear directly inside an *unguarded*
/// `eprint!` call. The only place the literal sequence is allowed to
/// survive is inside the `clear_screen_seq` helper, which gates it on
/// `IsTerminal`. So we assert no `eprint!("\x1B[2J\x1B[H")` substring
/// exists anywhere in the file.
#[test]
fn no_unguarded_clear_screen_eprint() {
    // The exact unguarded call shape that the audit flagged. Using the
    // real escape byte (0x1B) so this matches the literal source bytes
    // `eprint!("\x1B[2J\x1B[H")` would compile to — but the source stores
    // the escaped form `\x1B`, so match that textual form instead.
    let forbidden = "eprint!(\"\\x1B[2J\\x1B[H\")";
    assert!(
        !WATCH_SRC.contains(forbidden),
        "src/watch.rs still contains an unguarded clear-screen eprint!: {forbidden}. \
         Route it through clear_screen_seq() so it honors IsTerminal."
    );
}

/// The fix must wire the gated helper in. Assert the helper is defined
/// and that each (re)run site emits it via `eprint!("{}", clear_screen_seq())`.
#[test]
fn clear_screen_helper_is_wired_in() {
    assert!(
        WATCH_SRC.contains("fn clear_screen_seq()"),
        "expected a clear_screen_seq() helper in src/watch.rs that gates the \
         escape on IsTerminal"
    );
    assert!(
        WATCH_SRC.contains("IsTerminal::is_terminal"),
        "clear_screen_seq() must gate on std::io::IsTerminal::is_terminal"
    );
    assert!(
        WATCH_SRC.contains("eprint!(\"{}\", clear_screen_seq())"),
        "(re)run sites must emit the clear-screen sequence via \
         eprint!(\"{{}}\", clear_screen_seq())"
    );
}
