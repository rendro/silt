//! Phase C lock test: every function-typed builtin must carry a
//! non-`TOP` effect set after the stdlib sweep.
//!
//! `EffectSet::TOP` is the gradual-rollout permissive default; Phase C
//! classifies every builtin explicitly. Adding a new builtin in the
//! future without an effect annotation falls through to the
//! `Scheme::mono` default (still `TOP`) and trips this test, forcing
//! the developer to choose `pure_mono` / `io_mono` / `io_fs_mono` /
//! `io_net_mono` / `io_time_mono` / `io_random_mono` /
//! `Scheme::with_effects(_, _)`. See `docs/proposals/effect-rows.md`
//! Part 7 for the rollout plan and `src/types/effects.rs` for the
//! `EffectSet` constructors.

use silt::types::effects::EffectSet;

#[test]
fn no_function_typed_builtin_has_top_effects_after_phase_c_sweep() {
    let entries = silt::typechecker::iter_builtins_for_effects_audit();
    let offenders: Vec<&String> = entries
        .iter()
        .filter(|(_, eff)| *eff == EffectSet::TOP)
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "Phase C sweep missed {} function-typed builtin(s) (still EffectSet::TOP): {:?}\n\
         Every new stdlib registration must classify its effects explicitly via\n\
         `Scheme::pure_mono`, `Scheme::io_mono`, `Scheme::io_fs_mono`,\n\
         `Scheme::io_net_mono`, `Scheme::io_time_mono`, `Scheme::io_random_mono`,\n\
         or `Scheme::with_effects(_, EffectSet::...)`.",
        offenders.len(),
        offenders,
    );
}

#[test]
fn every_function_typed_builtin_has_a_classified_effect_set() {
    // Symmetric to the negative test above — assert there's at least
    // one builtin per major effect class so the sweep didn't
    // accidentally collapse everything to one set. This catches a
    // refactor that inverts the logic (every builtin marked pure).
    let entries = silt::typechecker::iter_builtins_for_effects_audit();
    assert!(
        !entries.is_empty(),
        "expected at least one function-typed builtin"
    );

    let mut has_pure = false;
    let mut has_io_fs = false;
    let mut has_io_net = false;
    let mut has_io_time = false;
    let mut has_io_random = false;
    for (_name, eff) in &entries {
        if *eff == EffectSet::pure() {
            has_pure = true;
        }
        if *eff == EffectSet::io_fs() {
            has_io_fs = true;
        }
        if *eff == EffectSet::io_net() {
            has_io_net = true;
        }
        if *eff == EffectSet::io_time() {
            has_io_time = true;
        }
        if *eff == EffectSet::io_random() {
            has_io_random = true;
        }
    }
    assert!(
        has_pure,
        "expected at least one pure builtin (e.g. list.map)"
    );
    assert!(
        has_io_fs,
        "expected at least one filesystem-effect builtin (e.g. io.read_file, fs.exists)"
    );
    // io_net needs the http feature — list.rs is unconditionally pure;
    // tcp/postgres/http builtins all carry io_net.
    assert!(
        has_io_net,
        "expected at least one network-effect builtin (e.g. http.get, postgres.query)"
    );
    assert!(
        has_io_time,
        "expected at least one wall-clock builtin (e.g. time.now)"
    );
    assert!(
        has_io_random,
        "expected at least one entropy-reading builtin (e.g. uuid.v4)"
    );
}

#[test]
fn specific_builtins_have_expected_effects() {
    // Pin a handful of canonical examples so a future refactor can't
    // silently flip an annotation without flipping this test red. The
    // selection covers each of the five classes; if the table in Part
    // 7 of `docs/proposals/effect-rows.md` is the contract, this test
    // is the lock.
    let entries: std::collections::HashMap<String, EffectSet> =
        silt::typechecker::iter_builtins_for_effects_audit()
            .into_iter()
            .collect();

    let cases: &[(&str, EffectSet)] = &[
        // Pure
        ("list.map", EffectSet::pure()),
        ("list.filter", EffectSet::pure()),
        ("string.split", EffectSet::pure()),
        ("math.sqrt", EffectSet::pure()),
        ("crypto.sha256", EffectSet::pure()),
        ("uuid.parse", EffectSet::pure()),
        ("time.format", EffectSet::pure()),
        // Filesystem
        ("io.read_file", EffectSet::io_fs()),
        ("io.write_file", EffectSet::io_fs()),
        ("fs.exists", EffectSet::io_fs()),
        ("fs.stat", EffectSet::io_fs()),
        // Plain io
        ("println", EffectSet::io()),
        ("print", EffectSet::io()),
        ("env.get", EffectSet::io()),
        // Wall-clock
        ("time.now", EffectSet::io_time()),
        ("time.today", EffectSet::io_time()),
        ("time.sleep", EffectSet::io_time()),
        // Entropy
        ("math.random", EffectSet::io_random()),
        ("uuid.v4", EffectSet::io_random()),
        ("crypto.random_bytes", EffectSet::io_random()),
        // Combined: time + random
        (
            "uuid.v7",
            EffectSet::io_time().insert(silt::types::effects::Effect::Random),
        ),
    ];
    for (name, expected) in cases {
        let got = entries.get(*name).copied().unwrap_or_else(|| {
            panic!("expected builtin `{name}` to be registered");
        });
        assert_eq!(
            got, *expected,
            "builtin `{name}` had unexpected effects: got {got}, expected {expected}",
        );
    }
}

#[test]
fn http_get_carries_io_net_when_http_feature_is_enabled() {
    // The http builtins are gated by the `http` cargo feature in some
    // workspace configurations. When present, they must be classified
    // as `!{io, net}`. When absent the test is silently skipped — the
    // bulk-sweep test above still locks the no-TOP invariant.
    let entries: std::collections::HashMap<String, EffectSet> =
        silt::typechecker::iter_builtins_for_effects_audit()
            .into_iter()
            .collect();
    if let Some(eff) = entries.get("http.get") {
        assert_eq!(*eff, EffectSet::io_net(), "http.get must be io_net");
    }
    if let Some(eff) = entries.get("postgres.query") {
        assert_eq!(*eff, EffectSet::io_net(), "postgres.query must be io_net");
    }
    if let Some(eff) = entries.get("tcp.connect") {
        assert_eq!(*eff, EffectSet::io_net(), "tcp.connect must be io_net");
    }
}
