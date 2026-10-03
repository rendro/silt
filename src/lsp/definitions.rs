//! Build the definition map for a program: the top-level-declaration
//! symbols (functions, types, enum variants, trait names, top-level
//! `let` bindings) mapped to their `DefInfo` record.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::source::Span;
use crate::types::Type;

use super::ast_walk::visit_expr_children;
use super::state::DefInfo;

// ── Build definitions map from declarations ────────────────────────

/// `top_level` is the checker's type of each top-level value (`None`
/// for a workspace file that is only parsed).
pub(super) fn build_definitions(
    program: &Program,
    top_level: Option<&HashMap<Symbol, Type>>,
) -> HashMap<Symbol, DefInfo> {
    let checked = |name: Symbol| top_level.and_then(|types| types.get(&name)).cloned();
    let mut defs = HashMap::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let fn_ty = checked(f.name);
                let params = fn_param_names(f);
                defs.insert(
                    f.name,
                    DefInfo {
                        // Use the identifier's span, not the keyword span,
                        // so LSP rename / references / definition land on
                        // the name token. Round-63 B1 fix.
                        span: f.name_span,
                        ty: fn_ty,
                        params,
                        doc: f.doc.clone(),
                    },
                );
            }
            Decl::Type(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        // Use the identifier's span (round-63 B1).
                        span: t.name_span,
                        ty: None,
                        params: vec![],
                        doc: t.doc.clone(),
                    },
                );
                if let TypeBody::Enum(variants) = &t.body {
                    for v in variants {
                        defs.insert(
                            v.name,
                            DefInfo {
                                // Use the variant-name identifier's span
                                // (parser-recorded), NOT the enum decl's
                                // span: `t.span` sits on the `type`
                                // keyword, so rename of a variant from a
                                // usage site text-edited the keyword into
                                // the new name (`Disc Shape { ... }`) and
                                // goto-def landed on `type`. Same bug
                                // class as round-63 B1 / round-75 DX-2.
                                span: v.name_span,
                                ty: None,
                                params: vec![],
                                // Variants inherit the enum's doc
                                // string. Phase 1 does not surface
                                // per-variant docs (no syntax); the
                                // enum-level doc is the nearest useful
                                // bit of context for hover on `Some`
                                // or `Red`.
                                doc: t.doc.clone(),
                            },
                        );
                    }
                }
            }
            Decl::Trait(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        // Round-75 DX-2: use the trait-name identifier
                        // span, not the `trait` keyword span, so LSP
                        // rename / references / goto-def replace the
                        // name and not the keyword.
                        span: t.name_span,
                        ty: None,
                        params: vec![],
                        doc: t.doc.clone(),
                    },
                );
            }
            Decl::Let {
                pattern,
                name_span,
                value,
                doc,
                ..
            } => {
                // Walk the pattern recursively so top-level destructuring
                // (`let (a, b) = ...`, `let P { x, y } = ...`, etc.) also
                // registers each leaf identifier as a definition. Each leaf
                // of a compound pattern uses its own ident span; a bare
                // `let x = ...` uses its name span.
                let value_ty = match &pattern.kind {
                    PatternKind::Ident(name) => checked(*name).or_else(|| value.ty.clone()),
                    _ => value.ty.clone(),
                };
                collect_let_pattern_defs(
                    pattern,
                    *name_span,
                    value_ty.as_ref(),
                    doc.as_deref(),
                    true,
                    &mut defs,
                );
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
/// `name_span` is the bare `let x = ...` binding's name span. `value_ty`
/// is the value expression's
/// type; when it matches the pattern's shape we propagate component types
/// to leaves so hover can render `Int` for `a` in `let (a, b) = (1, 2)`.
fn collect_let_pattern_defs(
    pattern: &Pattern,
    name_span: Option<Span>,
    value_ty: Option<&Type>,
    doc: Option<&str>,
    is_top: bool,
    defs: &mut HashMap<Symbol, DefInfo>,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            // For the bare top-level `let x = ...` case use the binding's
            // name-identifier span (round-71 DX-1 fix), mirroring the
            // FnDecl/TypeDecl name_span pattern from round-63 B1. Without
            // this, LSP rename uses the `let` keyword span and clobbers
            // `let` (or `pub`) instead of replacing the name. For leaves
            // of a compound pattern (e.g. `a` inside `(a, b)`) use the
            // ident's own span so goto-def lands on the identifier.
            // `is_top` is true at the outermost call; goes false for any
            // recursion into sub-patterns so destructured leaves get
            // their own span.
            defs.insert(
                *name,
                DefInfo {
                    span: if is_top {
                        // Prefer the parser-recorded name_span when present.
                        // For a bare `Ident` pattern this is the same as
                        // `pattern.span`; the `unwrap_or` is just defensive.
                        name_span.unwrap_or(pattern.span)
                    } else {
                        pattern.span
                    },
                    ty: value_ty.cloned(),
                    params: vec![],
                    // Only the bare binding inherits the let's doc; a
                    // destructured leaf has no dedicated doc comment.
                    doc: if is_top {
                        doc.map(|s| s.to_string())
                    } else {
                        None
                    },
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
                collect_let_pattern_defs(p, None, inner, None, false, defs);
            }
        }
        PatternKind::Or(pats) => {
            for p in pats {
                collect_let_pattern_defs(p, None, value_ty, None, false, defs);
            }
        }
        PatternKind::Constructor {
            name: ctor,
            args: fields,
            ..
        } => {
            let inner_ty: Option<Type> = match (resolve(*ctor).as_str(), value_ty) {
                ("Ok", Some(Type::Generic(_, args))) => args.first().cloned(),
                ("Err", Some(Type::Generic(_, args))) => args.get(1).cloned(),
                ("Some", Some(Type::Generic(_, args))) => args.first().cloned(),
                _ => None,
            };
            for p in fields {
                collect_let_pattern_defs(p, None, inner_ty.as_ref(), None, false, defs);
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
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_let_pattern_defs(p, None, ty.as_ref(), None, false, defs);
                } else if resolve(*name) != "_" {
                    defs.insert(
                        *name,
                        DefInfo {
                            // The shorthand field binding is at the field
                            // name.
                            span: *name_span,
                            ty: lookup_field_ty(*name),
                            params: vec![],
                            doc: None,
                        },
                    );
                }
            }
        }
        PatternKind::AnonRecord { fields, rest } => {
            // Round-62 B9: anonymous-record destructure at top-level
            // (`let { x, y } = some_anon_record`). Mirrors the nominal
            // `Record` case above.
            let field_tys: Option<Vec<(Symbol, Type)>> = match value_ty {
                Some(Type::Record(_, fs)) => Some(fs.clone()),
                _ => None,
            };
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_let_pattern_defs(p, None, ty.as_ref(), None, false, defs);
                } else if resolve(*name) != "_" {
                    defs.insert(
                        *name,
                        DefInfo {
                            span: *name_span,
                            ty: lookup_field_ty(*name),
                            params: vec![],
                            doc: None,
                        },
                    );
                }
            }
            // Round-101: the named rest binder (`{ x, ...rest }`) binds
            // `rest` — mirror the typechecker's `collect_pattern_vars`.
            if let Some((r, r_span)) = rest
                && resolve(*r) != "_"
            {
                defs.insert(
                    *r,
                    DefInfo {
                        span: *r_span,
                        ty: None,
                        params: vec![],
                        doc: None,
                    },
                );
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                collect_let_pattern_defs(p, None, None, None, false, defs);
            }
        }
        PatternKind::List(pats, rest) => {
            let (elem_ty, list_ty): (Option<Type>, Option<Type>) = match value_ty {
                Some(t @ Type::List(inner)) => (Some((**inner).clone()), Some(t.clone())),
                _ => (None, None),
            };
            for p in pats {
                collect_let_pattern_defs(p, None, elem_ty.as_ref(), None, false, defs);
            }
            if let Some(r) = rest {
                collect_let_pattern_defs(r, None, list_ty.as_ref(), None, false, defs);
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
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));

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
        let program = crate::lsp::testing::parsed(source);
        let defs = build_definitions(&program, None);

        let def = defs.get(&intern("greet")).unwrap();
        assert_eq!(def.params, vec!["name", "times"]);
    }

    // ── build_definitions: traits and let bindings ────────────────

    #[test]
    fn test_build_definitions_trait() {
        let source = "trait Printable {\n  fn show(self) -> String\n}\nfn main() { 0 }";
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));

        assert!(
            defs.contains_key(&intern("Printable")),
            "should have trait 'Printable'"
        );
    }

    #[test]
    fn test_build_definitions_let_type() {
        let source = "let x = 42\nfn main() { x }";
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));

        let def = defs.get(&intern("x")).expect("should have 'x'");
        assert_eq!(def.ty, Some(Type::Int));
    }

    // ── document_symbols via build_definitions ────────────────────

    #[test]
    fn test_build_definitions_enum_variants() {
        let source = "type Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\nfn main() { 0 }";
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));

        assert!(defs.contains_key(&intern("Shape")));
        assert!(defs.contains_key(&intern("Circle")));
        assert!(defs.contains_key(&intern("Rect")));
    }

    #[test]
    fn test_build_definitions_multiple_functions() {
        let source = "fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { 0 }";
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));

        assert!(defs.contains_key(&intern("add")));
        assert!(defs.contains_key(&intern("sub")));
        let add = defs.get(&intern("add")).unwrap();
        assert_eq!(add.params, vec!["a", "b"]);
        // Type should be (Int, Int) -> Int after inference
        assert!(add.ty.is_some());
    }

    /// A function's type is the checker's.
    #[test]
    fn a_function_has_the_checker_type() {
        let source = "fn double(n) { n * 2 }";
        let (program, top_level) = crate::lsp::testing::checked(source);
        let defs = build_definitions(&program, Some(&top_level));
        assert_eq!(
            defs[&intern("double")].ty,
            Some(Type::Fun(vec![Type::Int], Box::new(Type::Int)))
        );
    }

    #[test]
    fn test_fn_param_names() {
        let source = "fn add(x, y) { x + y }";
        let program = crate::lsp::testing::parsed(source);

        if let Decl::Fn(f) = &program.decls[0] {
            let names = fn_param_names(f);
            assert_eq!(names, vec!["x", "y"]);
        } else {
            panic!("expected Fn decl");
        }
    }
}
