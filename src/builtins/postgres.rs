//! Postgres builtin module (`postgres.*`).
//!
//! Provides connection pooling, parameterised query/execute, and
//! transaction support against a Postgres server. Modeled on the sync
//! `postgres` crate + `r2d2_postgres` connection pool, mirroring the
//! pattern used by the `http` builtin (sync ureq + io_pool bridge).
//!
//! Pool handles are opaque to silt code: a `Value::Variant("PgPool",
//! [Value::Int(id)])` carries an integer id into a process-global side
//! table that owns the actual `r2d2::Pool`. Explicit `postgres.close`
//! is required to drop the pool.
//!
//! Transactions pin a single `r2d2::PooledConnection` for the callback's
//! entire lifetime. A `Value::Variant("PgTx", [Value::Int(id)])` handle
//! identifies the pinned connection in a separate registry. `query` /
//! `execute` accept either a `PgPool` (fresh checkout per call) or a
//! `PgTx` (reuses the pinned conn), so statements inside `transact`'s
//! callback see each other's uncommitted writes — true per-call-graph
//! transactional scope.
//!
//! All blocking I/O is submitted to `vm.runtime.io_pool`, so a silt
//! task that calls e.g. `postgres.query` parks until the result lands
//! and never blocks a scheduler worker.

#![cfg(feature = "postgres")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fallible_iterator::FallibleIterator;
use postgres::NoTls;
use postgres::types::{IsNull, Kind, ToSql, Type as PgType};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;

use super::common::ok;
use super::typed::{Arg, List, Map, Ret, builtins};
use crate::runtime::sync::{Channel, Close};
use crate::scheduler::Scheduler;
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::{Native, Step, Vm, VmError};

/// The error of a `postgres` function whose operation has no value
/// of its own: `PgTimeout` when its deadline passed, `PgUnknown` with
/// the reason when it could not run or panicked.
fn pg_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::PG_TIMEOUT, vec![]),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::PG_UNKNOWN, vec![Value::String(why.into())])
        }
    };
    Value::variant(bv::ERR, vec![error])
}

// ── TLS-capable pool/conn wrappers ──────────────────────────────────
//
// When the `postgres-tls` feature is enabled, both the plain-TCP
// `NoTls` variant and the TLS `MakeTlsConnector` variant coexist —
// `do_connect` picks one based on the parsed `sslmode`. Without the
// feature, `PgPool`/`PinnedConn` collapse to single-variant enums
// that are identical in behavior to the pre-TLS code.

type PgPoolNoTls = Pool<PostgresConnectionManager<NoTls>>;
type PinnedConnNoTls = r2d2::PooledConnection<PostgresConnectionManager<NoTls>>;

#[cfg(feature = "postgres-tls")]
type PgPoolTls = Pool<PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>;
#[cfg(feature = "postgres-tls")]
type PinnedConnTls =
    r2d2::PooledConnection<PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>;

enum PgPool {
    NoTls(PgPoolNoTls),
    #[cfg(feature = "postgres-tls")]
    Tls(PgPoolTls),
}

impl PgPool {
    fn get(&self) -> Result<PinnedConn, r2d2::Error> {
        match self {
            PgPool::NoTls(p) => p.get().map(PinnedConn::NoTls),
            #[cfg(feature = "postgres-tls")]
            PgPool::Tls(p) => p.get().map(PinnedConn::Tls),
        }
    }
}

enum PinnedConn {
    NoTls(PinnedConnNoTls),
    #[cfg(feature = "postgres-tls")]
    Tls(PinnedConnTls),
}

impl PinnedConn {
    /// Access the underlying `postgres::Client` regardless of which
    /// TLS mode was used to build the pool.
    fn client_mut(&mut self) -> &mut postgres::Client {
        match self {
            PinnedConn::NoTls(c) => c,
            #[cfg(feature = "postgres-tls")]
            PinnedConn::Tls(c) => c,
        }
    }
}

// ── Pool + tx handle side tables ────────────────────────────────────

struct PoolRegistry {
    next_id: u64,
    pools: BTreeMap<u64, Arc<PgPool>>,
}

fn registry() -> &'static Mutex<PoolRegistry> {
    static REG: OnceLock<Mutex<PoolRegistry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(PoolRegistry {
            next_id: 1,
            pools: BTreeMap::new(),
        })
    })
}

fn insert_pool(pool: PgPool) -> u64 {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = reg.next_id;
    reg.next_id = reg.next_id.wrapping_add(1);
    reg.pools.insert(id, Arc::new(pool));
    id
}

fn lookup_pool(id: u64) -> Option<Arc<PgPool>> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.pools.get(&id).cloned()
}

fn remove_pool(id: u64) -> bool {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.pools.remove(&id).is_some()
}

/// Tx registry: each entry owns a single pooled connection for the
/// lifetime of one `transact` invocation. The outer `Mutex<HashMap>` is
/// locked briefly to fetch an `Arc` to the inner cell; the inner
/// `Mutex<PinnedConn>` is acquired per-query on the io_pool thread.
///
/// Keeping the inner lock scoped to a single query (rather than held
/// across VM re-entries) avoids deadlocks if the scheduler reschedules
/// another task that tries to touch the same tx handle while the
/// callback is mid-yield. The outer cell's `Arc` survives yields safely
/// because the registry holds it between calls.
struct TxRegistry {
    next_id: u64,
    txs: BTreeMap<u64, Arc<Mutex<PinnedConn>>>,
}

fn tx_registry() -> &'static Mutex<TxRegistry> {
    static REG: OnceLock<Mutex<TxRegistry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(TxRegistry {
            next_id: 1,
            txs: BTreeMap::new(),
        })
    })
}

fn insert_tx(conn: PinnedConn) -> u64 {
    let mut reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = reg.next_id;
    reg.next_id = reg.next_id.wrapping_add(1);
    reg.txs.insert(id, Arc::new(Mutex::new(conn)));
    id
}

fn lookup_tx(id: u64) -> Option<Arc<Mutex<PinnedConn>>> {
    let reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.txs.get(&id).cloned()
}

fn remove_tx(id: u64) -> Option<Arc<Mutex<PinnedConn>>> {
    let mut reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.txs.remove(&id)
}

// ── Cursor registry ────────────────────────────────────────────────
//
// A server-side cursor lives inside a `PgTx`. The registry tracks only
// enough bookkeeping to translate silt-side cursor ids into the
// corresponding tx + cursor name and to record whether the cursor has
// been exhausted (so a subsequent `cursor_next` can short-circuit).
// Actual cursor state lives inside Postgres; we just DECLARE / FETCH /
// CLOSE via SQL on the tx's pinned connection.
//
// Cleanup: when `postgres.transact` finalises (COMMIT / ROLLBACK), we
// call `drain_cursors_for_tx(tx_id)` to remove all entries associated
// with that tx. PG itself drops server-side cursors at tx end, so no
// SQL CLOSE is required at that point — we only need to reap the
// registry rows.

#[derive(Clone)]
struct CursorEntry {
    tx_id: u64,
    name: String,
    batch_size: u64,
    exhausted: bool,
}

struct CursorRegistry {
    next_id: u64,
    cursors: BTreeMap<u64, CursorEntry>,
}

fn cursors() -> &'static Mutex<CursorRegistry> {
    static REG: OnceLock<Mutex<CursorRegistry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(CursorRegistry {
            next_id: 1,
            cursors: BTreeMap::new(),
        })
    })
}

fn register_cursor(tx_id: u64, batch_size: u64) -> (u64, String) {
    let mut reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    let id = reg.next_id;
    reg.next_id = reg.next_id.wrapping_add(1);
    let name = format!("silt_cursor_{id}");
    reg.cursors.insert(
        id,
        CursorEntry {
            tx_id,
            name: name.clone(),
            batch_size,
            exhausted: false,
        },
    );
    (id, name)
}

fn lookup_cursor(id: u64) -> Option<CursorEntry> {
    let reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    reg.cursors.get(&id).cloned()
}

fn update_cursor_exhausted(id: u64, flag: bool) {
    let mut reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = reg.cursors.get_mut(&id) {
        entry.exhausted = flag;
    }
}

fn remove_cursor(id: u64) -> Option<CursorEntry> {
    let mut reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    reg.cursors.remove(&id)
}

/// Drop every cursor registry entry that belongs to `tx_id`. Called
/// from `transact`'s COMMIT / ROLLBACK finaliser so open cursors are
/// reaped alongside the tx. PG closes the server-side cursor objects
/// automatically at tx end, so we only reap local bookkeeping here.
fn drain_cursors_for_tx(tx_id: u64) {
    let mut reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    reg.cursors.retain(|_, entry| entry.tx_id != tx_id);
}

// ── Result / error builders ─────────────────────────────────────────

fn err(v: Value) -> Value {
    Value::variant(bv::ERR, vec![v])
}

