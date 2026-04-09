use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::lexer::Lexer;
use crate::parser::Parser;

const INDENT: &str = "  ";

// ── Comment extraction ──────────────────────────────────────────────

/// A standalone comment (on its own line) extracted from source.
#[derive(Debug, Clone)]
struct Comment {
    line: usize,  // 1-based line number where the comment starts
    text: String, // the raw comment text including `--` or `{- ... -}`
}

/// Extract standalone comments from source text.
///
/// A "standalone" comment is one that occupies its own line(s) — the line has
/// only whitespace before the comment marker and nothing after it (for line
/// comments) or the block comment starts on its own line.
fn extract_comments(source: &str) -> Vec<Comment> {
    let mut comments = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Line comment: entire line is `-- ...`
        if trimmed.starts_with("--") {
            comments.push(Comment {
                line: i + 1, // 1-based
                text: line.to_string(),
            });
            i += 1;
            continue;
        }

        // Block comment starting on its own line
        if trimmed.starts_with("{-") {
            let mut block = String::new();
            let start_line = i + 1; // 1-based
            // Accumulate lines until we close all nested block comments
            let mut depth: i32 = 0;
            let mut found_end = false;
            while i < lines.len() {
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(lines[i]);
                // Count openers and closers in this line
                let mut chars = lines[i].chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '{' && chars.peek() == Some(&'-') {
                        chars.next();
                        depth += 1;
                    } else if ch == '-' && chars.peek() == Some(&'}') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            // Check if there's only whitespace after the closer
                            let rest: String = chars.collect();
                            if rest.trim().is_empty() {
                                found_end = true;
                            } else {
                                // Not a standalone block comment — skip
                                found_end = false;
                            }
                            break;
                        }
                    }
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            if found_end || depth == 0 {
                comments.push(Comment {
                    line: start_line,
                    text: block,
                });
            }
            continue;
        }

        i += 1;
    }
    comments
}

/// Get the start line (1-based) of a declaration from its span, if available.
fn decl_start_line(decl: &Decl) -> Option<usize> {
    match decl {
        Decl::Fn(f) => Some(f.span.line),
        Decl::Type(t) => Some(t.span.line),
        Decl::Trait(t) => Some(t.span.line),
        Decl::TraitImpl(t) => Some(t.span.line),
        Decl::Import(_) => None, // no span on ImportTarget
        Decl::Let { span, .. } => Some(span.line),
    }
}

/// Get the start line (1-based) of a statement from its contained expression spans.
fn stmt_start_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Let { value, .. } => value.span.line,
        Stmt::Expr(expr) => expr.span.line,
        Stmt::When { expr, .. } => expr.span.line,
        Stmt::WhenBool { condition, .. } => condition.span.line,
    }
}

/// Find 1-based line numbers of top-level `import` statements in source.
fn find_import_lines(source: &str) -> Vec<usize> {
    let mut result = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed == "import" {
            result.push(i + 1); // 1-based
        }
    }
    result
}

/// Resolve the start line for every declaration. For most decls we use the
/// AST span; for `Import` (which has no span) we match against source lines.
fn resolve_decl_lines(decls: &[Decl], source: &str) -> Vec<usize> {
    let import_lines = find_import_lines(source);
    let mut import_idx = 0;
    let mut result = Vec::with_capacity(decls.len());
    for decl in decls {
        if let Some(line) = decl_start_line(decl) {
            result.push(line);
        } else {
            // Import without span — use next available import line from source
            if import_idx < import_lines.len() {
                result.push(import_lines[import_idx]);
                import_idx += 1;
            } else {
                // Fallback: use 0 so comments before it won't be lost
                result.push(0);
            }
        }
    }
    result
}

/// Check whether a declaration has a block body (i.e. uses `{ ... }` not `= expr`).
fn decl_has_block_body(decl: &Decl) -> bool {
    match decl {
        Decl::Fn(f) => matches!(f.body.kind, ExprKind::Block(_)),
        Decl::Trait(_) | Decl::TraitImpl(_) | Decl::Type(_) => true,
        Decl::Import(_) | Decl::Let { .. } => false,
    }
}

/// Compute the end line (1-based, inclusive) for each declaration by scanning
/// the source for balanced braces starting from each declaration's start line.
/// For single-line declarations (simple fn, import, let), the end line equals
/// the start line.
fn resolve_decl_end_lines(decls: &[Decl], decl_lines: &[usize], source: &str) -> Vec<usize> {
    let source_lines: Vec<&str> = source.lines().collect();
    let mut result = Vec::with_capacity(decls.len());

    for (i, decl) in decls.iter().enumerate() {
        if !decl_has_block_body(decl) {
            // Single-line declaration — end is same as start
            result.push(decl_lines[i]);
            continue;
        }

        // Scan from the declaration's start line to find the matching closing brace.
        let start = decl_lines[i]; // 1-based
        let mut depth: i32 = 0;
        let mut end_line = start;
        let mut found_open = false;

        for line_idx in (start - 1)..source_lines.len() {
            let line = source_lines[line_idx];
            // Skip content inside strings and comments for brace counting
            for ch in line.chars() {
                if ch == '{' {
                    depth += 1;
                    found_open = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            if found_open && depth == 0 {
                end_line = line_idx + 1; // 1-based
                break;
            }
        }

        result.push(end_line);
    }

    result
}

// ── Public entry point ──────────────────────────────────────────────

pub fn format(source: &str) -> Result<String, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|e| format!("lex error: {e}"))?;
    let program = Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {e}"))?;
    Ok(format_program_with_comments(&program, source))
}

