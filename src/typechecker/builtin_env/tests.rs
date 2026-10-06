use super::super::test_helpers::*;
use super::super::*;
use super::builtin_env;

// ── The builtin scope ───────────────────────────────────────────

#[test]
fn the_builtin_scope_binds_the_prelude_and_the_registry_s_rows() {
    let env = builtin_env();
    for name in ["print", "println", "panic", "Some", "None", "Ok", "Err"] {
        assert!(env.root.lookup(intern(name)).is_some(), "{name} is bound");
    }
    for name in ["list.map", "math.pi", "Message", "ChannelResult.Message"] {
        assert!(env.root.lookup(intern(name)).is_some(), "{name} is bound");
    }
    // A builtin type is not a value.
    assert!(env.root.lookup(intern("Duration")).is_none());
}

/// Each declared type of the registry has exactly the builtin traits its
/// declaration derives: all four unless its module names fewer, so a new
/// type cannot end up with none by being left off a list.
#[test]
fn a_registry_type_derives_what_its_declaration_says() {
    use crate::builtins::registry::{DERIVED, registry};
    let env = builtin_env();
    let mut fewer = Vec::new();
    for (module, ty) in registry().types() {
        let built = module.is_none_or(|m| crate::module::missing_feature(m).is_none());
        if !built {
            continue;
        }
        let of = TypeRef::builtin(ty.name);
        let mut stamped: Vec<&str> = DERIVED
            .iter()
            .copied()
            .filter(|t| {
                env.tables
                    .trait_impl_set
                    .contains(&(TraitKey::builtin(t), of))
            })
            .collect();
        stamped.sort();
        let mut declared = ty.derives.to_vec();
        declared.sort();
        assert_eq!(stamped, declared, "the traits of {}", ty.name);
        if ty.derives.len() < DERIVED.len() {
            fewer.push(ty.name);
        }
    }
    fewer.sort();
    assert_eq!(
        fewer,
        ["ChannelOp", "Option", "Request", "Response", "Result"]
    );
}

/// Each row's scheme is what its signature says.
#[test]
fn a_row_s_scheme_is_its_signature() {
    let env = builtin_env();
    let (mut checker, _) = env.start();
    checker.tables = env.tables.clone();
    let mut show = |name: &str| {
        let scheme = env.root.lookup(intern(name)).expect("bound").clone();
        let constraints: Vec<String> = scheme
            .preds
            .iter()
            .filter_map(|pred| match pred {
                Pred::Trait { tr, .. } => Some(tr.name.to_string()),
                Pred::Anon { .. } => None,
            })
            .collect();
        let ty = checker.instantiate(&scheme);
        (format!("{ty}"), constraints, scheme.optional_last_param)
    };
    assert_eq!(
        show("map.get"),
        (
            "Fn(Map(_, _), _) -> Option(_)".to_string(),
            vec!["Hash".to_string()],
            false
        )
    );
    assert_eq!(
        show("json.parse"),
        (
            "Fn(String, type _) -> Result(_, JsonError)".to_string(),
            vec![],
            false
        )
    );
    assert_eq!(
        show("set.new"),
        ("Fn() -> Set(_)".to_string(), vec![], false)
    );
    assert_eq!(show("math.pi"), ("Float".to_string(), vec![], false));
    assert_eq!(
        show("test.assert"),
        ("Fn(Bool, String) -> ()".to_string(), vec![], true)
    );
    assert_eq!(
        show("time.sleep"),
        ("Fn(Duration) -> ()".to_string(), vec![], false)
    );
}

#[test]
fn test_builtin_type_signatures_returns_qualified_names() {
    let sigs = builtin_type_signatures();
    assert!(
        sigs.contains_key("list.map"),
        "list.map missing from signatures"
    );
    assert!(
        sigs.contains_key("string.split"),
        "string.split missing from signatures"
    );
    assert!(
        sigs.contains_key("math.sqrt"),
        "math.sqrt missing from signatures"
    );
    // Should not contain unqualified names
    assert!(
        !sigs.contains_key("print"),
        "unqualified 'print' should not be in qualified signatures"
    );
}

// ── Math module ─────────────────────────────────────────────────

#[test]
fn test_math_sqrt() {
    assert_no_errors(
        r#"
import math
fn main() {
  let x = math.sqrt(4.0)
  x
}
        "#,
    );
}

#[test]
fn test_math_trig_functions() {
    assert_no_errors(
        r#"
import math
fn main() {
  let a = math.sin(1.0)
  let b = math.cos(1.0)
  let c = math.tan(1.0)
  a + b + c
}
        "#,
    );
}

#[test]
fn test_math_pow() {
    assert_no_errors(
        r#"
import math
fn main() {
  math.pow(2.0, 10.0)
}
        "#,
    );
}

// ── Time module ─────────────────────────────────────────────────

#[test]
fn test_time_now() {
    assert_no_errors(
        r#"
import time
fn main() {
  time.now()
}
        "#,
    );
}

#[test]
fn test_time_sleep() {
    assert_no_errors(
        r#"
import time
fn main() {
  time.sleep(time.ms(100))
}
        "#,
    );
}

// ── HTTP module ─────────────────────────────────────────────────

#[test]
fn test_http_get_type() {
    assert_no_errors(
        r#"
import http
fn main() {
  http.get("http://example.com")
}
        "#,
    );
}

// ── FS module ───────────────────────────────────────────────────

#[test]
fn test_fs_exists_type_check() {
    assert_no_errors(
        r#"
import fs
fn main() {
  fs.exists("file.txt")
}
        "#,
    );
}

// ── Test module ─────────────────────────────────────────────────

#[test]
fn test_test_module_assert_eq() {
    assert_no_errors(
        r#"
import test
fn main() {
  test.assert_eq(1, 1)
}
        "#,
    );
}

// ── Builtin type mismatches ─────────────────────────────────────

#[test]
fn test_math_sqrt_wrong_type() {
    assert_has_error(
        r#"
import math
fn main() {
  math.sqrt("hello")
}
        "#,
        "type mismatch",
    );
}
