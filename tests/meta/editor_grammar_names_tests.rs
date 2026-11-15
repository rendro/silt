//! The names the two editor grammars highlight are written from their
//! sources, not kept by hand: the keywords and the boolean literals from
//! the lexer (`silt::testing::lexer::KEYWORDS`, `KEYWORD_LITERALS`), and from the
//! builtin registry the builtin types (`silt::testing::types::builtins`), the
//! record and enum types of the builtin modules
//! (`silt::testing::module::builtin_module_types`), the variants of the builtin
//! enums (`builtin_enum_variants`), the prelude's free functions
//! (`builtin_free_function_names`) and the builtin modules
//! (`builtin_modules`).
//!
//! In `editors/vim/syntax/silt.vim` the lines between the two markers
//! (`" ── Names (generated)` and `" ── End of the generated names`) are
//! rendered here; in `editors/vscode/syntaxes/silt.tmLanguage.json` the
//! `match` of the repository entries `keywords`, `booleans`,
//! `primitives`, `constructors`, `builtins` and `modules`. The test
//! compares them with the files; after a change of the registry or the
//! lexer, `SILT_BLESS=1 cargo nextest run --test meta editor_grammar_names`
//! rewrites them. The names `docs/editor-setup.md` gives as examples of
//! what is highlighted must be names of those lists.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::env_switch;

const VIM: &str = "editors/vim/syntax/silt.vim";
const VSCODE: &str = "editors/vscode/syntaxes/silt.tmLanguage.json";
const VIM_BEGIN: &str = "\" ── Names (generated) ";
const VIM_END: &str = "\" ── End of the generated names ";

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The names, each list in the order of its source.
struct Names {
    keywords: Vec<&'static str>,
    literals: Vec<&'static str>,
    /// The variants of each builtin enum, a name that an enum before
    /// it has already left out.
    variants: Vec<Vec<&'static str>>,
    /// The builtin types (`()` is punctuation, not a name).
    types: Vec<&'static str>,
    module_types: Vec<&'static str>,
    functions: Vec<&'static str>,
    modules: Vec<&'static str>,
}

fn names() -> Names {
    let mut seen = BTreeSet::new();
    let variants = silt::testing::module::builtin_enum_variants()
        .iter()
        .map(|(_, variants)| {
            variants
                .iter()
                .copied()
                .filter(|v| seen.insert(*v))
                .collect::<Vec<_>>()
        })
        .filter(|vs| !vs.is_empty())
        .collect();
    Names {
        keywords: silt::testing::lexer::KEYWORDS.to_vec(),
        literals: silt::testing::lexer::KEYWORD_LITERALS.to_vec(),
        variants,
        types: silt::testing::types::builtins::iter_all()
            .map(|b| b.name)
            .filter(|n| *n != "()")
            .collect(),
        module_types: silt::testing::module::builtin_module_types().to_vec(),
        functions: silt::testing::module::builtin_free_function_names().to_vec(),
        modules: silt::testing::module::builtin_modules().to_vec(),
    }
}

/// `syntax keyword <group>` lines that hold `words`, none longer than
/// 100 characters (unless one word alone makes it so).
fn keyword_lines(group: &str, words: &[&str]) -> Vec<String> {
    let start = format!("syntax keyword {group}");
    let mut lines = Vec::new();
    let mut line = start.clone();
    for word in words {
        if line.len() > start.len() && line.len() + 1 + word.len() > 100 {
            lines.push(std::mem::replace(&mut line, start.clone()));
        }
        line.push(' ');
        line.push_str(word);
    }
    if line.len() > start.len() {
        lines.push(line);
    }
    lines
}

/// The generated part of the vim grammar, its two markers included.
fn vim_names(n: &Names) -> String {
    let mut out = vec![
        format!("{VIM_BEGIN}{}", "─".repeat(46)),
        "\" Written from the lexer and the builtin registry by".to_string(),
        "\" tests/meta/editor_grammar_names_tests.rs: do not edit by hand. After a".to_string(),
        "\" change of either, `SILT_BLESS=1 cargo nextest run --test meta".to_string(),
        "\" editor_grammar_names` writes these lines again.".to_string(),
    ];
    out.extend(keyword_lines("siltKeyword", &n.keywords));
    out.extend(keyword_lines("siltBoolean", &n.literals));
    out.push("\" The variants of the builtin enums, one enum a line. The types come".to_string());
    out.push("\" after them: a name that is both (json.Value's Int) is a type, as vim".to_string());
    out.push("\" takes the keyword defined last.".to_string());
    for variants in &n.variants {
        out.extend(keyword_lines("siltConstructor", variants));
    }
    out.push("\" The builtin types, then the record and enum types of the modules.".to_string());
    out.extend(keyword_lines("siltType", &n.types));
    out.extend(keyword_lines("siltType", &n.module_types));
    out.push("\" The free functions of the prelude.".to_string());
    out.extend(keyword_lines("siltBuiltin", &n.functions));
    out.push("\" A builtin module's name before a dot.".to_string());
    out.push(format!(
        "syntax match siltModule \"\\<\\({}\\)\\>\\ze\\.\"",
        n.modules.join("\\|")
    ));
    out.push(format!("{VIM_END}{}", "─".repeat(42)));
    out.join("\n")
}

