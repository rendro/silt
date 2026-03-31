use crate::ast::*;
use crate::lexer::Lexer;
use crate::parser::Parser;

const INDENT: &str = "  ";

pub fn format(source: &str) -> Result<String, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|e| format!("lex error: {e}"))?;
    let program = Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {e}"))?;
    Ok(format_program(&program))
}

fn format_program(program: &Program) -> String {
    let mut parts = Vec::new();
    for decl in &program.decls {
        parts.push(format_decl(decl, 0));
    }
    let mut result = parts.join("\n\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn format_decl(decl: &Decl, depth: usize) -> String {
    match decl {
        Decl::Fn(f) => format_fn(f, depth),
        Decl::Type(t) => format_type(t, depth),
        Decl::Trait(t) => format_trait(t, depth),
        Decl::TraitImpl(t) => format_trait_impl(t, depth),
        Decl::Import(i) => format_import(i, depth),
    }
}

fn indent(depth: usize) -> String {
    INDENT.repeat(depth)
}

fn format_fn(f: &FnDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let pub_prefix = if f.is_pub { "pub " } else { "" };
    let params = f
        .params
        .iter()
        .map(|p| format_param(p))
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
        let clauses: Vec<String> = f
            .where_clauses
            .iter()
            .map(|(name, trait_name)| format!("{name}: {trait_name}"))
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

    let body = format_body(&f.body, depth);
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
    match &expr.kind {
        ExprKind::Block(stmts) => {
            if stmts.is_empty() {
                "{}".to_string()
            } else {
                let inner = stmts
                    .iter()
                    .map(|s| format_stmt(s, depth + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{{\n{inner}\n{}}}", indent(depth))
            }
        }
        _ => format!("{{\n{}{}\n{}}}", indent(depth + 1), format_expr(expr, depth + 1), indent(depth)),
    }
}

fn format_type(t: &TypeDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let pub_prefix = if t.is_pub { "pub " } else { "" };
    let params = if t.params.is_empty() {
        String::new()
    } else {
        format!("({})", t.params.join(", "))
    };

    match &t.body {
        TypeBody::Enum(variants) => {
            let vars: Vec<String> = variants
                .iter()
                .map(|v| {
                    if v.fields.is_empty() {
                        format!("{}{}", indent(depth + 1), v.name)
                    } else {
                        let fields: Vec<String> =
                            v.fields.iter().map(|f| format_type_expr(f)).collect();
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

fn format_trait(t: &TraitDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let methods: Vec<String> = t
        .methods
        .iter()
        .map(|m| format_fn(m, depth + 1))
        .collect();
    format!(
        "{prefix}trait {} {{\n{}\n{prefix}}}",
        t.name,
        methods.join("\n\n")
    )
}

fn format_trait_impl(t: &TraitImpl, depth: usize) -> String {
    let prefix = indent(depth);
    let methods: Vec<String> = t
        .methods
        .iter()
        .map(|m| format_fn(m, depth + 1))
        .collect();
    format!(
        "{prefix}trait {} for {} {{\n{}\n{prefix}}}",
        t.trait_name,
        t.target_type,
        methods.join("\n\n")
    )
}

fn format_import(i: &ImportTarget, depth: usize) -> String {
    let prefix = indent(depth);
    match i {
        ImportTarget::Module(name) => format!("{prefix}import {name}"),
        ImportTarget::Items(module, items) => {
            format!("{prefix}import {module}.{{ {} }}", items.join(", "))
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
            format!(
                "{prefix}let {pat}{ty_str} = {}",
                format_expr(value, depth)
            )
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
            if s.contains('.') {
                s
            } else {
                format!("{s}.0")
            }
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
        ExprKind::Ident(name) => name.clone(),

        ExprKind::List(elems) => {
            if elems.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = elems.iter().map(|e| format_expr(e, depth)).collect();
                format!("[{}]", items.join(", "))
            }
        }

        ExprKind::Map(pairs) => {
            if pairs.is_empty() {
                "#{}".to_string()
            } else {
                let items: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| {
                        format!("{}: {}", format_expr(k, depth), format_expr(v, depth))
                    })
                    .collect();
                format!("#{{ {} }}", items.join(", "))
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
            format!(
                "{}..{}",
                format_expr(start, depth),
                format_expr(end, depth)
            )
        }

        ExprKind::QuestionMark(expr) => {
            format!("{}?", format_expr(expr, depth))
        }

        ExprKind::Call(callee, args) => {
            let callee_str = format_expr(callee, depth);
            // Trailing closure detection: if last arg is a lambda, format differently
            if let Some((last, init)) = args.split_last() {
                if matches!(last.kind, ExprKind::Lambda { .. }) {
                    let lambda_str = format_trailing_closure(last, depth);
                    if init.is_empty() {
                        return format!("{callee_str} {lambda_str}");
                    } else {
                        let arg_strs: Vec<String> =
                            init.iter().map(|a| format_expr(a, depth)).collect();
                        return format!(
                            "{callee_str}({}) {lambda_str}",
                            arg_strs.join(", ")
                        );
                    }
                }
            }
            let arg_strs: Vec<String> = args.iter().map(|a| format_expr(a, depth)).collect();
            format!("{callee_str}({})", arg_strs.join(", "))
        }

        ExprKind::Lambda { params, body } => {
            let param_strs: Vec<String> = params.iter().map(|p| format_param(p)).collect();
            let params_str = param_strs.join(", ");
            format!(
                "fn({params_str}) {}",
                format_body(body, depth)
            )
        }

        ExprKind::FieldAccess(expr, field) => {
            format!("{}.{field}", format_expr(expr, depth))
        }

        ExprKind::RecordCreate { name, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| {
                    format!("{fname}: {}", format_expr(fexpr, depth))
                })
                .collect();
            format!("{name} {{ {} }}", field_strs.join(", "))
        }

        ExprKind::RecordUpdate { expr, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| {
                    format!("{fname}: {}", format_expr(fexpr, depth))
                })
                .collect();
            format!(
                "{}.{{ {} }}",
                format_expr(expr, depth),
                field_strs.join(", ")
            )
        }

        ExprKind::Match { expr, arms } => {
            match expr {
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
                    format!(
                        "match {{\n{}\n{}}}",
                        arm_strs.join("\n"),
                        indent(depth)
                    )
                }
            }
        }

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
                let inner: Vec<String> =
                    stmts.iter().map(|s| format_stmt(s, depth + 1)).collect();
                format!("{{\n{}\n{}}}", inner.join("\n"), indent(depth))
            }
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
        let param_strs: Vec<String> = params.iter().map(|p| format_param(p)).collect();
        let params_str = param_strs.join(", ");
        if let ExprKind::Block(stmts) = &body.kind {
            if stmts.len() == 1 {
                if let Stmt::Expr(inner) = &stmts[0] {
                    return format!(
                        "{{ {params_str} -> {} }}",
                        format_expr(inner, depth)
                    );
                }
            }
            let inner: Vec<String> = stmts
                .iter()
                .map(|s| format_stmt(s, depth + 1))
                .collect();
            return format!(
                "{{ {params_str} ->\n{}\n{}}}",
                inner.join("\n"),
                indent(depth)
            );
        }
        format!(
            "{{ {params_str} -> {} }}",
            format_expr(body, depth)
        )
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
            format!(
                "{prefix}_ -> {}",
                format_expr(&arm.body, depth)
            )
        }
    } else {
        let pat = format_pattern(&arm.pattern);
        let guard = if let Some(g) = &arm.guard {
            format!(" when {}", format_expr(g, depth))
        } else {
            String::new()
        };
        format!(
            "{prefix}{pat}{guard} -> {}",
            format_expr(&arm.body, depth)
        )
    }
}

fn format_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Ident(name) => name.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Float(n) => {
            let s = n.to_string();
            if s.contains('.') {
                s
            } else {
                format!("{s}.0")
            }
        }
        Pattern::Bool(b) => b.to_string(),
        Pattern::StringLit(s) => format!("\"{}\"", escape_string(s)),
        Pattern::Tuple(pats) => {
            let items: Vec<String> = pats.iter().map(|p| format_pattern(p)).collect();
            format!("({})", items.join(", "))
        }
        Pattern::Constructor(name, pats) => {
            if pats.is_empty() {
                name.clone()
            } else {
                let items: Vec<String> = pats.iter().map(|p| format_pattern(p)).collect();
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
                        fname.clone()
                    }
                })
                .collect();
            let rest = if *has_rest { ", .." } else { "" };
            let name_str = name.as_deref().unwrap_or("");
            if name_str.is_empty() {
                format!("{{ {}{rest} }}", field_strs.join(", "))
            } else {
                format!("{name_str} {{ {}{rest} }}", field_strs.join(", "))
            }
        }
        Pattern::List(pats, rest) => {
            let mut items: Vec<String> = pats.iter().map(|p| format_pattern(p)).collect();
            if let Some(rest_pat) = rest {
                items.push(format!("..{}", format_pattern(rest_pat)));
            }
            format!("[{}]", items.join(", "))
        }
        Pattern::Or(alts) => {
            let items: Vec<String> = alts.iter().map(|p| format_pattern(p)).collect();
            items.join(" | ")
        }
        Pattern::Range(start, end) => format!("{start}..{end}"),
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
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Generic(name, args) => {
            let arg_strs: Vec<String> = args.iter().map(|a| format_type_expr(a)).collect();
            format!("{name}({})", arg_strs.join(", "))
        }
        TypeExpr::Tuple(elems) => {
            let items: Vec<String> = elems.iter().map(|e| format_type_expr(e)).collect();
            format!("({})", items.join(", "))
        }
        TypeExpr::Function(params, ret) => {
            let param_strs: Vec<String> = params.iter().map(|p| format_type_expr(p)).collect();
            format!("({}) -> {}", param_strs.join(", "), format_type_expr(ret))
        }
    }
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
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
