//! Build the definition map for a program: the top-level-declaration
//! symbols (functions, types, enum variants, trait names, top-level
//! `let` bindings) mapped to their `DefInfo` record.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::lexer::Span;
use crate::types::Type;

use super::ast_walk::visit_expr_children;
use super::state::DefInfo;

// ── Build definitions map from declarations ────────────────────────

pub(super) fn build_definitions(program: &Program) -> HashMap<Symbol, DefInfo> {
    let mut defs = HashMap::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let fn_ty = build_fn_type(f);
                let params = fn_param_names(f);
                defs.insert(
                    f.name,
                    DefInfo {
                        span: f.span,
                        ty: fn_ty,
                        params,
                    },
                );
            }
            Decl::Type(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
                if let TypeBody::Enum(variants) = &t.body {
                    for v in variants {
                        defs.insert(
                            v.name,
                            DefInfo {
                                span: t.span,
                                ty: None,
                                params: vec![],
                            },
                        );
                    }
                }
            }
            Decl::Trait(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
            }
            Decl::Let {
                pattern,
                span,
                value,
                ..
            } => {
                // Walk the pattern recursively so top-level destructuring
                // (`let (a, b) = ...`, `let P { x, y } = ...`, etc.) also
                // registers each leaf identifier as a definition. Each leaf
                // of a compound pattern uses its own ident span; a bare
                // `let x = ...` preserves the old behaviour of using the
                // enclosing decl span so goto-def still lands on `let`.
                collect_let_pattern_defs(pattern, *span, value.ty.as_ref(), &mut defs);
            }
            _ => {}
        }
    }
    defs
}

/// Recursively walk a `let` pattern from a top-level `Decl::Let`, inserting a
/// `DefInfo` for every leaf identifier introduced by the pattern. Tuple,
/// Record, Constructor, List, and Or patterns are traversed so that
/// destructured top-level bindings (e.g. `let (a, b) = (1, 2)`) show up in
/// goto-def just like bare `let x = ...`.
///
/// `decl_span` is the enclosing decl's span (used as the goto-def target
/// for the bare `let x = ...` case). `value_ty` is the value expression's
/// type; when it matches the pattern's shape we propagate component types
/// to leaves so hover can render `Int` for `a` in `let (a, b) = (1, 2)`.
fn collect_let_pattern_defs(
    pattern: &Pattern,
    decl_span: Span,
    value_ty: Option<&Type>,
    defs: &mut HashMap<Symbol, DefInfo>,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            // For the bare `let x = ...` case the pattern IS the whole LHS
            // ident; prefer the enclosing decl span (the `let` keyword
            // position) to preserve the pre-fix goto-def behaviour. For
            // leaves of a compound pattern (e.g. `a` inside `(a, b)`) use
            // the ident's own span so goto-def lands on the identifier.
            let is_bare = pattern.span.offset == decl_span.offset
                && pattern.span.line == decl_span.line;
            defs.insert(
                *name,
                DefInfo {
                    span: if is_bare { decl_span } else { pattern.span },
                    ty: value_ty.cloned(),
                    params: vec![],
                },
            );
        }
        PatternKind::Tuple(pats) => {
            let elem_tys: Option<Vec<Type>> = match value_ty {
                Some(Type::Tuple(tys)) if tys.len() == pats.len() => Some(tys.clone()),
                _ => None,
            };
            for (i, p) in pats.iter().enumerate() {
                let inner = elem_tys.as_ref().and_then(|t| t.get(i));
                collect_let_pattern_defs(p, decl_span, inner, defs);
            }
        }
        PatternKind::Or(pats) => {
            for p in pats {
                collect_let_pattern_defs(p, decl_span, value_ty, defs);
            }
        }
        PatternKind::Constructor(ctor, fields) => {
            let inner_ty: Option<Type> = match (resolve(*ctor).as_str(), value_ty) {
                ("Ok", Some(Type::Generic(_, args))) => args.first().cloned(),
                ("Err", Some(Type::Generic(_, args))) => args.get(1).cloned(),
                ("Some", Some(Type::Generic(_, args))) => args.first().cloned(),
                _ => None,
            };
            for p in fields {
                collect_let_pattern_defs(p, decl_span, inner_ty.as_ref(), defs);
            }
        }
        PatternKind::Record { fields, .. } => {
            let field_tys: Option<Vec<(Symbol, Type)>> = match value_ty {
                Some(Type::Record(_, fs)) => Some(fs.clone()),
                _ => None,
            };
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_let_pattern_defs(p, decl_span, ty.as_ref(), defs);
                } else if resolve(*name) != "_" {
                    defs.insert(
                        *name,
                        DefInfo {
                            // No dedicated Pattern node for the shorthand
                            // field binding; fall back to the decl span.
                            span: decl_span,
                            ty: lookup_field_ty(*name),
                            params: vec![],
                        },
                    );
                }
            }
        }
        PatternKind::List(pats, rest) => {
            let (elem_ty, list_ty): (Option<Type>, Option<Type>) = match value_ty {
                Some(t @ Type::List(inner)) => (Some((**inner).clone()), Some(t.clone())),
                _ => (None, None),
            };
            for p in pats {
                collect_let_pattern_defs(p, decl_span, elem_ty.as_ref(), defs);
            }
            if let Some(r) = rest {
                collect_let_pattern_defs(r, decl_span, list_ty.as_ref(), defs);
            }
        }
        _ => {}
    }
}