/// Scrub a Postgres error message before it crosses the VM boundary
/// into silt.
///
/// `detail_redacted: true` — we drop `DETAIL:`, `WHERE:`, and `HINT:`
/// follow-on lines because Postgres routinely embeds *user row values*
/// in those fields (e.g. `DETAIL: Key (email)=(alice@example.com)
/// already exists.`). A silt web handler that echoes an `Err(_)` into a
/// 5xx body would otherwise leak those values to unauthenticated
/// callers. The short `message()` / `severity` / SQLSTATE code remain
/// intact so callers can still discriminate on error kind.
///
/// We ALSO defensively strip parenthesised `Key (col)=(val)` segments
/// from the short message itself — Postgres occasionally rolls row
/// values into the primary message via extensions or custom
/// constraints. Keeping just the pre-`Key (` prefix preserves the
/// constraint-kind text without the offending values.
///
/// Rust callers that need the full un-redacted error can still call
/// `postgres::Error::as_db_error()` directly on the original error.
/// The scrub only applies to strings destined for silt-side `PgError`.
#[doc(hidden)] // Exposed for integration tests (tests/lang/postgres_hardening_tests.rs).
pub fn redact_pg_message(s: &str) -> String {
    // Drop follow-on lines that Postgres uses for user-data callouts.
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("DETAIL:")
            || trimmed.starts_with("WHERE:")
            || trimmed.starts_with("HINT:")
        {
            continue;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    // Strip inline `Key (col)=(val)` artefacts from whatever remains.
    // Postgres writes these as `Key (col1, col2)=(v1, v2)` — match the
    // leading `Key (` and cut everything after it on the same segment.
    if let Some(idx) = out.find("Key (") {
        // Preserve any trailing period / clause after the closing `)`
        // is unlikely to add signal and may itself carry data; drop it.
        out.truncate(idx);
        out = out.trim_end().to_string();
    }
    out
}

// ── PgError helpers (Phase 2 of stdlib error redesign) ─────────────
//
// Every fallible postgres.* call now surfaces a typed `PgError` variant
// wrapped in `Err(...)`. Variants (declared by the module's `types` in the builtin registry):
//   PgConnect(msg)         — pool checkout / URL parse / transport setup
//   PgTls(msg)             — TLS handshake / setup / cert read
//   PgAuthFailed(msg)      — SQLSTATE class 28 (invalid auth)
//   PgQuery(msg, sqlstate) — any other DbError with a SQLSTATE
//   PgTypeMismatch(col, expected, actual)
//   PgNoSuchColumn(col)
//   PgClosed               — connection closed / broken pipe
//   PgTimeout              — query canceled / statement timeout
//   PgTxnAborted           — 25P02 in_failed_sql_transaction
//   PgUnknown(msg)         — fallback for shapes we can't classify
//
// All message strings are first run through `redact_pg_message` so
// DETAIL / WHERE / HINT follow-on rows don't leak across the VM boundary.

/// Build a bare `PgConnect(msg)` variant (no Err wrapper).
fn pg_connect(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_CONNECT,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Build a bare `PgTls(msg)` variant (no Err wrapper).
fn pg_tls(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_TLS,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Build a bare `PgUnknown(msg)` variant (no Err wrapper).
fn pg_unknown(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_UNKNOWN,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Classify a `postgres::Error` into the matching `PgError` variant.
/// For DbError we inspect SQLSTATE; for transport errors we sniff the
/// Display text for "closed" / "broken pipe" / "timed out" keywords.
fn pg_error_to_variant(e: &postgres::Error) -> Value {
    if let Some(db) = e.as_db_error() {
        let message = redact_pg_message(db.message());
        let sqlstate = db.code().code().to_string();
        match sqlstate.as_str() {
            // 25P02: in_failed_sql_transaction.
            "25P02" => Value::variant(bv::PG_TXN_ABORTED, vec![]),
            // 57014: query_canceled (statement_timeout fires with this).
            "57014" => Value::variant(bv::PG_TIMEOUT, vec![]),
            // 42703: undefined_column.
            "42703" => Value::variant(bv::PG_NO_SUCH_COLUMN, vec![Value::String(message.into())]),
            code if code.starts_with("28") => {
                Value::variant(bv::PG_AUTH_FAILED, vec![Value::String(message.into())])
            }
            code if code.starts_with("08") => {
                Value::variant(bv::PG_CONNECT, vec![Value::String(message.into())])
            }
            code => Value::variant(
                bv::PG_QUERY,
                vec![Value::String(message.into()), Value::String(code.into())],
            ),
        }
    } else {
        // Non-DB: transport / protocol. Sniff keywords to pick a
        // more specific variant where we can; fall back to PgUnknown.
        let raw = format!("{e}");
        let lc = raw.to_ascii_lowercase();
        let scrubbed = redact_pg_message(&raw);
        if lc.contains("timed out") || lc.contains("timeout") {
            Value::variant(bv::PG_TIMEOUT, vec![])
        } else if lc.contains("closed")
            || lc.contains("broken pipe")
            || lc.contains("eof")
            || lc.contains("reset by peer")
        {
            Value::variant(bv::PG_CLOSED, vec![])
        } else {
            Value::variant(bv::PG_UNKNOWN, vec![Value::String(scrubbed.into())])
        }
    }
}

/// Build a typed `PgError` variant from an `r2d2::Error` (pool checkout
/// / build failure). r2d2 wraps the underlying postgres error inside —
/// we classify on the Display text: a timeout while waiting for a slot
/// maps to `PgTimeout`, anything else to `PgConnect`.
fn pool_error_value(e: &r2d2::Error) -> Value {
    let raw = format!("{e}");
    let lc = raw.to_ascii_lowercase();
    if lc.contains("timed out") || lc.contains("timeout") {
        Value::variant(bv::PG_TIMEOUT, vec![])
    } else {
        pg_connect(raw)
    }
}

/// Ad-hoc `PgUnknown(msg)` for misuse errors (wrong variant shape, bad
/// argument types, cursor registry miss). These aren't transport or
/// query failures — they're programmer errors that still need to
/// surface as a typed `PgError` because the signature says so.
fn other_error(detail: impl Into<String>) -> Value {
    pg_unknown(detail)
}

// ── Value <-> SQL marshalling ───────────────────────────────────────

/// Wrap a converted column value in the silt-side `Value` ADT
/// (`VInt`/`VStr`/`VBool`/`VFloat`/`VNull`/`VList`).
fn wrap_v_int(n: i64) -> Value {
    Value::variant(bv::V_INT, vec![Value::Int(n)])
}
fn wrap_v_str(s: String) -> Value {
    Value::variant(bv::V_STR, vec![Value::String(s.into())])
}
fn wrap_v_bool(b: bool) -> Value {
    Value::variant(bv::V_BOOL, vec![Value::Bool(b)])
}
/// A silt `Float` is always finite, so a column holding `NaN` or
/// `±Infinity` decodes like any other value that has no silt form.
fn wrap_v_float(f: f64) -> Value {
    if !f.is_finite() {
        return wrap_v_str(format!("<decode error: non-finite float {f}>"));
    }
    Value::variant(bv::V_FLOAT, vec![crate::builtins::numeric::float_value(f)])
}
fn wrap_v_null() -> Value {
    Value::variant(bv::V_NULL, vec![])
}
fn wrap_v_list(xs: Vec<Value>) -> Value {
    Value::variant(bv::V_LIST, vec![Value::list(xs)])
}

/// Convert a Postgres column cell to a silt-side wrapped `VXxx` `Value`.
fn pg_cell_to_value(row: &postgres::Row, idx: usize) -> Value {
    let col = &row.columns()[idx];
    let ty = col.type_();

    // Arrays: handle first via Kind::Array.
    if let Kind::Array(elem_ty) = ty.kind() {
        return convert_array(row, idx, elem_ty);
    }

    match *ty {
        PgType::BOOL => match row.try_get::<_, Option<bool>>(idx) {
            Ok(Some(b)) => wrap_v_bool(b),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::INT2 => match row.try_get::<_, Option<i16>>(idx) {
            Ok(Some(n)) => wrap_v_int(n as i64),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::INT4 => match row.try_get::<_, Option<i32>>(idx) {
            Ok(Some(n)) => wrap_v_int(n as i64),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::INT8 => match row.try_get::<_, Option<i64>>(idx) {
            Ok(Some(n)) => wrap_v_int(n),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::FLOAT4 => match row.try_get::<_, Option<f32>>(idx) {
            Ok(Some(f)) => wrap_v_float(f as f64),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::FLOAT8 => match row.try_get::<_, Option<f64>>(idx) {
            Ok(Some(f)) => wrap_v_float(f),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME | PgType::CHAR => {
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(s)) => wrap_v_str(s),
                Ok(None) => wrap_v_null(),
                Err(e) => wrap_v_str(format!("<decode error: {e}>")),
            }
        }
        PgType::UUID => {
            // With the `with-uuid-1` feature enabled, the postgres crate
            // decodes UUID columns directly into uuid::Uuid.
            match row.try_get::<_, Option<uuid::Uuid>>(idx) {
                Ok(Some(u)) => wrap_v_str(u.to_string()),
                Ok(None) => wrap_v_null(),
                Err(e) => wrap_v_str(format!("<decode error: {e}>")),
            }
        }
        PgType::JSON | PgType::JSONB => {
            // Use serde_json::Value via the `with-serde_json-1` feature is not
            // enabled; fetch as text via a cast-friendly path. The postgres
            // crate accepts &str/String for JSON only on input. For output,
            // request bytes and convert.
            match row.try_get::<_, Option<&[u8]>>(idx) {
                Ok(Some(bytes)) => {
                    // JSONB binary format prefixes a version byte (1).
                    if matches!(*ty, PgType::JSONB) && !bytes.is_empty() && bytes[0] == 1 {
                        match std::str::from_utf8(&bytes[1..]) {
                            Ok(s) => wrap_v_str(s.to_string()),
                            Err(_) => wrap_v_str(BASE64.encode(&bytes[1..])),
                        }
                    } else {
                        match std::str::from_utf8(bytes) {
                            Ok(s) => wrap_v_str(s.to_string()),
                            Err(_) => wrap_v_str(BASE64.encode(bytes)),
                        }
                    }
                }
                Ok(None) => wrap_v_null(),
                Err(e) => wrap_v_str(format!("<decode error: {e}>")),
            }
        }
        PgType::NUMERIC => {
            // The default postgres crate doesn't decode NUMERIC directly. Cast
            // via text on the SQL side or use a feature (rust_decimal). We try
            // a string-typed read first; on failure we render a placeholder.
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(s)) => wrap_v_str(s),
                Ok(None) => wrap_v_null(),
                Err(_) => {
                    wrap_v_str("<numeric: cast to text in SQL, e.g. select n::text>".to_string())
                }
            }
        }
        PgType::BYTEA => match row.try_get::<_, Option<Vec<u8>>>(idx) {
            Ok(Some(bytes)) => wrap_v_str(BASE64.encode(&bytes)),
            Ok(None) => wrap_v_null(),
            Err(e) => wrap_v_str(format!("<decode error: {e}>")),
        },
        PgType::TIMESTAMP | PgType::TIMESTAMPTZ | PgType::DATE | PgType::TIME | PgType::TIMETZ => {
            // The default postgres crate decodes these only with the
            // `with-chrono-0_4` feature. Without it, the user must
            // `to_char(...)` or `::text` cast in SQL. We attempt a String read
            // and return a placeholder otherwise.
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(s)) => wrap_v_str(s),
                Ok(None) => wrap_v_null(),
                Err(_) => {
                    wrap_v_str("<timestamp: cast to text in SQL, e.g. select t::text>".to_string())
                }
            }
        }
        _ => {
            // Unknown / unhandled type: try as raw bytes and base64-encode.
            match row.try_get::<_, Option<&[u8]>>(idx) {
                Ok(Some(bytes)) => match std::str::from_utf8(bytes) {
                    Ok(s) => wrap_v_str(s.to_string()),
                    Err(_) => wrap_v_str(BASE64.encode(bytes)),
                },
                Ok(None) => wrap_v_null(),
                Err(e) => wrap_v_str(format!("<unsupported type {ty}: {e}>")),
            }
        }
    }
}

fn convert_array(row: &postgres::Row, idx: usize, elem_ty: &PgType) -> Value {
    macro_rules! arr {
        ($t:ty, $wrap:expr) => {
            match row.try_get::<_, Option<Vec<Option<$t>>>>(idx) {
                Ok(Some(xs)) => {
                    let out: Vec<Value> = xs
                        .into_iter()
                        .map(|opt| match opt {
                            Some(v) => $wrap(v),
                            None => wrap_v_null(),
                        })
                        .collect();
                    wrap_v_list(out)
                }
                Ok(None) => wrap_v_null(),
                Err(e) => wrap_v_str(format!("<array decode error: {e}>")),
            }
        };
    }
    match *elem_ty {
        PgType::BOOL => arr!(bool, wrap_v_bool),
        PgType::INT2 => arr!(i16, |n: i16| wrap_v_int(n as i64)),
        PgType::INT4 => arr!(i32, |n: i32| wrap_v_int(n as i64)),
        PgType::INT8 => arr!(i64, wrap_v_int),
        PgType::FLOAT4 => arr!(f32, |f: f32| wrap_v_float(f as f64)),
        PgType::FLOAT8 => arr!(f64, wrap_v_float),
        PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME | PgType::CHAR => {
            arr!(String, wrap_v_str)
        }
        _ => {
            // Fall back to TEXT[] best effort.
            arr!(String, wrap_v_str)
        }
    }
}

// ── Param marshalling (silt VXxx -> postgres ToSql) ─────────────────

/// Owned, type-erased SQL parameter. We need owned values that outlive
/// the borrow handed to `postgres::Client::query`, so we collect them
/// here, then take a `&[&dyn ToSql]` slice over the unwrapped ToSql
/// references at the call site.
enum SqlParam {
    Null,
    Bool(bool),
    Int8(i64),
    Float8(f64),
    Text(MaybeUuidText),
    BoolArr(Vec<Option<bool>>),
    Int8Arr(Vec<Option<i64>>),
    Float8Arr(Vec<Option<f64>>),
    TextArr(Vec<Option<String>>),
}

impl SqlParam {
    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            SqlParam::Null => &NoneText,
            SqlParam::Bool(v) => v,
            SqlParam::Int8(v) => v,
            SqlParam::Float8(v) => v,
            SqlParam::Text(v) => v,
            SqlParam::BoolArr(v) => v,
            SqlParam::Int8Arr(v) => v,
            SqlParam::Float8Arr(v) => v,
            SqlParam::TextArr(v) => v,
        }
    }
}

/// Wrapper around an owned String that binds as TEXT-like by default but
/// also accepts UUID columns (parsing the string to a uuid::Uuid on the
/// fly). This lets silt programs bind VStr ids straight into UUID columns
/// without an extra param variant on the silt side.
#[derive(Debug)]
struct MaybeUuidText(String);

impl ToSql for MaybeUuidText {
    fn to_sql(
        &self,
        ty: &PgType,
        out: &mut postgres::types::private::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        if matches!(*ty, PgType::UUID) {
            let parsed: uuid::Uuid = self.0.parse()?;
            return parsed.to_sql(ty, out);
        }
        self.0.to_sql(ty, out)
    }
    fn accepts(ty: &PgType) -> bool {
        matches!(*ty, PgType::UUID) || <String as ToSql>::accepts(ty)
    }
    postgres::types::to_sql_checked!();
}

/// Sentinel that binds as a typed `NULL` of TEXT. Postgres needs a type
/// when binding NULL; TEXT is the safest default and will be implicitly
/// coerced by the server in most contexts.
#[derive(Debug)]
struct NoneText;

impl ToSql for NoneText {
    fn to_sql(
        &self,
        _ty: &PgType,
        _out: &mut postgres::types::private::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        Ok(IsNull::Yes)
    }
    fn accepts(_ty: &PgType) -> bool {
        true
    }
    postgres::types::to_sql_checked!();
}

/// Convert a silt-side wrapped `Value` ADT instance to an owned `SqlParam`.
fn value_to_sql_param(v: &Value) -> Result<SqlParam, String> {
    let Value::Variant(param) = v else {
        return Err(format!(
            "postgres requires Value variant (VInt/VStr/...), got {}",
            v.kind()
        ));
    };
    if !param.of(ty::PG_VALUE) {
        return Err(format!(
            "postgres requires Value variant (VInt/VStr/...), got {}",
            param.ty().name
        ));
    }
    match param.name() {
        "VNull" => Ok(SqlParam::Null),
        "VInt" => match param.fields().first() {
            Some(Value::Int(n)) => Ok(SqlParam::Int8(*n)),
            other => Err(format!("postgres: VInt payload must be Int, got {other:?}")),
        },
        "VStr" => match param.fields().first() {
            Some(Value::String(s)) => Ok(SqlParam::Text(MaybeUuidText(s.to_string()))),
            other => Err(format!(
                "postgres: VStr payload must be String, got {other:?}"
            )),
        },
        "VBool" => match param.fields().first() {
            Some(Value::Bool(b)) => Ok(SqlParam::Bool(*b)),
            other => Err(format!(
                "postgres: VBool payload must be Bool, got {other:?}"
            )),
        },
        "VFloat" => match param.fields().first() {
            Some(Value::Float(f)) => Ok(SqlParam::Float8(*f)),
            // Some silt programs store integers in Float slots; coerce.
            Some(Value::Int(n)) => Ok(SqlParam::Float8(*n as f64)),
            other => Err(format!(
                "postgres: VFloat payload must be Float, got {other:?}"
            )),
        },
        "VList" => match param.fields().first() {
            Some(Value::List(xs)) => {
                list_to_array_param(&xs.to_vec().map_err(|too_long| too_long.to_string())?)
            }
            other => Err(format!(
                "postgres: VList payload must be List, got {other:?}"
            )),
        },
        other => Err(format!(
            "postgres: unknown silt Value variant {other:?} as parameter"
        )),
    }
}

/// Convert a silt `List(Value)` into a typed Postgres array parameter.
/// Element type is inferred from the first non-null element. Empty / all-null
/// lists default to TEXT[].
fn list_to_array_param(xs: &[Value]) -> Result<SqlParam, String> {
    // Inspect the first non-null element to pick a type.
    let mut elem_kind: Option<&str> = None;
    for x in xs {
        if let Value::Variant(variant) = x
            && !variant.is(bv::V_NULL)
        {
            elem_kind = Some(variant.name());
            break;
        }
    }
    let kind = elem_kind.unwrap_or("VStr"); // empty / all null → TEXT[]
    match kind {
        "VBool" => {
            let mut out: Vec<Option<bool>> = Vec::with_capacity(xs.len());
            for x in xs {
                match x {
                    Value::Variant(variant) if variant.is(bv::V_BOOL) => {
                        match variant.fields().first() {
                            Some(Value::Bool(b)) => out.push(Some(*b)),
                            _ => return Err("postgres: bad VBool in array".into()),
                        }
                    }
                    Value::Variant(variant) if variant.is(bv::V_NULL) => out.push(None),
                    _ => return Err("postgres: mixed-type array (expected Bool)".into()),
                }
            }
            Ok(SqlParam::BoolArr(out))
        }
        "VInt" => {
            let mut out: Vec<Option<i64>> = Vec::with_capacity(xs.len());
            for x in xs {
                match x {
                    Value::Variant(variant) if variant.is(bv::V_INT) => {
                        match variant.fields().first() {
                            Some(Value::Int(n)) => out.push(Some(*n)),
                            _ => return Err("postgres: bad VInt in array".into()),
                        }
                    }
                    Value::Variant(variant) if variant.is(bv::V_NULL) => out.push(None),
                    _ => return Err("postgres: mixed-type array (expected Int)".into()),
                }
            }
            Ok(SqlParam::Int8Arr(out))
        }
        "VFloat" => {
            let mut out: Vec<Option<f64>> = Vec::with_capacity(xs.len());
            for x in xs {
                match x {
                    Value::Variant(variant) if variant.is(bv::V_FLOAT) => {
                        match variant.fields().first() {
                            Some(Value::Float(f)) => out.push(Some(*f)),
                            Some(Value::Int(n)) => out.push(Some(*n as f64)),
                            _ => return Err("postgres: bad VFloat in array".into()),
                        }
                    }
                    Value::Variant(variant) if variant.is(bv::V_NULL) => out.push(None),
                    _ => return Err("postgres: mixed-type array (expected Float)".into()),
                }
            }
            Ok(SqlParam::Float8Arr(out))
        }
        _ => {
            // Default: TEXT[]
            let mut out: Vec<Option<String>> = Vec::with_capacity(xs.len());
            for x in xs {
                match x {
                    Value::Variant(variant) if variant.is(bv::V_STR) => {
                        match variant.fields().first() {
                            Some(Value::String(s)) => out.push(Some(s.to_string())),
                            _ => return Err("postgres: bad VStr in array".into()),
                        }
                    }
                    Value::Variant(variant) if variant.is(bv::V_NULL) => out.push(None),
                    _ => {
                        return Err("postgres: nested arrays / mixed types not supported".into());
                    }
                }
            }
            Ok(SqlParam::TextArr(out))
        }
    }
}

// ── Row -> silt Map(String, Value) ─────────────────────────────────

fn row_to_map(row: &postgres::Row) -> Value {
    let mut map: BTreeMap<Value, Value> = BTreeMap::new();
    for (idx, col) in row.columns().iter().enumerate() {
        let key = Value::String(col.name().into());
        map.insert(key, pg_cell_to_value(row, idx));
    }
    Value::Map(Arc::new(map))
}

fn make_query_result(rows: Vec<Value>) -> Value {
    let row_count = rows.len() as i64;
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    fields.insert("rows".to_string(), Value::list(rows));
    fields.insert("row_count".to_string(), Value::Int(row_count));
    Value::builtin_record(ty::QUERY_RESULT, fields)
}

fn make_exec_result(affected: u64, returning: Vec<Value>) -> Value {
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    fields.insert("affected".to_string(), Value::Int(affected as i64));
    fields.insert("returning".to_string(), Value::list(returning));
    Value::builtin_record(ty::EXEC_RESULT, fields)
}

// ── Handle helpers ──────────────────────────────────────────────────

fn make_pool_handle(id: u64) -> Value {
    Value::variant(bv::PG_POOL, vec![Value::Int(id as i64)])
}

fn make_tx_handle(id: u64) -> Value {
    Value::variant(bv::PG_TX, vec![Value::Int(id as i64)])
}

fn make_cursor_handle(id: u64) -> Value {
    Value::variant(bv::PG_CURSOR, vec![Value::Int(id as i64)])
}

// ── The arguments ───────────────────────────────────────────────────

/// The id in a handle: the variant `tag` with one `Int` that is no
/// less than 0.
fn handle_id(value: &Value, tag: BuiltinVariant) -> Option<u64> {
    match value {
        Value::Variant(variant) if variant.is(tag) => match variant.fields() {
            [Value::Int(id)] => u64::try_from(*id).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// A `PgPool` argument: the id of the pool.
#[derive(Clone, Copy)]
struct PoolHandle(u64);

impl<'a> Arg<'a> for PoolHandle {
    fn take(value: &'a Value) -> Option<Self> {
        handle_id(value, bv::PG_POOL).map(PoolHandle)
    }
}

impl PoolHandle {
    /// The pool, or the `PgError` of one that is closed.
    fn open(self) -> Result<Arc<PgPool>, Value> {
        lookup_pool(self.0).ok_or_else(|| {
            pg_connect(format!(
                "postgres: pool handle {} is not registered (closed or never opened)",
                self.0
            ))
        })
    }
}

/// A `PgTx` argument: the id of the transaction.
#[derive(Clone, Copy)]
struct TxHandle(u64);

impl<'a> Arg<'a> for TxHandle {
    fn take(value: &'a Value) -> Option<Self> {
        handle_id(value, bv::PG_TX).map(TxHandle)
    }
}

impl TxHandle {
    /// The transaction's connection, or the `PgError` of one that has
    /// ended.
    fn open(self) -> Result<Arc<Mutex<PinnedConn>>, Value> {
        lookup_tx(self.0).ok_or_else(|| {
            pg_connect(format!(
                "postgres: tx handle {} is not registered (transaction ended)",
                self.0
            ))
        })
    }
}

/// A `PgCursor` argument: the id of the cursor.
#[derive(Clone, Copy)]
struct CursorHandle(u64);

impl<'a> Arg<'a> for CursorHandle {
    fn take(value: &'a Value) -> Option<Self> {
        handle_id(value, bv::PG_CURSOR).map(CursorHandle)
    }
}

/// The options argument of `connect_with`, a `Map(String, Int)`: its
/// entries.
struct Opts<'a>(Vec<(&'a str, i64)>);

impl<'a> Arg<'a> for Opts<'a> {
    fn take(value: &'a Value) -> Option<Self> {
        let entries = <Map as Arg>::take(value)?.iter();
        let entries = entries.map(|(key, value)| Some((<&str>::take(key)?, i64::take(value)?)));
        entries.collect::<Option<_>>().map(Opts)
    }
}

/// The parameters of a statement, or the `PgError` of a value that
/// is none.
fn sql_params(params: List) -> Result<Vec<SqlParam>, Value> {
    let params = params.iter().map(|param| value_to_sql_param(&param));
    params.collect::<Result<_, _>>().map_err(other_error)
}

/// The `PgError` a function ends with before its operation starts: a
/// body's `Err`, which is the function's `Err(error)`.
struct Failed(Value);

impl From<Value> for Failed {
    fn from(error: Value) -> Self {
        Failed(error)
    }
}

/// What a function gives that starts an operation, if it gets that
/// far.
type Started = Result<Result<Step, VmError>, Failed>;

impl<T: Ret> Ret for Result<T, Failed> {
    fn ret(self) -> Result<Step, VmError> {
        match self {
            Ok(done) => done.ret(),
            Err(Failed(error)) => Ok(Step::Done(err(error))),
        }
    }
}

/// Target of a query or execute: either a pool (pulls a fresh conn) or
/// a pinned tx conn (reuses the BEGIN'd connection).
enum ExecutorRef {
    Pool(Arc<PgPool>),
    Tx(Arc<Mutex<PinnedConn>>),
}

/// The executor that `conn` is, the argument of a function that takes
/// a pool or a transaction: the `PgError` if it is neither, or is one
/// that is closed.
fn resolve_executor(conn: &Value) -> Result<ExecutorRef, Value> {
    if let Some(pool) = PoolHandle::take(conn) {
        return pool.open().map(ExecutorRef::Pool);
    }
    if let Some(tx) = TxHandle::take(conn) {
        return tx.open().map(ExecutorRef::Tx);
    }
    Err(other_error(format!(
        "postgres requires PgPool or PgTx, got {}",
        match conn {
            Value::Variant(variant) => variant.name(),
            other => other.kind(),
        }
    )))
}

// ── Blocking workers (run inside io_pool) ───────────────────────────

fn do_connect(url: String) -> Value {
    do_connect_with(url, ConnectOpts::default())
}

/// Structured options for `postgres.connect_with`. Fields are `Option`
/// so absent keys fall through to built-in defaults rather than a
/// zero value that could silently disable something.
#[derive(Default, Clone, Debug)]
struct ConnectOpts {
    /// Override r2d2's default `max_size` (10). `None` → library default.
    max_pool_size: Option<u32>,
}

fn do_connect_with(url: String, opts: ConnectOpts) -> Value {
    // Pre-parse `sslmode` out of the query string: `postgres::Config`
    // only recognises `disable` / `prefer` / `require` and rejects
    // `verify-ca` / `verify-full` with a parse error. We also want the
    // optional `sslrootcert` override. Strip both from the URL before
    // handing it to `postgres::Config::from_str`.
    let (stripped_url, ext) = match extract_ssl_url_params(&url) {
        Ok(x) => x,
        Err(e) => {
            return err(pg_connect(format!("postgres: invalid URL: {e}")));
        }
    };

    let cfg: postgres::Config = match stripped_url.parse() {
        Ok(cfg) => cfg,
        Err(e) => {
            return err(pg_connect(format!("postgres: invalid URL: {e}")));
        }
    };

    // Derive effective TLS mode.
    //
    // Security default (HIGH-4 hardening): when the URL omits
    // `sslmode=` entirely, we default to **`verify-full`** rather than
    // libpq's historical `prefer`. Opting in to `prefer` / `require` /
    // `disable` now requires an explicit parameter. If the user DID
    // write `sslmode=require`, we honour that (encryption-only, no
    // cert/hostname validation) because it's an explicit request.
    //
    // Priority:
    //   1. `ext.mode` — explicit `sslmode=` query param, including
    //      extended modes (`verify-ca` / `verify-full`) that
    //      `postgres::Config` doesn't parse natively.
    //   2. If absent, `VerifyFull` (the new safe default).
    //
    // Note: we do NOT fall back to `cfg.get_ssl_mode()`. The
    // `postgres::Config` parser defaults its internal SslMode to
    // `Prefer` when nothing is specified, which is exactly the
    // unverified-TLS behaviour this fix is intended to prevent.
    let effective = match ext.mode {
        Some(m) => m,
        None => EffectiveSslMode::VerifyFull,
    };

    match build_pool(cfg, effective, ext.root_cert.as_deref(), opts) {
        Ok(pool) => {
            let id = insert_pool(pool);
            ok(make_pool_handle(id))
        }
        Err(v) => err(v),
    }
}

/// Effective `sslmode` once we've parsed both `postgres`-crate-native
/// values (`Disable` / `Prefer` / `Require`) and the extended modes
/// (`verify-ca` / `verify-full`) that we detect manually.
#[doc(hidden)] // public so integration tests can verify the default mode resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveSslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// Compute the effective SSL mode that `do_connect` would use for a
/// given URL, WITHOUT actually opening a connection. Integration tests
/// use this to lock the HIGH-4 fix: a URL that omits `sslmode=`
/// resolves to `VerifyFull`, NOT the libpq-default `Prefer`.
#[doc(hidden)]
pub fn resolve_effective_sslmode_for_tests(url: &str) -> Result<EffectiveSslMode, String> {
    let (_, ext) = extract_ssl_url_params(url)?;
    Ok(match ext.mode {
        Some(m) => m,
        None => EffectiveSslMode::VerifyFull,
    })
}

#[derive(Default)]
struct SslUrlParams {
    mode: Option<EffectiveSslMode>,
    root_cert: Option<String>,
}

/// Extract (and strip) TLS-related query params from a postgres URL.
/// Returns (stripped_url, parsed_params). Non-TLS URLs pass through
/// unchanged with an empty params struct.
///
/// We only touch the query string — scheme, userinfo, host, path are
/// left intact. If no query string exists, returns the input verbatim.
fn extract_ssl_url_params(url: &str) -> Result<(String, SslUrlParams), String> {
    let Some(q_idx) = url.find('?') else {
        return Ok((url.to_string(), SslUrlParams::default()));
    };
    let (prefix, query_with_q) = url.split_at(q_idx);
    let query = &query_with_q[1..]; // skip '?'

    let mut params = SslUrlParams::default();
    let mut kept: Vec<&str> = Vec::new();

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        match k {
            "sslmode" => {
                params.mode = Some(match v {
                    "disable" => EffectiveSslMode::Disable,
                    "prefer" => EffectiveSslMode::Prefer,
                    "require" => EffectiveSslMode::Require,
                    "verify-ca" => EffectiveSslMode::VerifyCa,
                    "verify-full" => EffectiveSslMode::VerifyFull,
                    other => {
                        return Err(format!("unknown sslmode: {other}"));
                    }
                });
            }
            "sslrootcert" => {
                params.root_cert = Some(v.to_string());
            }
            // `sslcert` / `sslkey` (client certs) are v2 — silently
            // ignore for now so URLs aren't rejected outright.
            "sslcert" | "sslkey" => {}
            _ => kept.push(pair),
        }
    }

    let stripped = if kept.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}?{}", kept.join("&"))
    };
    Ok((stripped, params))
}

