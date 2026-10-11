//! The reference pages of the builtin modules (`docs/stdlib/*.md`).
//!
//! A page is prose with two generated parts, which [`render_page`]
//! writes from the rows of the builtin registry:
//!
//! - in a module's tables of functions (the tables whose second column
//!   is headed `Signature`), the signature cell and, in a table of three
//!   columns or more, the last cell (the description) of each row;
//! - the signature block that opens a function's own section
//!   (`## \`list.map\``).
//!
//! `tests/meta/stdlib_reference_tests.rs` renders every page and
//! compares it with the file (`SILT_BLESS=1` rewrites the files).
//!
//! What an editor shows for a builtin name ([`builtin_docs`]) is cut
//! from the same pages: a name's own section where a page has one, its
//! module's page otherwise.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::{Module, Row, registry};

/// The pages that belong to no one module.
const ERRORS_PAGE: &str = include_str!("../../../docs/stdlib/errors.md");
const GLOBALS_PAGE: &str = include_str!("../../../docs/stdlib/globals.md");

/// The prelude's names, which the globals page documents as a whole.
const PRELUDE_VARIANTS: &[&str] = &["Ok", "Err", "Some", "None"];

/// Every reference page, by its file name under `docs/stdlib/`, in the
/// order of the modules; `errors.md` and `globals.md` last.
pub fn pages() -> Vec<(String, &'static str)> {
    let mut pages: Vec<(String, &'static str)> = Vec::new();
    for module in &registry().modules {
        if !pages.iter().any(|(file, _)| file == module.page_file) {
            pages.push((module.page_file.to_string(), module.page));
        }
    }
    pages.push(("errors.md".to_string(), ERRORS_PAGE));
    pages.push(("globals.md".to_string(), GLOBALS_PAGE));
    pages
}

/// The markdown an editor shows for each builtin name as it is reached
/// qualified: `list.map`, a module's variant (`io.NotFound`,
/// `channel.Message`), a prelude name bare. A variant's text is its
/// own module's: two modules that have a variant of one name
/// (`channel.Closed`, `tcp.Closed`) show two texts.
pub fn builtin_docs() -> &'static HashMap<String, String> {
    static DOCS: OnceLock<HashMap<String, String>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let registry = registry();
        let mut docs: HashMap<String, String> = HashMap::new();
        let mut names: HashSet<String> = HashSet::new();
        // A function with no section of its own shows its module's page.
        for module in registry.enabled_modules() {
            for row in module.enabled_rows() {
                let name = format!("{}.{}", module.name, row.name);
                docs.insert(name.clone(), strip_frontmatter(module.page).to_string());
                names.insert(name);
            }
            // A variant is named with its module, like a function: the
            // bare name is not one variant's (`Closed` is `channel`'s,
            // `tcp`'s and `postgres`'s).
            for ty in &module.type_decls {
                for (variant, _) in ty.variants() {
                    names.insert(format!("{}.{variant}", module.name));
                }
            }
        }
        // A name's own section, in whichever page has it.
        for (_, page) in pages() {
            for (keys, body) in iter_sections(page) {
                for key in keys {
                    if names.contains(&key) {
                        docs.insert(key, body.clone());
                    }
                }
            }
        }
        // A variant without a section of its own shows what documents
        // its enum, under a line that says whose it is: the enum's
        // section of the errors page for a variant of the module's
        // error enum, the module's page for any other.
        let error_sections = iter_sections(ERRORS_PAGE);
        for module in registry.enabled_modules() {
            for ty in &module.type_decls {
                let section = error_sections
                    .iter()
                    .find(|(keys, _)| {
                        module.error == Some(ty.name) && keys.iter().any(|k| k == ty.name)
                    })
                    .map(|(_, body)| body.as_str());
                let below = section.unwrap_or_else(|| strip_frontmatter(module.page));
                for (variant, _) in ty.variants() {
                    let name = format!("{}.{variant}", module.name);
                    docs.entry(name).or_insert_with(|| {
                        format!(
                            "A variant of `{0}.{1}`, reached as `{0}.{variant}`.\n\n{below}",
                            module.name, ty.name
                        )
                    });
                }
            }
        }
        let globals = strip_frontmatter(GLOBALS_PAGE);
        for name in crate::module::builtin_free_function_names()
            .iter()
            .chain(PRELUDE_VARIANTS)
        {
            docs.insert(name.to_string(), globals.to_string());
        }
        // A function of a feature that is not built (its module's, or
        // its own): what it is, and what it needs. The checker does not
        // know it, so this is all an editor can say of it.
        for module in &registry.modules {
            let off = module.rows.iter().filter(|row| !row.enabled);
            for row in off.filter(|row| !row.is_constant()) {
                let name = format!("{}.{}", module.name, row.name);
                let feature = match module.enabled {
                    true => row.feature,
                    false => module.feature,
                };
                let Some(feature) = feature else { continue };
                let needs = crate::module::needs_feature(&format!("`{name}`"), feature);
                let text = format!(
                    "```silt\n{}\n```\n\n{}\n\n{needs}.",
                    row.signature, row.summary
                );
                docs.insert(name, text);
            }
        }
        docs
    })
}

