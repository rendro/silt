//! The reference pages of the builtin modules (`docs/stdlib/*.md`).
//!
//! A page is prose with two generated parts, which [`render_page`]
//! writes from the rows of the builtin registry:
//!
//! - in a module's tables of functions (the tables whose second column
//!   is headed `Signature`), the signature cell and, in a table of three
//!   columns or more, the last cell (the description) of each row;
//! - the signature block that opens a function's own section
//!   (`## \`list.map\``);
//! - in a table whose second column is headed `Type argument`, that
//!   cell of each decoding builtin's row: what its type argument may be
//!   ([`Decoding::type_arguments`](super::Decoding::type_arguments));
//! - on a page that is no module's (`globals.md`), the same of the
//!   prelude's functions: their rows of a table of functions, which
//!   lists all of them, and the signature blocks of their sections.
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
            // `tcp`'s and `postgres`'s). So is a type with a section of
            // its own (`json.Value`).
            for ty in &module.type_decls {
                names.insert(format!("{}.{}", module.name, ty.name));
                for (variant, _) in ty.variants() {
                    names.insert(format!("{}.{variant}", module.name));
                }
            }
            // So is a trait (`json.Encode`).
            for name in &module.trait_names {
                names.insert(format!("{}.{name}", module.name));
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
        // its enum (`type_docs`), under a line that says whose it is,
        // or else the module's page.
        for module in registry.enabled_modules() {
            for ty in &module.type_decls {
                let below = type_docs()
                    .get(&format!("{}.{}", module.name, ty.name))
                    .cloned()
                    .unwrap_or_else(|| strip_frontmatter(module.page).to_string());
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
                let text = unbuilt_text(&name, row.signature, row.summary, feature);
                docs.insert(name, text);
            }
        }
        docs
    })
}

/// What an editor shows for the function `name` of a cargo feature that
/// this build lacks: its signature and summary, and what it needs.
pub(crate) fn unbuilt_text(name: &str, signature: &str, summary: &str, feature: &str) -> String {
    let needs = crate::module::needs_feature(&format!("`{name}`"), feature);
    format!("```silt\n{signature}\n```\n\n{summary}\n\n{needs}.")
}

/// The docs of each type that a built module declares, by its name as
/// it is reached (`json.Value`, `io.IoError`), where it has any: the
/// errors page's section on the module's error enum, else the type's
/// own section of a page.
pub fn type_docs() -> &'static HashMap<String, String> {
    static DOCS: OnceLock<HashMap<String, String>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let modules: Vec<&Module> = registry().enabled_modules().collect();
        let names: HashSet<String> = modules
            .iter()
            .flat_map(|m| {
                m.type_decls
                    .iter()
                    .map(|ty| format!("{}.{}", m.name, ty.name))
            })
            .collect();
        let mut docs = HashMap::new();
        for (_, page) in pages() {
            for (keys, body) in iter_sections(page) {
                for key in keys.into_iter().filter(|key| names.contains(key)) {
                    docs.insert(key, body.clone());
                }
            }
        }
        let error_sections = iter_sections(ERRORS_PAGE);
        for module in modules {
            let Some(error) = module.error else { continue };
            if let Some((_, body)) = error_sections
                .iter()
                .find(|(keys, _)| keys.iter().any(|k| k == error))
            {
                docs.insert(format!("{}.{error}", module.name), body.clone());
            }
        }
        docs
    })
}