/// Build a `PgPool` for the given config and effective SSL mode.
/// Returns an already-wrapped silt error value on failure.
fn build_pool(
    cfg: postgres::Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    opts: ConnectOpts,
) -> Result<PgPool, Value> {
    #[cfg(feature = "postgres-tls")]
    {
        match mode {
            EffectiveSslMode::Disable => build_pool_notls(cfg, &opts),
            EffectiveSslMode::Prefer => build_pool_prefer_tls(cfg, root_cert_path, &opts),
            EffectiveSslMode::Require => build_pool_tls(cfg, root_cert_path, false, false, &opts),
            EffectiveSslMode::VerifyCa => build_pool_tls(cfg, root_cert_path, true, false, &opts),
            EffectiveSslMode::VerifyFull => build_pool_tls(cfg, root_cert_path, true, true, &opts),
        }
    }
    #[cfg(not(feature = "postgres-tls"))]
    {
        let _ = root_cert_path; // unused without TLS
        match mode {
            EffectiveSslMode::Disable | EffectiveSslMode::Prefer => build_pool_notls(cfg, &opts),
            EffectiveSslMode::Require
            | EffectiveSslMode::VerifyCa
            | EffectiveSslMode::VerifyFull => Err(pg_tls(
                "TLS required but silt was built without the postgres-tls feature \
                 (URL had no `sslmode=`, which now defaults to verify-full; \
                 use `?sslmode=disable` to connect without TLS)",
            )),
        }
    }
}