fn format_program_with_comments(program: &Program, source: &str) -> String {
    if program.decls.is_empty() {
        // Even with no declarations, there might be comments
        let comments = extract_comments(source);
        if comments.is_empty() {
            return String::from("\n");
        }
        let mut result: String = comments
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    let comments = extract_comments(source);
    let decl_lines = resolve_decl_lines(&program.decls, source);
    let decl_end_lines = resolve_decl_end_lines(&program.decls, &decl_lines, source);

    // Partition comments into:
    // - top-level buckets (between declarations)
    // - body comments (inside a declaration's body)
    let n = program.decls.len();
    let mut buckets: Vec<Vec<&Comment>> = vec![Vec::new(); n + 1];
    // body_comments[i] holds comments inside decl[i]'s body
    let mut body_comments: Vec<Vec<&Comment>> = vec![Vec::new(); n];

    for comment in &comments {
        // A comment is inside decl[i]'s body if its line is strictly between
        // the decl's start line and its end line (inclusive of end line, since
        // a comment before the closing `}` is still inside).
        let mut is_body = false;
        for i in 0..n {
            if comment.line > decl_lines[i] && comment.line <= decl_end_lines[i] {
                body_comments[i].push(comment);
                is_body = true;
                break;
            }
        }
        if !is_body {
            // Top-level comment: place in the appropriate bucket.
            let mut placed = false;
            for (i, &dline) in decl_lines.iter().enumerate() {
                if comment.line < dline {
                    buckets[i].push(comment);
                    placed = true;
                    break;
                }
            }
            if !placed {
                buckets[n].push(comment);
            }
        }
    }

    // Separate imports from other declarations, sort imports alphabetically.
    let mut import_strs: Vec<String> = Vec::new();
    let mut has_imports = false;

    let formatted_decls: Vec<String> = program
        .decls
        .iter()
        .enumerate()
        .map(|(i, d)| format_decl_with_comments(d, 0, &body_comments[i]))
        .collect();

    // Collect and sort import strings; track which decl indices are imports.
    let mut is_import = vec![false; program.decls.len()];
    for (i, decl) in program.decls.iter().enumerate() {
        if matches!(decl, Decl::Import(_)) {
            import_strs.push(formatted_decls[i].clone());
            is_import[i] = true;
            has_imports = true;
        }
    }
    import_strs.sort();

    let mut result = String::new();

    // Comments before first declaration
    for c in &buckets[0] {
        result.push_str(&c.text);
        result.push('\n');
    }
    if !buckets[0].is_empty() {
        result.push('\n');
    }

    // Emit sorted imports grouped together (single newline between them)
    for (i, imp) in import_strs.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        result.push_str(imp);
    }

    // Emit non-import declarations with blank lines between them
    let mut first_non_import = true;
    for (i, decl_str) in formatted_decls.iter().enumerate() {
        if is_import[i] {
            continue;
        }
        if has_imports || !first_non_import {
            // Blank line separator
            result.push_str("\n\n");
        }
        // Insert any comments that belong before this declaration
        // (skip bucket[0] since it was already emitted above)
        if i > 0 && !buckets[i].is_empty() {
            for c in &buckets[i] {
                result.push_str(&c.text);
                result.push('\n');
            }
            result.push('\n');
        }
        result.push_str(decl_str);
        first_non_import = false;
    }

    // Comments after last declaration
    if !buckets[n].is_empty() {
        result.push_str("\n\n");
        for c in &buckets[n] {
            result.push_str(&c.text);
            result.push('\n');
        }
    }

    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn format_decl_with_comments(decl: &Decl, depth: usize, body_comments: &[&Comment]) -> String {
    match decl {
        Decl::Fn(f) => format_fn_with_comments(f, depth, body_comments),
        Decl::Type(t) => format_type(t, depth),
        Decl::Trait(t) => format_trait_with_comments(t, depth, body_comments),
        Decl::TraitImpl(t) => format_trait_impl_with_comments(t, depth, body_comments),
        Decl::Import(i) => format_import(i, depth),
        Decl::Let {
            pattern,
            ty,
            value,
            is_pub,
            ..
        } => {
            let indent = "  ".repeat(depth);
            let pub_prefix = if *is_pub { "pub " } else { "" };
            let pat = format_pattern(pattern);
            let ty_str = if let Some(t) = ty {
                format!(": {}", format_type_expr(t))
            } else {
                String::new()
            };
            let val = format_expr(value, depth);
            format!("{indent}{pub_prefix}let {pat}{ty_str} = {val}")
        }
    }
}

fn indent(depth: usize) -> String {
    INDENT.repeat(depth)
}

fn format_fn_with_comments(f: &FnDecl, depth: usize, body_comments: &[&Comment]) -> String {
    let prefix = indent(depth);
    let pub_prefix = if f.is_pub { "pub " } else { "" };
    let params = f
        .params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    let ret = if let Some(ty) = &f.return_type {
        format!(" -> {}", format_type_expr(ty))
    } else {
        String::new()
    };
    let where_clause = if f.where_clauses.is_empty() {
        String::new()
    } else {
        // Group trait bounds by type param, preserving insertion order.
        let mut grouped: Vec<(Symbol, Vec<Symbol>)> = Vec::new();
        for (name, trait_name) in &f.where_clauses {
            if let Some(entry) = grouped.iter_mut().find(|(n, _)| n == name) {
                entry.1.push(*trait_name);
            } else {
                grouped.push((*name, vec![*trait_name]));
            }
        }
        let clauses: Vec<String> = grouped
            .iter()
            .map(|(name, traits)| {
                let trait_strs: Vec<String> = traits.iter().map(|t| resolve(*t)).collect();
                format!("{name}: {}", trait_strs.join(" + "))
            })
            .collect();
        format!(" where {}", clauses.join(", "))
    };

    // Check if body is a simple expression (single-expression function using =)
    if is_simple_body(&f.body) {
        return format!(
            "{prefix}{pub_prefix}fn {}({params}){ret}{where_clause} = {}",
            f.name,
            format_expr(&f.body, depth)
        );
    }

    let body = format_body_with_comments(&f.body, depth, body_comments);
    format!(
        "{prefix}{pub_prefix}fn {}({params}){ret}{where_clause} {body}",
        f.name
    )
}

fn is_simple_body(expr: &Expr) -> bool {
    // A body is "simple" if it's not a block — single expression fn
    !matches!(expr.kind, ExprKind::Block(_))
}

fn format_param(p: &Param) -> String {
    let pat = format_pattern(&p.pattern);
    if let Some(ty) = &p.ty {
        format!("{pat}: {}", format_type_expr(ty))
    } else {
        pat
    }
}

fn format_body(expr: &Expr, depth: usize) -> String {
    format_body_with_comments(expr, depth, &[])
}

fn format_body_with_comments(expr: &Expr, depth: usize, body_comments: &[&Comment]) -> String {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            if stmts.is_empty() {
                if body_comments.is_empty() {
                    "{}".to_string()
                } else {
                    // Emit comments inside an otherwise empty block
                    let comment_strs: Vec<String> = body_comments
                        .iter()
                        .map(|c| format!("{}{}", indent(depth + 1), c.text.trim()))
                        .collect();
                    format!("{{\n{}\n{}}}", comment_strs.join("\n"), indent(depth))
                }
            } else if body_comments.is_empty() {
                let inner = stmts
                    .iter()
                    .map(|s| format_stmt(s, depth + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{{\n{inner}\n{}}}", indent(depth))
            } else {
                // Interleave comments with statements based on line numbers.
                let inner = format_stmts_with_comments(stmts, depth + 1, body_comments);
                format!("{{\n{inner}\n{}}}", indent(depth))
            }
        }
        _ => format!(
            "{{\n{}{}\n{}}}",
            indent(depth + 1),
            format_expr(expr, depth + 1),
            indent(depth)
        ),
    }
}

/// Format a list of statements with interleaved comments.
///
/// Comments are placed before the first statement whose source line is greater
/// than the comment's line. Comments after all statements go at the end.
fn format_stmts_with_comments(stmts: &[Stmt], depth: usize, body_comments: &[&Comment]) -> String {
    let mut result = Vec::new();
    let mut comment_idx = 0;

    for stmt in stmts {
        let stmt_line = stmt_start_line(stmt);

        // Emit any comments that come before this statement
        while comment_idx < body_comments.len() && body_comments[comment_idx].line < stmt_line {
            result.push(format!(
                "{}{}",
                indent(depth),
                body_comments[comment_idx].text.trim()
            ));
            comment_idx += 1;
        }

        result.push(format_stmt(stmt, depth));
    }

    // Emit any remaining comments after the last statement
    while comment_idx < body_comments.len() {
        result.push(format!(
            "{}{}",
            indent(depth),
            body_comments[comment_idx].text.trim()
        ));
        comment_idx += 1;
    }

    result.join("\n")
}

fn format_type(t: &TypeDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let pub_prefix = if t.is_pub { "pub " } else { "" };
    let params = if t.params.is_empty() {
        String::new()
    } else {
        let param_strs: Vec<String> = t.params.iter().map(|p| resolve(*p)).collect();
        format!("({})", param_strs.join(", "))
    };

    match &t.body {
        TypeBody::Enum(variants) => {
            let vars: Vec<String> = variants
                .iter()
                .map(|v| {
                    if v.fields.is_empty() {
                        format!("{}{}", indent(depth + 1), v.name)
                    } else {
                        let fields: Vec<String> = v.fields.iter().map(format_type_expr).collect();
                        format!("{}{}({})", indent(depth + 1), v.name, fields.join(", "))
                    }
                })
                .collect();
            format!(
                "{prefix}{pub_prefix}type {}{params} {{\n{}\n{prefix}}}",
                t.name,
                vars.join(",\n")
            )
        }
        TypeBody::Record(fields) => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}{}: {}",
                        indent(depth + 1),
                        f.name,
                        format_type_expr(&f.ty)
                    )
                })
                .collect();
            format!(
                "{prefix}{pub_prefix}type {}{params} {{\n{},\n{prefix}}}",
                t.name,
                field_strs.join(",\n")
            )
        }
    }
}

