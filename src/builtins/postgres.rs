//! Postgres builtin module (`postgres.*`).
//!
//! Provides connection pooling, parameterised query/execute, and
//! transaction support against a Postgres server. Modeled on the sync
//! `postgres` crate + an `r2d2` connection pool, mirroring the
//! pattern used by the `http` builtin (sync ureq + io_pool bridge).
//!
//! Pool handles are opaque to silt code: a `PgPool` variant with one
//! field, an `Int`, carries an integer id into a process-global side
//! table that owns the actual `r2d2::Pool`. Explicit `postgres.close`
//! is required to drop the pool.
//!
//! Transactions pin a single `r2d2::PooledConnection` for the callback's
//! entire lifetime. A `PgTx` variant with one field, an `Int`,
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
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fallible_iterator::FallibleIterator;
use postgres::NoTls;
use postgres::types::{IsNull, Kind, ToSql, Type as PgType};
use r2d2::Pool;

use super::common::ok;
use super::typed::{Arg, List, Map, Ret, builtins};
use crate::runtime::sync::{Arm, Fired, Wait};
use crate::runtime::sync::{Channel, Close};
use crate::scheduler::IO_TIMEOUT;
use crate::scheduler::Scheduler;
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::{IoOp, Native, Step, Vm, VmError};

/// The error of a `postgres` function whose operation has no value
/// of its own: `Timeout` when its deadline passed, `Other` with
/// the reason when it could not run or panicked.
fn pg_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::PG_TIMEOUT, vec![]),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::PG_OTHER, vec![Value::String(why.into())])
        }
    };
    Value::variant(bv::ERR, vec![error])
}

// ── The pool and its connections ────────────────────────────────────
//
// The pool is r2d2's, with a manager of the module's own: it hands
// out the connection that `connect` made first, its connections carry
// what a cancel request needs, and they can be marked broken, so that
// the pool drops them instead of taking them back.

/// How long connecting may take when the URL sets no
/// `connect_timeout`: the first connection, which `connect` makes
/// itself, and each one the pool opens later.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How a pool's connections are encrypted, once `sslmode` is read.
#[derive(Clone)]
enum Tls {
    Off,
    #[cfg(feature = "postgres-tls")]
    On(postgres_native_tls::MakeTlsConnector),
}

impl Tls {
    fn connect(&self, config: &postgres::Config) -> Result<postgres::Client, postgres::Error> {
        match self {
            Tls::Off => config.connect(NoTls),
            #[cfg(feature = "postgres-tls")]
            Tls::On(connector) => config.connect(connector.clone()),
        }
    }
}

/// What asks the server to cancel the statement that runs on one
/// connection: the request goes over a connection of its own, which
/// is why it can be sent while the statement holds the connection.
#[derive(Clone)]
struct Canceller {
    token: postgres::CancelToken,
    tls: Tls,
    /// A cancel request was sent, or is on its way. The server acts on
    /// a request when it comes, and one that comes after the statement
    /// it was for has ended cancels the connection's next statement,
    /// whoever runs it: such a connection never goes back to the pool,
    /// and its transaction counts as aborted.
    sent: Arc<std::sync::atomic::AtomicBool>,
}

impl Canceller {
    /// A cancel request is on its way: from now on the connection
    /// counts as one for which it was sent.
    fn mark(&self) {
        self.sent.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn was_sent(&self) -> bool {
        self.sent.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Ask once. The server does not say whether a statement was
    /// cancelled, and a request that comes when none runs does nothing.
    fn cancel(&self) {
        self.mark();
        let _ = match &self.tls {
            Tls::Off => self.token.cancel_query(NoTls),
            #[cfg(feature = "postgres-tls")]
            Tls::On(connector) => self.token.cancel_query(connector.clone()),
        };
    }
}

/// A connection of a pool.
struct Conn {
    client: postgres::Client,
    canceller: Canceller,
    /// It must not go back to the pool: its transaction did not roll
    /// back. (One for which a cancel request was sent does not go back
    /// either: `Canceller::sent`.)
    broken: bool,
}

impl Conn {
    fn client_mut(&mut self) -> &mut postgres::Client {
        &mut self.client
    }
}

struct Manager {
    config: postgres::Config,
    tls: Tls,
    /// How long a connect may take, login included.
    limit: Duration,
    stalled: Stalled,
    /// The connection `connect` made itself, the first one the pool
    /// takes.
    first: Mutex<Option<postgres::Client>>,
}

impl r2d2::ManageConnection for Manager {
    type Connection = Conn;
    type Error = ConnectFailure;

    fn connect(&self) -> Result<Conn, ConnectFailure> {
        let first = self.first.lock().unwrap_or_else(|e| e.into_inner()).take();
        let client = match first {
            Some(client) => client,
            None => connect_within(&self.config, &self.tls, self.limit, &self.stalled)?,
        };
        Ok(Conn {
            canceller: Canceller {
                token: client.cancel_token(),
                tls: self.tls.clone(),
                sent: Arc::default(),
            },
            client,
            broken: false,
        })
    }

    fn is_valid(&self, conn: &mut Conn) -> Result<(), ConnectFailure> {
        conn.client
            .simple_query("")
            .map(|_| ())
            .map_err(ConnectFailure::Failed)
    }

    fn has_broken(&self, conn: &mut Conn) -> bool {
        conn.broken || conn.canceller.was_sent() || conn.client.is_closed()
    }
}

type PgPool = Pool<Manager>;
type PinnedConn = r2d2::PooledConnection<Manager>;

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
/// `Mutex<TxConn>` is acquired per-query on the io_pool thread.
///
/// Keeping the inner lock scoped to a single query (rather than held
/// across VM re-entries) avoids deadlocks if the scheduler reschedules
/// another task that tries to touch the same tx handle while the
/// callback is mid-yield. The outer cell's `Arc` survives yields safely
/// because the registry holds it between calls.
struct TxRegistry {
    next_id: u64,
    txs: BTreeMap<u64, TxEntry>,
}

/// A transaction: its connection, and what cancels the statement that
/// runs on it, which needs no lock (the statement holds it).
struct TxEntry {
    cell: Arc<Mutex<TxConn>>,
    canceller: Canceller,
}

/// The connection of a transaction, as its statements share it.
struct TxConn {
    conn: PinnedConn,
    /// The transaction has ended, or is ending: no statement runs on
    /// the connection any more.
    ended: bool,
}

impl TxConn {
    fn client_mut(&mut self) -> &mut postgres::Client {
        self.conn.client_mut()
    }

    /// End the transaction with `ROLLBACK`. A connection whose
    /// transaction did not end so is not given back to the pool.
    fn roll_back(&mut self) -> Result<(), postgres::Error> {
        self.ended = true;
        let ended = self.conn.client_mut().batch_execute("ROLLBACK");
        if ended.is_err() {
            self.conn.broken = true;
        }
        ended
    }

    /// End the transaction with `COMMIT`, or with the error of a commit
    /// that failed, after which it is rolled back.
    ///
    /// The server answers the `COMMIT` of a transaction that a failed
    /// statement aborted with a rollback and no error. So the `COMMIT`
    /// goes with a statement before it, in the same round trip, which
    /// fails in such a transaction (25P02, `TxnAborted`), and the
    /// `COMMIT` is then not run.
    ///
    /// A transaction for one of whose statements a cancel request was
    /// sent counts as aborted too (`tx_conn`): it is rolled back.
    fn commit(&mut self) -> Result<(), Value> {
        if self.conn.canceller.was_sent() {
            let _ = self.roll_back();
            return Err(Value::variant(bv::PG_TXN_ABORTED, vec![]));
        }
        self.ended = true;
        let committed = self.conn.client_mut().batch_execute("SELECT 1; COMMIT");
        if committed.is_err() {
            let _ = self.roll_back();
        }
        committed.map_err(|e| pg_error_to_variant(&e))
    }
}

impl Drop for TxConn {
    /// A connection dropped in the middle of its transaction never goes
    /// back to the pool.
    fn drop(&mut self) {
        if !self.ended {
            self.conn.broken = true;
        }
    }
}

/// The connection of a transaction, for a statement: the
/// `PostgresError` of one whose transaction has ended. A statement that
/// was on its way when the transaction ended would otherwise run after
/// its end, outside it.
/// The `PostgresError` of a statement on a transaction that has ended:
/// `Closed`, as the connection is, for the transaction.
fn tx_ended() -> Value {
    Value::variant(bv::PG_CLOSED, vec![])
}

fn tx_conn(cell: &Mutex<TxConn>) -> Result<std::sync::MutexGuard<'_, TxConn>, Value> {
    let conn = cell.lock().unwrap_or_else(|e| e.into_inner());
    if conn.ended {
        return Err(tx_ended());
    }
    // A statement whose task stopped waiting for it was cancelled: the
    // transaction is aborted, whether or not the request reached the
    // statement in time.
    match conn.conn.canceller.was_sent() {
        false => Ok(conn),
        true => Err(Value::variant(bv::PG_TXN_ABORTED, vec![])),
    }
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
    let canceller = conn.canceller.clone();
    let cell = Arc::new(Mutex::new(TxConn { conn, ended: false }));
    let mut reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = reg.next_id;
    reg.next_id = reg.next_id.wrapping_add(1);
    reg.txs.insert(id, TxEntry { cell, canceller });
    id
}

fn lookup_tx(id: u64) -> Option<Arc<Mutex<TxConn>>> {
    let reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.txs.get(&id).map(|entry| entry.cell.clone())
}

fn remove_tx(id: u64) -> Option<TxEntry> {
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
// registry rows. Their ids are kept, as runs of ids (`ended`), so that
// a `cursor_next` on one says that its transaction has ended, as every
// use of an ended `PgTx` does, while one on a cursor the program closed
// itself says that.

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
    /// The ids of the cursors whose transaction has ended, as runs:
    /// the first id of each, and its last.
    ended: BTreeMap<u64, u64>,
}

impl CursorRegistry {
    /// The cursor `id`'s transaction has ended.
    fn end(&mut self, id: u64) {
        let before = self.ended.range(..id).next_back().map(|(&f, &l)| (f, l));
        let after = self.ended.get(&(id + 1)).copied();
        let first = match before {
            Some((first, last)) if last + 1 == id => first,
            _ => id,
        };
        let last = match after {
            Some(last) => {
                self.ended.remove(&(id + 1));
                last
            }
            None => id,
        };
        self.ended.insert(first, last);
    }

