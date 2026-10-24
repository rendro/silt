//! The evidence the checker writes on the places that pass some on.

use super::super::resolve::each_expr_mut;
use crate::ast::{Decl, Expr, ExprKind, Program};
use crate::evidence::{Evidence, Site};
use crate::intern::resolve;

/// Each place of `source` that passes evidence on, as `name` (a name)
/// or `obj.field` (an access), in the order of the declarations, with
/// the span of each function's body by its name.
fn sites(source: &str) -> (Vec<(String, Site)>, Vec<(String, crate::source::Span)>) {
    let (ast, diagnostics) = crate::session::testing::analyze_str(source);
    let errors: Vec<_> = diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let mut program: Program = (*ast).clone();
    let mut found = Vec::new();
    let mut bodies = Vec::new();
    let mut record = |expr: &mut Expr| {
        let Some(site) = &expr.ev else {
            return;
        };
        let name = match &expr.kind {
            ExprKind::Ident(name) => resolve(*name),
            ExprKind::FieldAccess(obj, field, _) => match &obj.kind {
                ExprKind::Ident(obj) => format!("{}.{}", resolve(*obj), resolve(*field)),
                _ => format!("_.{}", resolve(*field)),
            },
            _ => "?".to_string(),
        };
        found.push((name, (**site).clone()));
    };
    for decl in &mut program.decls {
        match decl {
            Decl::Fn(f) => {
                bodies.push((resolve(f.name), f.body.span));
                each_expr_mut(&mut f.body, &mut record);
            }
            Decl::TraitImpl(ti) => {
                for m in &mut ti.methods {
                    bodies.push((
                        format!("{}.{}", resolve(ti.target_type), resolve(m.name)),
                        m.body.span,
                    ));
                    each_expr_mut(&mut m.body, &mut record);
                }
            }
            Decl::Trait(t) => {
                for m in &mut t.methods {
                    bodies.push((
                        format!("{}.{}", resolve(t.name), resolve(m.name)),
                        m.body.span,
                    ));
                    each_expr_mut(&mut m.body, &mut record);
                }
            }
            _ => {}
        }
    }
    (found, bodies)
}

fn body(bodies: &[(String, crate::source::Span)], name: &str) -> crate::source::Span {
    bodies
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, span)| *span)
        .unwrap_or_else(|| panic!("no body '{name}' in {bodies:?}"))
}

const GREET: &str = r#"
trait Greet { fn greet(self) -> String }
type Box(a) { Full(a), Empty }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self {
      Full(x) -> Greet.greet(x)
      Empty -> "empty"
    }
  }
}
fn hello(x: a) -> String where a: Greet { Greet.greet(x) }
fn main() {
  println(hello(Full(5)))
}
"#;

/// A call through a trait on a bounded variable names the bound of
/// the function it is in; a use of a function with a bound names the
/// impl that answers it, with the evidence of what the impl's header
/// asks.
#[test]
fn a_bound_is_a_param_of_its_function_and_a_use_names_the_impl() {
    let (found, bodies) = sites(GREET);
    let call = |owner: &str| Site::Method {
        recv: Evidence::Param {
            owner: body(&bodies, owner),
            k: 0,
        },
        own: Vec::new(),
    };
    let in_hello = found.iter().filter(|(n, _)| n == "Greet.greet");
    assert!(
        in_hello.clone().any(|(_, site)| *site == call("hello")),
        "{found:?}"
    );
    // (In the impl's method: the header's bound `a: Greet`.)
    assert!(
        in_hello.clone().any(|(_, site)| *site == call("Box.greet")),
        "{found:?}"
    );
    let uses: Vec<&Site> = (found.iter())
        .filter(|(n, _)| n == "hello")
        .map(|(_, site)| site)
        .collect();
    match uses.as_slice() {
        [Site::Ref(bounds)] => match bounds.as_slice() {
            [Evidence::Impl { subs, .. }] => {
                assert!(matches!(subs.as_slice(), [Evidence::Impl { subs, .. }] if subs.is_empty()))
            }
            other => panic!("expected the impl for Box(Int), got {other:?}"),
        },
        other => panic!("expected one use of 'hello', got {other:?}"),
    }
}