/// Apply `ConnectOpts` overrides to an `r2d2::Builder`. Centralised so
/// every pool-construction path (TLS / NoTLS / prefer) picks up the
/// same tunables.
fn apply_pool_opts<M: r2d2::ManageConnection>(
    mut builder: r2d2::Builder<M>,
    opts: &ConnectOpts,
) -> r2d2::Builder<M> {
    if let Some(n) = opts.max_pool_size {
        builder = builder.max_size(n);
    }
    builder
}

fn build_pool_notls(cfg: postgres::Config, opts: &ConnectOpts) -> Result<PgPool, Value> {
    let manager = PostgresConnectionManager::new(cfg, NoTls);
    let pool = apply_pool_opts(Pool::builder(), opts)
        .build(manager)
        .map_err(|e| pool_error_value(&e))?;
    Ok(PgPool::NoTls(pool))
}

#[cfg(feature = "postgres-tls")]
fn build_tls_connector(
    root_cert_path: Option<&str>,
    accept_invalid_certs: bool,
    accept_invalid_hostnames: bool,
) -> Result<postgres_native_tls::MakeTlsConnector, Value> {
    use std::fs;

    let mut builder = native_tls::TlsConnector::builder();
    builder.danger_accept_invalid_certs(accept_invalid_certs);
    builder.danger_accept_invalid_hostnames(accept_invalid_hostnames);

    if let Some(path) = root_cert_path {
        let pem = fs::read(path)
            .map_err(|e| pg_tls(format!("postgres: failed to read sslrootcert {path}: {e}")))?;
        let cert = native_tls::Certificate::from_pem(&pem)
            .map_err(|e| pg_tls(format!("postgres: invalid sslrootcert PEM at {path}: {e}")))?;
        builder.add_root_certificate(cert);
    }

    let connector = builder
        .build()
        .map_err(|e| pg_tls(format!("postgres: TLS setup failed: {e}")))?;
    Ok(postgres_native_tls::MakeTlsConnector::new(connector))
}

