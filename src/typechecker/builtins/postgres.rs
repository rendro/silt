//! Type signatures for the `postgres` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // The postgres builtin module exposes opaque handles and ADTs that
    // the user *also* declares in their silt-side `pg.silt` library.
    // We don't redeclare those types here; we just refer to them by
    // name via `Type::Generic(intern("..."), vec![])`. Unification
    // resolves them once the user's `pg.silt` is parsed (which adds
    // the matching record / enum entries to `checker.enums` /
    // `checker.records`).
    //
    // This means: the postgres module is only useful when paired with
    // a silt-side pg.silt that declares PgPool, PgError, QueryResult,
    // ExecResult, and the Value(VInt|VStr|VBool|VFloat|VNull|VList)
    // ADT used for parameters.

    let pg_pool = Type::Generic(intern("PgPool"), vec![]);
    let pg_tx = Type::Generic(intern("PgTx"), vec![]);
    let pg_error = Type::Generic(intern("PgError"), vec![]);
    let pg_value = Type::Generic(intern("Value"), vec![]);
    let query_result = Type::Generic(intern("QueryResult"), vec![]);
    let exec_result = Type::Generic(intern("ExecResult"), vec![]);

    let result_pool = Type::Generic(intern("Result"), vec![pg_pool.clone(), pg_error.clone()]);
    let result_query = Type::Generic(intern("Result"), vec![query_result, pg_error.clone()]);
    let result_exec = Type::Generic(intern("Result"), vec![exec_result, pg_error.clone()]);

    // postgres.connect: (String) -> Result(PgPool, PgError)
    env.define(
        intern("postgres.connect"),
        Scheme::mono(Type::Fun(vec![Type::String], Box::new(result_pool))),
    );

    // postgres.query: (T, String, List(Value)) -> Result(QueryResult, PgError)
    //
    // The first argument is polymorphic because both `PgPool` and
    // `PgTx` are valid targets (the Rust builtin dispatches at
    // runtime based on the variant tag). Silt has no ad-hoc
    // subtyping, so we use a fresh type variable; the caller's
    // concrete type at each call site nails it down.
    {
        let (t, tv) = checker.fresh_tv();
        env.define(
            intern("postgres.query"),
            Scheme {
                vars: vec![tv],
                ty: Type::Fun(
                    vec![t, Type::String, Type::List(Box::new(pg_value.clone()))],
                    Box::new(result_query),
                ),
                constraints: vec![],
            },
        );
    }

    // postgres.execute: (T, String, List(Value)) -> Result(ExecResult, PgError)
    {
        let (t, tv) = checker.fresh_tv();
        env.define(
            intern("postgres.execute"),
            Scheme {
                vars: vec![tv],
                ty: Type::Fun(
                    vec![t, Type::String, Type::List(Box::new(pg_value))],
                    Box::new(result_exec),
                ),
                constraints: vec![],
            },
        );
    }

    // postgres.transact: (PgPool, Fn(PgTx) -> Result(a, PgError)) -> Result(a, PgError)
    //
    // The callback now receives a pinned `PgTx` handle, not a
    // `PgPool`. Queries inside the callback must go through that
    // handle to share the transactional connection.
    {
        let (a, av) = checker.fresh_tv();
        let inner_result = Type::Generic(intern("Result"), vec![a.clone(), pg_error.clone()]);
        env.define(
            intern("postgres.transact"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        pg_pool.clone(),
                        Type::Fun(vec![pg_tx.clone()], Box::new(inner_result.clone())),
                    ],
                    Box::new(inner_result),
                ),
                constraints: vec![],
            },
        );
    }

    // postgres.close: (PgPool) -> Unit
    env.define(
        intern("postgres.close"),
        Scheme::mono(Type::Fun(vec![pg_pool.clone()], Box::new(Type::Unit))),
    );

    // postgres.stream: (T, String, List(Value)) -> Result(Channel(a), PgError)
    //
    // First arg polymorphic (PgPool | PgTx), like query/execute.
    // The channel element is left fully polymorphic (`a`) because
    // silt's type system can't express `Result(Map(String, Value), PgError)`
    // directly here without spelling out the whole nested shape —
    // runtime guarantees each item is `Ok(row_map) | Err(pg_error)`
    // and the adapter wrapper in pg.silt narrows that back.
    {
        let (t, tv) = checker.fresh_tv();
        let (a, av) = checker.fresh_tv();
        let row_type = Type::Map(
            Box::new(Type::String),
            Box::new(Type::Generic(intern("Value"), vec![])),
        );
        let _ = row_type; // element type kept abstract (see comment)
        let channel_ty = Type::Channel(Box::new(a));
        let result_channel = Type::Generic(
            intern("Result"),
            vec![channel_ty, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.stream"),
            Scheme {
                vars: vec![tv, av],
                ty: Type::Fun(
                    vec![
                        t,
                        Type::String,
                        Type::List(Box::new(Type::Generic(intern("Value"), vec![]))),
                    ],
                    Box::new(result_channel),
                ),
                constraints: vec![],
            },
        );
    }

    // postgres.cursor: (PgTx, String, List(Value), Int) -> Result(PgCursor, PgError)
    {
        let pg_cursor = Type::Generic(intern("PgCursor"), vec![]);
        let result_cursor = Type::Generic(
            intern("Result"),
            vec![pg_cursor, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.cursor"),
            Scheme::mono(Type::Fun(
                vec![
                    pg_tx.clone(),
                    Type::String,
                    Type::List(Box::new(Type::Generic(intern("Value"), vec![]))),
                    Type::Int,
                ],
                Box::new(result_cursor),
            )),
        );
    }

    // postgres.cursor_next: (PgCursor) -> Result(List(Map(String, Value)), PgError)
    {
        let pg_cursor = Type::Generic(intern("PgCursor"), vec![]);
        let row_list = Type::List(Box::new(Type::Map(
            Box::new(Type::String),
            Box::new(Type::Generic(intern("Value"), vec![])),
        )));
        let result_rows = Type::Generic(
            intern("Result"),
            vec![row_list, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.cursor_next"),
            Scheme::mono(Type::Fun(vec![pg_cursor], Box::new(result_rows))),
        );
    }

    // postgres.cursor_close: (PgCursor) -> Result((), PgError)
    {
        let pg_cursor = Type::Generic(intern("PgCursor"), vec![]);
        let result_unit = Type::Generic(
            intern("Result"),
            vec![Type::Unit, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.cursor_close"),
            Scheme::mono(Type::Fun(vec![pg_cursor], Box::new(result_unit))),
        );
    }

    // postgres.listen: (PgPool, String) -> Result(Channel(a), PgError)
    //
    // Same abstract-channel-element trick as postgres.stream: the
    // runtime guarantees each item is a `Notification` record, but
    // silt's type system can't refer to the user-declared
    // `Notification` type from here without a forward reference.
    // Callers destructure the record by field, which type-checks
    // against whatever Notification declaration the user's pg.silt
    // provides.
    {
        let (a, av) = checker.fresh_tv();
        let channel_ty = Type::Channel(Box::new(a));
        let result_channel = Type::Generic(
            intern("Result"),
            vec![channel_ty, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.listen"),
            Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![pg_pool.clone(), Type::String],
                    Box::new(result_channel),
                ),
                constraints: vec![],
            },
        );
    }

    // postgres.notify: (T, String, String) -> Result((), PgError)
    // First arg polymorphic so either a PgPool or PgTx works.
    {
        let (t, tv) = checker.fresh_tv();
        let result_unit = Type::Generic(
            intern("Result"),
            vec![Type::Unit, Type::Generic(intern("PgError"), vec![])],
        );
        env.define(
            intern("postgres.notify"),
            Scheme {
                vars: vec![tv],
                ty: Type::Fun(vec![t, Type::String, Type::String], Box::new(result_unit)),
                constraints: vec![],
            },
        );
    }

    // postgres.uuidv7: () -> String  (RFC 9562 UUIDv7, time-ordered)
    env.define(
        intern("postgres.uuidv7"),
        Scheme::mono(Type::Fun(vec![], Box::new(Type::String))),
    );
}
