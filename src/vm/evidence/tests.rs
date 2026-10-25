//! The debug VM's check of a method call against the checker's
//! evidence.

use std::sync::Arc;

use super::{Env, Found, Ground, check_call};
use crate::bytecode::{Asm, Emitter, Globals, VmClosure};
use crate::defs::{DefId, TraitId, TypeId};
use crate::evidence::{Evidence, Site};
use crate::source::Span;
use crate::value::Value;

const TR: TraitId = TraitId(DefId(1));
const INT: TypeId = TypeId(DefId(2));
const STR: TypeId = TypeId(DefId(3));

/// The span that names the definition the running code is.
fn owner() -> Span {
    Span {
        file: crate::source::FileId::default(),
        start: 10,
        end: 20,
    }
}

/// Code whose first instruction (ending at offset 1) passes on `site`,
/// running where its definition's one bound is `bound`.
fn running(site: Site, bound: Ground) -> VmClosure {
    let span = Span::BUILTIN;
    let mut emitter = Emitter::new("f".into(), 0, span).expect("an emitter");
    emitter.emit(Asm::Unit, span).expect("unit");
    emitter.evidence_site(&site);
    emitter.emit(Asm::Return, span).expect("return");
    let mut closure = VmClosure::new(Arc::new(emitter.finish(0).expect("code")), Vec::new());
    closure.ev = Some(Arc::new(Env {
        owner: owner(),
        grounds: Some(vec![bound]),
        parent: None,
    }));
    closure
}

fn found(ty: TypeId, impl_found: bool) -> Found<'static> {
    Found {
        tr: Some(TR),
        ty,
        impl_found,
        native_row: false,
        method: "m",
        receiver: &Value::Unit,
    }
}

/// The receiver's evidence is the bound of the running definition: the
/// call agrees with the impl for the type the caller gave, and not with
/// one for another type or with none.
#[test]
fn a_call_on_a_bound_is_checked_against_the_callers_evidence() {
    let site = Site::Method {
        recv: Evidence::Param {
            owner: owner(),
            k: 0,
        },
        own: Vec::new(),
    };
    let int = Ground::Impl {
        tr: TR,
        ty: INT,
        subs: Some(Arc::from(Vec::new())),
    };
    let code = running(site, int);
    let globals = Globals::default();
    assert!(check_call(&code, 1, &found(INT, true), None, &globals).is_ok());
    let other = check_call(&code, 1, &found(STR, true), None, &globals);
    assert!(
        other
            .as_ref()
            .is_err_and(|e| e.message.contains("evidence:")),
        "the impl for another type: {other:?}"
    );
    assert!(check_call(&code, 1, &found(INT, false), None, &globals).is_err());
}

/// Structural evidence agrees with the VM's own method only; evidence
/// the VM cannot work out is not checked.
#[test]
fn structural_evidence_is_the_vms_own_method_and_unknown_is_not_checked() {
    let structural = Site::Method {
        recv: Evidence::Structural { tr: TR },
        own: Vec::new(),
    };
    let globals = Globals::default();
    let code = running(structural, Ground::Unknown);
    assert!(check_call(&code, 1, &found(INT, false), None, &globals).is_ok());
    assert!(check_call(&code, 1, &found(INT, true), None, &globals).is_err());
    // A bound of a definition the code is not in: nothing to check.
    let elsewhere = Site::Method {
        recv: Evidence::Param {
            owner: Span::BUILTIN,
            k: 0,
        },
        own: Vec::new(),
    };
    let code = running(elsewhere, Ground::Unknown);
    assert!(check_call(&code, 1, &found(STR, true), None, &globals).is_ok());
}

/// An impl the runtime provides agrees only with the call the VM
/// answers by that type's registry row: not with a compiled impl, not
/// with the VM's own method of another kind, not for another type.
#[test]
fn a_provided_impl_is_the_registry_row_of_its_type() {
    let site = Site::Method {
        recv: Evidence::Native { tr: TR, ty: INT },
        own: Vec::new(),
    };
    let globals = Globals::default();
    let code = running(site, Ground::Unknown);
    let row = Found {
        native_row: true,
        ..found(INT, false)
    };
    assert!(check_call(&code, 1, &row, None, &globals).is_ok());
    assert!(check_call(&code, 1, &found(INT, false), None, &globals).is_err());
    assert!(check_call(&code, 1, &found(INT, true), None, &globals).is_err());
    let other = Found {
        native_row: true,
        ..found(STR, false)
    };
    assert!(check_call(&code, 1, &other, None, &globals).is_err());
}