#[cfg(feature = "postgres-tls")]
fn build_pool_tls(
    cfg: postgres::Config,
    root_cert_path: Option<&str>,
    verify_ca: bool,
    verify_hostname: bool,
    opts: &ConnectOpts,
) -> Result<PgPool, Value> {
    // `danger_accept_invalid_certs` = skip CA validation. Invert the
    // verify flags: verify_ca=true → accept_invalid_certs=false.
    let accept_invalid_certs = !verify_ca;
    let accept_invalid_hostnames = !verify_hostname;
    let connector = build_tls_connector(
        root_cert_path,
        accept_invalid_certs,
        accept_invalid_hostnames,
    )?;
    let manager = PostgresConnectionManager::new(cfg, connector);
    let pool = apply_pool_opts(Pool::builder(), opts)
        .build(manager)
        .map_err(|e| pool_error_value(&e))?;
    Ok(PgPool::Tls(pool))
}

/// `sslmode=prefer`: attempt TLS; on any TLS setup / handshake
/// failure, fall back to plain TCP. `postgres-native-tls` itself
/// doesn't implement this; we emulate by building the TLS pool and
/// probing with a single `get()`. If that fails, rebuild as NoTls.
#[cfg(feature = "postgres-tls")]
fn build_pool_prefer_tls(
    cfg: postgres::Config,
    root_cert_path: Option<&str>,
    opts: &ConnectOpts,
) -> Result<PgPool, Value> {
    // In "prefer" mode we don't validate the cert — the libpq docs say
    // prefer is opportunistic encryption, not an authentication hop.
    match build_tls_connector(root_cert_path, true, true) {
        Ok(connector) => {
            let manager = PostgresConnectionManager::new(cfg.clone(), connector);
            match apply_pool_opts(Pool::builder(), opts).build(manager) {
                Ok(pool) => {
                    // Probe: does the server actually accept TLS? r2d2's
                    // `build` already ran a test connection, so if we're
                    // here the TLS handshake succeeded.
                    Ok(PgPool::Tls(pool))
                }
                Err(_) => build_pool_notls(cfg, opts),
            }
        }
        Err(_) => build_pool_notls(cfg, opts),
    }
}