/// Strip a leading YAML frontmatter block (`---\n…\n---\n`, plus the
/// blank lines that follow it) off a page: it is metadata for the docs
/// website, and an editor would render it as a stray rule followed by
/// raw `title: "…"` text. A page without one, or with an unterminated
/// one, is returned unchanged.
pub fn strip_frontmatter(md: &str) -> &str {
    let Some(rest) = md.strip_prefix("---\n") else {
        return md;
    };
    let Some(close) = rest.find("\n---\n") else {
        return md;
    };
    rest[close + "\n---\n".len()..].trim_start_matches('\n')
}

/// The sections of a page as `(keys, body)`. A section starts at a `## `
/// or `### ` heading that names something, either backticked
/// (`## \`list.map\``, `## \`time.hours\`, \`time.minutes\`` for
/// several names) or bare (`## list.map`); its body runs to the next
/// heading of any of the three top levels, without the blank lines at
/// its ends.
pub fn iter_sections(md: &str) -> Vec<(Vec<String>, String)> {
    let mut sections: Vec<(Vec<String>, String)> = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(keys) = parse_heading_keys(line) {
            let start = i + 1;
            let mut end = lines.len();
            for (j, line) in lines.iter().enumerate().skip(start) {
                if is_section_break(line) {
                    end = j;
                    break;
                }
            }
            let body = trim_blank_edges(&lines[start..end]);
            sections.push((keys, body));
            i = end;
        } else {
            i += 1;
        }
    }
    sections
}

/// The names a heading line gives its section; `None` for a line that is
/// not a `##` / `###` heading or names nothing.
fn parse_heading_keys(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim_start();
    let after_hash = if let Some(rest) = trimmed.strip_prefix("### ") {
        rest
    } else {
        trimmed.strip_prefix("## ")?
    };
    let stripped = after_hash.trim();

    // Backticked: every backticked identifier-shaped token.
    let mut keys: Vec<String> = Vec::new();
    if stripped.starts_with('`') {
        let mut rest = stripped;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let close = match after.find('`') {
                Some(c) => c,
                None => break,
            };
            let candidate = &after[..close];
            let key =
                candidate.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_');
            if !key.is_empty() && looks_like_identifier(key) {
                keys.push(key.to_string());
            }
            rest = &after[close + 1..];
        }
        if keys.is_empty() {
            return None;
        }
        return Some(keys);
    }

    // Bare: the first word.
    let candidate = stripped.split_whitespace().next()?;
    let key = candidate.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_');
    if key.is_empty() {
        return None;
    }
    Some(vec![key.to_string()])
}

/// At least one letter, otherwise only alphanumerics, `_` and `.`.
fn looks_like_identifier(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic())
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Whether `line` ends the section before it: a `#`, `##` or `###`
/// heading. A `####` heading is content of its section.
fn is_section_break(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ")
}