pub(super) fn fn_param_names(f: &FnDecl) -> Vec<String> {
    f.params
        .iter()
        .map(|p| match &p.pattern.kind {
            PatternKind::Ident(name) => name.to_string(),
            _ => "_".to_string(),
        })
        .collect()
}

/// Build a function's type signature from its typed body.
/// Extracts parameter types from the body expression's typed sub-expressions.
pub(super) fn build_fn_type(f: &FnDecl) -> Option<Type> {
    // After type checking, the body has a resolved type (the return type).
    let ret_ty = f.body.ty.as_ref()?;

    // Extract param types: each param pattern may have been given a type
    // during checking. We look at the body — if it's a block, the params
    // were bound there. But the simplest reliable source is the function's
    // own usage. As a practical approach: walk the body to find Ident nodes
    // matching param names and grab their types.
    let param_names: Vec<Symbol> = f
        .params
        .iter()
        .filter_map(|p| {
            if let PatternKind::Ident(name) = &p.pattern.kind {
                Some(*name)
            } else {
                None
            }
        })
        .collect();

    let mut param_types = Vec::new();
    for name in &param_names {
        if let Some(ty) = find_param_type(&f.body, *name) {
            param_types.push(ty);
        } else {
            return None; // Can't determine a param type
        }
    }

    Some(Type::Fun(param_types, Box::new(ret_ty.clone())))
}

/// Find the type of the first Ident expression matching `name` in the body.
pub(super) fn find_param_type(expr: &Expr, name: Symbol) -> Option<Type> {
    if let ExprKind::Ident(n) = &expr.kind
        && *n == name
    {
        return expr.ty.clone();
    }
    // Search children
    let mut result = None;
    visit_expr_children(expr, |child| {
        if result.is_none() {
            result = find_param_type(child, name);
        }
    });
    result
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;

    // ── build_definitions ─────────────────────────────────────────

    #[test]
    fn test_build_definitions_from_program() {
        let source =
            "fn add(a, b) { a + b }\ntype Color {\n  Red,\n  Green,\n  Blue,\n}\nlet x = 42";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("add")), "should have fn 'add'");
        assert!(
            defs.contains_key(&intern("Color")),
            "should have type 'Color'"
        );
        assert!(
            defs.contains_key(&intern("Red")),
            "should have variant 'Red'"
        );
        assert!(
            defs.contains_key(&intern("Green")),
            "should have variant 'Green'"
        );
        assert!(
            defs.contains_key(&intern("Blue")),
            "should have variant 'Blue'"
        );
        assert!(
            defs.contains_key(&intern("x")),
            "should have let binding 'x'"
        );
    }

    #[test]
    fn test_build_definitions_fn_has_params() {
        let source = "fn greet(name, times) { name }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let defs = build_definitions(&program);

        let def = defs.get(&intern("greet")).unwrap();
        assert_eq!(def.params, vec!["name", "times"]);
    }

    // ── build_definitions: traits and let bindings ────────────────

    #[test]
    fn test_build_definitions_trait() {
        let source = "trait Printable {\n  fn show(self) -> String\n}\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(
            defs.contains_key(&intern("Printable")),
            "should have trait 'Printable'"
        );
    }

    #[test]
    fn test_build_definitions_let_type() {
        let source = "let x = 42\nfn main() { x }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        let def = defs.get(&intern("x")).expect("should have 'x'");
        assert_eq!(def.ty, Some(Type::Int));
    }

    // ── document_symbols via build_definitions ────────────────────

    #[test]
    fn test_build_definitions_enum_variants() {
        let source = "type Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("Shape")));
        assert!(defs.contains_key(&intern("Circle")));
        assert!(defs.contains_key(&intern("Rect")));
    }

    #[test]
    fn test_build_definitions_multiple_functions() {
        let source = "fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("add")));
        assert!(defs.contains_key(&intern("sub")));
        let add = defs.get(&intern("add")).unwrap();
        assert_eq!(add.params, vec!["a", "b"]);
        // Type should be (Int, Int) -> Int after inference
        assert!(add.ty.is_some());
    }

    // ── build_fn_type ────────────────────────────────────────────

    #[test]
    fn test_build_fn_type_simple() {
        let source = "fn double(n) { n * 2 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        if let Decl::Fn(f) = &program.decls[0] {
            let ty = build_fn_type(f);
            assert_eq!(ty, Some(Type::Fun(vec![Type::Int], Box::new(Type::Int))));
        } else {
            panic!("expected Fn decl");
        }
    }

    #[test]
    fn test_fn_param_names() {
        let source = "fn add(x, y) { x + y }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();

        if let Decl::Fn(f) = &program.decls[0] {
            let names = fn_param_names(f);
            assert_eq!(names, vec!["x", "y"]);
        } else {
            panic!("expected Fn decl");
        }
    }
}
