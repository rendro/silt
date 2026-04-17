//! Build the definition map for a program: the top-level-declaration
//! symbols (functions, types, enum variants, trait names, top-level
//! `let` bindings) mapped to their `DefInfo` record.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::Symbol;
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
            } if matches!(pattern.kind, PatternKind::Ident(_)) => {
                if let PatternKind::Ident(name) = &pattern.kind {
                    defs.insert(
                        *name,
                        DefInfo {
                            span: *span,
                            ty: value.ty.clone(),
                            params: vec![],
                        },
                    );
                }
            }
            _ => {}
        }
    }
    defs
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