    /// Whether the cursor `id`'s transaction has ended.
    fn has_ended(&self, id: u64) -> bool {
        self.ended
            .range(..=id)
            .next_back()
            .is_some_and(|(_, &last)| last >= id)
    }
}

fn cursors() -> &'static Mutex<CursorRegistry> {
    static REG: OnceLock<Mutex<CursorRegistry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(CursorRegistry {
            next_id: 1,
            cursors: BTreeMap::new(),
            ended: BTreeMap::new(),
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
    let of_tx: Vec<u64> = reg
        .cursors
        .iter()
        .filter(|(_, entry)| entry.tx_id == tx_id)
        .map(|(&id, _)| id)
        .collect();
    for id in of_tx {
        reg.cursors.remove(&id);
        reg.end(id);
    }
}

/// Whether the cursor `id`'s transaction has ended.
fn cursor_tx_ended(id: u64) -> bool {
    let reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    reg.has_ended(id)
}

// ── The streams of a transaction ───────────────────────────────────
//
// A stream opened on a `PgTx` holds the transaction's connection for
// as long as its worker runs. When the transaction ends, its streams
// are closed: a worker that waits for room ends, and gives the
// connection back for the COMMIT or ROLLBACK.

fn tx_streams() -> &'static Mutex<BTreeMap<u64, Vec<Weak<Channel>>>> {
    static REG: OnceLock<Mutex<BTreeMap<u64, Vec<Weak<Channel>>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn register_tx_stream(tx_id: u64, channel: &Arc<Channel>) {
    let mut reg = tx_streams().lock().unwrap_or_else(|e| e.into_inner());
    reg.entry(tx_id).or_default().push(Arc::downgrade(channel));
}

/// Close the streams of the transaction `tx_id` that are still there.
fn close_tx_streams(tx_id: u64, wake: &dyn crate::runtime::sync::Wake) {
    let streams = {
        let mut reg = tx_streams().lock().unwrap_or_else(|e| e.into_inner());
        reg.remove(&tx_id).unwrap_or_default()
    };
    for stream in streams.iter().filter_map(Weak::upgrade) {
        stream.close(Close::default(), wake);
    }
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
/// The scrub only applies to strings destined for silt-side `PostgresError`.
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

// ── PostgresError helpers (Phase 2 of stdlib error redesign) ─────────────
//
// Every fallible postgres.* call now surfaces a typed `PostgresError` variant
// wrapped in `Err(...)`. Variants (declared by the module's `types` in the builtin registry):
//   Connect(msg)         — pool checkout / URL parse / transport setup
//   Tls(msg)             — TLS handshake / setup / cert read
//   AuthFailed(msg)      — SQLSTATE class 28 (invalid auth)
//   Query(msg, sqlstate) — any other DbError with a SQLSTATE
//   TypeMismatch(col, expected, actual)
//   NoSuchColumn(col)
//   Closed               — connection closed / broken pipe
//   Timeout              — query canceled / statement timeout
//   TxnAborted           — 25P02 in_failed_sql_transaction
//   Other(msg)         — fallback for shapes we can't classify
//
// All message strings are first run through `redact_pg_message` so
// DETAIL / WHERE / HINT follow-on rows don't leak across the VM boundary.

/// Build a bare `Connect(msg)` variant (no Err wrapper).
fn pg_connect(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_CONNECT,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Build a bare `Tls(msg)` variant (no Err wrapper).
fn pg_tls(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_TLS,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Build a bare `Other(msg)` variant (no Err wrapper).
fn pg_unknown(msg: impl Into<String>) -> Value {
    Value::variant(
        bv::PG_OTHER,
        vec![Value::String(redact_pg_message(&msg.into()).into())],
    )
}

/// Classify a `postgres::Error` into the matching `PostgresError` variant.
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
        // more specific variant where we can; fall back to Other.
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
            Value::variant(bv::PG_OTHER, vec![Value::String(scrubbed.into())])
        }
    }
}

/// Build a typed `PostgresError` variant from an `r2d2::Error`: a
/// checkout that found no connection in the pool's 30 s. r2d2 wraps the
/// underlying postgres error inside — we classify on the Display text:
/// a timeout while waiting for a slot maps to `Timeout`, anything else
/// to `Connect`.
fn pool_error_value(e: &r2d2::Error) -> Value {
    let raw = format!("{e}");
    let lc = raw.to_ascii_lowercase();
    if lc.contains("timed out") || lc.contains("timeout") {
        Value::variant(bv::PG_TIMEOUT, vec![])
    } else {
        pg_connect(raw)
    }
}

/// The `PostgresError` of a connect that failed: `AuthFailed` when the
/// server refused the role or its password (SQLSTATE class `28`), `Tls`
/// for the handshake, `Timeout` for a socket that got no answer in
/// time, and `Connect` for every other reason: a refused port, an
/// unknown host, a database that does not exist, a URL that lacks
/// something.
fn connect_error_value(e: &postgres::Error) -> Value {
    if let Some(db) = e.as_db_error() {
        let message = redact_pg_message(db.message());
        return match db.code().code().starts_with("28") {
            true => Value::variant(bv::PG_AUTH_FAILED, vec![Value::String(message.into())]),
            false => pg_connect(message),
        };
    }
    // The error's text names its kind only ("error connecting to
    // server"): the causes say what happened.
    let mut text = e.to_string();
    let (mut timed_out, mut tls) = (false, false);
    let causes = std::iter::successors(std::error::Error::source(e), |cause| cause.source());
    for cause in causes {
        text = format!("{text}: {cause}");
        let io = cause.downcast_ref::<std::io::Error>();
        timed_out |= io.is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut);
        tls |= is_tls_failure(cause);
    }
    if tls {
        pg_tls(text)
    } else if timed_out {
        Value::variant(bv::PG_TIMEOUT, vec![])
    } else if text.starts_with("authentication error") {
        // The exchange of the login failed on this side: a method of
        // the server's that the client does not have, channel binding
        // the server did not use, a SCRAM answer that does not check
        // out. (A password the server asks for and the URL does not
        // give is an error of the configuration: `Connect`.)
        Value::variant(bv::PG_AUTH_FAILED, vec![Value::String(text.into())])
    } else {
        pg_connect(text)
    }
}

/// Whether `cause` is a failure of TLS.
#[cfg(feature = "postgres-tls")]
fn is_tls_failure(cause: &(dyn std::error::Error + 'static)) -> bool {
    cause.downcast_ref::<native_tls::Error>().is_some()
}

/// Whether `cause` is a failure of TLS: never, in a build without it.
#[cfg(not(feature = "postgres-tls"))]
fn is_tls_failure(_cause: &(dyn std::error::Error + 'static)) -> bool {
    false
}

/// Whether `error` is the `PostgresError` `Timeout`.
#[cfg(feature = "postgres-tls")]
fn is_pg_timeout(error: &Value) -> bool {
    matches!(error, Value::Variant(variant) if variant.is(bv::PG_TIMEOUT))
}

/// Ad-hoc `Other(msg)` for misuse errors (wrong variant shape, bad
/// argument types, cursor registry miss). These aren't transport or
/// query failures — they're programmer errors that still need to
/// surface as a typed `PostgresError` because the signature says so.
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
    match crate::value::Float::new(f) {
        Some(f) => Value::variant(bv::V_FLOAT, vec![Value::Float(f)]),
        None => wrap_v_str(format!("<decode error: non-finite float {f}>")),
    }
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
            "postgres requires PgValue variant (VInt/VStr/...), got {}",
            v.kind()
        ));
    };
    if !param.of(ty::PG_VALUE) {
        return Err(format!(
            "postgres requires PgValue variant (VInt/VStr/...), got {}",
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
            Some(Value::Float(f)) => Ok(SqlParam::Float8(f.get())),
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
            "postgres: unknown silt PgValue variant {other:?} as parameter"
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
                            Some(Value::Float(f)) => out.push(Some(f.get())),
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
    let columns = row.columns().iter().enumerate();
    Value::map(
        columns.map(|(idx, col)| (Value::String(col.name().into()), pg_cell_to_value(row, idx))),
    )
}

fn make_query_result(rows: Vec<Value>) -> Value {
    let row_count = rows.len() as i64;
    Value::builtin_record(
        ty::QUERY_RESULT,
        [
            ("row_count", Value::Int(row_count)),
            ("rows", Value::list(rows)),
        ],
    )
}

fn make_exec_result(affected: u64, returning: Vec<Value>) -> Value {
    Value::builtin_record(
        ty::EXEC_RESULT,
        [
            ("affected", Value::Int(affected as i64)),
            ("returning", Value::list(returning)),
        ],
    )
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
    /// The pool, or the `PostgresError` of one that is closed.
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
    /// The transaction's connection, or the `PostgresError` of one that has
    /// ended.
    fn open(self) -> Result<Arc<Mutex<TxConn>>, Value> {
        lookup_tx(self.0).ok_or_else(tx_ended)
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

/// The parameters of a statement, or the `PostgresError` of a value that
/// is none.
fn sql_params(params: List) -> Result<Vec<SqlParam>, Value> {
    let params = params.iter().map(|param| value_to_sql_param(&param));
    params.collect::<Result<_, _>>().map_err(other_error)
}

/// The `PostgresError` a function ends with before its operation starts: a
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
    Tx(Arc<Mutex<TxConn>>),
}

/// The executor that `conn` is, the argument of a function that takes
/// a pool or a transaction: the `PostgresError` if it is neither, or is one
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

    let mut cfg: postgres::Config = match stripped_url.parse() {
        Ok(cfg) => cfg,
        Err(e) => {
            return err(pg_connect(format!("postgres: invalid URL: {e}")));
        }
    };
    let limit = match connect_limit(&url) {
        Ok(limit) => limit,
        Err(why) => return err(pg_connect(why)),
    };
    cfg.connect_timeout(limit);

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

    match open_pool(cfg, effective, ext.root_cert.as_deref(), limit, &opts) {
        Ok(pool) => {
            let id = insert_pool(pool);
            ok(make_pool_handle(id))
        }
        Err(v) => err(v),
    }
}

/// How long a connect to `url` may take: its `connect_timeout`, in
/// whole seconds, which must be more than 0, or `CONNECT_TIMEOUT`. It
/// is read as the URL's parser reads it, which has refused `url`
/// already if the value is no whole number, and which ignores one that
/// is not more than 0: the last one stands, in either form of `url`.
fn connect_limit(url: &str) -> Result<Duration, &'static str> {
    let options = connection_options(url);
    let set = options
        .iter()
        .rev()
        .find(|(key, _)| key == "connect_timeout");
    match set.map(|(_, value)| value.parse::<i64>()) {
        None => Ok(CONNECT_TIMEOUT),
        Some(Ok(seconds @ 1..)) => Ok(Duration::from_secs(seconds.unsigned_abs())),
        Some(_) => Err("connect_timeout must be > 0"),
    }
}

/// The options of the connection string `s`, in order, as the
/// postgres crate's parser reads them: the query of a
/// `postgres://` or `postgresql://` URL (percent-decoded), or the
/// `key=value` pairs of the other form (a value may be quoted in `'`,
/// and `\` escapes a character). What that parser refuses is not read
/// here: it has refused the string before.
fn connection_options(s: &str) -> Vec<(String, String)> {
    let url = s
        .strip_prefix("postgresql://")
        .or_else(|| s.strip_prefix("postgres://"));
    if let Some(rest) = url {
        // The credentials end at the first `@`; the options begin at
        // the first `?` after them.
        let rest = rest.split_once('@').map_or(rest, |(_, rest)| rest);
        let Some((_, query)) = rest.split_once('?') else {
            return Vec::new();
        };
        let decode = |part: &str| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8_lossy()
                .into_owned()
        };
        return query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (decode(key), decode(value)))
            .collect();
    }
    let mut options = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=') {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if key.is_empty() || chars.next() != Some('=') {
            return options;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let quoted = chars.next_if_eq(&'\'').is_some();
        let mut value = String::new();
        while let Some(c) = chars.next_if(|c| match quoted {
            true => *c != '\'',
            false => !c.is_whitespace(),
        }) {
            match c {
                '\\' => value.extend(chars.next()),
                c => value.push(c),
            }
        }
        if quoted {
            chars.next();
        }
        options.push((key, value));
    }
}