/// What an editor says of each builtin module whose feature is built
/// (`json` in `import json`, in `json.parse`, in `json.Encode`): the
/// first sentence of the first paragraph of the module's part of its
/// page (under its `# json` heading), on one line.
pub fn module_docs() -> &'static HashMap<&'static str, String> {
    static DOCS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let mut docs = HashMap::new();
        for module in registry().enabled_modules() {
            let paragraph: Vec<&str> = strip_frontmatter(module.page)
                .lines()
                .skip_while(|line| heading_module(line).is_none_or(|m| m.name != module.name))
                .skip(1)
                .take_while(|line| !line.starts_with("# "))
                .skip_while(|line| line.trim().is_empty() || line.starts_with('#'))
                .take_while(|line| !line.trim().is_empty())
                .flat_map(|line| line.split_whitespace())
                .collect();
            let paragraph = paragraph.join(" ");
            let sentence = match paragraph.find(". ") {
                Some(end) => &paragraph[..=end],
                None => &paragraph,
            };
            docs.insert(module.name, sentence.to_string());
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

/// The line that the signature block of `module.name` holds
/// (`list.map(xs: List(a), f: Fn(a) -> b) -> List(b)`), if the module
/// has such a row.
pub fn signature_block(module: &str, name: &str) -> Option<String> {
    let module = registry().module(module)?;
    let row = module.rows.iter().find(|row| row.name == name)?;
    Some(section_signature(module, row))
}

// ── Declarations, as the language card and the REPL's `:doc` write them

/// The lines that state the module `module`: its types and traits as
/// declarations (without `pub`), its opaque types, then each function
/// and constant in the order of their names, as its signature block
/// writes it; a row of a cargo feature beyond its module's says so.
pub fn module_lines(module: &Module) -> Vec<String> {
    let mut lines = declarations(module.types);
    lines.extend(declarations(module.traits));
    lines.extend((module.opaque.iter()).map(|(name, arity)| opaque_line(name, *arity)));
    let mut rows: Vec<&Row> = module.rows.iter().collect();
    rows.sort_by_key(|row| row.name);
    lines.extend(rows.into_iter().map(|row| {
        let line = section_signature(module, row);
        match row.feature {
            Some(feature) => format!("{line}  -- cargo feature `{feature}`"),
            None => line,
        }
    }));
    lines
}

/// The lines that state the prelude: its types, its opaque types and
/// its functions, as [`module_lines`] writes a module's.
pub fn prelude_lines() -> Vec<String> {
    let mut lines = declarations(super::PRELUDE_TYPES);
    lines.extend(
        (super::PRELUDE_OPAQUE.iter())
            .filter(|(name, _)| *name != crate::defs::TYPE_OF)
            .map(|(name, arity)| opaque_line(name, *arity)),
    );
    let mut rows: Vec<&Row> = registry().prelude.iter().collect();
    rows.sort_by_key(|row| row.name);
    lines.extend(rows.into_iter().map(prelude_signature));
    lines
}

/// The signature block of a function of the prelude: `println(value:
/// a) -> () where a: Display`.
fn prelude_signature(row: &Row) -> String {
    row.signature.trim_start_matches("fn ").to_string()
}

/// The declarations of a module's text of types or traits, one a line,
/// without `pub`.
fn declarations(text: &str) -> Vec<String> {
    (text.lines())
        .map(|line| line.trim().trim_start_matches("pub ").to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// The line of an opaque type: `type Handle(a)  -- opaque`, a type
/// whose values only the module's functions make and take.
fn opaque_line(name: &str, arity: usize) -> String {
    let params: Vec<String> = (b'a'..)
        .take(arity)
        .map(|c| char::from(c).to_string())
        .collect();
    match params.is_empty() {
        true => format!("type {name}  -- opaque"),
        false => format!("type {name}({})  -- opaque", params.join(", ")),
    }
}

/// The line among `lines` that declares `keyword name`: `type Value {
/// ... }` for `("type", "Value")`, also `type Handle(a)  -- opaque`.
fn declaring<'a>(lines: &'a [String], keyword: &str, name: &str) -> Option<&'a String> {
    lines.iter().find(|line| {
        line.strip_prefix(keyword)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| matches!(rest.chars().next(), None | Some(' ' | '(' | ':')))
    })
}

/// The lines of the module part of `qualified` (`json` of
/// `json.Value`), or the prelude's for a bare name, with the name
/// after the module.
fn lines_of(qualified: &str) -> Option<(Vec<String>, &str)> {
    match qualified.split_once('.') {
        Some((module, name)) => Some((module_lines(registry().module(module)?), name)),
        None => Some((prelude_lines(), qualified)),
    }
}

/// The declaration of the builtin type `name` as [`module_lines`]
/// writes it: a type of the prelude by its bare name (`Option`), a
/// module's with its module (`json.Value`, `task.Handle`).
pub fn type_line(name: &str) -> Option<String> {
    let (lines, name) = lines_of(name)?;
    declaring(&lines, "type", name).cloned()
}

/// The declaration of the builtin trait `name`: a trait of a module
/// as its module states it (`json.Encode`), one of the prelude
/// (`Display`) as the checker declares it, on one line.
pub fn trait_line(name: &str) -> Option<String> {
    if name.contains('.') {
        let (lines, name) = lines_of(name)?;
        return declaring(&lines, "trait", name).cloned();
    }
    let decl = (crate::typechecker::builtin_trait_decls().into_iter())
        .find(|t| crate::intern::resolve(t.name) == name)?;
    Some(trait_text(&decl))
}

/// What the documentation of the prelude's trait `name` says first: its
/// first sentence.
pub fn prelude_trait_summary(name: &str) -> Option<String> {
    let decl = (crate::typechecker::builtin_trait_decls().into_iter())
        .find(|t| crate::intern::resolve(t.name) == name)?;
    summary(decl.doc.as_deref()?)
}

/// `trait Compare: Equal { fn compare(self, other: Self) -> Int }`.
fn trait_text(decl: &crate::ast::TraitDecl) -> String {
    use crate::ast::{PatternKind, TypeExpr, TypeExprKind};
    fn ty(te: &TypeExpr) -> String {
        let all = |types: &[TypeExpr]| types.iter().map(ty).collect::<Vec<_>>().join(", ");
        match &te.kind {
            TypeExprKind::Named { name, .. } => crate::intern::resolve(*name),
            TypeExprKind::Generic { name, args, .. } => {
                format!("{}({})", crate::intern::resolve(*name), all(args))
            }
            TypeExprKind::Tuple(elems) => format!("({})", all(elems)),
            TypeExprKind::Function(params, ret) => format!("Fn({}) -> {}", all(params), ty(ret)),
            TypeExprKind::SelfType => "Self".to_string(),
            _ => "_".to_string(),
        }
    }
    let mut header = format!("trait {}", decl.name);
    if !decl.supertraits.is_empty() {
        let supers: Vec<String> = (decl.supertraits.iter())
            .map(|s| crate::intern::resolve(s.name))
            .collect();
        header.push_str(&format!(": {}", supers.join(" + ")));
    }
    let methods: Vec<String> = (decl.methods.iter())
        .map(|f| {
            let params: Vec<String> = (f.params.iter())
                .map(|p| {
                    let name = match &p.pattern.kind {
                        PatternKind::Ident(name) => crate::intern::resolve(*name),
                        _ => "_".to_string(),
                    };
                    match &p.ty {
                        Some(t) => format!("{name}: {}", ty(t)),
                        None => name,
                    }
                })
                .collect();
            let ret = f.return_type.as_ref().map_or("()".to_string(), ty);
            format!("fn {}({}) -> {ret}", f.name, params.join(", "))
        })
        .collect();
    match methods.is_empty() {
        true => format!("{header} {{}}"),
        false => format!("{header} {{ {} }}", methods.join("; ")),
    }
}

/// The enum the builtin variant `name` is of, as [`type_line`] writes
/// it, and its name as [`type_line`] takes it: a variant of the prelude
/// by its bare name (`Some`), a module's with its module
/// (`channel.Message`) or with its module and its enum
/// (`time.Weekday.Monday`).
pub fn variant_enum(name: &str) -> Option<(String, String)> {
    let registry = registry();
    let (module, ty, variant) = match name.split('.').collect::<Vec<_>>()[..] {
        [variant] => (None, None, variant),
        [module, variant] => (Some(module), None, variant),
        [module, ty, variant] => (Some(module), Some(ty), variant),
        _ => return None,
    };
    let decls = match module {
        Some(module) => &registry.module(module)?.type_decls,
        None => &registry.prelude_types,
    };
    let decl = (decls.iter())
        .filter(|decl| ty.is_none_or(|ty| decl.name == ty))
        .find(|decl| decl.variants().iter().any(|(v, _)| *v == variant))?;
    let name = match module {
        Some(module) => format!("{module}.{}", decl.name),
        None => decl.name.to_string(),
    };
    Some((type_line(&name)?, name))
}

/// The summary of the builtin type `name` (`json.Value`): the first
/// sentence of what documents it ([`type_docs`]).
pub fn type_summary(name: &str) -> Option<String> {
    summary(type_docs().get(name)?)
}

/// The summary of the variant `variant` of the builtin enum `ty`
/// (`io.IoError`, `NotFound`) where its docs are a table of the
/// variants (an error enum's): the last cell of its row.
pub fn variant_meaning(ty: &str, variant: &str) -> Option<String> {
    let body = type_docs().get(ty)?;
    body.lines().find_map(|line| {
        let cells = table_cells(line)?;
        let first = cells.first()?.trim_matches('`');
        let name = first.split_once('(').map_or(first, |(name, _)| name);
        (name == variant && cells.len() >= 2).then(|| cells[cells.len() - 1].clone())
    })
}

/// The summary of `key` from its own section of a reference page
/// (`## \`json.Value\``, `## \`Some\``): the first sentence below the
/// section's signature block.
pub fn section_summary(key: &str) -> Option<String> {
    (pages().into_iter())
        .flat_map(|(_, page)| iter_sections(page))
        .find(|(keys, _)| keys.iter().any(|k| k == key))
        .and_then(|(_, body)| summary(&body))
}

/// The first sentence of the prose of `text`, past its code blocks,
/// headings and tables, on one line; a sentence that only says whose
/// variant a name is and how to write it (`A variant of \`channel\`:
/// write \`channel.Closed\`.`) does not count.
fn summary(text: &str) -> Option<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut fence = false;
    for line in text.lines() {
        let fence_line = line.trim_start().starts_with("```");
        fence ^= fence_line;
        let prose =
            !fence_line && !fence && !line.trim().is_empty() && !line.starts_with(['#', '|']);
        if prose {
            current.extend(line.split_whitespace());
        } else if !current.is_empty() {
            paragraphs.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.iter().find_map(|paragraph| {
        let mut rest = paragraph.as_str();
        while !rest.is_empty() {
            let (sentence, after) = match rest.find(". ") {
                Some(end) => (&rest[..=end], &rest[end + 2..]),
                None => (rest, ""),
            };
            if !sentence.starts_with("A variant of `") {
                return Some(sentence.to_string());
            }
            rest = after;
        }
        None
    })
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
/// functions lists that the registry does not have, each row of a
/// module that no table of its page lists, each function a table of
/// type arguments lists that decodes nothing, and each decoding row of a
/// module that no table of type arguments of its page lists.
pub fn render_page(page: &str) -> Result<String, Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut module: Option<&'static Module> = None;
    // Whether the line is in a table whose second column is headed
    // `Signature`: a table of the module's functions.
    let mut in_table = false;
    let mut in_summary = false;
    // Whether the line is in a table whose second column is headed
    // `Type argument`: a table of what the module's decoders decode.
    let mut in_type_arguments = false;
    let mut in_fence = false;
    let mut listed: HashSet<(&str, &str)> = HashSet::new();
    let mut listed_decoders: HashSet<(&str, &str)> = HashSet::new();
    let mut modules: Vec<&'static Module> = Vec::new();
    // The functions of the prelude a table lists, on a page that is no
    // module's (`globals.md`).
    let mut listed_prelude: HashSet<&str> = HashSet::new();
    // The row whose section has just opened (of a module, or of the
    // prelude): its signature block is the first fence, if a plain one
    // comes before any prose.
    let mut pending: Option<(Option<&'static Module>, &'static Row)> = None;
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
                out.push(match m {
                    Some(m) => section_signature(m, row),
                    None => prelude_signature(row),
                });
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
                in_type_arguments = cells.get(1).is_some_and(|cell| cell == "Type argument");
            }
            Some(_) => {}
            None => (in_table, in_summary, in_type_arguments) = (false, false, false),
        }
        if is_section_break(line) {
            pending = parse_heading_keys(line).and_then(|keys| {
                let [key] = keys.as_slice() else {
                    return None;
                };
                // `## \`list.map\``, or the bare `## \`map\`` in the
                // part of the page that is the module's; `## \`println\``
                // in no module's part.
                let (m, f) = match (key.split_once('.'), module) {
                    (Some((m, f)), _) => (registry().module(m)?, f),
                    (None, Some(m)) => (m, key.as_str()),
                    (None, None) => {
                        let row = registry().prelude.iter().find(|row| row.name == *key)?;
                        return Some((None, row));
                    }
                };
                let row = m.rows.iter().find(|row| row.name == f)?;
                Some((Some(m), row))
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
        if in_summary
            && module.is_none()
            && let Some(cells) = table_cells(line)
            && let Some(name) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`'))
            && let Some(row) = registry().prelude.iter().find(|row| row.name == name)
        {
            listed_prelude.insert(row.name);
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
        if in_type_arguments
            && let Some(m) = module
            && let Some(cells) = table_cells(line)
            && let Some(name) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`'))
        {
            let name = name.strip_prefix(&format!("{}.", m.name)).unwrap_or(name);
            let row = m.rows.iter().find(|row| row.name == name);
            match row.and_then(|row| Some((row, row.decoding?))) {
                Some((row, decoding)) if cells.len() >= 2 => {
                    listed_decoders.insert((m.name, row.name));
                    let mut cells = cells.clone();
                    cells[1] = decoding.type_arguments(|t| format!("`{t}`"));
                    out.push(format!("| {} |", cells.join(" | ")));
                    continue;
                }
                _ => problems.push(format!(
                    "a table of type arguments of `{}` lists `{name}`, which decodes nothing",
                    m.name
                )),
            }
        }
        out.push(line.to_string());
    }
    if !listed_prelude.is_empty() {
        for row in &registry().prelude {
            if !listed_prelude.contains(row.name) {
                problems.push(format!("no table of the page lists `{}`", row.name));
            }
        }
    }
    for m in modules {
        for row in &m.rows {
            if !listed.contains(&(m.name, row.name)) {
                problems.push(format!(
                    "no table of functions of the page lists `{}.{}`",
                    m.name, row.name
                ));
            }
            if row.decoding.is_some() && !listed_decoders.contains(&(m.name, row.name)) {
                problems.push(format!(
                    "no table of type arguments of the page lists `{}.{}`",
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

/// The line of the language card (`docs/language-card.md`) after which
/// [`render_card`] writes the card from the registry.
pub const CARD_MARKER: &str = "<!-- Generated from the builtin registry from here on: \
     SILT_BLESS=1 cargo nextest run --test meta language_card -->";

/// The language card `card` with its standard library written from the
/// registry: the prose up to [`CARD_MARKER`] as it is, then the
/// prelude's types and functions and each module's types, traits,
/// functions and constants, one declaration a line, whether the cargo
/// feature of a module is built or not. `None` when `card` has no
/// marker line.
pub fn render_card(card: &str) -> Option<String> {
    let (prose, _) = card.split_once(CARD_MARKER)?;
    let mut out = format!("{prose}{CARD_MARKER}\n");
    out.push_str("\n### Prelude\n\n```\n");
    out.push_str(&(prelude_lines().join("\n") + "\n```\n"));
    for module in &registry().modules {
        let heading = match module.feature {
            Some(feature) => format!("{} (cargo feature `{feature}`)", module.name),
            None => module.name.to_string(),
        };
        out.push_str(&format!("\n### {heading}\n\n```\n"));
        out.push_str(&(module_lines(module).join("\n") + "\n```\n"));
    }
    Some(out)
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

    /// A table of type arguments has its cells written from the
    /// decoders' rows; a function that decodes nothing in it is a
    /// problem, and so is a decoder that no such table of its page
    /// lists.
    #[test]
    fn a_table_of_type_arguments_is_written_from_the_rows() {
        let table = |rows: &str| {
            format!(
                "# toml\n\n| Function | Type argument |\n|---|---|\n{rows}\n\n\
                 | Function | Signature |\n|---|---|\n{}\n",
                registry()
                    .module("toml")
                    .expect("toml")
                    .rows
                    .iter()
                    .map(|row| format!("| `{}` | x |", row.name))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let page = render_page(&table(
            "| `toml.parse` | stale |\n| `toml.parse_list` | stale |\n| `toml.parse_map` | stale |",
        ))
        .expect("a page");
        assert!(
            page.contains("| `toml.parse` | a record type, an enum type, or a type with a `json.Decode` impl |"),
            "{page}"
        );
        assert!(
            page.contains(
                "| `toml.parse_map` | `Int`, `Float`, `String`, `Bool`, a record type, an enum type, or a type with a `json.Decode` impl |"
            ),
            "{page}"
        );
        let problems = render_page(&table("| `toml.stringify` | x |")).expect_err("problems");
        assert!(
            problems.contains(
                &"a table of type arguments of `toml` lists `stringify`, which decodes nothing"
                    .to_string()
            ),
            "{problems:?}"
        );
        assert!(
            problems
                .contains(&"no table of type arguments of the page lists `toml.parse`".to_string()),
            "{problems:?}"
        );
    }

    /// Every built module has a sentence of its own: the one that opens
    /// its part of the page, also where the page is shared (`env` and
    /// `fs` are on `io`'s page, below `io`'s part).
    #[test]
    fn every_builtin_module_has_the_first_sentence_of_its_own_part() {
        let docs = module_docs();
        for module in registry().enabled_modules() {
            let text = &docs[module.name];
            assert!(
                text.ends_with('.') && !text.contains('\n') && text.len() > 10,
                "`{}`: {text:?}",
                module.name
            );
        }
        assert_eq!(
            docs["json"],
            "Parse JSON strings into typed silt values and serialize values to JSON."
        );
        assert_eq!(docs["env"], "Environment variable access.");
        assert_eq!(
            docs["option"],
            "Functions for transforming and querying `Option(a)` values without pattern matching."
        );
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
        assert!(variants.len() > 50, "{} variants", variants.len());
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
        assert!(shared > 10, "{shared} pairs of one name");
    }

    #[test]
    fn a_page_is_named_after_its_modules() {
        let files: Vec<String> = pages().into_iter().map(|(file, _)| file).collect();
        assert!(files.contains(&"int-float.md".to_string()), "{files:?}");
        assert!(files.contains(&"io-fs.md".to_string()), "{files:?}");
    }

    /// On a page that is no module's, the prelude's functions are
    /// written from their rows: a table's signature and summary cells,
    /// a section's signature block; a table that lists some of them
    /// lists all of them.
    #[test]
    fn the_prelude_s_functions_are_written_from_the_rows() {
        let page = "# Globals\n\n| Name | Signature | Description |\n|---|---|---|\n\
                    | `print` | `stale` | stale |\n| `println` | `stale` | stale |\n\
                    | `panic` | `stale` | stale |\n| `Ok` | `(a) -> Result(a, e)` | kept |\n\n\
                    ## `panic`\n\n```\nstale\n```\n";
        let page = render_page(page).expect("a page");
        assert!(
            page.contains(
                "| `println` | `(a) -> () where a: Display` | Print a value with trailing newline |"
            ),
            "{page}"
        );
        assert!(
            page.contains("| `Ok` | `(a) -> Result(a, e)` | kept |"),
            "{page}"
        );
        assert!(
            page.contains("```\npanic(message: a) -> Never where a: Display\n```"),
            "{page}"
        );
        let problems =
            render_page("| Name | Signature | Description |\n|---|---|---|\n| `print` | x | y |\n")
                .expect_err("problems");
        assert!(
            problems.contains(&"no table of the page lists `println`".to_string()),
            "{problems:?}"
        );
    }
}
