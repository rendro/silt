//! Round-93 regression lock for `Hash` / `Compare` on user records and
//! variants whose fields are NOT auto-derive supportable (Channel, Map,
//! Tuple, Function, Bytes, Handle). The field-aware gate
//! (`compute_auto_derive_field_negatives` in `src/typechecker/mod.rs`)
//! un-stamps `(trait, type)` pairs whose fields cannot satisfy the trait;
//! those rejections, and the still-running tuple-field Hash case, are
//! golden cases (`tests/golden/lang/traits/vm_dispatch_unsupportable_field__*`).
//!
//! The mechanism lock below inspects the typechecked AST: `(Hash, Tuple)`
//! is stamped, so the synth pass emits an auto-derived `Pair.hash` for a
//! tuple-field record, and the call resolves through that global rather
//! than the `Value::Record` hash dispatch arm.

/// MECHANISM LOCK: run the exact `Pair { t: (Int, Int) }` program
/// from `hash_runs_on_user_record_with_tuple_field` through the
/// typechecker and assert its auto-derive synth pass pushed a
/// `trait Hash for Pair` TraitImpl (with a `hash` method) into
/// `program.decls`. This is the decl the compiler lowers to the
/// `Pair.hash` qualified global that `Op::CallMethod` resolves ahead
/// of `dispatch_trait_method` — i.e. the runtime test above locks the
/// SYNTH path plus the Tuple hash-allowlist entry, not the
/// `Value::Record` entry. If this ever fails, the doc claims on both
/// hash tests must be re-audited.
#[test]
fn synth_pass_emits_pair_hash_trait_impl_for_tuple_field_record() {
    let src = r#"
type Pair { t: (Int, Int) }
fn h(x: a) -> Int where a: Hash { x.hash() }
fn main() {
    let p = Pair { t: (1, 2) }
    println(h(p))
}
"#;
    let tokens = silt::lexer::Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens, src)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    assert!(
        errors
            .iter()
            .all(|e| e.severity != silt::diagnostic::Severity::Error),
        "tuple-field record Hash program must typecheck cleanly: {errors:?}"
    );

    let hash_trait = silt::intern::intern("Hash");
    let pair_type = silt::intern::intern("Pair");
    let synth_impl = program.decls.iter().find_map(|d| match d {
        silt::ast::Decl::TraitImpl(ti)
            if ti.is_auto_derived && ti.trait_name == hash_trait && ti.target_type == pair_type =>
        {
            Some(ti)
        }
        _ => None,
    });
    let ti = synth_impl.expect(
        "the auto-derive synth pass must emit an is_auto_derived `trait Hash for \
         Pair` impl for a tuple-field record — (Hash, Tuple) is stamped, so the \
         field-aware gate passes and the call resolves via the synth global, \
         never the Value::Record hash dispatch arm",
    );
    let hash_method = silt::intern::intern("hash");
    assert!(
        ti.methods.iter().any(|m| m.name == hash_method),
        "synthesized `trait Hash for Pair` impl must contain a `hash` method"
    );
}