fn format_trait_with_comments(t: &TraitDecl, depth: usize, body_comments: &[&Comment]) -> String {
    let prefix = indent(depth);
    let methods: Vec<String> = partition_method_comments(&t.methods, body_comments)
        .into_iter()
        .zip(t.methods.iter())
        .map(|(mc, m)| format_fn_with_comments(m, depth + 1, &mc))
        .collect();
    format!(
        "{prefix}trait {} {{\n{}\n{prefix}}}",
        t.name,
        methods.join("\n\n")
    )
}

fn format_trait_impl_with_comments(
    t: &TraitImpl,
    depth: usize,
    body_comments: &[&Comment],
) -> String {
    let prefix = indent(depth);
    let methods: Vec<String> = partition_method_comments(&t.methods, body_comments)
        .into_iter()
        .zip(t.methods.iter())
        .map(|(mc, m)| format_fn_with_comments(m, depth + 1, &mc))
        .collect();
    format!(
        "{prefix}trait {} for {} {{\n{}\n{prefix}}}",
        t.trait_name,
        t.target_type,
        methods.join("\n\n")
    )
}

/// Partition body comments among methods based on their line numbers.
/// Returns a Vec of Vec<&Comment>, one per method.
fn partition_method_comments<'a>(
    methods: &[FnDecl],
    body_comments: &[&'a Comment],
) -> Vec<Vec<&'a Comment>> {
    if methods.is_empty() || body_comments.is_empty() {
        return vec![Vec::new(); methods.len()];
    }

    let method_lines: Vec<usize> = methods.iter().map(|m| m.span.line).collect();
    let mut result: Vec<Vec<&'a Comment>> = vec![Vec::new(); methods.len()];

    for comment in body_comments {
        // Find which method this comment belongs to: the last method
        // whose start line is <= the comment's line.
        let mut assigned = false;
        for i in (0..methods.len()).rev() {
            if comment.line >= method_lines[i] {
                result[i].push(comment);
                assigned = true;
                break;
            }
        }
        if !assigned {
            // Comment before first method — skip (it's a structural comment
            // inside the trait block but before any methods).
            // For now, attach to the first method.
            if !methods.is_empty() {
                result[0].push(comment);
            }
        }
    }

    result
}

fn format_import(i: &ImportTarget, depth: usize) -> String {
    let prefix = indent(depth);
    match i {
        ImportTarget::Module(name) => format!("{prefix}import {name}"),
        ImportTarget::Items(module, items) => {
            let item_strs: Vec<String> = items.iter().map(|i| resolve(*i)).collect();
            format!("{prefix}import {module}.{{ {} }}", item_strs.join(", "))
        }
        ImportTarget::Alias(module, alias) => {
            format!("{prefix}import {module} as {alias}")
        }
    }
}

