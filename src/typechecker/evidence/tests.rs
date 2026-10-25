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
/// the supertrait; the default method's `Self` is its first bound; a
/// builtin trait's bound passes no evidence (the value finds it).
#[test]
fn a_supertrait_is_a_step_from_the_bound_and_a_builtin_bound_passes_none() {
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
    // (A builtin trait is found by the value: its bound passes nothing.)
    assert!(order.is_none(), "{found:?}");
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
            Decl::TraitImpl(ti) if resolve(ti.trait_name) == "Sub" => {
                Some(ti.evidence.supers.clone())
            }
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

/// A bound an impl's method restates is the bound the trait declares,
/// not one of its own: in the impl for every tuple, `x: a` with the
/// restated `a: Greet` is the declared bound (the first), and `self`'s
/// trait comes after the declared bounds.
#[test]
fn a_restated_bound_is_the_declared_one_and_self_comes_after_them() {
    let (found, bodies) = sites(
        r#"
trait Greet { fn greet(self) -> String }
type K { K }
trait Greet for K { fn greet(self) -> String { "K" } }
trait Tag {
  fn tag(self) -> String
  fn with(self, x: a) -> String where a: Greet
}
trait Tag for Tuple {
  fn tag(self) -> String { "tuple" }
  fn with(self, x: a) -> String where a: Greet { "{Tag.tag(self)}+{Greet.greet(x)}" }
}
fn main() { println(Tag.with((1, 2), K)) }
"#,
    );
    let owner = body(&bodies, "Tuple.with");
    let recv = |name: &str| {
        (found.iter())
            .filter(|(n, _)| n == name)
            .find_map(|(_, site)| match site {
                Site::Method {
                    recv: Evidence::Param { owner: o, k },
                    ..
                } if *o == owner => Some(*k),
                _ => None,
            })
    };
    assert_eq!(recv("Greet.greet"), Some(0), "{found:?}");
    assert_eq!(recv("Tag.tag"), Some(1), "{found:?}");
}

/// A `where` clause a trait declares on its parameter is a bound of its
/// default methods, after their own: `Greet.greet` on a value of the
/// parameter's type names it.
#[test]
fn a_where_clause_on_a_trait_parameter_is_a_bound_of_its_defaults() {
    let (found, bodies) = sites(
        r#"
trait Greet { fn greet(self) -> String }
type K { K }
trait Greet for K { fn greet(self) -> String { "K" } }
trait Pr(a) where a: Greet {
  fn item(self) -> a
  fn pr(self) -> String { Greet.greet(Pr.item(self)) }
}
type Holder { Holder }
trait Pr(K) for Holder { fn item(self) -> K { K } }
fn main() { println(Pr.pr(Holder)) }
"#,
    );
    let owner = body(&bodies, "Pr.pr");
    let greet = (found.iter())
        .find(|(n, _)| n == "Greet.greet")
        .map(|(_, site)| site);
    assert!(
        matches!(
            greet,
            Some(Site::Method { recv: Evidence::Param { owner: o, k: 1 }, .. }) if *o == owner
        ),
        "{found:?}"
    );
}

/// A use of a definition inside the group it is checked with, where it
/// has one type (its own body too), and a `let`'s closure used inside
/// itself pass the evidence of their bounds from the user's own.
#[test]
fn a_use_inside_its_group_or_itself_passes_the_users_bounds() {
    let (found, bodies) = sites(
        r#"
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
fn ping(x, n) {
  match n {
    0 -> Greet.greet(x)
    _ -> pong(x, n - 1)
  }
}
fn pong(x, n) {
  match n {
    0 -> Greet.greet(x)
    _ -> ping(x, n - 1)
  }
}
fn main() {
  let go = { x, n ->
    match n {
      0 -> Greet.greet(x)
      _ -> go(x, n - 1)
    }
  }
  println(ping(1, 3))
  println(go(1, 2))
}
"#,
    );
    let param_of = |name: &str| {
        (found.iter())
            .filter(|(n, _)| n == name)
            .find_map(|(_, site)| match site {
                Site::Ref(bounds) => match bounds.as_slice() {
                    [Evidence::Param { owner, k: 0 }] => Some(*owner),
                    _ => None,
                },
                _ => None,
            })
    };
    assert_eq!(param_of("pong"), Some(body(&bodies, "ping")), "{found:?}");
    assert_eq!(param_of("ping"), Some(body(&bodies, "pong")), "{found:?}");
    assert!(param_of("go").is_some(), "{found:?}");
}