fn trim_blank_edges(lines: &[&str]) -> String {
    let mut start = 0;
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

// ── The generated parts of a page ───────────────────────────────────

/// A row's signature without its parameter names, as a summary table
/// shows it: `(List(a), Fn(a) -> b) -> List(b)`; a constant's type.
fn summary_signature(row: &Row) -> String {
    let Some((label, ranges)) = row.qualified_signature("") else {
        return row
            .signature
            .split_once(": ")
            .map_or(row.signature, |(_, ty)| ty)
            .to_string();
    };
    let types: Vec<String> = ranges
        .iter()
        .map(|[start, end]| {
            let param = &label[*start as usize..*end as usize];
            param
                .split_once(": ")
                .map_or(param, |(_, ty)| ty)
                .to_string()
        })
        .collect();
    let close = ranges.last().map_or_else(
        || label.find(')').unwrap_or(label.len()),
        |[_, end]| *end as usize,
    );
    format!("({}){}", types.join(", "), &label[close + 1..])
}

/// The signature block of a function's section: `list.map(xs: List(a),
/// f: Fn(a) -> b) -> List(b)`; `math.pi: Float` for a constant.
fn section_signature(module: &Module, row: &Row) -> String {
    match row.qualified_signature(module.name) {
        Some((label, _)) => label["fn ".len()..].to_string(),
        None => format!("{}.{}", module.name, row.signature),
    }
}

/// The module whose part of a page a `# ` heading opens (`# postgres
/// (opt-in feature)`), if it opens one.
fn heading_module(line: &str) -> Option<&'static Module> {
    let name = line.strip_prefix("# ")?.split_whitespace().next()?;
    registry().module(name)
}

/// The cells of a table row, `| a | b |`, without the outer bars. A
/// `\|` inside a cell is not a separator.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push_str("\\|");
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    Some(cells.iter().map(|c| c.trim().to_string()).collect())
}