fn format_stmt(stmt: &Stmt, depth: usize) -> String {
    let prefix = indent(depth);
    match stmt {
        Stmt::Let { pattern, ty, value } => {
            let pat = format_pattern(pattern);
            let ty_str = if let Some(t) = ty {
                format!(": {}", format_type_expr(t))
            } else {
                String::new()
            };
            format!("{prefix}let {pat}{ty_str} = {}", format_expr(value, depth))
        }
        Stmt::When {
            pattern,
            expr,
            else_body,
        } => {
            let pat = format_pattern(pattern);
            format!(
                "{prefix}when {pat} = {} else {}",
                format_expr(expr, depth),
                format_body(else_body, depth)
            )
        }
        Stmt::WhenBool {
            condition,
            else_body,
        } => {
            format!(
                "{prefix}when {} else {}",
                format_expr(condition, depth),
                format_body(else_body, depth)
            )
        }
        Stmt::Expr(expr) => {
            format!("{prefix}{}", format_expr(expr, depth))
        }
    }
}

fn format_expr(expr: &Expr, depth: usize) -> String {
    format_expr_inner(&expr.kind, depth)
}

fn format_expr_inner(kind: &ExprKind, depth: usize) -> String {
    match kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Float(n) => {
            let s = n.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        }
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::StringLit(s) => format!("\"{}\"", escape_string(s)),
        ExprKind::StringInterp(parts) => {
            let mut result = String::from('"');
            for part in parts {
                match part {
                    StringPart::Literal(s) => result.push_str(&escape_string(s)),
                    StringPart::Expr(e) => {
                        result.push('{');
                        result.push_str(&format_expr(e, depth));
                        result.push('}');
                    }
                }
            }
            result.push('"');
            result
        }
        ExprKind::Unit => "()".to_string(),
        ExprKind::Ident(name) => resolve(*name),

        ExprKind::List(elems) => {
            if elems.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = elems
                    .iter()
                    .map(|elem| match elem {
                        ListElem::Single(e) => format_expr(e, depth),
                        ListElem::Spread(e) => format!("..{}", format_expr(e, depth)),
                    })
                    .collect();
                format!("[{}]", items.join(", "))
            }
        }

        ExprKind::Map(pairs) => {
            if pairs.is_empty() {
                "#{}".to_string()
            } else {
                let items: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| format!("{}: {}", format_expr(k, depth), format_expr(v, depth)))
                    .collect();
                format!("#{{ {} }}", items.join(", "))
            }
        }

        ExprKind::SetLit(elems) => {
            if elems.is_empty() {
                "#[]".to_string()
            } else {
                let items: Vec<String> = elems.iter().map(|e| format_expr(e, depth)).collect();
                format!("#[{}]", items.join(", "))
            }
        }

        ExprKind::Tuple(elems) => {
            let items: Vec<String> = elems.iter().map(|e| format_expr(e, depth)).collect();
            format!("({})", items.join(", "))
        }

        ExprKind::Binary(left, op, right) => {
            let l = format_expr_with_parens(left, *op, true, depth);
            let r = format_expr_with_parens(right, *op, false, depth);
            format!("{l} {op} {r}")
        }

        ExprKind::Unary(op, expr) => {
            let inner = format_expr(expr, depth);
            match op {
                UnaryOp::Neg => format!("-{inner}"),
                UnaryOp::Not => format!("!{inner}"),
            }
        }

        ExprKind::Pipe(_left, _right) => {
            // Collect all pipe stages
            let mut stages = Vec::new();
            collect_pipe_stages(kind, &mut stages);
            let first = format_expr_inner(stages[0], depth);
            let rest: Vec<String> = stages[1..]
                .iter()
                .map(|s| format!("{}|> {}", indent(depth), format_expr_inner(s, depth)))
                .collect();
            let mut result = first;
            for r in rest {
                result.push('\n');
                result.push_str(&r);
            }
            result
        }

        ExprKind::Range(start, end) => {
            format!("{}..{}", format_expr(start, depth), format_expr(end, depth))
        }

        ExprKind::QuestionMark(expr) => {
            format!("{}?", format_expr(expr, depth))
        }

        ExprKind::Ascription(expr, ty) => {
            format!("{} as {}", format_expr(expr, depth), format_type_expr(ty))
        }

        ExprKind::Call(callee, args) => {
            let callee_str = format_expr(callee, depth);
            // Trailing closure detection: if last arg is a lambda, format differently
            if let Some((last, init)) = args.split_last()
                && matches!(last.kind, ExprKind::Lambda { .. })
            {
                let lambda_str = format_trailing_closure(last, depth);
                if init.is_empty() {
                    return format!("{callee_str} {lambda_str}");
                } else {
                    let arg_strs: Vec<String> =
                        init.iter().map(|a| format_expr(a, depth)).collect();
                    return format!("{callee_str}({}) {lambda_str}", arg_strs.join(", "));
                }
            }
            let arg_strs: Vec<String> = args.iter().map(|a| format_expr(a, depth)).collect();
            format!("{callee_str}({})", arg_strs.join(", "))
        }

        ExprKind::Lambda { params, body } => {
            let param_strs: Vec<String> = params.iter().map(format_param).collect();
            let params_str = param_strs.join(", ");
            format!("fn({params_str}) {}", format_body(body, depth))
        }

        ExprKind::FieldAccess(expr, field) => {
            format!("{}.{field}", format_expr(expr, depth))
        }

        ExprKind::RecordCreate { name, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| format!("{fname}: {}", format_expr(fexpr, depth)))
                .collect();
            format!("{name} {{ {} }}", field_strs.join(", "))
        }

        ExprKind::RecordUpdate { expr, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| format!("{fname}: {}", format_expr(fexpr, depth)))
                .collect();
            format!(
                "{}.{{ {} }}",
                format_expr(expr, depth),
                field_strs.join(", ")
            )
        }

        ExprKind::Match { expr, arms } => match expr {
            Some(scrutinee) => {
                let scrutinee_str = format_expr(scrutinee, depth);
                let arm_strs: Vec<String> = arms
                    .iter()
                    .map(|arm| format_match_arm(arm, depth + 1, false))
                    .collect();
                format!(
                    "match {scrutinee_str} {{\n{}\n{}}}",
                    arm_strs.join("\n"),
                    indent(depth)
                )
            }
            None => {
                let arm_strs: Vec<String> = arms
                    .iter()
                    .map(|arm| format_match_arm(arm, depth + 1, true))
                    .collect();
                format!("match {{\n{}\n{}}}", arm_strs.join("\n"), indent(depth))
            }
        },

        ExprKind::Return(val) => {
            if let Some(e) = val {
                format!("return {}", format_expr(e, depth))
            } else {
                "return".to_string()
            }
        }

        ExprKind::Block(stmts) => {
            if stmts.is_empty() {
                "{}".to_string()
            } else {
                let inner: Vec<String> = stmts.iter().map(|s| format_stmt(s, depth + 1)).collect();
                format!("{{\n{}\n{}}}", inner.join("\n"), indent(depth))
            }
        }

        ExprKind::Loop { bindings, body } => {
            let body_str = format_expr(body, depth);
            if bindings.is_empty() {
                format!("loop {body_str}")
            } else {
                let binding_strs: Vec<String> = bindings
                    .iter()
                    .map(|(name, init)| format!("{name} = {}", format_expr(init, depth)))
                    .collect();
                format!("loop {} {body_str}", binding_strs.join(", "))
            }
        }

        ExprKind::Recur(args) => {
            if args.is_empty() {
                "loop()".to_string()
            } else {
                let arg_strs: Vec<String> = args.iter().map(|a| format_expr(a, depth)).collect();
                format!("loop({})", arg_strs.join(", "))
            }
        }
        ExprKind::FloatElse(expr, fallback) => {
            format!(
                "{} else {}",
                format_expr(expr, depth),
                format_expr(fallback, depth)
            )
        }
    }
}