/// A bound reached through a supertrait is the bound, then the step to
/// the supertrait; a trait the type has by its structure is
/// structural; the default method's `Self` is its first bound.
#[test]
fn a_supertrait_is_a_step_from_the_bound_and_structure_is_structural() {
    let (found, bodies) = sites(
        r#"
trait Named { fn name(self) -> String }
trait Loud: Named { fn shout(self) -> String { "{Named.name(self)}!" } }
trait Named for Int { fn name(self) -> String { "n" } }
trait Loud for Int {}
fn both(x: a) -> String where a: Loud { Named.name(x) }
fn order(x: a, y: a) -> Int where a: Compare { Compare.compare(x, y) }
fn main() {
  println(both(1))
  println(order(1, 2))
}
"#,
    );
    let named: Vec<&Site> = (found.iter())
        .filter(|(n, _)| n == "Named.name")
        .map(|(_, site)| site)
        .collect();
    let step = |owner: &str| Evidence::Super {
        of: Box::new(Evidence::Param {
            owner: body(&bodies, owner),
            k: 0,
        }),
        to: match &named[0] {
            Site::Method {
                recv: Evidence::Super { to, .. },
                ..
            } => *to,
            other => panic!("expected a step to the supertrait, got {other:?}"),
        },
    };
    assert!(
        named
            .iter()
            .any(|site| matches!(site, Site::Method { recv, .. } if *recv == step("both"))),
        "{found:?}"
    );
    assert!(
        named
            .iter()
            .any(|site| matches!(site, Site::Method { recv, .. } if *recv == step("Loud.shout"))),
        "{found:?}"
    );
    let order = (found.iter())
        .find(|(n, _)| n == "order")
        .map(|(_, site)| site);
    assert!(
        matches!(order, Some(Site::Ref(bounds)) if matches!(bounds.as_slice(), [Evidence::Structural { .. }])),
        "{found:?}"
    );
}

/// An impl records the evidence of its trait's supertraits from its
/// header: `Sub for Box(a) where a: Sub` has `Super` by the impl of
/// `Super` for `Box`, whose header's `a: Super` is the step from its
/// own `a: Sub`.
#[test]
fn an_impl_records_its_supertraits_from_its_header() {
    let source = r#"
trait Super { fn sup(self) -> String }
trait Sub: Super { fn sub(self) -> String { Super.sup(self) } }
type Box(a) { Bx(a) }
trait Super for Box(a) where a: Super { fn sup(self) -> String { "box" } }
trait Sub for Box(a) where a: Sub {}
fn main() { () }
"#;
    let (ast, _) = crate::session::testing::analyze_str(source);
    let supers = (ast.decls.iter())
        .find_map(|decl| match decl {
            Decl::TraitImpl(ti) if resolve(ti.trait_name) == "Sub" => Some(ti.supers.clone()),
            _ => None,
        })
        .expect("the impl of Sub");
    match supers.as_slice() {
        [(_, Evidence::Impl { subs, .. })] => match subs.as_slice() {
            [Evidence::Super { of, .. }] => assert_eq!(
                **of,
                Evidence::Param {
                    owner: crate::evidence::IMPL_HEADER,
                    k: 0
                }
            ),
            other => panic!("expected the step from the header's bound, got {other:?}"),
        },
        other => panic!("expected the impl of Super for Box, got {other:?}"),
    }
}

/// The `Error` of a builtin module's error enum is an impl the runtime
/// provides: no program wrote it.
#[test]
fn the_error_of_a_builtin_error_enum_is_provided_by_the_runtime() {
    let (found, _) = sites(
        r#"
import io
fn main() {
  match io.read_file("/nowhere") {
    Ok(_) -> ()
    Err(e) -> println(Error.message(e))
  }
}
"#,
    );
    let message = (found.iter())
        .find(|(n, _)| n == "Error.message")
        .map(|(_, site)| site);
    assert!(
        matches!(
            message,
            Some(Site::Method {
                recv: Evidence::Native { .. },
                ..
            })
        ),
        "{found:?}"
    );
}