/// Run a query (SELECT-style). Accepts either a pool (fresh conn per
/// call) or a tx-pinned conn. The inner `Mutex<PinnedConn>` is held for
/// the duration of this single query only — never across a VM yield.
fn do_query(target: ExecutorRef, sql: String, params: Vec<SqlParam>) -> Value {
    let bind: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_to_sql()).collect();
    match target {
        ExecutorRef::Pool(pool) => {
            let mut conn = match pool.get() {
                Ok(c) => c,
                Err(e) => return err(pool_error_value(&e)),
            };
            match conn.client_mut().query(&sql, &bind) {
                Ok(rows) => {
                    let mapped: Vec<Value> = rows.iter().map(row_to_map).collect();
                    ok(make_query_result(mapped))
                }
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
        ExecutorRef::Tx(cell) => {
            let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
            match conn.client_mut().query(&sql, &bind) {
                Ok(rows) => {
                    let mapped: Vec<Value> = rows.iter().map(row_to_map).collect();
                    ok(make_query_result(mapped))
                }
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
    }
}

/// Run an INSERT/UPDATE/DELETE (with optional RETURNING). Dispatches
/// to `query` or `execute` on the underlying conn based on a best-
/// effort RETURNING-keyword scan.
fn do_execute(target: ExecutorRef, sql: String, params: Vec<SqlParam>) -> Value {
    let bind: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_to_sql()).collect();

    // Decide whether to call `execute` or `query` based on the presence of
    // a RETURNING clause. We do a simple case-insensitive check; this is a
    // best-effort heuristic and may misfire on SQL that mentions "returning"
    // in a comment. For v1 it's good enough.
    let has_returning = sql
        .to_ascii_uppercase()
        .split_whitespace()
        .any(|tok| tok == "RETURNING");

    fn run(
        conn: &mut postgres::Client,
        sql: &str,
        bind: &[&(dyn ToSql + Sync)],
        has_returning: bool,
    ) -> Value {
        if has_returning {
            match conn.query(sql, bind) {
                Ok(rows) => {
                    let returning: Vec<Value> = rows.iter().map(row_to_map).collect();
                    let affected = returning.len() as u64;
                    ok(make_exec_result(affected, returning))
                }
                Err(e) => err(pg_error_to_variant(&e)),
            }
        } else {
            match conn.execute(sql, bind) {
                Ok(n) => ok(make_exec_result(n, Vec::new())),
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
    }

    match target {
        ExecutorRef::Pool(pool) => {
            let mut conn = match pool.get() {
                Ok(c) => c,
                Err(e) => return err(pool_error_value(&e)),
            };
            run(conn.client_mut(), &sql, &bind, has_returning)
        }
        ExecutorRef::Tx(cell) => {
            let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
            run(conn.client_mut(), &sql, &bind, has_returning)
        }
    }
}

// ── Streaming worker (postgres.stream) ─────────────────────────────
//
// Drives a server-side `query_raw` row iterator and ships each row to
// a bounded silt `Channel`. Each row is wrapped as `Ok(row_map)`; on
// a mid-iteration DB error we send a final `Err(pg_error)` before
// closing the channel. The channel's `is_closed` flag is checked
// between rows so silt-side `channel.close` cancels the stream (the
// RowIter is then dropped, which sends CancelRequest to the server).

/// Bounded capacity for streaming channels. Enough to hide typical
/// per-row latency; small enough that a 1M-row query costs < 1 MB of
/// in-flight buffer at steady state.
const STREAM_CHANNEL_CAPACITY: usize = 256;

/// Run the streaming query inside the io_pool. Owns the executor for
/// its whole lifetime — for tx executors that means the inner
/// `Mutex<PinnedConn>` is locked here until the stream finishes, so
/// concurrent tx queries block until we're done. Documented contract.
fn do_stream_worker(
    target: ExecutorRef,
    sql: String,
    params: Vec<SqlParam>,
    ch: Arc<Channel>,
    scheduler: Arc<Scheduler>,
) {
    // If the consumer already cancelled before we even started, bail
    // without touching the DB.
    if ch.is_closed() {
        return;
    }
    let bind: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_to_sql()).collect();

    // Helper: send a single wrapped row or error; stop if the channel
    // was closed by the consumer. Returns `true` to keep going.
    let send_or_stop = |v: Value| -> bool { scheduler.send_wait(&ch, v) };

    let pump = |client: &mut postgres::Client, sql: &str, bind: &[&(dyn ToSql + Sync)]| match client
        .query_raw(sql, bind.iter().copied())
    {
        Ok(mut iter) => loop {
            if ch.is_closed() {
                break;
            }
            match iter.next() {
                Ok(Some(row)) => {
                    let wrapped = Value::variant(bv::OK, vec![row_to_map(&row)]);
                    if !send_or_stop(wrapped) {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let wrapped = Value::variant(bv::ERR, vec![pg_error_to_variant(&e)]);
                    let _ = send_or_stop(wrapped);
                    break;
                }
            }
        },
        Err(e) => {
            let wrapped = Value::variant(bv::ERR, vec![pg_error_to_variant(&e)]);
            let _ = send_or_stop(wrapped);
        }
    };

    match target {
        ExecutorRef::Pool(pool) => match pool.get() {
            Ok(mut conn) => pump(conn.client_mut(), &sql, &bind),
            Err(e) => {
                let wrapped = Value::variant(bv::ERR, vec![pool_error_value(&e)]);
                let _ = send_or_stop(wrapped);
            }
        },
        ExecutorRef::Tx(cell) => {
            let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
            pump(conn.client_mut(), &sql, &bind);
        }
    }

    // End of stream: always close, even if we already sent an Err
    // (close after Err signals "no more items" so the consumer's
    // channel.receive returns Closed on the next pull).
    ch.close(Close::default(), scheduler.wake());
}

// ── Cursor workers (postgres.cursor / .cursor_next / .cursor_close) ─

/// Open a cursor: runs `DECLARE <name> CURSOR FOR <sql>` with bound
/// params on the tx's pinned connection. Registry entry is inserted
/// before DECLARE so a concurrent `drain_cursors_for_tx` sees us;
/// if DECLARE fails we remove it again.
fn do_cursor_open(
    tx_id: u64,
    cell: Arc<Mutex<PinnedConn>>,
    sql: String,
    params: Vec<SqlParam>,
    batch_size: u64,
) -> Value {
    let bind: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_to_sql()).collect();
    let (cursor_id, name) = register_cursor(tx_id, batch_size);
    let declare_sql = format!("DECLARE {name} CURSOR FOR {sql}");
    let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
    match conn.client_mut().execute(declare_sql.as_str(), &bind) {
        Ok(_) => ok(make_cursor_handle(cursor_id)),
        Err(e) => {
            // Undo the registry insert so the id is not leaked.
            let _ = remove_cursor(cursor_id);
            err(pg_error_to_variant(&e))
        }
    }
}

/// Fetch the next batch from an open cursor. Empty list = exhausted.
fn do_cursor_next(cursor_id: u64) -> Value {
    let entry = match lookup_cursor(cursor_id) {
        Some(e) => e,
        None => {
            return err(other_error(format!(
                "postgres: cursor {cursor_id} is not registered (closed or tx ended)"
            )));
        }
    };
    if entry.exhausted {
        return ok(Value::list(Vec::new()));
    }
    let cell = match lookup_tx(entry.tx_id) {
        Some(c) => c,
        None => {
            // The owning tx finished without draining us (shouldn't
            // happen — transact finalisation calls drain_cursors_for_tx
            // — but be defensive).
            let _ = remove_cursor(cursor_id);
            return err(other_error(format!(
                "postgres: cursor {cursor_id}'s tx {} is no longer registered",
                entry.tx_id
            )));
        }
    };
    let fetch_sql = format!("FETCH FORWARD {} FROM {}", entry.batch_size, entry.name);
    let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
    match conn.client_mut().query(fetch_sql.as_str(), &[]) {
        Ok(rows) => {
            let n = rows.len();
            let mapped: Vec<Value> = rows.iter().map(row_to_map).collect();
            drop(conn);
            if (n as u64) < entry.batch_size {
                update_cursor_exhausted(cursor_id, true);
            }
            ok(Value::list(mapped))
        }
        Err(e) => err(pg_error_to_variant(&e)),
    }
}

/// Explicitly close a cursor. Runs `CLOSE <name>` if the tx is still
/// alive; otherwise no-ops (the tx end implicitly closed it).
fn do_cursor_close(cursor_id: u64) -> Value {
    let entry = match remove_cursor(cursor_id) {
        Some(e) => e,
        None => {
            // Already closed (or never opened); treat as idempotent.
            return ok(Value::Unit);
        }
    };
    let cell = match lookup_tx(entry.tx_id) {
        Some(c) => c,
        None => {
            // Tx is gone — cursor is already closed server-side. OK.
            return ok(Value::Unit);
        }
    };
    let close_sql = format!("CLOSE {}", entry.name);
    let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
    match conn.client_mut().execute(close_sql.as_str(), &[]) {
        Ok(_) => ok(Value::Unit),
        Err(e) => err(pg_error_to_variant(&e)),
    }
}

// ── LISTEN / NOTIFY (pub-sub) ───────────────────────────────────────
//
// `listen(pool, channel_name)` pins a single pool connection for the
// listener's lifetime, runs `LISTEN <channel_name>`, and returns a
// bounded silt `Channel` of `Notification` records. The worker polls
// the pg connection's notification buffer via `timeout_iter` (200ms
// tick) so `channel.close(ch)` is observed within one tick.
//
// `notify(executor, channel_name, payload)` runs `SELECT pg_notify($1, $2)`
// against either a pool or tx executor. `pg_notify` handles identifier
// escaping internally and accepts dynamic channel names via bind params.

/// Bounded capacity for listener channels. Notifications are tiny
/// (three short strings) so 64 is a generous buffer without costing
/// meaningful memory; the slow consumer just pauses the worker via
/// backpressure, and if the consumer goes away entirely
/// the worker's send ends on close.
const LISTEN_CHANNEL_CAPACITY: usize = 64;

/// Poll tick for the listener worker's notification iterator. Upper
/// bound on the delay between `channel.close(listen_ch)` and the
/// worker dropping the pinned connection back into the pool.
const LISTEN_POLL_TICK_MS: u64 = 200;

/// Regex-free validator for LISTEN/UNLISTEN identifiers. Matches a
/// standard SQL unquoted identifier: `[A-Za-z_][A-Za-z0-9_]*`.
/// The `LISTEN <name>` / `UNLISTEN <name>` forms cannot be parameter-
/// bound, so we refuse anything that could break out of the identifier
/// slot. Names that need arbitrary bytes should use `pg_notify`'s
/// channel-name argument instead (which does accept parameters) — but
/// the listener side still has to emit an identifier, so for symmetry
/// we reject on both sides.
fn is_valid_ident(s: &str) -> bool {
    let mut iter = s.chars();
    let Some(first) = iter.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    iter.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Build a silt `Notification` record from a tokio-postgres notification.
fn notification_to_record(n: &postgres::Notification) -> Value {
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    fields.insert("channel".to_string(), Value::String(n.channel().into()));
    fields.insert("payload".to_string(), Value::String(n.payload().into()));
    fields.insert("pid".to_string(), Value::Int(n.process_id() as i64));
    Value::builtin_record(ty::NOTIFICATION, fields)
}

/// Long-lived worker for `listen`. Owns the `PooledConnection` for the
/// whole listener lifetime — when this function returns, the conn is
/// dropped back into the pool. LISTEN was already issued before this
/// function starts (see `listen`'s io-pool closure); we just pump
/// notifications until the channel closes or the connection errors out.
fn do_listen_worker(
    mut conn: PinnedConn,
    channel_name: String,
    ch: Arc<Channel>,
    scheduler: Arc<Scheduler>,
) {
    use std::time::Duration;

    let tick = Duration::from_millis(LISTEN_POLL_TICK_MS);

    'outer: loop {
        if ch.is_closed() {
            break;
        }
        // `timeout_iter` yields buffered notifications immediately, then
        // waits up to `tick` for new ones before returning. Each call
        // gives us a fresh iterator; we drain all ready notifications
        // on this pass and loop back to re-check `is_closed`.
        let mut notifications = conn.client_mut().notifications();
        let mut iter = notifications.timeout_iter(tick);
        loop {
            match iter.next() {
                Ok(Some(n)) => {
                    let rec = notification_to_record(&n);
                    if !scheduler.send_wait(&ch, rec) {
                        break 'outer;
                    }
                }
                // `None` = timeout elapsed with no notification. Re-
                // check the cancel flag and re-arm the iterator.
                Ok(None) => break,
                Err(_) => {
                    // Connection-level error: stop the worker. We
                    // don't surface the error on the channel because
                    // the silt-side contract is a stream of
                    // Notification records, not a Result wrapper.
                    break 'outer;
                }
            }
        }
    }

    // Best-effort UNLISTEN so the server stops queuing notifications for
    // this backend. Errors here are ignored — the conn is about to go
    // back into the pool, and r2d2 will recycle a broken conn on next
    // checkout anyway.
    let unlisten_sql = format!("UNLISTEN {channel_name}");
    let _ = conn.client_mut().batch_execute(&unlisten_sql);
    ch.close(Close::default(), scheduler.wake());
    // `conn` drops here → returns to the pool.
}

/// Blocking worker for `notify`. Uses `SELECT pg_notify($1, $2)` so the
/// channel name and payload are both bind parameters — no escaping
/// needed and dynamic channel names are supported.
fn do_notify(target: ExecutorRef, channel_name: String, payload: String) -> Value {
    let sql = "SELECT pg_notify($1, $2)";
    let params: [&(dyn ToSql + Sync); 2] = [&channel_name, &payload];
    match target {
        ExecutorRef::Pool(pool) => {
            let mut conn = match pool.get() {
                Ok(c) => c,
                Err(e) => return err(pool_error_value(&e)),
            };
            match conn.client_mut().execute(sql, &params) {
                Ok(_) => ok(Value::Unit),
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
        ExecutorRef::Tx(cell) => {
            let mut conn = cell.lock().unwrap_or_else(|e| e.into_inner());
            match conn.client_mut().execute(sql, &params) {
                Ok(_) => ok(Value::Unit),
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
    }
}

// ── trait Error for PgError ─────────────────────────────────────────

/// What `PgError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("PgConnect", [Value::String(m)]) => format!("postgres connect failed: {m}"),
        ("PgTls", [Value::String(m)]) => format!("postgres TLS error: {m}"),
        ("PgAuthFailed", [Value::String(m)]) => {
            format!("postgres authentication failed: {m}")
        }
        ("PgQuery", [Value::String(msg), Value::String(sqlstate)]) => {
            if sqlstate.is_empty() {
                format!("postgres query error: {msg}")
            } else {
                format!("postgres query error [{sqlstate}]: {msg}")
            }
        }
        ("PgTypeMismatch", [Value::String(col), Value::String(exp), Value::String(act)]) => {
            format!("postgres type mismatch on column `{col}`: expected {exp}, got {act}")
        }
        ("PgNoSuchColumn", [Value::String(col)]) => {
            format!("postgres: no such column `{col}`")
        }
        ("PgClosed", []) => "postgres connection closed".to_string(),
        ("PgTimeout", []) => "postgres operation timed out".to_string(),
        ("PgTxnAborted", []) => "postgres transaction aborted; rollback required".to_string(),
        ("PgUnknown", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── The functions ───────────────────────────────────────────────────

/// Test-only re-export of `pg_timeout_err` so integration tests can
/// lock the typed-timeout shape without needing a live DB. Returns the
/// exact `Value` a postgres operation gives when its deadline passes,
/// before it starts or while the task waits for it.
#[doc(hidden)]
pub fn pg_timeout_err_for_tests(msg: &str) -> Value {
    pg_timeout_err(crate::vm::IoFailure::Timeout(msg))
}

/// Test-only mirror of `parse_connect_opts`'s `max_pool_size`
/// extraction so integration tests can lock the shape without a live
/// DB. `None` if `opts` is no `Map(String, Int)`, as the argument of
/// `connect_with` is; else `Ok(Some(n))` if the map carries a valid
/// `max_pool_size` key, `Ok(None)` if the key is absent, `Err` if its
/// value is none a pool can have.
#[doc(hidden)]
pub fn read_max_pool_size_for_tests(opts: &Value) -> Option<Result<Option<u32>, String>> {
    let opts = Opts::take(opts)?;
    Some(parse_connect_opts(&opts).map(|parsed| parsed.max_pool_size))
}

/// The options of `connect_with` as the Rust `ConnectOpts` struct.
/// Unknown keys are ignored so options-bag additions stay
/// backwards-compatible.
fn parse_connect_opts(opts: &Opts) -> Result<ConnectOpts, String> {
    let mut out = ConnectOpts::default();
    for &(key, n) in &opts.0 {
        if key == "max_pool_size" {
            if n <= 0 {
                return Err(format!(
                    "postgres.connect_with: max_pool_size must be > 0, got {n}"
                ));
            }
            // r2d2 takes u32. Clamp to avoid a silent wrap.
            out.max_pool_size = Some(u32::try_from(n).unwrap_or(u32::MAX));
        }
    }
    Ok(out)
}

builtins! {
    fn connect(vm, url: &str) -> Result<Step, VmError> {
        let url = url.to_string();
        vm.io("postgres", pg_timeout_err, move || do_connect(url))
    }

    fn connect_with(vm, url: &str, opts: Opts) -> Started {
        let opts = parse_connect_opts(&opts).map_err(other_error)?;
        let url = url.to_string();
        Ok(vm.io("postgres", pg_timeout_err, move || do_connect_with(url, opts)))
    }

    fn query(vm, conn: &Value, sql: &str, params: List) -> Started {
        let target = resolve_executor(conn)?;
        let params = sql_params(params)?;
        let sql = sql.to_string();
        Ok(vm.io("postgres", pg_timeout_err, move || do_query(target, sql, params)))
    }

    fn execute(vm, conn: &Value, sql: &str, params: List) -> Started {
        let target = resolve_executor(conn)?;
        let params = sql_params(params)?;
        let sql = sql.to_string();
        Ok(vm.io("postgres", pg_timeout_err, move || do_execute(target, sql, params)))
    }

    // `postgres.transact(pool, callback)` — pins a single pooled
    // connection for the callback's lifetime.
    //
    // Lifecycle
    // ---------
    // 1. The call: check out a conn from the pool,
    //    run `BEGIN`, register the conn in the tx registry, mint a `PgTx`
    //    handle. The builtin goes on as a frame ([`Transact`]) that calls
    //    the callback with the handle.
    //
    // 2. Completion, when the callback has returned:
    //    - `Ok(_)` → `COMMIT` on the pinned conn.
    //    - `Err(_)` → `ROLLBACK`.
    //    - a non-Result value → `COMMIT`, wrap as `Ok`.
    //
    //    An error of the program inside the callback drops the frame,
    //    which rolls back.
    //
    //    The pinned conn is dropped after commit/rollback, returning it
    //    to the pool. COMMIT errors are surfaced as the function's Err;
    //    the callback's return value is discarded in that edge case.
    //
    // There is no nested transaction: a `PgTx` is no `PgPool`. `SAVEPOINT`
    // by hand does it.
    fn transact(pool: PoolHandle, f: &Value) -> Result<Step, Failed> {
        let pool = pool.open()?;
        let mut conn = pool.get().map_err(|e| pool_error_value(&e))?;
        conn.client_mut()
            .batch_execute("BEGIN")
            .map_err(|e| pg_error_to_variant(&e))?;
        Ok(Step::Run(Box::new(Transact {
            tx_id: Some(insert_tx(conn)),
            callback: Some(f.clone()),
        })))
    }

    // `postgres.stream(target, sql, params)` — returns a bounded silt
    // `Channel` of `Result(Map(String, Value), PgError)` row wrappers.
    //
    // The caller does not wait for the worker, which is
    // long-lived: it produces many rows over time, not a single result.
    // We submit it to the io_pool and return the channel immediately,
    // without parking the caller. Backpressure is via the channel's
    // bounded capacity; cancellation is via `channel.close` (checked
    // between rows).
    fn stream(vm, conn: &Value, sql: &str, params: List) -> Result<Value, Failed> {
        let target = resolve_executor(conn)?;
        let params = sql_params(params)?;

        // Mint the channel up-front so we can hand the caller an owned
        // Arc while the worker holds its own clone.
        let ch_id = vm.next_channel_id();
        let channel = Channel::new(ch_id, STREAM_CHANNEL_CAPACITY);
        let worker_channel = channel.clone();

        // Fire-and-forget: submit to io_pool. The completion's return
        // value is unused (we return the channel, not a completion).
        let sql = sql.to_string();
        let scheduler = vm.scheduler().clone();
        let op = vm.runtime.io_pool.submit(pg_timeout_err, move || {
            do_stream_worker(target, sql, params, worker_channel, scheduler);
            Value::Unit
        });
        op.detach();

        Ok(ok(Value::Channel(channel)))
    }

    // (A cursor lives inside a transaction: `tx` is a `PgTx`.)
    fn cursor(vm, tx: TxHandle, sql: &str, params: List, batch_size: i64) -> Started {
        let cell = tx.open()?;
        let Ok(batch_size @ 1..) = u64::try_from(batch_size) else {
            return Err(other_error("postgres.cursor: batch_size must be >= 1".to_string()).into());
        };
        let params = sql_params(params)?;
        let sql = sql.to_string();
        Ok(vm.io("postgres", pg_timeout_err, move || {
            do_cursor_open(tx.0, cell, sql, params, batch_size)
        }))
    }

    fn cursor_next(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        vm.io("postgres", pg_timeout_err, move || do_cursor_next(cursor.0))
    }

    fn cursor_close(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        vm.io("postgres", pg_timeout_err, move || do_cursor_close(cursor.0))
    }

    fn close(pool: PoolHandle) {
        remove_pool(pool.0);
    }

    // A fresh UUID v7 (RFC 9562). Time-ordered: first 48 bits are Unix
    // timestamp in ms, remainder is random. Good for B-tree primary keys
    // (monotonic inserts) while being unguessable. Returned as a
    // lowercase hyphenated string.
    fn uuidv7(vm) -> String {
        super::uuid::now_v7(vm).to_string()
    }

    // `postgres.listen(pool, channel_name)` — open a notification
    // listener. Returns `Result(Channel, PgError)`. On success the channel
    // yields `Notification` records until the underlying conn dies or the
    // caller calls `channel.close`.
    //
    // The LISTEN statement is issued synchronously inside this function (on
    // the io_pool) so LISTEN errors — e.g. an invalid channel name that
    // somehow slipped past the identifier validator, or a pool checkout
    // failure — surface as an immediate `Err(PgError)` rather than a
    // channel that silently closes. The long-lived worker is spawned only
    // after LISTEN succeeds.
    fn listen(vm, pool: PoolHandle, channel: &str) -> Started {
        let pool = pool.open()?;
        if !is_valid_ident(channel) {
            return Err(other_error(format!(
                "postgres.listen: channel name {channel:?} must be a valid SQL identifier \
                 (letters, digits, underscore; cannot start with digit)"
            ))
            .into());
        }

        // Mint the silt-side channel up front so we can hand the caller an
        // owned Arc synchronously.
        let ch_id = vm.next_channel_id();
        let worker_channel = Channel::new(ch_id, LISTEN_CHANNEL_CAPACITY);

        // LISTEN happens on the io_pool so it doesn't block the VM thread,
        // but we still wait for its completion before returning so LISTEN
        // errors propagate cleanly. Use the same entry-guard dance as
        // query/execute to park-and-resume the scheduled task.
        if let Some(r) = vm.deadline_exceeded_with(pg_timeout_err) {
            return Ok(Ok(Step::Done(r)));
        }

        // Blocking work we submit to io_pool: checkout + LISTEN. On success
        // we spawn the long-lived worker and return the silt channel; on
        // failure we return the PgError without spawning. We need a handle
        // to the io_pool inside the closure so it can spawn the worker
        // itself — `runtime` is an `Arc<Runtime>` so the clone is cheap.
        let runtime = vm.runtime.clone();
        let channel_name = channel.to_string();
        let work = move || -> Value {
            let mut conn = match pool.get() {
                Ok(c) => c,
                Err(e) => return err(pool_error_value(&e)),
            };
            let listen_sql = format!("LISTEN {channel_name}");
            if let Err(e) = conn.client_mut().batch_execute(&listen_sql) {
                return err(pg_error_to_variant(&e));
            }
            // LISTEN ok — spawn the long-lived worker onto the io_pool.
            // The worker owns `conn` for its whole lifetime; on exit the
            // conn drops back into the pool.
            let worker_ch = worker_channel.clone();
            let scheduler = runtime.scheduler.clone();
            let op = runtime.io_pool.submit(pg_timeout_err, move || {
                do_listen_worker(conn, channel_name, worker_ch, scheduler);
                Value::Unit
            });
            op.detach();
            ok(Value::Channel(worker_channel))
        };

        Ok(vm.io_started("postgres", pg_timeout_err, work))
    }

    // Fires a NOTIFY, on a pool or in a transaction. Uses
    // `SELECT pg_notify($1, $2)` so channel names are bind-parameter
    // safe.
    fn notify(vm, conn: &Value, channel: &str, payload: &str) -> Started {
        let target = resolve_executor(conn)?;
        let (channel, payload) = (channel.to_string(), payload.to_string());
        Ok(vm.io("postgres", pg_timeout_err, move || do_notify(target, channel, payload)))
    }
}

/// A transaction whose callback is running.
struct Transact {
    /// The registered transaction, until it is committed or rolled
    /// back.
    tx_id: Option<u64>,
    /// The callback, until it is called.
    callback: Option<Value>,
}

impl Native for Transact {
    fn name(&self) -> &str {
        "postgres.transact"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        let Some(tx_id) = self.tx_id else {
            return Ok(Step::Done(input));
        };
        if let Some(callee) = self.callback.take() {
            return Ok(vm.call(callee, [make_tx_handle(tx_id)]));
        }
        self.tx_id = None;
        Ok(Step::Done(end_tx(tx_id, Some(input))))
    }

    fn abandon(&mut self, _vm: &mut Vm) {
        if let Some(tx_id) = self.tx_id.take() {
            end_tx(tx_id, None);
        }
    }
}

/// End the transaction `tx_id`: commit or roll back as the callback's
/// value `returned` says, or roll back when the callback failed with
/// an error of the program (`None`). The value is `postgres.transact`'s.
fn end_tx(tx_id: u64, returned: Option<Value>) -> Value {
    // Pull the conn out of the tx registry and finalise it.
    let Some(cell) = remove_tx(tx_id) else {
        // Shouldn't happen — the registry entry was valid on entry
        // and nothing else removes it. If it does, surface the
        // callback's result as-is and skip commit/rollback.
        return returned.unwrap_or(Value::Unit);
    };
    // We are the last owner of `cell`. Moving the conn out of the
    // `Arc<Mutex<...>>` requires unwrapping both layers. `Arc::try_unwrap`
    // can fail if a racing query on the io_pool thread still holds a
    // reference — that's a logic bug (all query submits for this tx
    // should have completed by now: the callback has returned). In the
    // rare race, fall back to locking in-place and running COMMIT /
    // ROLLBACK against the locked cell without reclaiming ownership.
    let finalise = |sql: &str, cell: Arc<Mutex<PinnedConn>>| -> Option<Value> {
        match Arc::try_unwrap(cell) {
            Ok(mutex) => {
                let mut conn = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = conn.client_mut().batch_execute(sql) {
                    return Some(err(pg_error_to_variant(&e)));
                }
                // `conn` drops here → returns to pool.
                None
            }
            Err(shared) => {
                // Can't reclaim ownership; run against the locked cell.
                // The conn lingers until the other reference is dropped.
                let mut conn = shared.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = conn.client_mut().batch_execute(sql) {
                    return Some(err(pg_error_to_variant(&e)));
                }
                None
            }
        }
    };

    // Any cursors opened inside the tx are about to become invalid —
    // PG drops server-side cursors at tx end, so we only need to reap
    // the local registry entries. Do this before running COMMIT /
    // ROLLBACK so a subsequent `cursor_next` with the same id sees a
    // clean "not registered" error rather than racing with the reap.
    drain_cursors_for_tx(tx_id);

    match returned {
        Some(v) => match &v {
            Value::Variant(variant) if variant.is(bv::OK) => finalise("COMMIT", cell).unwrap_or(v),
            Value::Variant(variant) if variant.is(bv::ERR) => {
                let _ = finalise("ROLLBACK", cell);
                v
            }
            // Callback didn't return Result(_, _). Treat as Ok and
            // forward the raw value, but COMMIT first so the user's
            // statements persist. Mirrors what most pg wrappers do
            // for callbacks that return a non-Result type.
            _ => finalise("COMMIT", cell).unwrap_or_else(|| ok(v)),
        },
        None => {
            let _ = finalise("ROLLBACK", cell);
            Value::Unit
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ssl_params_preserves_rest() {
        let url = "postgres://u:p@h/db?connect_timeout=5&sslmode=verify-full&sslrootcert=/tmp/ca.pem&application_name=x";
        let (stripped, params) = extract_ssl_url_params(url).expect("parse ok");
        assert_eq!(params.mode, Some(EffectiveSslMode::VerifyFull));
        assert_eq!(params.root_cert.as_deref(), Some("/tmp/ca.pem"));
        // Stripped URL keeps connect_timeout and application_name, in
        // original order, and drops sslmode/sslrootcert.
        assert!(stripped.contains("connect_timeout=5"));
        assert!(stripped.contains("application_name=x"));
        assert!(!stripped.contains("sslmode"));
        assert!(!stripped.contains("sslrootcert"));
    }

    /// No query string → pass-through unchanged.
    #[test]
    fn strip_ssl_params_no_query() {
        let url = "postgres://u:p@h/db";
        let (stripped, params) = extract_ssl_url_params(url).expect("parse ok");
        assert_eq!(stripped, url);
        assert_eq!(params.mode, None);
        assert_eq!(params.root_cert, None);
    }

    /// All standard sslmode values parse to the right variant.
    #[test]
    fn strip_ssl_params_modes() {
        for (s, expected) in [
            ("disable", EffectiveSslMode::Disable),
            ("prefer", EffectiveSslMode::Prefer),
            ("require", EffectiveSslMode::Require),
            ("verify-ca", EffectiveSslMode::VerifyCa),
            ("verify-full", EffectiveSslMode::VerifyFull),
        ] {
            let url = format!("postgres://u:p@h/db?sslmode={s}");
            let (_, p) = extract_ssl_url_params(&url).expect("parse ok");
            assert_eq!(p.mode, Some(expected), "sslmode={s}");
        }
    }

    /// In a non-TLS build (base `postgres` feature only), connecting
    /// with `sslmode=require` must surface a clean, actionable error
    /// rather than silently falling back to NoTls.
    #[test]
    #[cfg(not(feature = "postgres-tls"))]
    fn connect_require_without_tls_feature_errors() {
        let result = do_connect("postgres://x:y@127.0.0.1/db?sslmode=require".to_string());
        let Value::Variant(outer) = &result else {
            panic!("expected Variant, got {result:?}");
        };
        assert_eq!(outer.name(), "Err");
        let inner = outer.fields().first().expect("err payload");
        let Value::Variant(error) = inner else {
            panic!("expected inner Variant, got {inner:?}");
        };
        // Error redesign Phase 2: non-TLS builds that are asked for
        // TLS now surface `PgTls(msg)` instead of the legacy
        // `ConnectionError`. The message text still mentions TLS /
        // postgres-tls so operators see the feature-flag hint.
        assert_eq!(error.name(), "PgTls");
        let Some(Value::String(msg)) = error.fields().first() else {
            panic!("expected message string");
        };
        assert!(msg.contains("TLS"), "message: {msg}");
        assert!(msg.contains("postgres-tls"), "message: {msg}");
    }

    /// Same as above for `verify-ca` / `verify-full`.
    #[test]
    #[cfg(not(feature = "postgres-tls"))]
    fn connect_verify_modes_without_tls_feature_error() {
        for mode in ["verify-ca", "verify-full"] {
            let url = format!("postgres://x:y@127.0.0.1/db?sslmode={mode}");
            let result = do_connect(url);
            let Value::Variant(outer) = &result else {
                panic!("expected Variant for {mode}");
            };
            assert_eq!(outer.name(), "Err", "{mode} should Err");
            let inner = outer.fields().first().expect("err payload");
            let Value::Variant(error) = inner else {
                panic!("expected inner Variant for {mode}");
            };
            let Some(Value::String(msg)) = error.fields().first() else {
                panic!("expected message string for {mode}");
            };
            assert!(
                msg.contains("postgres-tls"),
                "{mode} message missing feature hint: {msg}"
            );
        }
    }

    /// Round-60 L6 behavioural companion to the source-grep lock in
    /// tests/meta/postgres_poisoned_lock_tests.rs. Force-poisons each of the
    /// three module-private registry mutexes (pool / tx / cursor) by
    /// panicking a background thread while it holds the guard, then
    /// exercises the recovery-pattern call sites and asserts they
    /// return cleanly instead of re-panicking.
    ///
    /// A regression that reintroduces a bare unwrap on the guard
    /// (rather than `unwrap_or_else(|e| e.into_inner())`) on any of
    /// the probed accessors would re-panic here and fail the test
    /// — which is exactly the runtime-level guarantee the source-grep
    /// form cannot verify.
    ///
    /// NOTE: the Rust test harness prints "thread '<name>' panicked
    /// at 'deliberate poison: ...'" to stderr three times during this
    /// test. That output is EXPECTED — the panics are deliberate, they
    /// are caught by `JoinHandle::join`, and the test passes when the
    /// post-poison registry operations succeed.
    #[test]
    fn poisoned_registry_locks_are_recovered_not_repanicked() {
        use std::thread;

        // Pool registry. Spawn a thread that acquires the guard and
        // panics, unwinding through the `MutexGuard` Drop and poisoning
        // the mutex. `.join()` returns Err(Box<dyn Any>) carrying the
        // panic payload, which we discard.
        let pool_reg = registry();
        thread::Builder::new()
            .name("poison-pool-registry".into())
            .spawn(|| {
                let _guard = registry().lock().expect("first lock on fresh mutex");
                panic!("deliberate poison: pool registry");
            })
            .unwrap()
            .join()
            .expect_err("poison thread must have panicked");
        assert!(pool_reg.is_poisoned(), "pool registry should be poisoned");
        // Both ops use `.lock().unwrap_or_else(|e| e.into_inner())` —
        // must not re-panic on the poisoned guard.
        assert!(lookup_pool(u64::MAX).is_none());
        assert!(!remove_pool(u64::MAX));

        // Tx registry.
        let tx_reg = tx_registry();
        thread::Builder::new()
            .name("poison-tx-registry".into())
            .spawn(|| {
                let _guard = tx_registry().lock().expect("first lock on fresh mutex");
                panic!("deliberate poison: tx registry");
            })
            .unwrap()
            .join()
            .expect_err("poison thread must have panicked");
        assert!(tx_reg.is_poisoned(), "tx registry should be poisoned");
        assert!(lookup_tx(u64::MAX).is_none());
        assert!(remove_tx(u64::MAX).is_none());

        // Cursor registry.
        let cursor_reg = cursors();
        thread::Builder::new()
            .name("poison-cursor-registry".into())
            .spawn(|| {
                let _guard = cursors().lock().expect("first lock on fresh mutex");
                panic!("deliberate poison: cursor registry");
            })
            .unwrap()
            .join()
            .expect_err("poison thread must have panicked");
        assert!(
            cursor_reg.is_poisoned(),
            "cursor registry should be poisoned"
        );
        assert!(lookup_cursor(u64::MAX).is_none());
        assert!(remove_cursor(u64::MAX).is_none());
        // drain_cursors_for_tx returns (); just verify it doesn't
        // panic on the poisoned guard.
        drain_cursors_for_tx(u64::MAX);
    }

    /// Live smoke test against a local Postgres. Ignored by default —
    /// run with:
    ///   cargo test --features postgres -- --ignored postgres::
    /// Assumes a Postgres listening on 127.0.0.1 with the
    /// `silt_orm:silt_orm_dev` role and `silt_orm_test` DB.
    #[test]
    #[ignore]
    fn live_select_one() {
        let result = do_connect(
            "postgres://silt_orm:silt_orm_dev@127.0.0.1/silt_orm_test?sslmode=disable".to_string(),
        );
        let Value::Variant(connected) = &result else {
            panic!("expected Variant, got {result:?}");
        };
        assert_eq!(connected.name(), "Ok", "connect failed: {result:?}");
        let handle = connected.fields().first().cloned().expect("pool handle");
        let handle = PoolHandle::take(&handle).expect("a pool handle");
        let pool = handle.open().expect("registered");

        let q = do_query(ExecutorRef::Pool(pool), "SELECT 1".to_string(), Vec::new());
        let Value::Variant(queried) = &q else {
            panic!("expected Variant from query: {q:?}");
        };
        assert_eq!(queried.name(), "Ok", "query failed: {q:?}");
        // Payload is `QueryResult { rows: [...] }`. Pull out the rows
        // list and confirm one row with `?column?` = 1.
        let record = queried.fields().first().expect("query result record");
        let Value::Record(_, fields) = record else {
            panic!("expected Record, got {record:?}");
        };
        let rows = fields.get("rows").expect("rows field");
        let Value::List(rows) = rows else {
            panic!("expected rows list, got {rows:?}");
        };
        let rows = rows.to_vec().expect("a list of rows");
        assert_eq!(rows.len(), 1, "expected 1 row, got {}", rows.len());
        // Each row is a `Map<Value::String, Value>`.
        let Value::Map(row_map) = &rows[0] else {
            panic!("expected row Map, got {:?}", rows[0]);
        };
        let col = row_map
            .get(&Value::String("?column?".into()))
            .expect("column ?column? missing");
        // Wrapped in VInt variant.
        let Value::Variant(cell) = col else {
            panic!("expected VInt variant, got {col:?}");
        };
        assert_eq!(cell.name(), "VInt");
        assert_eq!(cell.fields().first(), Some(&Value::Int(1)));

        let _ = remove_pool(handle.0);
    }
}