fn collect_pipe_stages<'a>(kind: &'a ExprKind, stages: &mut Vec<&'a ExprKind>) {
    if let ExprKind::Pipe(left, right) = kind {
        collect_pipe_stages(&left.kind, stages);
        stages.push(&right.kind);
    } else {
        stages.push(kind);
    }
}

fn format_trailing_closure(expr: &Expr, depth: usize) -> String {
    if let ExprKind::Lambda { params, body } = &expr.kind {
        let param_strs: Vec<String> = params.iter().map(format_param).collect();
        let params_str = param_strs.join(", ");
        if let ExprKind::Block(stmts) = &body.kind {
            if stmts.len() == 1
                && let Stmt::Expr(inner) = &stmts[0]
            {
                return format!("{{ {params_str} -> {} }}", format_expr(inner, depth));
            }
            let inner: Vec<String> = stmts.iter().map(|s| format_stmt(s, depth + 1)).collect();
            return format!(
                "{{ {params_str} ->\n{}\n{}}}",
                inner.join("\n"),
                indent(depth)
            );
        }
        format!("{{ {params_str} -> {} }}", format_expr(body, depth))
    } else {
        format_expr(expr, depth)
    }
}

fn format_match_arm(arm: &MatchArm, depth: usize, guardless: bool) -> String {
    let prefix = indent(depth);
    if guardless {
        // Guardless match: print the condition expression or `_` for bare wildcard
        if let Some(g) = &arm.guard {
            format!(
                "{prefix}{} -> {}",
                format_expr(g, depth),
                format_expr(&arm.body, depth)
            )
        } else {
            format!("{prefix}_ -> {}", format_expr(&arm.body, depth))
        }
    } else {
        let pat = format_pattern(&arm.pattern);
        let guard = if let Some(g) = &arm.guard {
            format!(" when {}", format_expr(g, depth))
        } else {
            String::new()
        };
        format!("{prefix}{pat}{guard} -> {}", format_expr(&arm.body, depth))
    }
}

fn format_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Ident(name) => resolve(*name),
        Pattern::Int(n) => n.to_string(),
        Pattern::Float(n) => {
            let s = n.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        }
        Pattern::Bool(b) => b.to_string(),
        Pattern::StringLit(s) => format!("\"{}\"", escape_string(s)),
        Pattern::Tuple(pats) => {
            let items: Vec<String> = pats.iter().map(format_pattern).collect();
            format!("({})", items.join(", "))
        }
        Pattern::Constructor(name, pats) => {
            if pats.is_empty() {
                resolve(*name)
            } else {
                let items: Vec<String> = pats.iter().map(format_pattern).collect();
                format!("{name}({})", items.join(", "))
            }
        }
        Pattern::Record {
            name,
            fields,
            has_rest,
        } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, sub)| {
                    if let Some(p) = sub {
                        format!("{fname}: {}", format_pattern(p))
                    } else {
                        resolve(*fname)
                    }
                })
                .collect();
            let rest = if *has_rest { ", .." } else { "" };
            match name {
                Some(n) => format!("{n} {{ {}{rest} }}", field_strs.join(", ")),
                None => format!("{{ {}{rest} }}", field_strs.join(", ")),
            }
        }
        Pattern::List(pats, rest) => {
            let mut items: Vec<String> = pats.iter().map(format_pattern).collect();
            if let Some(rest_pat) = rest {
                items.push(format!("..{}", format_pattern(rest_pat)));
            }
            format!("[{}]", items.join(", "))
        }
        Pattern::Or(alts) => {
            let items: Vec<String> = alts.iter().map(format_pattern).collect();
            items.join(" | ")
        }
        Pattern::Range(start, end) => format!("{start}..{end}"),
        Pattern::FloatRange(start, end) => format!("{start}..{end}"),
        Pattern::Map(entries) => {
            let items: Vec<String> = entries
                .iter()
                .map(|(key, pat)| format!("\"{key}\": {}", format_pattern(pat)))
                .collect();
            format!("#{{ {} }}", items.join(", "))
        }
        Pattern::Pin(name) => format!("^{name}"),
    }
}

