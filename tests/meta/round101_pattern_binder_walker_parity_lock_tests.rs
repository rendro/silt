//! Round-101 GAP lock: binders introduced by every binding-capable
//! pattern form of a top-level `let` reach REPL tab completion. The REPL
//! offers the names of the session's scope of an input
//! (`repl::scope_completion_names`), which the resolver builds from the
//! patterns' binders; these tests read it for a program of one `let`.

use silt::session::testing::session_with;

fn completion_names(src: &str) -> Vec<String> {
    let (mut session, file) = session_with(&[("main.silt", src)]);
    session.analyze(file);
    let module = session.module_of(file);
    let analysis = session.module_analysis(module).expect("analysed");
    silt::repl::scope_completion_names(&analysis.scope, session.defs(), |_| None)
}

fn assert_names(src: &str, expected: &[&str]) {
    let names = completion_names(src);
    for name in expected {
        assert!(
            names.iter().any(|n| n == name),
            "`{src}` must offer `{name}`: {names:?}"
        );
    }
}

#[test]
fn repl_completion_names_include_anon_record_rest_binder() {
    assert_names("let {x, ...rest} = r", &["x", "rest"]);
}

#[test]
fn repl_completion_names_include_list_rest_binder() {
    assert_names("let [h, ..t] = xs", &["h", "t"]);
}

#[test]
fn repl_completion_names_include_map_value_binder() {
    assert_names("let #{\"k\": v} = m", &["v"]);
}
