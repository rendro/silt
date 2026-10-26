//! The quick fixes of the warnings: each one, applied to the text,
//! leaves a program without that warning and without an error.

use silt::diagnostic::{Code, Diagnostic};
use silt::session::testing::check_str;

/// `text` with the edits of the first fix of `d` applied.
fn fixed(text: &str, d: &Diagnostic) -> String {
    let fix = d.fixes.first().expect("the warning has a quick fix");
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut out = text.to_string();
    for (span, new) in edits {
        out.replace_range(span.start as usize..span.end as usize, &new);
    }
    out
}

/// The warnings of `code` in `text`.
fn warnings(text: &str, code: Code) -> Vec<Diagnostic> {
    check_str(text)
        .into_iter()
        .filter(|d| d.code == code)
        .collect()
}

#[test]
fn removing_an_unused_import_removes_its_line() {
    let text = "import list\nimport string\n\nfn main() {\n  println(string.length(\"ab\"))\n}\n";
    let found = warnings(text, Code::UnusedImport);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].fixes[0].title, "remove the line `import list`");
    let after = fixed(text, &found[0]);
    assert_eq!(
        after,
        "import string\n\nfn main() {\n  println(string.length(\"ab\"))\n}\n"
    );
    assert!(check_str(&after).is_empty(), "{:?}", check_str(&after));
}

#[test]
fn removing_an_unused_item_keeps_the_others() {
    for (text, after) in [
        (
            "import string.{ length, trim }\n\nfn main() {\n  println(length(\"ab\"))\n}\n",
            "import string.{ length }\n\nfn main() {\n  println(length(\"ab\"))\n}\n",
        ),
        (
            "import string.{ trim, length }\n\nfn main() {\n  println(length(\"ab\"))\n}\n",
            "import string.{ length }\n\nfn main() {\n  println(length(\"ab\"))\n}\n",
        ),
        (
            "import string.{ trim }\nimport list\n\nfn main() {\n  println(list.length([1]))\n}\n",
            "import list\n\nfn main() {\n  println(list.length([1]))\n}\n",
        ),
    ] {
        let found = warnings(text, Code::UnusedImport);
        assert_eq!(found.len(), 1, "{text}: {found:?}");
        assert_eq!(found[0].fixes[0].title, "remove `trim` from the import");
        let fixed_text = fixed(text, &found[0]);
        assert_eq!(fixed_text, after);
        assert!(check_str(&fixed_text).is_empty(), "{fixed_text}");
    }
}

#[test]
fn an_import_used_only_in_a_type_or_an_impl_header_is_used() {
    let text = "import http\nimport json\n\ntype Tag {\n  name: String,\n}\n\n\
                trait json.Encode for Tag {\n  fn encode(self) -> json.Value {\n    \
                json.Value.Null\n  }\n}\n\nfn status(_r: http.Request) -> Int {\n  200\n}\n";
    assert!(check_str(text).is_empty(), "{:?}", check_str(text));
}

#[test]
fn removing_an_unused_import_keeps_the_comment_behind_it() {
    let text = "import list -- for later\nimport string\n\nfn main() {\n  println(string.length(\"ab\"))\n}\n";
    let found = warnings(text, Code::UnusedImport);
    assert_eq!(found.len(), 1, "{found:?}");
    let after = fixed(text, &found[0]);
    assert_eq!(
        after,
        "-- for later\nimport string\n\nfn main() {\n  println(string.length(\"ab\"))\n}\n"
    );
}

#[test]
fn naming_an_unused_name_with_an_underscore_silences_it() {
    for (text, after) in [
        (
            "fn main() {\n  let total = 1\n  println(\"x\")\n}\n",
            "fn main() {\n  let _total = 1\n  println(\"x\")\n}\n",
        ),
        // A field written alone keeps its field.
        (
            "type P {\n  x: Int,\n  y: Int,\n}\n\nfn main() {\n  let P { x, y } = P { x: 1, y: 2 }\n  println(x)\n}\n",
            "type P {\n  x: Int,\n  y: Int,\n}\n\nfn main() {\n  let P { x, y: _y } = P { x: 1, y: 2 }\n  println(x)\n}\n",
        ),
        // Every alternative of an or-pattern binds the name.
        (
            "type E {\n  A(Int),\n  B(Int),\n}\n\nfn main() {\n  match A(1) {\n    A(n) | B(n) -> println(\"ab\")\n  }\n}\n",
            "type E {\n  A(Int),\n  B(Int),\n}\n\nfn main() {\n  match A(1) {\n    A(_n) | B(_n) -> println(\"ab\")\n  }\n}\n",
        ),
    ] {
        let found = warnings(text, Code::UnusedName);
        assert_eq!(found.len(), 1, "{text}: {found:?}");
        let fixed_text = fixed(text, &found[0]);
        assert_eq!(fixed_text, after);
        assert!(
            check_str(&fixed_text).is_empty(),
            "{:?}",
            check_str(&fixed_text)
        );
    }
}

#[test]
fn a_parameter_that_nothing_uses_does_not_warn() {
    let text = "fn f(x: Int, y: Int) -> Int {\n  x\n}\n\nfn main() {\n  println(f(1, 2))\n  \
                let g = { a, b -> a }\n  println(g(1, 2))\n}\n";
    assert!(check_str(text).is_empty(), "{:?}", check_str(text));
}