/// The vim grammar with its generated part rendered again.
fn render_vim(text: &str, n: &Names) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.starts_with(VIM_BEGIN))
        .unwrap_or_else(|| panic!("{VIM} has no line starting `{VIM_BEGIN}`"));
    let end = lines
        .iter()
        .position(|l| l.starts_with(VIM_END))
        .unwrap_or_else(|| panic!("{VIM} has no line starting `{VIM_END}`"));
    assert!(
        begin < end,
        "{VIM}: the end marker comes before the begin marker"
    );
    let mut out: Vec<String> = lines[..begin].iter().map(|l| l.to_string()).collect();
    out.push(vim_names(n));
    out.extend(lines[end + 1..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// The `match` of each generated entry of the VS Code grammar, as the
/// JSON text writes it (a backslash doubled).
fn vscode_matches(n: &Names) -> Vec<(&'static str, String)> {
    let words = |names: Vec<&str>| format!(r"\\b({})\\b", names.join("|"));
    let types = n.types.iter().chain(&n.module_types).copied().collect();
    let variants = n.variants.iter().flatten().copied().collect();
    vec![
        ("keywords", words(n.keywords.clone())),
        ("booleans", words(n.literals.clone())),
        ("primitives", words(types)),
        ("constructors", words(variants)),
        ("builtins", words(n.functions.clone())),
        (
            "modules",
            format!(r"\\b({})\\b(?=\\.)", n.modules.join("|")),
        ),
    ]
}

/// The VS Code grammar with the `match` line of each generated entry
/// rendered again.
fn render_vscode(text: &str, n: &Names) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (entry, pattern) in vscode_matches(n) {
        let key = format!("\"{entry}\": {{");
        let at = lines
            .iter()
            .position(|l| l.trim_start().starts_with(&key))
            .unwrap_or_else(|| panic!("{VSCODE} has no repository entry `{entry}`"));
        let line = lines[at + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with("\"match\": \""))
            .map(|i| at + 1 + i)
            .unwrap_or_else(|| panic!("{VSCODE}: the entry `{entry}` has no `match`"));
        let indent: String = lines[line].chars().take_while(|c| *c == ' ').collect();
        let comma = if lines[line].trim_end().ends_with(',') {
            ","
        } else {
            ""
        };
        lines[line] = format!("{indent}\"match\": \"{pattern}\"{comma}");
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[test]
fn the_editor_grammars_list_the_names_of_the_lexer_and_the_registry() {
    let n = names();
    let bless = env_switch::on("SILT_BLESS");
    let mut stale = Vec::new();
    for (file, render) in [
        (VIM, render_vim as fn(&str, &Names) -> String),
        (VSCODE, render_vscode),
    ] {
        let text = repo_file(file);
        let rendered = render(&text, &n);
        if rendered != text {
            if bless {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file);
                std::fs::write(path, &rendered).expect("write the grammar");
            }
            stale.push(file);
        }
    }
    assert!(
        stale.is_empty() || bless,
        "the names in {stale:?} differ from what the lexer and the builtin registry give. \
         Run `SILT_BLESS=1 cargo nextest run --test meta editor_grammar_names` and review \
         the diff."
    );
}

/// The VS Code grammar is still JSON, and each generated `match` is the
/// string it was rendered as (no escape went wrong on the way).
#[test]
fn the_vscode_grammar_is_json_with_the_rendered_patterns() {
    let grammar: serde_json::Value =
        serde_json::from_str(&repo_file(VSCODE)).expect("the VS Code grammar is JSON");
    for (entry, pattern) in vscode_matches(&names()) {
        let found = grammar["repository"][entry]["match"]
            .as_str()
            .unwrap_or_else(|| panic!("{VSCODE}: no repository.{entry}.match"));
        assert_eq!(
            found,
            pattern.replace(r"\\", r"\"),
            "repository.{entry}.match"
        );
    }
}

/// The examples of `docs/editor-setup.md`'s "What's highlighted" are
/// names the grammars highlight as it says.
#[test]
fn the_editor_setup_page_names_only_highlighted_names() {
    let n = names();
    let page = repo_file("docs/editor-setup.md");
    let examples = |bullet: &str| -> Vec<String> {
        let head = format!("- {bullet} (");
        let line = page
            .lines()
            .find(|l| l.starts_with(&head))
            .unwrap_or_else(|| panic!("docs/editor-setup.md has no bullet `{head}`"));
        line[head.len()..]
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    };
    let variants: Vec<&str> = n.variants.iter().flatten().copied().collect();
    for (bullet, allowed) in [
        ("Keywords", n.keywords.clone()),
        (
            "Builtins",
            n.functions.iter().chain(&variants).copied().collect(),
        ),
        ("Module names before `.`", n.modules.clone()),
    ] {
        let given = examples(bullet);
        assert!(!given.is_empty(), "`- {bullet}` gives no example");
        for name in given {
            assert!(
                allowed.contains(&name.as_str()),
                "docs/editor-setup.md, `{bullet}`: `{name}` is not one of the names highlighted so"
            );
        }
    }
}
