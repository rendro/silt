//! A deterministic reproduction of the language server's "a definition of
//! a module that is entered" panic (src/defs.rs:288), at the session:
//! no server, no timing. Written by the stage 8 lane for the lane that
//! owns the method table (src/typechecker/tables.rs, declare_traits.rs).

use silt::session::testing::{session_with, test_path};

const A: &str = "pub type T { T(Int) }\npub trait Ta { fn m(self) -> String }\n\
                 trait Ta for T { fn m(self) -> String { \"a\" } }\n\
                 pub fn via_a(t: T) -> String { t.m() }\n";
const B: &str = "import a\nimport a.{ T }\npub trait Tb { fn m(self) -> String }\n\
                 trait Tb for T { fn m(self) -> String { \"b\" } }\n\
                 pub fn via_b(t: T) -> String { t.m() }\n";
const C: &str = "import a\nimport a.{ T, Ta }\nfn main() {\n  println(T(1).m())\n  \
                 println(a.via_a(T(2)))\n}\n";

/// `b` gives `T` a second method `m`, by its own trait; then `b` drops
/// the trait and its impl. `c`, which imports `a` only, means `a`'s `m`
/// before and after, whichever of `b` and `c` is checked first.
fn c_after_b_drops_its_trait(b_first: bool) -> Vec<String> {
    let (mut session, c) = session_with(&[("c.silt", C), ("a.silt", A), ("b.silt", B)]);
    let b = session.open(&test_path("b.silt")).expect("b.silt");
    assert!(session.analyze(c).diagnostics.is_empty());
    session.analyze(b);
    let b = session.set_overlay(&test_path("b.silt"), "import a\n".to_string());
    let c = session.set_overlay(&test_path("c.silt"), format!("{C}\n"));
    if b_first {
        session.analyze(b);
    }
    session
        .analyze(c)
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

/// `b` is checked again before `c`: its trait `Tb` is no definition any
/// more, and the row it left in the method table names it. (Panics on
/// main b22fba2d: "a definition of a module that is entered".)
#[test]
fn a_dropped_trait_checked_first_leaves_no_row() {
    assert_eq!(c_after_b_drops_its_trait(true), Vec::<String>::new());
}

/// `c` is checked before `b` is: the row still names `Tb`, which `c`
/// does not reach, and `a`'s `m` is lost. (On main b22fba2d: "unknown
/// field or method 'm' on type T".)
#[test]
fn a_dropped_trait_not_yet_checked_leaves_no_row() {
    assert_eq!(c_after_b_drops_its_trait(false), Vec::<String>::new());
}