/// Test-only: [`connect_limit`], for a test without a server.
#[doc(hidden)]
pub fn connect_limit_for_tests(url: &str) -> Result<Duration, String> {
    connect_limit(url).map_err(str::to_string)
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

/// Open a pool for `cfg`, encrypted as `mode` says. Its first
/// connection is made here, so that a connect that fails says why at
/// once; the pool takes it, and opens the others itself, each when it
/// can. The `PostgresError` of a connect that failed.
fn open_pool(
    cfg: postgres::Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    limit: Duration,
    opts: &ConnectOpts,
) -> Result<PgPool, Value> {
    let (tls, first) = first_connection(&cfg, mode, root_cert_path, limit)?;
    let manager = Manager {
        config: cfg,
        tls,
        limit,
        stalled: Stalled::default(),
        first: Mutex::new(Some(first)),
    };
    let mut builder = Pool::builder();
    if let Some(n) = opts.max_pool_size {
        builder = builder.max_size(n);
    }
    Ok(builder.build_unchecked(manager))
}

/// The first connection of a pool, and the encryption of the pool's
/// connections.
#[cfg(feature = "postgres-tls")]
fn first_connection(
    cfg: &postgres::Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    limit: Duration,
) -> Result<(Tls, postgres::Client), Value> {
    // `build_tls_connector` takes what is NOT checked: the
    // certificate, the host name.
    let tls = match mode {
        EffectiveSslMode::Disable => Tls::Off,
        // Opportunistic encryption, no authentication of the server
        // (libpq's `prefer`): TLS, and a plain connection if that
        // fails other than by getting no answer.
        EffectiveSslMode::Prefer => match build_tls_connector(root_cert_path, true, true) {
            Ok(connector) => match connect_first(cfg, Tls::On(connector), limit) {
                Ok(done) => return Ok(done),
                Err(error) if is_pg_timeout(&error) => return Err(error),
                Err(_) => Tls::Off,
            },
            Err(_) => Tls::Off,
        },
        EffectiveSslMode::Require => Tls::On(build_tls_connector(root_cert_path, true, true)?),
        EffectiveSslMode::VerifyCa => Tls::On(build_tls_connector(root_cert_path, false, true)?),
        EffectiveSslMode::VerifyFull => Tls::On(build_tls_connector(root_cert_path, false, false)?),
    };
    connect_first(cfg, tls, limit)
}

/// The first connection of a pool, and the encryption of the pool's
/// connections: none, in a build without TLS, which refuses a mode
/// that asks for it.
#[cfg(not(feature = "postgres-tls"))]
fn first_connection(
    cfg: &postgres::Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    limit: Duration,
) -> Result<(Tls, postgres::Client), Value> {
    let _ = root_cert_path; // unused without TLS
    match mode {
        EffectiveSslMode::Disable | EffectiveSslMode::Prefer => connect_first(cfg, Tls::Off, limit),
        EffectiveSslMode::Require | EffectiveSslMode::VerifyCa | EffectiveSslMode::VerifyFull => {
            Err(pg_tls(
                "TLS required but silt was built without the postgres-tls feature \
                 (URL had no `sslmode=`, which now defaults to verify-full; \
                 use `?sslmode=disable` to connect without TLS)",
            ))
        }
    }
}

/// The pool's first connection, encrypted as `tls` says, or the
/// `PostgresError` of a connect that failed.
fn connect_first(
    cfg: &postgres::Config,
    tls: Tls,
    limit: Duration,
) -> Result<(Tls, postgres::Client), Value> {
    match connect_within(cfg, &tls, limit, &Stalled::default()) {
        Ok(client) => Ok((tls, client)),
        Err(failure) => Err(failure.value()),
    }
}

/// The connect attempts of one pool that got no answer in time and
/// whose threads still wait for one: while there is one, the pool
/// starts no other (R10). So a server that takes connections and never
/// answers costs a pool one thread, not one per attempt of the pool's.
type Stalled = Arc<std::sync::atomic::AtomicUsize>;

/// Connect once, and wait for it `limit` at most. The config bounds
/// the connect of the socket, and nothing bounds what follows it (TLS,
/// authentication), so the connect runs on a thread of its own. One
/// that gets no answer in time goes on there until the server or the
/// system ends it, and what it gets then is dropped; it counts in
/// `stalled` until then, and while one does, this starts no thread and
/// fails at once.
fn connect_within(
    cfg: &postgres::Config,
    tls: &Tls,
    limit: Duration,
    stalled: &Stalled,
) -> Result<postgres::Client, ConnectFailure> {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;
    /// Where an attempt is, for its thread and its waiter.
    #[derive(Default)]
    struct Attempt {
        done: bool,
        /// The waiter went without the answer: the attempt counts in
        /// `stalled` until it is done.
        given_up: bool,
    }
    if stalled.load(Ordering::SeqCst) > 0 {
        return Err(ConnectFailure::NoAnswer);
    }
    let attempt = Arc::new(Mutex::new(Attempt::default()));
    let (sent, answer) = std::sync::mpsc::channel();
    let (config, tls_of_attempt) = (cfg.clone(), tls.clone());
    let (its, its_stalled) = (attempt.clone(), stalled.clone());
    let started = std::thread::Builder::new()
        .name("silt-pg-connect".into())
        .spawn(move || {
            let connect = std::panic::AssertUnwindSafe(|| tls_of_attempt.connect(&config));
            let connected = crate::scheduler::catch_as_error(connect);
            {
                let mut its = its.lock().unwrap_or_else(|e| e.into_inner());
                its.done = true;
                if its.given_up {
                    its_stalled.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _ = sent.send(connected);
        });
    if let Err(e) = started {
        return Err(ConnectFailure::Broke(format!(
            "postgres: cannot start a thread to connect: {e}"
        )));
    }
    let answered = match answer.recv_timeout(limit) {
        Err(RecvTimeoutError::Timeout) => {
            let mut attempt = attempt.lock().unwrap_or_else(|e| e.into_inner());
            if !attempt.done {
                attempt.given_up = true;
                stalled.fetch_add(1, Ordering::SeqCst);
                return Err(ConnectFailure::NoAnswer);
            }
            // The answer came as the time ran out: it is on its way.
            drop(attempt);
            answer.recv().map_err(|_| RecvTimeoutError::Disconnected)
        }
        answered => answered,
    };
    match answered {
        Ok(Ok(Ok(client))) => Ok(client),
        Ok(Ok(Err(e))) => Err(ConnectFailure::Failed(e)),
        Err(RecvTimeoutError::Timeout) => Err(ConnectFailure::NoAnswer),
        Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err(ConnectFailure::Broke(
            "postgres: the connect panicked".to_string(),
        )),
    }
}

/// Why a connect failed.
#[derive(Debug)]
enum ConnectFailure {
    /// The server or the system said why.
    Failed(postgres::Error),
    /// No answer came in time.
    NoAnswer,
    /// The connect could not run, or panicked.
    Broke(String),
}

impl ConnectFailure {
    /// Its `PostgresError`.
    fn value(&self) -> Value {
        match self {
            ConnectFailure::Failed(e) => connect_error_value(e),
            ConnectFailure::NoAnswer => Value::variant(bv::PG_TIMEOUT, vec![]),
            ConnectFailure::Broke(why) => pg_unknown(why.as_str()),
        }
    }
}

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectFailure::Failed(e) => write!(f, "{e}"),
            ConnectFailure::NoAnswer => f.write_str("the server gave no answer in time"),
            ConnectFailure::Broke(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ConnectFailure {}

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

/// Run a query (SELECT-style). Accepts either a pool (fresh conn per
/// call) or a tx-pinned conn. The inner `Mutex<TxConn>` is held for
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
            let mut conn = match tx_conn(&cell) {
                Ok(conn) => conn,
                Err(e) => return err(e),
            };
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
        ExecutorRef::Tx(cell) => match tx_conn(&cell) {
            Ok(mut conn) => run(conn.client_mut(), &sql, &bind, has_returning),
            Err(e) => err(e),
        },
    }
}

// ── Streaming worker (postgres.stream) ─────────────────────────────
//
// Drives a server-side `query_raw` row iterator and ships each row to
// a bounded silt `Channel`. Each row is wrapped as `Ok(row_map)`; on
// a mid-iteration DB error we send a final `Err(pg_error)` before
// closing the channel. The channel's `is_closed` flag is checked
// between rows, and a send that finds the channel closed fails, so a
// silt-side `channel.close` stops the stream; so does the end of the
// program, which drops a worker that waits for room.
//
// A stream stopped early leaves its statement with rows unread. On a
// pool the worker then asks the server to cancel the statement and
// drops the connection instead of giving it back with the rest still
// to come. In a transaction the connection is the transaction's: the
// rest is read and dropped before its next statement, as the client
// does with a result nobody reads (a cancel would abort the
// transaction).

/// Bounded capacity for streaming channels. Enough to hide typical
/// per-row latency; small enough that a 1M-row query costs < 1 MB of
/// in-flight buffer at steady state.
const STREAM_CHANNEL_CAPACITY: usize = 256;

/// Run the streaming query inside the io_pool. Owns the executor for
/// its whole lifetime — for tx executors that means the inner
/// `Mutex<TxConn>` is locked here until the stream finishes, so
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

    // `false` when the stream stopped with rows of the statement unread.
    let pump = |client: &mut postgres::Client, sql: &str, bind: &[&(dyn ToSql + Sync)]| match client
        .query_raw(sql, bind.iter().copied())
    {
        Ok(mut iter) => loop {
            if ch.is_closed() {
                break false;
            }
            match iter.next() {
                Ok(Some(row)) => {
                    let wrapped = Value::variant(bv::OK, vec![row_to_map(&row)]);
                    if !send_or_stop(wrapped) {
                        break false;
                    }
                }
                Ok(None) => break true,
                Err(e) => {
                    let wrapped = Value::variant(bv::ERR, vec![pg_error_to_variant(&e)]);
                    let _ = send_or_stop(wrapped);
                    break true;
                }
            }
        },
        Err(e) => {
            let wrapped = Value::variant(bv::ERR, vec![pg_error_to_variant(&e)]);
            let _ = send_or_stop(wrapped);
            true
        }
    };

    match target {
        ExecutorRef::Pool(pool) => match pool.get() {
            Ok(mut conn) => {
                // Left with rows unread, the connection is not given
                // back: a cancel request was sent for it.
                if !pump(conn.client_mut(), &sql, &bind) {
                    conn.canceller.cancel();
                }
            }
            Err(e) => {
                let wrapped = Value::variant(bv::ERR, vec![pool_error_value(&e)]);
                let _ = send_or_stop(wrapped);
            }
        },
        ExecutorRef::Tx(cell) => match tx_conn(&cell) {
            Ok(mut conn) => {
                let _ = pump(conn.client_mut(), &sql, &bind);
            }
            Err(e) => {
                let _ = send_or_stop(Value::variant(bv::ERR, vec![e]));
            }
        },
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
    cell: Arc<Mutex<TxConn>>,
    sql: String,
    params: Vec<SqlParam>,
    batch_size: u64,
) -> Value {
    let bind: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_to_sql()).collect();
    let (cursor_id, name) = register_cursor(tx_id, batch_size);
    let declare_sql = format!("DECLARE {name} CURSOR FOR {sql}");
    let declared =
        tx_conn(&cell).map(|mut conn| conn.client_mut().execute(declare_sql.as_str(), &bind));
    match declared {
        Err(e) => {
            let _ = remove_cursor(cursor_id);
            err(e)
        }
        Ok(declared) => match declared {
            Ok(_) => ok(make_cursor_handle(cursor_id)),
            Err(e) => {
                // Undo the registry insert so the id is not leaked.
                let _ = remove_cursor(cursor_id);
                err(pg_error_to_variant(&e))
            }
        },
    }
}

/// Fetch the next batch from an open cursor. Empty list = exhausted.
fn do_cursor_next(cursor_id: u64) -> Value {
    let entry = match lookup_cursor(cursor_id) {
        Some(e) => e,
        None if cursor_tx_ended(cursor_id) => return err(tx_ended()),
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
    let mut conn = match tx_conn(&cell) {
        Ok(conn) => conn,
        Err(e) => return err(e),
    };
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
    let Ok(mut conn) = tx_conn(&cell) else {
        // The transaction has ended, and the cursor with it.
        return ok(Value::Unit);
    };
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
    make_notification(n.channel(), n.payload(), n.process_id() as i64)
}

fn make_notification(channel: &str, payload: &str, pid: i64) -> Value {
    Value::builtin_record(
        ty::NOTIFICATION,
        [
            ("channel", Value::String(channel.into())),
            ("payload", Value::String(payload.into())),
            ("pid", Value::Int(pid)),
        ],
    )
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
            let mut conn = match tx_conn(&cell) {
                Ok(conn) => conn,
                Err(e) => return err(e),
            };
            match conn.client_mut().execute(sql, &params) {
                Ok(_) => ok(Value::Unit),
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
    }
}

// ── trait Error for PostgresError ─────────────────────────────────────────

/// What `PostgresError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Connect", [Value::String(m)]) => format!("postgres connect failed: {m}"),
        ("Tls", [Value::String(m)]) => format!("postgres TLS error: {m}"),
        ("AuthFailed", [Value::String(m)]) => {
            format!("postgres authentication failed: {m}")
        }
        ("Query", [Value::String(msg), Value::String(sqlstate)]) => {
            if sqlstate.is_empty() {
                format!("postgres query error: {msg}")
            } else {
                format!("postgres query error [{sqlstate}]: {msg}")
            }
        }
        ("TypeMismatch", [Value::String(col), Value::String(exp), Value::String(act)]) => {
            format!("postgres type mismatch on column `{col}`: expected {exp}, got {act}")
        }
        ("NoSuchColumn", [Value::String(col)]) => {
            format!("postgres: no such column `{col}`")
        }
        ("Closed", []) => "postgres connection closed".to_string(),
        ("Timeout", []) => "postgres operation timed out".to_string(),
        ("TxnAborted", []) => "postgres transaction aborted; rollback required".to_string(),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── The functions ───────────────────────────────────────────────────

/// Test-only re-export of `pg_timeout_err` so integration tests can
/// lock the typed-timeout shape without needing a live DB. Returns the
/// exact `Value` a postgres operation gives when the time of its wait
/// (`SILT_IO_TIMEOUT`) is over.
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

/// The options of `connect_with` as the Rust `ConnectOpts` struct, or
/// what is wrong with them: a key that is no option, or a value that
/// the option cannot have.
fn parse_connect_opts(opts: &Opts) -> Result<ConnectOpts, String> {
    let mut out = ConnectOpts::default();
    for &(key, n) in &opts.0 {
        match key {
            "max_pool_size" => {
                if n <= 0 {
                    return Err(format!(
                        "postgres.connect_with: max_pool_size must be > 0, got {n}"
                    ));
                }
                // r2d2 takes u32. Clamp to avoid a silent wrap.
                out.max_pool_size = Some(u32::try_from(n).unwrap_or(u32::MAX));
            }
            _ => {
                return Err(format!(
                    "postgres.connect_with: unknown option `{key}`; the options are: max_pool_size"
                ));
            }
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
        let opts = parse_connect_opts(&opts).map_err(pg_connect)?;
        let url = url.to_string();
        Ok(vm.io("postgres", pg_timeout_err, move || do_connect_with(url, opts)))
    }

    fn query(vm, conn: &Value, sql: &str, params: List) -> Started {
        let target = resolve_executor(conn)?;
        let params = sql_params(params)?;
        let sql = sql.to_string();
        let tx = TxHandle::take(conn).and_then(tx_canceller);
        Ok(statement(vm, tx, move || do_query(target, sql, params)))
    }

    fn execute(vm, conn: &Value, sql: &str, params: List) -> Started {
        let target = resolve_executor(conn)?;
        let params = sql_params(params)?;
        let sql = sql.to_string();
        let tx = TxHandle::take(conn).and_then(tx_canceller);
        Ok(statement(vm, tx, move || do_execute(target, sql, params)))
    }

    // `postgres.transact(pool, callback)` — pins a single pooled
    // connection for the callback's lifetime.
    //
    // Lifecycle
    // ---------
    // 1. On the I/O pool: check out a conn from the pool, run `BEGIN`,
    //    register the conn in the tx registry, mint a `PgTx` handle. The
    //    builtin goes on as a frame ([`Transact`]) that waits for that
    //    and calls the callback with the handle.
    //
    // 2. Completion, when the callback has returned, on the I/O pool:
    //    - `Ok(_)` → `COMMIT` on the pinned conn.
    //    - `Err(_)` → `ROLLBACK`.
    //    - a non-Result value → `COMMIT`, wrap as `Ok`.
    //
    //    An error of the program inside the callback drops the frame,
    //    which rolls back, as a deadline or a cancel does
    //    ([`abandon_tx`]).
    //
    //    The pinned conn is dropped after commit/rollback, returning it
    //    to the pool. COMMIT errors are surfaced as the function's Err;
    //    the callback's return value is discarded in that edge case.
    //
    // Nothing of it waits on the task's thread, so a deadline or a
    // cancel ends the task in each of the three waits.
    //
    // There is no nested transaction: a `PgTx` is no `PgPool`. `SAVEPOINT`
    // by hand does it.
    fn transact(vm, pool: PoolHandle, f: &Value) -> Result<Step, Failed> {
        let pool = pool.open()?;
        let runtime = vm.runtime.clone();
        let op = vm
            .start_io(pg_timeout_err, move || begin_tx(&pool))
            // The task went before it had its transaction: nobody ends
            // it but this.
            .unheard_with(move |began| {
                if let Value::Variant(began) = began
                    && began.is(bv::OK)
                    && let [handle] = began.fields()
                    && let Some(tx) = TxHandle::take(handle)
                {
                    abandon_tx(&runtime, tx.0);
                }
            });
        Ok(Step::Run(Box::new(Transact {
            wait: Some(io_wait(vm, &op)),
            stage: Stage::Begin(op),
            callback: Some(f.clone()),
        })))
    }

    // `postgres.stream(target, sql, params)` — returns a bounded silt
    // `Channel` of `Result(Map(String, Value), PostgresError)` row wrappers.
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

        // A stream opened in a transaction ends with it.
        if let Some(tx) = TxHandle::take(conn) {
            register_tx_stream(tx.0, &channel);
        }

        // Fire-and-forget: submit to io_pool. The completion's return
        // value is unused (we return the channel, not a completion).
        //
        // The worker counts as a task of the program (`enter`): one
        // that runs while it fetches rows, and one that waits while it
        // waits for room in the channel. So the program is neither
        // ended nor deadlocked while a row may still come, and when
        // the program ends, a worker that waits for room is dropped
        // there, as a task that waits to send is. Until the worker's
        // thread has begun, `starting` stands in for it.
        let sql = sql.to_string();
        let scheduler = vm.scheduler().clone();
        let starting = scheduler.external();
        let op = vm.start_io(pg_timeout_err, move || {
            let _running = scheduler.enter();
            drop(starting);
            do_stream_worker(target, sql, params, worker_channel, scheduler.clone());
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
        Ok(statement(vm, tx_canceller(tx), move || {
            do_cursor_open(tx.0, cell, sql, params, batch_size)
        }))
    }

    fn cursor_next(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        let tx = lookup_cursor(cursor.0).and_then(|entry| tx_canceller(TxHandle(entry.tx_id)));
        statement(vm, tx, move || do_cursor_next(cursor.0))
    }

    fn cursor_close(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        let tx = lookup_cursor(cursor.0).and_then(|entry| tx_canceller(TxHandle(entry.tx_id)));
        statement(vm, tx, move || do_cursor_close(cursor.0))
    }

    fn close(pool: PoolHandle) {
        remove_pool(pool.0);
    }

    // `postgres.listen(pool, channel_name)` — open a notification
    // listener. Returns `Result(Channel, PostgresError)`. On success the channel
    // yields `Notification` records until the underlying conn dies or the
    // caller calls `channel.close`.
    //
    // The LISTEN statement is issued synchronously inside this function (on
    // the io_pool) so LISTEN errors — e.g. an invalid channel name that
    // somehow slipped past the identifier validator, or a pool checkout
    // failure — surface as an immediate `Err(PostgresError)` rather than a
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
        // Blocking work we submit to io_pool: checkout + LISTEN. On success
        // we spawn the long-lived worker and return the silt channel; on
        // failure we return the PostgresError without spawning. We need a handle
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
            // The worker counts as pending for as long as it runs: a
            // notification may come at any time.
            let worker_ch = worker_channel.clone();
            let scheduler = runtime.scheduler.clone();
            let serving = scheduler.external();
            let op = runtime.io_pool.submit(pg_timeout_err, move || {
                do_listen_worker(conn, channel_name, worker_ch, scheduler);
                drop(serving);
                Value::Unit
            });
            op.detach();
            ok(Value::Channel(worker_channel))
        };

        Ok(vm.io("postgres", pg_timeout_err, work))
    }

    // Fires a NOTIFY, on a pool or in a transaction. Uses
    // `SELECT pg_notify($1, $2)` so channel names are bind-parameter
    // safe.
    fn notify(vm, conn: &Value, channel: &str, payload: &str) -> Started {
        let target = resolve_executor(conn)?;
        let (channel, payload) = (channel.to_string(), payload.to_string());
        let tx = TxHandle::take(conn).and_then(tx_canceller);
        Ok(statement(vm, tx, move || do_notify(target, channel, payload)))
    }
}

/// The canceller of the transaction `tx`, if it is open.
fn tx_canceller(tx: TxHandle) -> Option<Canceller> {
    let reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.txs.get(&tx.0).map(|entry| entry.canceller.clone())
}

/// The step of a statement that runs `f` on the I/O pool and waits for
/// its value: one of the transaction of `tx`, if that is given.
///
/// A statement of a transaction that its task stops waiting for while
/// it runs (a deadline or a cancel that the callback set around it, or
/// `SILT_IO_TIMEOUT`) is cancelled in the server, and the transaction
/// counts as aborted from then on: its later statements are
/// `Err(TxnAborted)` at once, and so is its COMMIT. A statement on the
/// pool that nobody waits for finishes in the server.
fn statement(
    vm: &mut Vm,
    tx: Option<Canceller>,
    f: impl FnOnce() -> Value + Send + 'static,
) -> Result<Step, VmError> {
    let op = vm.start_io(pg_timeout_err, f);
    let op = match tx {
        None => op,
        Some(canceller) => {
            let runtime = vm.runtime.clone();
            // Called where the waiter goes, which must not wait.
            op.stop_with(move || {
                canceller.mark();
                let request = runtime.io_pool.submit(pg_timeout_err, move || {
                    canceller.cancel();
                    Value::Unit
                });
                request.detach();
            })
        }
    };
    vm.io_wait("postgres", pg_timeout_err, op)
}

/// The frame of `postgres.transact`.
struct Transact {
    /// The wait the frame parks on next.
    wait: Option<Wait>,
    stage: Stage,
    /// The callback, until it is called.
    callback: Option<Value>,
}

/// Where a transaction is.
enum Stage {
    /// A connection is checked out of the pool, and `BEGIN` runs on it.
    Begin(IoOp),
    /// The callback runs, in the transaction of this id.
    Body(u64),
    /// `COMMIT` or `ROLLBACK` runs.
    End(IoOp),
    Ended,
}

/// The wait of the frame of `transact` for `op`, until
/// `SILT_IO_TIMEOUT` if that is set.
fn io_wait(vm: &Vm, op: &IoOp) -> Wait {
    Wait::new(vec![Arm::Cell(op.cell.clone())]).deadline(vm.scheduler().io_deadline())
}

/// The value of `op`, which the wait that has ended was for: the
/// typed `Timeout` if the wait ended at `SILT_IO_TIMEOUT`.
fn waited_for(vm: &mut Vm, op: &IoOp) -> Result<Value, VmError> {
    let value = match vm.woken()? {
        Fired::Arm(..) => op.take(),
        _ => None,
    };
    Ok(value.unwrap_or_else(|| pg_timeout_err(crate::vm::IoFailure::Timeout(IO_TIMEOUT))))
}

impl Native for Transact {
    fn name(&self) -> &str {
        "postgres.transact"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(wait) = self.wait.take() {
            return Ok(Step::Park(wait));
        }
        match std::mem::replace(&mut self.stage, Stage::Ended) {
            Stage::Begin(op) => {
                let began = waited_for(vm, &op)?;
                // Not taken, a value that came after the wait's end is
                // the `unheard` of `transact`'s.
                drop(op);
                let handle = match &began {
                    Value::Variant(variant) if variant.is(bv::OK) => variant.fields().first(),
                    _ => None,
                };
                let (Some(handle), Some(callee)) = (handle, self.callback.take()) else {
                    return Ok(Step::Done(began));
                };
                let Some(tx) = TxHandle::take(handle) else {
                    return Ok(Step::Done(began));
                };
                self.stage = Stage::Body(tx.0);
                Ok(vm.call(callee, [handle.clone()]))
            }
            Stage::Body(tx_id) => {
                // The callback has returned `input`. The end is not
                // withheld from a task that takes nothing new: the
                // callback's answer stands, though the task may not
                // be there to take the value.
                let entry = take_tx(tx_id, vm.scheduler().wake());
                let op = vm
                    .runtime
                    .io_pool
                    .submit(pg_timeout_err, move || end_tx(entry, input));
                // Not ended at `SILT_IO_TIMEOUT`: an `Err` must never
                // stand for a transaction that was committed. (A
                // deadline or a cancel does end it; the task then
                // learns nothing.)
                let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]);
                self.stage = Stage::End(op);
                Ok(Step::Park(wait))
            }
            Stage::End(op) => Ok(Step::Done(waited_for(vm, &op)?)),
            Stage::Ended => Ok(Step::Done(input)),
        }
    }

    /// The task goes without the transaction's answer: it was
    /// cancelled, or its deadline passed, or the callback failed with
    /// an error of the program. Nothing here waits: the thread that
    /// drops the task may be any.
    fn abandon(&mut self, vm: &mut Vm) {
        match std::mem::replace(&mut self.stage, Stage::Ended) {
            // The operation does not start, or ends the transaction it
            // began itself (`transact`'s `unheard`).
            Stage::Begin(op) => drop(op),
            // A statement of the callback may be running.
            Stage::Body(tx_id) => abandon_tx(&vm.runtime, tx_id),
            // COMMIT or ROLLBACK goes on, and finishes in the server.
            Stage::End(op) => op.detach(),
            Stage::Ended => {}
        }
    }
}