fn format_type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name) => resolve(*name),
        TypeExpr::Generic(name, args) => {
            let arg_strs: Vec<String> = args.iter().map(format_type_expr).collect();
            format!("{name}({})", arg_strs.join(", "))
        }
        TypeExpr::Tuple(elems) => {
            let items: Vec<String> = elems.iter().map(format_type_expr).collect();
            format!("({})", items.join(", "))
        }
        TypeExpr::Function(params, ret) => {
            let param_strs: Vec<String> = params.iter().map(format_type_expr).collect();
            format!("Fn({}) -> {}", param_strs.join(", "), format_type_expr(ret))
        }
        TypeExpr::SelfType => "Self".to_string(),
    }
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('{', "\\{")
        .replace('}', "\\}")
}

fn precedence(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Neq => 3,
        BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => 4,
        BinOp::Add | BinOp::Sub => 5,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
    }
}

fn format_expr_with_parens(expr: &Expr, parent_op: BinOp, is_left: bool, depth: usize) -> String {
    if let ExprKind::Binary(_, child_op, _) = &expr.kind {
        let parent_prec = precedence(parent_op);
        let child_prec = precedence(*child_op);
        // Need parens if child has lower precedence, or same precedence on the right
        // (for left-associative operators)
        if child_prec < parent_prec || (child_prec == parent_prec && !is_left) {
            return format!("({})", format_expr(expr, depth));
        }
    }
    format_expr(expr, depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comment_between_decls() {
        let source = r#"fn foo() = 1

-- helper function
fn bar() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- helper function"),
            "comment should be preserved"
        );
        assert!(result.contains("fn foo() = 1"));
        assert!(result.contains("fn bar() = 2"));
    }

    #[test]
    fn test_comment_before_first_decl() {
        let source = r#"-- module header
fn main() = 42
"#;
        let result = format(source).unwrap();
        assert!(
            result.starts_with("-- module header\n"),
            "header comment should be at top"
        );
        assert!(result.contains("fn main() = 42"));
    }

    #[test]
    fn test_multiple_comments_between_decls() {
        let source = r#"fn a() = 1

-- first comment
-- second comment
fn b() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- first comment\n-- second comment"),
            "multiple comments preserved"
        );
    }

    #[test]
    fn test_block_comment_preserved() {
        let source = r#"fn a() = 1

{- block comment -}
fn b() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("{- block comment -}"),
            "block comment should be preserved"
        );
    }

    #[test]
    fn test_no_comments_unchanged() {
        let source = r#"fn a() = 1

fn b() = 2
"#;
        let result = format(source).unwrap();
        let expected = "fn a() = 1\n\nfn b() = 2\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_idempotent_with_comments() {
        let source = r#"fn foo() = 1

-- a comment
fn bar() = 2
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_with_header_comment() {
        let source = r#"-- header
fn foo() = 1
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_with_multiple_comments() {
        let source = r#"-- header

fn a() = 1

-- between
fn b() = 2

-- another
fn c() = 3
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_comment_after_last_decl() {
        let source = r#"fn foo() = 1

-- trailing comment
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- trailing comment"),
            "trailing comment should be preserved"
        );
    }

    #[test]
    fn test_extract_comments_basic() {
        let comments = extract_comments("-- hello\nfn foo() = 1\n-- bye");
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[0].text, "-- hello");
        assert_eq!(comments[1].line, 3);
        assert_eq!(comments[1].text, "-- bye");
    }

    #[test]
    fn test_extract_block_comment() {
        let comments = extract_comments("{- block\ncomment -}\nfn foo() = 1");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, 1);
        assert!(comments[0].text.contains("{- block"));
        assert!(comments[0].text.contains("comment -}"));
    }

    // ── Idempotency tests ──────────────────────────────────────────

    #[test]
    fn test_idempotent_simple_fn() {
        let source = "fn add(a, b) = a + b\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_block_fn() {
        let source = r#"fn main() {
  let x = 42
  println(x)
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_imports_sorted() {
        let source = r#"import list
import channel
import string

fn main() = 42
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
        // Verify imports are sorted alphabetically
        let channel_pos = first.find("import channel").unwrap();
        let list_pos = first.find("import list").unwrap();
        let string_pos = first.find("import string").unwrap();
        assert!(channel_pos < list_pos, "channel should come before list");
        assert!(list_pos < string_pos, "list should come before string");
    }

    #[test]
    fn test_idempotent_match_expression() {
        let source = r#"fn classify(x) {
  match x {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_nested_match() {
        let source = r#"type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(s) {
  match s {
    Circle(r) -> match r > 10.0 {
      true -> "big circle"
      false -> "small circle"
    }
    Rect(w, h) -> match w == h {
      true -> "square"
      false -> "rectangle"
    }
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_pipe_chain() {
        let source = r#"import list

fn main() {
  [1, 2, 3]
  |> list.map(fn(x) { x * 2 })
  |> list.filter(fn(x) { x > 2 })
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_trait_and_impl() {
        let source = r#"trait Printable {
  fn show(self) -> String
}

trait Printable for Int {
  fn show(self) = "{self}"
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_record_type() {
        let source = r#"type User {
  name: String,
  age: Int,
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_lambda_in_call() {
        let source = r#"import list

fn main() {
  list.map([1, 2, 3], fn(x) {
    x * 2
  })
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_where_clause() {
        let source = "fn show(x) where x: Display = x.display()\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_complex_program() {
        let source = r#"-- Module header comment
import list
import string

-- A type definition
type Color {
  Red,
  Green,
  Blue,
}

-- Main function
fn main() {
  let colors = [Red, Green, Blue]
  colors
  |> list.map(fn(c) {
    match c {
      Red -> "red"
      Green -> "green"
      Blue -> "blue"
    }
  })
  |> string.join(", ")
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    // ── Edge case tests ────────────────────────────────────────────

    #[test]
    fn test_format_empty_source() {
        let result = format("").unwrap();
        assert_eq!(result, "\n");
    }

    #[test]
    fn test_format_only_comments() {
        let result = format("-- just a comment").unwrap();
        assert!(result.contains("-- just a comment"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_only_block_comment() {
        let result = format("{- a block comment -}").unwrap();
        assert!(result.contains("{- a block comment -}"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_single_expression_fn() {
        let result = format("fn add(a, b) = a + b\n").unwrap();
        assert_eq!(result, "fn add(a, b) = a + b\n");
    }

    #[test]
    fn test_format_empty_fn_body() {
        let result = format("fn noop() {}\n").unwrap();
        assert!(result.contains("fn noop()"));
    }

    #[test]
    fn test_format_pub_fn() {
        let result = format("pub fn add(a, b) = a + b\n").unwrap();
        assert!(result.starts_with("pub fn add"));
    }

    #[test]
    fn test_format_return_type_annotation() {
        let result = format("fn add(a: Int, b: Int) -> Int = a + b\n").unwrap();
        assert!(result.contains("-> Int"));
        assert!(result.contains("a: Int, b: Int"));
    }

    // ── Complex expression formatting ──────────────────────────────

    #[test]
    fn test_format_nested_match() {
        let source = r#"fn foo(x) {
  match x {
    Some(v) -> match v {
      1 -> "one"
      _ -> "other"
    }
    None -> "none"
  }
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("match x"));
        assert!(result.contains("Some(v) ->"));
        assert!(result.contains("None ->"));
    }

    #[test]
    fn test_format_pipe_chain() {
        let source = r#"import list
fn main() { [1, 2, 3] |> list.map(fn(x) { x * 2 }) |> list.filter(fn(x) { x > 2 }) }
"#;
        let result = format(source).unwrap();
        assert!(result.contains("|>"), "pipe operator should be preserved");
    }

    #[test]
    fn test_format_trailing_closure() {
        let source = r#"import list
fn main() {
  list.map([1, 2], fn(x) { x * 2 })
}
"#;
        let result = format(source).unwrap();
        // Should produce a trailing closure format
        assert!(result.contains("list.map"));
    }

    #[test]
    fn test_format_deeply_nested_block() {
        let source = r#"fn main() {
  let x = {
    let y = {
      let z = 42
      z
    }
    y
  }
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "deeply nested blocks should be idempotent");
    }

    #[test]
    fn test_format_loop_expression() {
        let source = "fn countdown(n) = loop i = n { match i { 0 -> 0 _ -> loop(i - 1) } }\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "loop formatting should be idempotent");
    }

    #[test]
    fn test_format_record_create() {
        let source = r#"type Point { x: Int, y: Int }
fn main() = Point { x: 1, y: 2 }
"#;
        let result = format(source).unwrap();
        assert!(result.contains("Point { x: 1, y: 2 }"));
    }

    #[test]
    fn test_format_map_literal() {
        let source = r#"fn main() = #{"a": 1, "b": 2}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("#{ "));
    }

    #[test]
    fn test_format_list_literal() {
        let result = format("fn main() = [1, 2, 3]\n").unwrap();
        assert!(result.contains("[1, 2, 3]"));
    }

    #[test]
    fn test_format_empty_list() {
        let result = format("fn main() = []\n").unwrap();
        assert!(result.contains("[]"));
    }

    #[test]
    fn test_format_tuple() {
        let result = format("fn main() = (1, 2, 3)\n").unwrap();
        assert!(result.contains("(1, 2, 3)"));
    }

    #[test]
    fn test_format_unary_ops() {
        let result = format("fn main() = -42\n").unwrap();
        assert!(result.contains("-42"));
    }

    #[test]
    fn test_format_not_op() {
        let result = format("fn main() = !true\n").unwrap();
        assert!(result.contains("!true"));
    }

    #[test]
    fn test_format_binary_precedence_parens() {
        // Ensure parentheses are added when needed for precedence
        let source = "fn main() = (1 + 2) * 3\n";
        let result = format(source).unwrap();
        assert!(result.contains("(1 + 2) * 3"));
    }

    #[test]
    fn test_format_string_interpolation() {
        let source = r#"fn greet(name) = "hello {name}"
"#;
        let result = format(source).unwrap();
        assert!(result.contains("{name}"));
    }

    #[test]
    fn test_format_question_mark() {
        let source = "fn try_it(x) = x?\n";
        let result = format(source).unwrap();
        assert!(result.contains("x?"));
    }

    #[test]
    fn test_format_range() {
        let result = format("fn main() = 1..10\n").unwrap();
        assert!(result.contains("1..10"));
    }

    #[test]
    fn test_format_when_bool_stmt() {
        let source = r#"fn main() {
  when true else {
    return 0
  }
  42
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "when-bool formatting should be idempotent");
    }

    #[test]
    fn test_format_when_pattern_stmt() {
        let source = r#"fn main() {
  when Some(x) = Some(42) else {
    return 0
  }
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "when-pattern formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_enum_type() {
        let result = format("type Color { Red, Green, Blue }\n").unwrap();
        assert!(result.contains("Red"));
        assert!(result.contains("Green"));
        assert!(result.contains("Blue"));
    }

    #[test]
    fn test_format_enum_with_fields() {
        let source = "type Shape { Circle(Float), Rect(Float, Float) }\n";
        let result = format(source).unwrap();
        assert!(result.contains("Circle(Float)"));
        assert!(result.contains("Rect(Float, Float)"));
    }

    #[test]
    fn test_format_import_sorting() {
        let source = r#"import string
import list
import channel

fn main() = 1
"#;
        let result = format(source).unwrap();
        let channel_pos = result.find("import channel").unwrap();
        let list_pos = result.find("import list").unwrap();
        let string_pos = result.find("import string").unwrap();
        assert!(
            channel_pos < list_pos,
            "imports should be sorted alphabetically"
        );
        assert!(
            list_pos < string_pos,
            "imports should be sorted alphabetically"
        );
    }

    #[test]
    fn test_format_selective_import() {
        let result = format("import list.{ map, filter }\nfn main() = 1\n").unwrap();
        assert!(result.contains("import list.{ map, filter }"));
    }

    #[test]
    fn test_format_alias_import() {
        let result = format("import list as l\nfn main() = 1\n").unwrap();
        assert!(result.contains("import list as l"));
    }

    #[test]
    fn test_format_guardless_match() {
        let source = r#"fn classify(x) {
  match {
    x > 100 -> "big"
    x > 0 -> "positive"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "guardless match formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_match_with_guard() {
        let source = r#"fn main() {
  match 42 {
    x when x > 0 -> "positive"
    _ -> "non-positive"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "match with guard formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_record_update() {
        let source = r#"type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  p.{ x: 10 }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "record update formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_let_with_type() {
        let source = r#"fn main() {
  let x: Int = 42
  x
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("let x: Int = 42"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_pub_let() {
        let source = "pub let version = 1\n";
        let result = format(source).unwrap();
        assert!(result.contains("pub let version = 1"));
    }

    // ── Pattern formatting ──────────────────────────────────────────

    #[test]
    fn test_format_or_pattern() {
        let source = r#"fn main() {
  match 1 {
    1 | 2 | 3 -> "low"
    _ -> "high"
  }
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("1 | 2 | 3"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_list_pattern() {
        let source = r#"fn main() {
  match [1, 2, 3] {
    [h, ..rest] -> h
    [] -> 0
  }
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("[h, ..rest]"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_pin_pattern() {
        let source = r#"fn main() {
  let x = 42
  match 42 {
    ^x -> "match"
    _ -> "no match"
  }
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("^x"));
    }

    #[test]
    fn test_format_field_access() {
        let result = format("fn main() = foo.bar.baz\n").unwrap();
        assert!(result.contains("foo.bar.baz"));
    }

    #[test]
    fn test_format_return_expression() {
        let source = r#"fn main() {
  return 42
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("return 42"));
    }

    #[test]
    fn test_format_return_void() {
        let source = r#"fn main() {
  return
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("return"));
    }

    // ── Idempotency: string interpolation ───────────────────────────

    #[test]
    fn test_idempotent_string_interpolation() {
        let source = r#"fn main() {
  let name = "world"
  println("hello {name}, count={1 + 2}")
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "string interpolation should be idempotent");
    }

    // ── Idempotency: map literals ───────────────────────────────────

    #[test]
    fn test_idempotent_map_literal() {
        let source = r#"fn main() {
  let m = #{"a": 1, "b": 2, "c": 3}
  m
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "map literal should be idempotent");
    }

    // ── Idempotency: tuple literals and patterns ────────────────────

    #[test]
    fn test_idempotent_tuple() {
        let source = r#"fn main() {
  let t = (1, "hello", true)
  match t {
    (1, s, _) -> s
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "tuple literal and pattern should be idempotent"
        );
    }

    // ── Idempotency: map patterns ───────────────────────────────────

    #[test]
    fn test_idempotent_map_pattern() {
        let source = r#"fn check(m) {
  match m {
    #{"key": v} -> v
    _ -> "missing"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "map pattern should be idempotent");
    }

    // ── Idempotency: constructor patterns ───────────────────────────

    #[test]
    fn test_idempotent_constructor_pattern() {
        let source = r#"fn unwrap(opt) {
  match opt {
    Some(x) -> x
    None -> 0
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "constructor pattern should be idempotent");
    }

    // ── Idempotency: negative number patterns ───────────────────────

    #[test]
    fn test_idempotent_negative_pattern() {
        let source = r#"fn classify(n) {
  match n {
    -1 -> "neg one"
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "negative pattern should be idempotent");
    }

    // ── Idempotency: range patterns ─────────────────────────────────

    #[test]
    fn test_idempotent_range_pattern() {
        let source = r#"fn classify(n) {
  match n {
    1..10 -> "small"
    10..100 -> "medium"
    _ -> "large"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "range pattern should be idempotent");
    }

    // ── Idempotency: complex type annotations ───────────────────────

    #[test]
    fn test_idempotent_type_annotations() {
        let source = r#"fn add(a: Int, b: Int) -> Int {
  a + b
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "type annotations should be idempotent");
    }

    // ── Idempotency: loop with multiple bindings ────────────────────

    #[test]
    fn test_idempotent_loop_bindings() {
        let source = r#"fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "loop with bindings should be idempotent");
    }

    // ── Idempotency: record pattern ─────────────────────────────────

    #[test]
    fn test_idempotent_record_pattern() {
        let source = r#"type Point { x: Int, y: Int }

fn origin(p) {
  match p {
    Point { x: 0, y: 0 } -> true
    _ -> false
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "record pattern should be idempotent");
    }

    // ── Idempotency: chained method calls ───────────────────────────

    #[test]
    fn test_idempotent_chained_calls() {
        let source = r#"import list

fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "chained calls should be idempotent");
    }

    // ── Body comment tests ──────────────────────────────────────────

    #[test]
    fn test_comment_inside_fn_body() {
        let source = r#"fn main() {
  -- setup variables
  let x = 42
  x
}
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- setup variables"),
            "comment inside fn body should be preserved, got: {result}"
        );
    }

    #[test]
    fn test_multiple_comments_inside_fn_body() {
        let source = r#"fn main() {
  -- first comment
  let x = 1
  -- second comment
  let y = 2
  x + y
}
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- first comment"),
            "first body comment should be preserved, got: {result}"
        );
        assert!(
            result.contains("-- second comment"),
            "second body comment should be preserved, got: {result}"
        );
    }

    #[test]
    fn test_body_comment_and_between_comment() {
        let source = r#"fn foo() {
  -- inside foo
  let x = 1
  x
}

-- between functions
fn bar() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- inside foo"),
            "body comment should be preserved, got: {result}"
        );
        assert!(
            result.contains("-- between functions"),
            "between-decl comment should be preserved, got: {result}"
        );
    }

    #[test]
    fn test_idempotent_body_comment() {
        let source = r#"fn main() {
  -- setup
  let x = 42
  -- use it
  println(x)
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "formatting with body comments should be idempotent"
        );
    }

    #[test]
    fn test_idempotent_body_and_toplevel_comments() {
        let source = r#"-- header
fn foo() {
  -- body comment
  let x = 1
  x
}

-- between
fn bar() = 2
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "formatting with mixed comments should be idempotent"
        );
    }

    #[test]
    fn test_comment_after_last_stmt_in_body() {
        let source = r#"fn main() {
  let x = 42
  -- trailing body comment
}
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- trailing body comment"),
            "trailing body comment should be preserved, got: {result}"
        );
    }
}