/// The page `page` with its generated parts written from the registry:
/// see the module documentation. `Err` names each function a table of
/// functions lists that the registry does not have, and each row of a
/// module that no table of its page lists.
pub fn render_page(page: &str) -> Result<String, Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut module: Option<&'static Module> = None;
    // Whether the line is in a table whose second column is headed
    // `Signature`: a table of the module's functions.
    let mut in_table = false;
    let mut in_summary = false;
    let mut in_fence = false;
    let mut listed: HashSet<(&str, &str)> = HashSet::new();
    let mut modules: Vec<&'static Module> = Vec::new();
    // The row whose section has just opened: its signature block is
    // the first fence, if a plain one comes before any prose.
    let mut pending: Option<(&'static Module, &'static Row)> = None;
    let mut replacing = false;
    for line in page.lines() {
        if replacing {
            if line.trim() == "```" {
                replacing = false;
                out.push(line.to_string());
            }
            continue;
        }
        if line.trim_start().starts_with("```") {
            if let Some((m, row)) = pending.take()
                && !in_fence
                && line.trim() == "```"
            {
                out.push(line.to_string());
                out.push(section_signature(m, row));
                replacing = true;
                continue;
            }
            in_fence = !in_fence;
            out.push(line.to_string());
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }
        if line.starts_with("# ") {
            module = heading_module(line);
            modules.extend(module);
            pending = None;
        }
        match table_cells(line) {
            Some(cells) if !in_table => {
                in_table = true;
                in_summary = cells.get(1).is_some_and(|cell| cell == "Signature");
            }
            Some(_) => {}
            None => (in_table, in_summary) = (false, false),
        }
        if is_section_break(line) {
            pending = parse_heading_keys(line).and_then(|keys| {
                let [key] = keys.as_slice() else {
                    return None;
                };
                let (m, f) = key.split_once('.')?;
                let m = registry().module(m)?;
                let row = m.rows.iter().find(|row| row.name == f)?;
                Some((m, row))
            });
        } else if !line.trim().is_empty() {
            pending = None;
        }
        if in_summary
            && let Some(m) = module
            && let Some(cells) = table_cells(line)
            && let Some(name) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`'))
        {
            let name = name.strip_prefix(&format!("{}.", m.name)).unwrap_or(name);
            match m.rows.iter().find(|row| row.name == name) {
                Some(row) => {
                    listed.insert((m.name, row.name));
                    let mut cells = cells.clone();
                    if cells.len() >= 2 {
                        cells[1] = format!("`{}`", summary_signature(row));
                    }
                    if cells.len() >= 3 {
                        let last = cells.len() - 1;
                        cells[last] = row.summary.to_string();
                    }
                    out.push(format!("| {} |", cells.join(" | ")));
                    continue;
                }
                None => problems.push(format!(
                    "a table of `{}` lists `{name}`, which is not a row of the module",
                    m.name
                )),
            }
        }
        out.push(line.to_string());
    }
    for m in modules {
        for row in &m.rows {
            if !listed.contains(&(m.name, row.name)) {
                problems.push(format!(
                    "no table of functions of the page lists `{}.{}`",
                    m.name, row.name
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let mut text = out.join("\n");
    if page.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backticked_heading() {
        let md = "## `list.map`\nbody line\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, vec!["list.map"]);
        assert_eq!(sections[0].1, "body line");
    }

    #[test]
    fn parses_bare_heading() {
        let md = "## list.filter\nbody\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, vec!["list.filter"]);
    }

    #[test]
    fn body_terminates_at_next_heading() {
        let md = "## `a.b`\n\n\none\ntwo\n\n## `c.d`\nthree\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].1, "one\ntwo");
        assert_eq!(sections[1].1, "three");
    }

    #[test]
    fn parses_multi_name_heading() {
        let md = "## `time.hours`, `time.minutes`, `time.seconds`\nbody\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(
            sections[0].0,
            vec!["time.hours", "time.minutes", "time.seconds"]
        );
    }

    #[test]
    fn strip_frontmatter_drops_leading_yaml_block() {
        let md = "---\ntitle: \"bytes\"\norder: 16\n---\n\n# bytes\n\nProse.\n";
        assert_eq!(strip_frontmatter(md), "# bytes\n\nProse.\n");
        let plain = "# bytes\n\nProse with a --- rule later.\n---\n";
        assert_eq!(strip_frontmatter(plain), plain);
        let unterminated = "---\ntitle: \"x\"\nno closing delimiter\n";
        assert_eq!(strip_frontmatter(unterminated), unterminated);
    }

    #[test]
    fn a_summary_signature_drops_the_parameter_names() {
        let row = registry().row("list", "fold").expect("list.fold");
        assert_eq!(summary_signature(row), "(List(a), b, Fn(b, a) -> b) -> b");
        let row = registry().row("io", "args").expect("io.args");
        assert_eq!(summary_signature(row), "() -> List(String)");
        let row = registry().row("math", "pi").expect("math.pi");
        assert_eq!(summary_signature(row), "Float");
    }

    /// Every variant of every builtin module, as the registry has them,
    /// shows a text of its own module and enum: it names the variant as
    /// it is reached and its enum, and it is not the text of another
    /// module's variant of the same name (`tcp.Closed` does not show
    /// what is written for `channel.Closed`: "A variant of `channel`").
    #[test]
    fn every_builtin_variant_shows_the_docs_of_its_own_enum() {
        let docs = builtin_docs();
        let registry = registry();
        let mut homes: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut variants = Vec::new();
        for module in registry.enabled_modules() {
            for ty in &module.type_decls {
                for (variant, _) in ty.variants() {
                    homes.entry(variant).or_default().push(module.name);
                    variants.push((module.name, ty.name, variant));
                }
            }
        }
        assert!(variants.len() > 80, "{} variants", variants.len());
        let mut shared = 0;
        for (module, ty, variant) in variants {
            let name = format!("{module}.{variant}");
            let text = docs
                .get(&name)
                .unwrap_or_else(|| panic!("`{name}` has no text"));
            assert!(text.contains(&name), "`{name}` is not named in:\n{text}");
            assert!(
                text.contains(ty),
                "`{ty}` of `{name}` is not named in:\n{text}"
            );
            for other in homes[variant].iter().filter(|other| **other != module) {
                shared += 1;
                let theirs = format!("{other}.{variant}");
                // (Its own text may speak of the other: "`float.Empty`
                // is not `int.Empty`". What says whose variant a text
                // is for is its line "A variant of ...".)
                assert!(
                    !text.contains(&format!("A variant of `{other}`"))
                        && !text.contains(&format!("A variant of `{other}.")),
                    "`{name}` shows the text of `{theirs}`:\n{text}"
                );
                assert_ne!(
                    text, &docs[&theirs],
                    "`{name}` and `{theirs}` show one text"
                );
            }
        }
        // `Closed`, `Timeout`, `Other`, ...: the case is not empty.
        assert!(shared > 20, "{shared} pairs of one name");
    }

    #[test]
    fn a_page_is_named_after_its_modules() {
        let files: Vec<String> = pages().into_iter().map(|(file, _)| file).collect();
        assert!(files.contains(&"int-float.md".to_string()), "{files:?}");
        assert!(files.contains(&"io-fs.md".to_string()), "{files:?}");
    }
}