/// On the I/O pool: a connection of `pool`, and `BEGIN` on it. The
/// value is `Ok(PgTx)` of the transaction, or `Err` of what failed.
fn begin_tx(pool: &PgPool) -> Value {
    let mut conn = match pool.get() {
        Ok(conn) => conn,
        Err(e) => return err(pool_error_value(&e)),
    };
    if let Err(e) = conn.client_mut().batch_execute("BEGIN") {
        return err(pg_error_to_variant(&e));
    }
    ok(make_tx_handle(insert_tx(conn)))
}

/// Take the transaction `tx_id` out of the registry, for its end: its
/// streams are closed and its cursors reaped first.
fn take_tx(tx_id: u64, wake: &dyn crate::runtime::sync::Wake) -> Option<TxEntry> {
    close_tx_streams(tx_id, wake);
    // Any cursors opened inside the tx are about to become invalid —
    // PG drops server-side cursors at tx end, so we only need to reap
    // the local registry entries. Do this before running COMMIT /
    // ROLLBACK so a subsequent `cursor_next` with the same id sees a
    // clean "not registered" error rather than racing with the reap.
    drain_cursors_for_tx(tx_id);
    remove_tx(tx_id)
}

/// End the transaction `tx_id` that nobody ends: take it out of the
/// registry, and hand the rest to the I/O pool
/// (`roll_back_abandoned`). Nothing here waits.
fn abandon_tx(runtime: &crate::vm::Runtime, tx_id: u64) {
    let Some(entry) = take_tx(tx_id, runtime.scheduler.wake()) else {
        return;
    };
    let op = runtime.io_pool.submit(pg_timeout_err, move || {
        roll_back_abandoned(entry);
        Value::Unit
    });
    op.detach();
}

/// Roll back a transaction that nobody ends. A statement that runs on
/// it holds its connection: the server is asked to cancel it, and the
/// rollback waits for the connection. The connection goes back to the
/// pool once the rollback is done (when the last statement that had it
/// lets it go), or is dropped if the rollback failed, or if a cancel
/// request was sent for it (`Canceller::sent`).
fn roll_back_abandoned(entry: TxEntry) {
    use std::sync::TryLockError;
    let mut conn = match entry.cell.try_lock() {
        Ok(conn) => conn,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            entry.canceller.cancel();
            entry.cell.lock().unwrap_or_else(|e| e.into_inner())
        }
    };
    if !conn.ended {
        let _ = conn.roll_back();
    }
}

/// On the I/O pool: end the transaction `entry` whose callback has
/// returned `returned`, committing or rolling back as `returned` says.
/// The value is `postgres.transact`'s.
fn end_tx(entry: Option<TxEntry>, returned: Value) -> Value {
    let Some(entry) = entry else {
        // Shouldn't happen — the registry entry was valid on entry
        // and nothing else removes it. If it does, surface the
        // callback's result as-is and skip commit/rollback.
        return returned;
    };
    // A statement of the transaction that is still on its way (from a
    // task the callback started) finds it ended when it gets the
    // connection.
    let mut conn = entry.cell.lock().unwrap_or_else(|e| e.into_inner());
    let mut commit = || conn.commit().err().map(err);
    match &returned {
        Value::Variant(variant) if variant.is(bv::OK) => commit().unwrap_or(returned),
        Value::Variant(variant) if variant.is(bv::ERR) => {
            let _ = conn.roll_back();
            returned
        }
        // Callback didn't return Result(_, _). Treat as Ok and
        // forward the raw value, but COMMIT first so the user's
        // statements persist. Mirrors what most pg wrappers do
        // for callbacks that return a non-Result type.
        _ => commit().unwrap_or_else(|| ok(returned)),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The records the module builds have the fields of their types,
    /// in the types' order (`Value::builtin_record` says so in a debug
    /// build), and are shown under their names.
    #[test]
    fn the_records_of_the_module_have_their_types_fields() {
        let rows = make_query_result(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(rows.to_string(), "QueryResult {row_count: 2, rows: [1, 2]}");
        let done = make_exec_result(3, vec![]);
        assert_eq!(done.to_string(), "ExecResult {affected: 3, returning: []}");
        let told = make_notification("jobs", "42", 7);
        assert_eq!(
            told.to_string(),
            "Notification {channel: jobs, payload: 42, pid: 7}"
        );
    }

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
        // TLS now surface `Tls(msg)` instead of the legacy
        // `ConnectionError`. The message text still mentions TLS /
        // postgres-tls so operators see the feature-flag hint.
        assert_eq!(error.name(), "Tls");
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

    /// A server that takes the connection and never answers: the
    /// connect is `Timeout` once the URL's `connect_timeout` is over,
    /// login included (the socket's connect itself succeeds at once).
    #[test]
    fn a_connect_that_gets_no_answer_is_a_timeout() {
        use std::net::TcpListener;
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (done, over) = mpsc::channel::<()>();
        let silent = std::thread::spawn(move || {
            // Hold the connection, unanswered, until the test is over.
            let held = listener.accept();
            let _ = over.recv();
            drop(held);
        });
        let url = format!("postgres://u:p@127.0.0.1:{port}/db?sslmode=disable&connect_timeout=1");
        let result = do_connect(url);
        let _ = done.send(());
        let _ = silent.join();
        let Value::Variant(outer) = &result else {
            panic!("expected Variant, got {result:?}");
        };
        assert_eq!(outer.name(), "Err", "{result:?}");
        let Some(Value::Variant(error)) = outer.fields().first() else {
            panic!("expected an error variant: {result:?}");
        };
        assert_eq!(error.name(), "Timeout", "{result:?}");
    }

    /// A pool's attempt that got no answer counts as stalled until its
    /// thread is done; while it does, the next attempt starts no thread
    /// and opens no connection (R10).
    #[test]
    fn a_stalled_attempt_holds_back_the_next() {
        use std::net::TcpListener;
        use std::sync::atomic::Ordering;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let cfg: postgres::Config = format!("postgres://u:p@127.0.0.1:{port}/db")
            .parse()
            .expect("config");
        let stalled = Stalled::default();
        let first = connect_within(&cfg, &Tls::Off, Duration::from_millis(200), &stalled);
        assert!(
            matches!(first, Err(ConnectFailure::NoAnswer)),
            "{:?}",
            first.err()
        );
        assert_eq!(stalled.load(Ordering::SeqCst), 1);
        let held = listener.accept().expect("the first attempt's connection");
        let second = connect_within(&cfg, &Tls::Off, Duration::from_millis(200), &stalled);
        assert!(
            matches!(second, Err(ConnectFailure::NoAnswer)),
            "{:?}",
            second.err()
        );
        // No second connection reached the listener.
        listener.set_nonblocking(true).expect("nonblocking");
        assert!(listener.accept().is_err(), "a second attempt connected");
        // The server ends the first attempt's socket: its thread ends,
        // and the pool may try again.
        drop(held);
        while stalled.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The ids of cursors whose transaction ended are kept as runs; a
    /// cursor the program closed itself is none of them.
    #[test]
    fn ended_cursors_are_kept_as_runs() {
        let mut reg = CursorRegistry {
            next_id: 1,
            cursors: BTreeMap::new(),
            ended: BTreeMap::new(),
        };
        for id in [3, 1, 2, 7, 5, 6] {
            reg.end(id);
        }
        assert_eq!(reg.ended, BTreeMap::from([(1, 3), (5, 7)]));
        let ended: Vec<u64> = (0..10).filter(|&id| reg.has_ended(id)).collect();
        assert_eq!(ended, [1, 2, 3, 5, 6, 7]);
        reg.end(4);
        assert_eq!(reg.ended, BTreeMap::from([(1, 7)]));
    }

    /// `connect_timeout=0` is refused before anything is sent.
    #[test]
    fn a_connect_timeout_of_0_is_refused() {
        let result =
            do_connect("postgres://u:p@127.0.0.1:1/db?sslmode=disable&connect_timeout=0".into());
        assert_eq!(
            result.to_string(),
            "Err(postgres connect failed: connect_timeout must be > 0)"
        );
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
}
