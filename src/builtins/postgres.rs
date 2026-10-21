//! Postgres builtin module (`postgres.*`).
//!
//! Provides connection pooling, parameterised query/execute, and
//! transaction support against a Postgres server. Built on
//! `tokio-postgres` and an `r2d2` connection pool: each connection has
//! a runtime of its own on which its protocol task runs, driven by the
//! thread that waits on the connection, the thread of a statement's I/O
//! operation, as the `http` builtin's requests wait on theirs (sync
//! ureq + io_pool bridge).
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
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use r2d2::Pool;
use tokio_postgres::config::SslMode;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Config, NoTls, Notification};

use super::common::ok;
use super::data;
use super::decode::Decoder;
use super::typed::{Arg, Map, Ret, Type, builtins};
use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{Arm, Wait};
use crate::runtime::sync::{Channel, Close, TrySend};
use crate::scheduler::Scheduler;
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::{IoOp, Leave, Limit, Native, Runtime, Step, Vm, VmError, Wanted};

mod rows;

use crate::bytecode::Globals;
use rows::{Fill, Filling, Plan, Row, RowType, Rows, TextParam, Unsendable};

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

/// A runtime of one connection, or of one cancel request: on the
/// current thread, so that the thread that waits on the connection
/// drives it, and nothing runs while nobody waits.
fn new_runtime() -> Result<tokio::runtime::Runtime, ConnectFailure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ConnectFailure::Broke(format!("postgres: cannot start a runtime: {e}")))
}

/// Wait for `future` on `runtime`, `limit` at most: `None` when the
/// time ran out, and `future` was dropped.
fn block_on_within<F: std::future::Future>(
    runtime: &tokio::runtime::Runtime,
    limit: Duration,
    future: F,
) -> Option<F::Output> {
    runtime.block_on(async { tokio::time::timeout(limit, future).await.ok() })
}

/// The runtime of a connection and the connection's protocol task,
/// which runs while somebody waits on the connection.
struct Driver {
    runtime: Option<tokio::runtime::Runtime>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Driver {
    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        let runtime = self
            .runtime
            .as_ref()
            .expect("the runtime lives as long as its driver");
        runtime.block_on(future)
    }
}

impl Drop for Driver {
    /// The client is gone (`Conn` drops it first): the protocol task
    /// ends the session once the answers it waits for have come, and is
    /// driven here until it has.
    fn drop(&mut self) {
        if let (Some(runtime), Some(task)) = (self.runtime.take(), self.task.take()) {
            let _ = runtime.block_on(task);
            runtime.shutdown_background();
        }
    }
}

/// Where the protocol task of a connection hands the notifications it
/// reads: to the listener of the connection, while one is attached
/// (`postgres.listen`). With none, a notification is dropped.
type Told = Arc<Mutex<Option<tokio::sync::mpsc::Sender<Notification>>>>;

/// The protocol task of a connection: while somebody waits on the
/// connection, it sends the client's requests and reads what the server
/// sends, until the connection ends, which it does when its client is
/// dropped or the socket fails. A notification goes to the listener; a
/// full listener holds the connection's reading up, as a slow reader of
/// a result does.
async fn drive<S, T>(mut connection: tokio_postgres::Connection<S, T>, told: Told)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(message) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
        match message {
            Ok(tokio_postgres::AsyncMessage::Notification(notification)) => {
                let listener = told.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(listener) = listener {
                    let _ = listener.send(notification).await;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // A listener that waits learns that no notification comes.
    told.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// A connection that was made: its client, where its notifications
/// go, and its protocol task.
type Connected = (Client, Told, tokio::task::JoinHandle<()>);

/// How a pool's connections are encrypted, once `sslmode` is read.
#[derive(Clone)]
enum Tls {
    Off,
    #[cfg(feature = "postgres-tls")]
    On(postgres_native_tls::MakeTlsConnector),
}

impl Tls {
    /// Connect once, and start the connection's protocol task on the
    /// runtime that runs this.
    async fn connect(&self, config: &Config) -> Result<Connected, tokio_postgres::Error> {
        let told = Told::default();
        let (client, task) = match self {
            Tls::Off => {
                let (client, connection) = config.connect(NoTls).await?;
                (client, tokio::spawn(drive(connection, told.clone())))
            }
            #[cfg(feature = "postgres-tls")]
            Tls::On(connector) => {
                let (client, connection) = config.connect(connector.clone()).await?;
                (client, tokio::spawn(drive(connection, told.clone())))
            }
        };
        Ok((client, told, task))
    }

    /// Send the cancel request of `token`.
    async fn cancel(
        &self,
        token: &tokio_postgres::CancelToken,
    ) -> Result<(), tokio_postgres::Error> {
        match self {
            Tls::Off => token.cancel_query(NoTls).await,
            #[cfg(feature = "postgres-tls")]
            Tls::On(connector) => token.cancel_query(connector.clone()).await,
        }
    }
}

/// What asks the server to cancel the statement that runs on one
/// connection: the request goes over a connection of its own, which
/// is why it can be sent while the statement holds the connection.
#[derive(Clone)]
struct Canceller {
    token: tokio_postgres::CancelToken,
    tls: Tls,
    /// How long sending the request may take, as connecting may.
    limit: Duration,
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
        if let Ok(runtime) = new_runtime() {
            let _ = block_on_within(&runtime, self.limit, self.tls.cancel(&self.token));
            runtime.shutdown_background();
        }
    }
}

/// A connection of a pool.
struct Conn {
    client: Client,
    /// What runs the connection. (Dropped after `client`: see its
    /// `drop`.)
    driver: Driver,
    canceller: Canceller,
    /// Where its notifications go.
    told: Told,
    /// It must not go back to the pool: its transaction did not roll
    /// back. (One for which a cancel request was sent does not go back
    /// either: `Canceller::sent`.)
    broken: bool,
}

impl Conn {
    fn client(&self) -> &Client {
        &self.client
    }

    /// Wait for `future`, an operation of the client, and run the
    /// connection meanwhile.
    fn wait<F: std::future::Future>(&self, future: F) -> F::Output {
        self.driver.block_on(future)
    }
}

struct Manager {
    config: Config,
    tls: Tls,
    /// How long a connect may take, login included.
    limit: Duration,
    /// The connection `connect` made itself, the first one the pool
    /// takes.
    first: Mutex<Option<Conn>>,
}

impl r2d2::ManageConnection for Manager {
    type Connection = Conn;
    type Error = ConnectFailure;

    fn connect(&self) -> Result<Conn, ConnectFailure> {
        let first = self.first.lock().unwrap_or_else(|e| e.into_inner()).take();
        match first {
            Some(conn) => Ok(conn),
            None => connect_within(&self.config, &self.tls, self.limit),
        }
    }

    /// Whether a connection taken from the pool is still open: a round
    /// trip, which finds a session the server has ended, also one whose
    /// end is still on its way to the client.
    fn is_valid(&self, conn: &mut Conn) -> Result<(), ConnectFailure> {
        conn.wait(conn.client.simple_query(""))
            .map(|_| ())
            .map_err(ConnectFailure::Failed)
    }

    fn has_broken(&self, conn: &mut Conn) -> bool {
        conn.is_broken()
    }
}

impl Conn {
    /// It must not go back to the pool.
    fn is_broken(&self) -> bool {
        self.broken || self.canceller.was_sent() || self.client.is_closed()
    }
}

type PgPool = Pool<Manager>;

/// A connection taken from its pool, which it goes back to when it is
/// dropped.
struct PinnedConn {
    conn: Option<r2d2::PooledConnection<Manager>>,
    pool: PgPool,
}

impl PinnedConn {
    /// A connection of `pool`: an idle one, or one it opens, waiting
    /// for one 30 seconds at most.
    fn take(pool: &PgPool) -> Result<PinnedConn, r2d2::Error> {
        Ok(PinnedConn {
            conn: Some(pool.get()?),
            pool: pool.clone(),
        })
    }
}

impl std::ops::Deref for PinnedConn {
    type Target = Conn;

    fn deref(&self) -> &Conn {
        self.conn
            .as_ref()
            .expect("a pinned connection until it is dropped")
    }
}

impl std::ops::DerefMut for PinnedConn {
    fn deref_mut(&mut self) -> &mut Conn {
        self.conn
            .as_mut()
            .expect("a pinned connection until it is dropped")
    }
}

impl Drop for PinnedConn {
    /// A broken connection is closed, not given back
    /// (`Manager::has_broken`). The pool keeps no idle connection in
    /// reserve, so it would not open one in its place, and a statement
    /// that waits for a connection would wait its 30 seconds: where the
    /// pool has no idle connection, one is opened for it, as for a
    /// checkout that finds none. (Where it has one, nobody waits.)
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        let broken = conn.is_broken();
        drop(conn);
        if broken && self.pool.state().idle_connections == 0 {
            drop(self.pool.get_timeout(Duration::ZERO));
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
    cell: Arc<TxCell>,
    canceller: Canceller,
}

/// The connection of a transaction, and who holds it. A statement takes
/// it through `tx_conn`, which waits while another statement or a
/// stream of the transaction holds it, unless that is a stream that
/// waits for room in its channel: the room may never come (its reader
/// may be the task that waits for the statement).
struct TxCell {
    conn: Mutex<TxConn>,
    holder: Mutex<Holder>,
    /// `holder` changed.
    changed: Condvar,
}

/// Who holds the connection of a transaction, as `TxCell` keeps it.
#[derive(Default)]
struct Holder {
    /// A statement or a stream holds it (`TxGuard`).
    held: bool,
    /// The stream that holds it waits for room in this channel, its
    /// own.
    stream_waits: Option<Arc<Channel>>,
}

impl TxCell {
    fn holder(&self) -> std::sync::MutexGuard<'_, Holder> {
        self.holder.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The stream that holds the connection waits for room in its
    /// channel `waits`, or (`None`) no longer does.
    fn stream_waits(&self, waits: Option<Arc<Channel>>) {
        self.holder().stream_waits = waits;
        self.changed.notify_all();
    }
}

/// The connection of a transaction, held: given back when it is
/// dropped.
struct TxGuard<'a> {
    guard: std::sync::MutexGuard<'a, TxConn>,
    cell: &'a TxCell,
}

impl std::ops::Deref for TxGuard<'_> {
    type Target = TxConn;

    fn deref(&self) -> &TxConn {
        &self.guard
    }
}

impl std::ops::DerefMut for TxGuard<'_> {
    fn deref_mut(&mut self) -> &mut TxConn {
        &mut self.guard
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        let mut holder = self.cell.holder();
        holder.held = false;
        holder.stream_waits = None;
        self.cell.changed.notify_all();
    }
}

/// What `postgres.query` and the other statements of a transaction are
/// while its stream holds the connection and waits for room in its
/// channel.
const STREAM_STILL_OPEN: &str =
    "postgres: a stream of this transaction is still open; read or close it first";

/// The connection of a transaction, as its statements share it.
struct TxConn {
    conn: PinnedConn,
    /// The transaction has ended, or is ending: no statement runs on
    /// the connection any more.
    ended: bool,
}

impl TxConn {
    fn conn(&self) -> &Conn {
        &self.conn
    }

    /// End the transaction with `ROLLBACK`. A connection whose
    /// transaction did not end so is not given back to the pool.
    fn roll_back(&mut self) -> Result<(), tokio_postgres::Error> {
        self.ended = true;
        let ended = self.conn.wait(self.conn.client().batch_execute("ROLLBACK"));
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
        let committed = self
            .conn
            .wait(self.conn.client().batch_execute("SELECT 1; COMMIT"));
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

/// The connection of the transaction of `cell`, for a statement or a
/// stream: it waits while another one holds it. The `PostgresError` of
/// one whose transaction has ended or is aborted, and of one that comes
/// while a stream of the transaction holds the connection and waits for
/// room in its channel (`STREAM_STILL_OPEN`). A statement (whose waiter
/// is `wanted`; a stream has none) that is not wanted any more before
/// it gets the connection gives up (with a value nobody reads): it
/// claims the connection when it gets it ([`Wanted::claim`]), and a
/// statement that never had it is not cancelled in the server and does
/// not abort the transaction.
fn tx_conn<'a>(cell: &'a TxCell, wanted: Option<&Wanted>) -> Result<TxGuard<'a>, Value> {
    let gave_up = not_wanted;
    let mut holder = cell.holder();
    loop {
        if wanted.is_some_and(Wanted::gone) {
            return Err(gave_up());
        }
        if !holder.held {
            if wanted.is_some_and(|wanted| !wanted.claim()) {
                return Err(gave_up());
            }
            holder.held = true;
            break;
        }
        // (A stream whose channel was closed is about to stop.)
        if holder
            .stream_waits
            .as_ref()
            .is_some_and(|ch| !ch.is_closed())
        {
            return Err(other_error(STREAM_STILL_OPEN));
        }
        holder = cell.changed.wait(holder).unwrap_or_else(|e| e.into_inner());
    }
    drop(holder);
    let conn = TxGuard {
        guard: cell.conn.lock().unwrap_or_else(|e| e.into_inner()),
        cell,
    };
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

/// The value of a statement that gave up before it had its connection,
/// which nobody reads: its task stopped waiting for it.
fn not_wanted() -> Value {
    other_error("postgres: the statement's task stopped waiting for it")
}

/// A connection of `pool`, for a statement (or a `BEGIN`, a `LISTEN`)
/// whose waiter is `wanted`. One whose task stops waiting for it before
/// it has the connection gives up, with a value nobody reads: it is
/// never sent, and the connection goes back to the pool. It claims the
/// connection when it has it ([`Wanted::claim`]); from then on it is
/// left to finish.
fn pool_conn(pool: &PgPool, wanted: &Wanted) -> Result<PinnedConn, Value> {
    if wanted.gone() {
        return Err(not_wanted());
    }
    let conn = PinnedConn::take(pool).map_err(|e| pool_error_value(&e))?;
    match wanted.claim() {
        true => Ok(conn),
        false => Err(not_wanted()),
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
    let cell = Arc::new(TxCell {
        conn: Mutex::new(TxConn { conn, ended: false }),
        holder: Mutex::default(),
        changed: Condvar::new(),
    });
    let mut reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = reg.next_id;
    reg.next_id = reg.next_id.wrapping_add(1);
    reg.txs.insert(id, TxEntry { cell, canceller });
    id
}

fn lookup_tx(id: u64) -> Option<Arc<TxCell>> {
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
    /// The error of the batch that failed, which ended the cursor: every
    /// later `cursor_next` is that error again.
    failed: Option<Value>,
    /// How its rows are decoded.
    plan: Arc<Plan>,
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

fn register_cursor(tx_id: u64, batch_size: u64, plan: Arc<Plan>) -> (u64, String) {
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
            failed: None,
            plan,
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

/// The batch of the cursor `id` failed with `error`: the cursor has
/// ended.
fn fail_cursor(id: u64, error: &Value) {
    let mut reg = cursors().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = reg.cursors.get_mut(&id) {
        entry.failed = Some(error.clone());
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
/// `tokio_postgres::Error::as_db_error()` directly on the original error.
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

/// Classify a `tokio_postgres::Error` into the matching `PostgresError` variant.
/// For DbError we inspect SQLSTATE; for transport errors we sniff the
/// Display text for "closed" / "broken pipe" / "timed out" keywords.
fn pg_error_to_variant(e: &tokio_postgres::Error) -> Value {
    if let Some(db) = e.as_db_error() {
        let message = redact_pg_message(db.message());
        let sqlstate = db.code().code().to_string();
        match sqlstate.as_str() {
            // 25P02: in_failed_sql_transaction.
            "25P02" => Value::variant(bv::PG_TXN_ABORTED, vec![]),
            // 57014: query_canceled (statement_timeout fires with this).
            "57014" => Value::variant(bv::PG_TIMEOUT, vec![]),
            // 42703: undefined_column.
            "42703" => rows::no_such_column(column_named(&message)),
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
        if lc.contains("timed out") || lc.contains("timeout") {
            Value::variant(bv::PG_TIMEOUT, vec![])
        } else if lc.contains("closed")
            || lc.contains("broken pipe")
            || lc.contains("eof")
            || lc.contains("reset by peer")
        {
            Value::variant(bv::PG_CLOSED, vec![])
        } else {
            // The error's text names its kind only ("error serializing
            // parameter 0"): its cause says what happened.
            let causes = std::iter::successors(std::error::Error::source(e), |c| c.source());
            let text = causes.fold(raw, |text, cause| format!("{text}: {cause}"));
            Value::variant(
                bv::PG_OTHER,
                vec![Value::String(redact_pg_message(&text).into())],
            )
        }
    }
}

/// The column that the message of an `undefined_column` error names:
/// `column "nope" does not exist`, `column t.nope does not exist`,
/// `column "nope" of relation "t" does not exist`. The whole message
/// if it names none.
fn column_named(message: &str) -> &str {
    let named = message
        .strip_prefix("column ")
        .and_then(|rest| match rest.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"').map(|(name, _)| name),
            None => rest.split_once(' ').map(|(name, _)| name),
        });
    named.unwrap_or(message)
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
/// for the handshake, or a server without TLS where the mode requires
/// it, `Timeout` for a socket that got no answer in
/// time, and `Connect` for every other reason: a refused port, an
/// unknown host, a database that does not exist, a URL that lacks
/// something.
fn connect_error_value(e: &tokio_postgres::Error) -> Value {
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
    // (The text of a failed TLS handshake, of a server without TLS
    // among others.)
    let mut tls = text == "error performing TLS handshake";
    let mut timed_out = false;
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
    fn open(self) -> Result<Arc<TxCell>, Value> {
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

/// The parameters of a statement of `caller`, and where the encoder
/// leaves why one could not be sent.
type Params = (Vec<TextParam>, Option<Unsendable>);

/// Write the parameters of a statement of `caller`, `params`, and go on
/// with `then`, which gets them; the error of the program if `params`
/// is no tuple or a parameter has no form. A part of a type with a
/// written `json.Encode` impl is written as the impl says, which is the
/// program's code: a step. What `then` starts is the builtin's value.
fn with_params(
    vm: &mut Vm,
    caller: &'static str,
    params: &Value,
    then: impl FnOnce(&mut Vm, Params) -> Started + Send + 'static,
) -> Result<Step, VmError> {
    let values = rows::param_values(caller, params)?;
    vm.encode_each(
        caller,
        data::Format::Postgres,
        values,
        move |vm, written| {
            let params = rows::params(caller, written?)?;
            then(vm, params).ret()
        },
    )
}

/// Open the stream of the rows of `sql` with `params` on `conn`, decoded
/// as `decoding` says: the channel they come on.
fn open_stream(
    vm: &mut Vm,
    conn: &Value,
    sql: String,
    params: Params,
    decoding: Decoding,
) -> Started {
    let target = resolve_executor(conn)?;

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
    let scheduler = vm.scheduler().clone();
    let starting = scheduler.external();
    // (A task that takes nothing new starts nothing: it is ended where
    // it waits next.)
    if !vm.takes_nothing_new() {
        // Rows of a type that holds parts the program's impls decode
        // are finished by a task each (`Helper`).
        let helper = rows::reaches_impl(&decoding.1, &decoding.2, &decoding.0).then(|| Helper {
            vm: vm.spawn_child(),
            owner: scheduler.current_owner(),
        });
        let decode = (decoding, helper);
        vm.runtime.io_pool.background(pg_timeout_err, move || {
            let _running = scheduler.enter();
            drop(starting);
            do_stream_worker(
                target,
                sql,
                params,
                decode,
                worker_channel,
                scheduler.clone(),
            );
            Value::Unit
        });
    }

    Ok(Ok(Step::Done(ok(Value::Channel(channel)))))
}

/// The postgres functions that decode rows.
static QUERY: Decoder = Decoder::new("postgres.query");
static STREAM: Decoder = Decoder::new("postgres.stream");
static CURSOR: Decoder = Decoder::new("postgres.cursor");

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
    Tx(Arc<TxCell>),
}

/// The executor that `conn` is, the argument of a function that takes
/// a pool or a transaction: the `PostgresError` if it is neither (which
/// the checker refuses wherever a program gives one: a guard), or is one
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
    // Read `sslmode` and `sslrootcert` out of the connection string, in
    // either form: `Config` only recognises `disable` / `prefer` /
    // `require` and rejects `verify-ca` / `verify-full` and
    // `sslrootcert` with a parse error. Strip them before handing the
    // string to `Config::from_str`.
    let (stripped_url, ext) = match without_ssl_options(&url) {
        Ok(x) => x,
        Err(e) => {
            return err(pg_connect(format!("postgres: invalid URL: {e}")));
        }
    };

    let mut cfg: Config = match stripped_url.parse() {
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
    //      `Config` doesn't parse natively.
    //   2. If absent, `VerifyFull` (the new safe default).
    //
    // Note: we do NOT fall back to `cfg.get_ssl_mode()`. The
    // `Config` parser defaults its internal SslMode to
    // `Prefer` when nothing is specified, which is exactly the
    // unverified-TLS behaviour this fix is intended to prevent.
    let effective = match (only_sockets(&cfg), ext.mode) {
        // A Unix socket carries no TLS, whatever `sslmode` says (as in
        // libpq): a string whose every host is one connects without.
        (true, _) => EffectiveSslMode::Disable,
        (false, Some(m)) => m,
        (false, None) => EffectiveSslMode::VerifyFull,
    };
    // A mode that requires TLS refuses a server without it: the
    // crate's own default, `prefer`, would go on without TLS.
    cfg.ssl_mode(match effective {
        EffectiveSslMode::Disable => SslMode::Disable,
        EffectiveSslMode::Prefer => SslMode::Prefer,
        EffectiveSslMode::Require | EffectiveSslMode::VerifyCa | EffectiveSslMode::VerifyFull => {
            SslMode::Require
        }
    });

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
        .find(|option| option.key == "connect_timeout");
    match set.map(|option| option.value.parse::<i64>()) {
        None => Ok(CONNECT_TIMEOUT),
        Some(Ok(seconds @ 1..)) => Ok(Duration::from_secs(seconds.unsigned_abs())),
        Some(_) => Err("connect_timeout must be > 0"),
    }
}

/// An option of a connection string: its key and its value, decoded,
/// and where the option is in the string.
struct ConnOption {
    key: String,
    value: String,
    span: std::ops::Range<usize>,
}

/// The query of the connection string `s` if it is a `postgres://` or
/// `postgresql://` URL, and where the query begins in `s` (an empty
/// query at the end of a URL that has none); `None` for the `key=value`
/// form. The credentials end at the first `@`; the query begins after
/// the first `?` after them.
fn url_query(s: &str) -> Option<(usize, &str)> {
    let rest = s
        .strip_prefix("postgresql://")
        .or_else(|| s.strip_prefix("postgres://"))?;
    let after_credentials = rest.find('@').map_or(0, |at| at + 1);
    let base = s.len() - rest.len() + after_credentials;
    Some(match s[base..].find('?') {
        Some(at) => (base + at + 1, &s[base + at + 1..]),
        None => (s.len(), ""),
    })
}

/// The options of the connection string `s`, in order, as the
/// `tokio_postgres` parser reads them: the query of a
/// `postgres://` or `postgresql://` URL (percent-decoded), or the
/// `key=value` pairs of the other form (a value may be quoted in `'`,
/// and `\` escapes a character). What that parser refuses is not read
/// here: it refuses the string.
fn connection_options(s: &str) -> Vec<ConnOption> {
    if let Some((start, query)) = url_query(s) {
        let decode = |part: &str| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8_lossy()
                .into_owned()
        };
        let mut options = Vec::new();
        let mut at = start;
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                options.push(ConnOption {
                    key: decode(key),
                    value: decode(value),
                    span: at..at + pair.len(),
                });
            }
            at += pair.len() + 1;
        }
        return options;
    }
    let mut options = Vec::new();
    let mut chars = s.char_indices().peekable();
    let at = |chars: &mut std::iter::Peekable<std::str::CharIndices>| {
        chars.peek().map_or(s.len(), |&(i, _)| i)
    };
    loop {
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let start = at(&mut chars);
        let mut key = String::new();
        while let Some((_, c)) = chars.next_if(|(_, c)| !c.is_whitespace() && *c != '=') {
            key.push(c);
        }
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        if key.is_empty() || chars.next().map(|(_, c)| c) != Some('=') {
            return options;
        }
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let quoted = chars.next_if(|(_, c)| *c == '\'').is_some();
        let mut value = String::new();
        while let Some((_, c)) = chars.next_if(|(_, c)| match quoted {
            true => *c != '\'',
            false => !c.is_whitespace(),
        }) {
            match c {
                '\\' => value.extend(chars.next().map(|(_, c)| c)),
                c => value.push(c),
            }
        }
        if quoted {
            chars.next();
        }
        let span = start..at(&mut chars);
        options.push(ConnOption { key, value, span });
    }
}

/// Whether every host of `cfg` is a Unix socket (a path), reached as
/// one: no `hostaddr` stands in for it.
#[cfg(unix)]
fn only_sockets(cfg: &Config) -> bool {
    use tokio_postgres::config::Host;
    let hosts = cfg.get_hosts();
    let sockets = hosts.iter().all(|host| matches!(host, Host::Unix(_)));
    !hosts.is_empty() && sockets && cfg.get_hostaddrs().is_empty()
}

/// Whether every host of `cfg` is a Unix socket: never, where there are
/// none.
#[cfg(not(unix))]
fn only_sockets(_cfg: &Config) -> bool {
    false
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
    let (_, ext) = without_ssl_options(url)?;
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

/// The connection string `s` without its TLS options, which the crate
/// does not read as silt does (it refuses `verify-ca`, `verify-full`
/// and `sslrootcert`), and what they say: in either form of `s`, the
/// last one of each standing. The rest of `s` is left as it is.
fn without_ssl_options(s: &str) -> Result<(String, SslUrlParams), String> {
    let mut params = SslUrlParams::default();
    let mut cut = Vec::new();
    for option in connection_options(s) {
        match option.key.as_str() {
            "sslmode" => {
                params.mode = Some(match option.value.as_str() {
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
            "sslrootcert" => params.root_cert = Some(option.value),
            // `sslcert` / `sslkey` (client certs) are v2 — silently
            // ignore for now so URLs aren't rejected outright.
            "sslcert" | "sslkey" => {}
            _ => continue,
        }
        cut.push(option.span);
    }
    if cut.is_empty() {
        return Ok((s.to_string(), params));
    }
    let mut kept = String::with_capacity(s.len());
    let mut from = 0;
    for span in cut {
        kept.push_str(&s[from..span.start]);
        from = span.end;
    }
    kept.push_str(&s[from..]);
    // A URL's query keeps its other options, joined as before.
    if let Some((start, query)) = url_query(&kept) {
        let pairs: Vec<&str> = query.split('&').filter(|pair| !pair.is_empty()).collect();
        let prefix = kept[..start].trim_end_matches('?');
        kept = match pairs.is_empty() {
            true => prefix.to_string(),
            false => format!("{prefix}?{}", pairs.join("&")),
        };
    }
    Ok((kept, params))
}

/// Open a pool for `cfg`, encrypted as `mode` says. Its first
/// connection is made here, so that a connect that fails says why at
/// once; the pool takes it, and opens the others itself, each when it
/// needs one. The `PostgresError` of a connect that failed.
fn open_pool(
    cfg: Config,
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
        first: Mutex::new(Some(first)),
    };
    Ok(pool_of(manager, opts))
}

/// The pool of `manager`. It keeps no idle connections in reserve
/// (r2d2 would keep `max_size` of them, and open them all at once): a
/// statement that finds no idle connection opens one. A connection
/// idle for 10 minutes, or open for 30, is closed while it is idle
/// (r2d2's defaults), and not opened again until one is needed.
fn pool_of(manager: Manager, opts: &ConnectOpts) -> PgPool {
    let mut builder = Pool::builder().min_idle(Some(0));
    if let Some(n) = opts.max_pool_size {
        builder = builder.max_size(n);
    }
    builder.build_unchecked(manager)
}

/// The first connection of a pool, and the encryption of the pool's
/// connections.
#[cfg(feature = "postgres-tls")]
fn first_connection(
    cfg: &Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    limit: Duration,
) -> Result<(Tls, Conn), Value> {
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
    cfg: &Config,
    mode: EffectiveSslMode,
    root_cert_path: Option<&str>,
    limit: Duration,
) -> Result<(Tls, Conn), Value> {
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
fn connect_first(cfg: &Config, tls: Tls, limit: Duration) -> Result<(Tls, Conn), Value> {
    match connect_within(cfg, &tls, limit) {
        Ok(conn) => Ok((tls, conn)),
        Err(failure) => Err(failure.value()),
    }
}

/// Connect once: the socket, TLS and the login, in `limit` at most, on
/// a runtime of the connection's own. An attempt that gets no answer in
/// time is dropped, its socket and its runtime with it: nothing of it
/// stays behind.
fn connect_within(cfg: &Config, tls: &Tls, limit: Duration) -> Result<Conn, ConnectFailure> {
    let runtime = new_runtime()?;
    let attempt =
        std::panic::AssertUnwindSafe(|| block_on_within(&runtime, limit, tls.connect(cfg)));
    let (client, told, task) = match crate::scheduler::catch_as_error(attempt) {
        Ok(Some(Ok(connected))) => connected,
        failed => {
            runtime.shutdown_background();
            return Err(match failed {
                Ok(Some(Err(e))) => ConnectFailure::Failed(e),
                Ok(None) => ConnectFailure::NoAnswer,
                _ => ConnectFailure::Broke("postgres: the connect panicked".to_string()),
            });
        }
    };
    Ok(Conn {
        canceller: Canceller {
            token: client.cancel_token(),
            tls: tls.clone(),
            limit,
            sent: Arc::default(),
        },
        client,
        driver: Driver {
            runtime: Some(runtime),
            task: Some(task),
        },
        told,
        broken: false,
    })
}

/// Why a connect failed.
#[derive(Debug)]
enum ConnectFailure {
    /// The server or the system said why.
    Failed(tokio_postgres::Error),
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

/// The parameters of a statement as `tokio_postgres` takes them.
fn binding(params: &[TextParam]) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|param| param as &(dyn ToSql + Sync))
        .collect()
}

/// `f` on the connection of `target`: one of the pool, or the
/// transaction's. A transaction's `Mutex<TxConn>` is held while `f`
/// runs, never across a VM yield. The value is `Ok` of `f`'s, or `Err`
/// of the `PostgresError`.
fn with_conn(
    target: ExecutorRef,
    wanted: &Wanted,
    f: impl FnOnce(&Conn) -> Result<Value, Value>,
) -> Value {
    let done = match target {
        ExecutorRef::Pool(pool) => match pool_conn(&pool, wanted) {
            Ok(conn) => f(&conn),
            Err(e) => Err(e),
        },
        ExecutorRef::Tx(cell) => match tx_conn(&cell, Some(wanted)) {
            Ok(conn) => f(conn.conn()),
            Err(e) => Err(e),
        },
    };
    done.map_or_else(err, ok)
}

/// How the rows of a statement are decoded: into `row`, by the runtime
/// and the table of the program's globals (its `json.Decode` impls).
type Decoding = (RowType, Arc<Runtime>, Arc<Globals>);

/// The statement `sql` prepared, and the plan of its result's rows: the
/// columns are checked against the row type of `decoding` before the
/// statement runs.
fn prepare(
    conn: &Conn,
    (row, rt, impls): &Decoding,
    sql: &str,
) -> Result<(tokio_postgres::Statement, Plan), Value> {
    let statement = conn
        .wait(conn.client().prepare(sql))
        .map_err(|e| pg_error_to_variant(&e))?;
    let plan = Plan::new(rt, impls.clone(), row, statement.columns())?;
    Ok((statement, plan))
}

/// Where a statement's thread leaves the rows of its result that wait
/// for the program's `json.Decode` impls, for the task to finish
/// ([`Filler`]).
#[derive(Clone, Default)]
struct ToFill(Arc<Mutex<Option<(Filling, Option<u64>)>>>);

impl ToFill {
    /// The value of the statement whose rows `rows` `plan` decoded: the
    /// list of them, or, if some wait for impls, `()` with the rows
    /// left here (`cursor`, the cursor whose batch they are).
    fn give(&self, rt: &Arc<Runtime>, plan: &Plan, rows: Rows, cursor: Option<u64>) -> Value {
        match rows {
            Rows::Done(rows) => Value::list(rows),
            Rows::ToFill(rows, error) => {
                let filling = Filling::new(rt.clone(), plan.impls().clone(), rows, error);
                *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((filling, cursor));
                Value::Unit
            }
        }
    }

    /// What the task makes of the statement's value `value`: that value,
    /// or a frame that finishes the rows left here.
    fn then(&self, value: Value) -> Result<Step, VmError> {
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some((filling, cursor)) => Ok(Step::Run(Box::new(Filler {
                filling,
                cursor,
                one_row: false,
                calling: false,
            }))),
            None => Ok(Step::Done(value)),
        }
    }
}

/// The frame that finishes rows that wait for the program's
/// `json.Decode` impls: it calls each, then gives `Ok` of the rows or
/// `Err` of the first error. A cursor's batch that fails so ends the
/// cursor, as one the decoder refuses does.
struct Filler {
    filling: Filling,
    cursor: Option<u64>,
    /// Its value is the one row it finishes, not a list (a stream's).
    one_row: bool,
    /// Whether a call is under way.
    calling: bool,
}

impl Native for Filler {
    fn name(&self) -> &str {
        "postgres"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        let given = match std::mem::replace(&mut self.calling, false) {
            true => Some(vm.decode_returned(&input)?),
            false => None,
        };
        match self.filling.next(given) {
            Fill::Call(slot, json) => {
                self.calling = true;
                vm.call_written_decode(slot, json)
            }
            Fill::Done(Ok(rows)) if self.one_row => Ok(Step::Done(ok(rows
                .into_iter()
                .next()
                .unwrap_or(Value::Unit)))),
            Fill::Done(Ok(rows)) => Ok(Step::Done(ok(Value::list(rows)))),
            Fill::Done(Err(error)) => {
                if let Some(id) = self.cursor {
                    fail_cursor(id, &error);
                }
                Ok(Step::Done(err(error)))
            }
        }
    }
}

/// Run a statement and decode its rows as `decoding` says: on a pool (a
/// connection of it for the statement) or in a transaction. Rows that
/// wait for the program's impls are left in `to_fill`.
fn do_query(
    target: ExecutorRef,
    sql: String,
    params: Vec<TextParam>,
    decoding: Decoding,
    to_fill: ToFill,
    wanted: &Wanted,
) -> Value {
    let bind = binding(&params);
    with_conn(target, wanted, |conn| {
        let (statement, plan) = prepare(conn, &decoding, &sql)?;
        let rows = conn.wait(conn.client().query(&statement, &bind));
        let rows = rows.map_err(|e| pg_error_to_variant(&e))?;
        let rows = plan.rows(&decoding.1, rows.iter())?;
        Ok(to_fill.give(&decoding.1, &plan, rows, None))
    })
}

/// Run a statement and give the number of rows it affected.
fn do_execute(target: ExecutorRef, sql: String, params: Vec<TextParam>, wanted: &Wanted) -> Value {
    let bind = binding(&params);
    with_conn(target, wanted, |conn| {
        match conn.wait(conn.client().execute(sql.as_str(), &bind)) {
            Ok(n) => Ok(Value::Int(i64::try_from(n).unwrap_or(i64::MAX))),
            Err(e) => Err(pg_error_to_variant(&e)),
        }
    })
}

// ── Streaming worker (postgres.stream) ─────────────────────────────
//
// Drives a server-side `query_raw` row stream and ships each row to
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

/// What finishes the rows of a stream that wait for the program's
/// `json.Decode` impls: a task for each row, which the worker waits for
/// (the impls are silt code, which no thread of the I/O pool runs).
struct Helper {
    /// The VM the tasks are made of.
    vm: Vm,
    /// The owner of the stream's task, whose tasks they count with.
    owner: u64,
}

impl Helper {
    /// The row that `partial`, of a result `plan` decodes, is once the
    /// impls have given their parts, or its error; or (`Err`) the
    /// failure of an impl, `None` if the program ended first.
    fn fill(
        &mut self,
        scheduler: &Scheduler,
        rt: &Arc<Runtime>,
        plan: &Plan,
        partial: rows::Partial,
    ) -> Result<Result<Value, Value>, Option<VmError>> {
        let id = self.vm.next_task_id();
        let handle = Arc::new(TaskHandle::with_owner(id, self.owner));
        let mut child = self.vm.spawn_child();
        child.spawned = true;
        let rows = vec![Row::Partial(partial)];
        child.push_native_frame(Box::new(Filler {
            filling: Filling::new(rt.clone(), plan.impls().clone(), rows, None),
            cursor: None,
            one_row: true,
            calling: false,
        }));
        scheduler
            .submit(id, child, handle.clone())
            .map_err(|e| Some(VmError::new(e)))?;
        let wait = Wait::new(vec![Arm::Cell(handle.done())]);
        let _ = scheduler.block_thread(wait, crate::scheduler::Blocks::Thread);
        match handle.try_get() {
            Some(Ok(Value::Variant(result))) if result.is(bv::OK) => {
                Ok(Ok(result.fields().first().cloned().unwrap_or(Value::Unit)))
            }
            Some(Ok(Value::Variant(result))) if result.is(bv::ERR) => {
                Ok(Err(result.fields().first().cloned().unwrap_or(Value::Unit)))
            }
            Some(Ok(_)) => Err(Some(VmError::type_confusion(
                "a row was finished as no Result",
            ))),
            Some(Err(failure)) => {
                handle.mark_joined();
                Err(Some(failure))
            }
            None => Err(None),
        }
    }
}

/// Bounded capacity for streaming channels. Enough to hide typical
/// per-row latency; small enough that a 1M-row query costs < 1 MB of
/// in-flight buffer at steady state.
const STREAM_CHANNEL_CAPACITY: usize = 256;

/// Run the streaming query inside the io_pool. Owns the executor for
/// its whole lifetime — for tx executors that means the transaction's
/// connection is held here until the stream finishes, so concurrent tx
/// queries wait until we're done, or fail while the stream waits for
/// room in its channel (`STREAM_STILL_OPEN`). Documented contract.
fn do_stream_worker(
    target: ExecutorRef,
    sql: String,
    (params, unsendable): (Vec<TextParam>, Option<Unsendable>),
    (decoding, mut helper): (Decoding, Option<Helper>),
    ch: Arc<Channel>,
    scheduler: Arc<Scheduler>,
) {
    let rt = decoding.1.clone();
    // If the consumer already cancelled before we even started, bail
    // without touching the DB.
    if ch.is_closed() {
        return;
    }
    let bind = binding(&params);

    // Helper: send a single wrapped row or error; stop if the channel
    // was closed by the consumer. Returns `true` to keep going. A
    // stream of a transaction that waits for room says so meanwhile:
    // the transaction's statements fail rather than wait for it.
    let waits = match &target {
        ExecutorRef::Tx(cell) => Some(cell.clone()),
        ExecutorRef::Pool(_) => None,
    };
    let send_or_stop = |v: Value| -> bool {
        let Some(cell) = &waits else {
            return scheduler.send_wait(&ch, v);
        };
        let v = match ch.try_send(v, scheduler.wake()) {
            TrySend::Sent => return true,
            TrySend::Closed(_) => return false,
            TrySend::Full(v) => v,
        };
        cell.stream_waits(Some(ch.clone()));
        let sent = scheduler.send_wait(&ch, v);
        cell.stream_waits(None);
        sent
    };

    // `false` when the stream stopped with rows of the statement unread.
    let mut pump = |conn: &Conn, sql: &str, bind: &[&(dyn ToSql + Sync)]| {
        let (statement, plan) = match prepare(conn, &decoding, sql) {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = send_or_stop(Value::variant(bv::ERR, vec![e]));
                return true;
            }
        };
        match conn.wait(conn.client().query_raw(&statement, bind.iter().copied())) {
            Ok(rows) => {
                let mut rows = std::pin::pin!(rows);
                loop {
                    if ch.is_closed() {
                        break false;
                    }
                    match conn.wait(rows.next()) {
                        Some(Ok(found)) => match plan.row(&rt, &found).map(|row| match row {
                            Row::Done(value) => Ok(Ok(value)),
                            Row::Partial(partial) => match &mut helper {
                                Some(helper) => helper.fill(&scheduler, &rt, &plan, partial),
                                None => Err(None),
                            },
                        }) {
                            Ok(Ok(Ok(value))) => {
                                if !send_or_stop(Value::variant(bv::OK, vec![value])) {
                                    break false;
                                }
                            }
                            // A row that cannot be decoded ends the stream.
                            Err(e) | Ok(Ok(Err(e))) => {
                                let _ = send_or_stop(Value::variant(bv::ERR, vec![e]));
                                break false;
                            }
                            // An impl failed: the stream ends with its
                            // failure, as a stream stage does. (Or the
                            // program ended meanwhile.)
                            Ok(Err(failure)) => {
                                let failure = failure.map(Arc::new);
                                ch.close(Close { failure }, scheduler.wake());
                                break false;
                            }
                        },
                        None => break true,
                        Some(Err(e)) => {
                            let wrapped = Value::variant(bv::ERR, vec![pg_error_to_variant(&e)]);
                            let _ = send_or_stop(wrapped);
                            break true;
                        }
                    }
                }
            }
            Err(e) => {
                // A parameter the encoder could not send: no statement
                // was sent, and the stream (which no task waits for)
                // says why.
                let why = unsendable.as_ref().and_then(Unsendable::take);
                let error = why.map_or_else(|| pg_error_to_variant(&e), other_error);
                let _ = send_or_stop(Value::variant(bv::ERR, vec![error]));
                true
            }
        }
    };

    match target {
        ExecutorRef::Pool(pool) => match PinnedConn::take(&pool) {
            Ok(conn) => {
                // Left with rows unread, the connection is not given
                // back: a cancel request was sent for it.
                if !pump(&conn, &sql, &bind) {
                    conn.canceller.cancel();
                }
            }
            Err(e) => {
                let wrapped = Value::variant(bv::ERR, vec![pool_error_value(&e)]);
                let _ = send_or_stop(wrapped);
            }
        },
        ExecutorRef::Tx(cell) => match tx_conn(&cell, None) {
            Ok(conn) => {
                let _ = pump(conn.conn(), &sql, &bind);
            }
            // (Without the connection: nothing for a statement to fail
            // on while the error waits for room.)
            Err(e) => {
                let _ = scheduler.send_wait(&ch, Value::variant(bv::ERR, vec![e]));
            }
        },
    }

    // End of stream: always close, even if we already sent an Err
    // (close after Err signals "no more items" so the consumer's
    // channel.receive returns Closed on the next pull).
    ch.close(Close::default(), scheduler.wake());
}

// ── Cursor workers (postgres.cursor / .cursor_next / .cursor_close) ─

/// Open a cursor: the query `sql` is prepared, and its columns checked
/// against `row`; then `DECLARE <name> CURSOR FOR <sql>` runs with
/// bound params on the tx's pinned connection. Registry entry is
/// inserted before DECLARE so a concurrent `drain_cursors_for_tx` sees
/// us; if DECLARE fails we remove it again.
fn do_cursor_open(
    tx_id: u64,
    cell: Arc<TxCell>,
    sql: String,
    params: Vec<TextParam>,
    batch_size: u64,
    decoding: Decoding,
    wanted: &Wanted,
) -> Value {
    let bind = binding(&params);
    let planned =
        tx_conn(&cell, Some(wanted)).and_then(|conn| prepare(conn.conn(), &decoding, &sql));
    let plan = match planned {
        Ok((_, plan)) => Arc::new(plan),
        Err(e) => return err(e),
    };
    let (cursor_id, name) = register_cursor(tx_id, batch_size, plan);
    let declare_sql = format!("DECLARE {name} CURSOR FOR {sql}");
    let declared = tx_conn(&cell, Some(wanted)).map(|conn| {
        let conn = conn.conn();
        conn.wait(conn.client().execute(declare_sql.as_str(), &bind))
    });
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
/// Rows that wait for the program's impls are left in `to_fill`.
fn do_cursor_next(cursor_id: u64, rt: Arc<Runtime>, to_fill: ToFill, wanted: &Wanted) -> Value {
    let entry = match lookup_cursor(cursor_id) {
        Some(e) => e,
        None if cursor_tx_ended(cursor_id) => return err(tx_ended()),
        None => {
            return err(other_error(format!(
                "postgres: cursor {cursor_id} is not registered (closed or tx ended)"
            )));
        }
    };
    if let Some(error) = entry.failed {
        return err(error);
    }
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
    let conn = match tx_conn(&cell, Some(wanted)) {
        Ok(conn) => conn,
        Err(e) => return err(e),
    };
    let fetched = conn
        .conn()
        .wait(conn.conn().client().query(fetch_sql.as_str(), &[]));
    match fetched {
        Ok(rows) => {
            drop(conn);
            if (rows.len() as u64) < entry.batch_size {
                update_cursor_exhausted(cursor_id, true);
            }
            match entry.plan.rows(&rt, rows.iter()) {
                Ok(rows) => ok(to_fill.give(&rt, &entry.plan, rows, Some(cursor_id))),
                // A batch that fails ends the cursor, as a stream ends
                // after its `Err`: the rows of the batch are read, and
                // a later batch would go on as though none were lost.
                Err(e) => {
                    fail_cursor(cursor_id, &e);
                    err(e)
                }
            }
        }
        Err(e) => {
            let e = pg_error_to_variant(&e);
            fail_cursor(cursor_id, &e);
            err(e)
        }
    }
}

/// Explicitly close a cursor. Runs `CLOSE <name>` if the tx is still
/// alive; otherwise no-ops (the tx end implicitly closed it).
fn do_cursor_close(cursor_id: u64, wanted: &Wanted) -> Value {
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
    let Ok(conn) = tx_conn(&cell, Some(wanted)) else {
        // The transaction has ended, and the cursor with it.
        return ok(Value::Unit);
    };
    match conn
        .conn()
        .wait(conn.conn().client().execute(close_sql.as_str(), &[]))
    {
        Ok(_) => ok(Value::Unit),
        Err(e) => err(pg_error_to_variant(&e)),
    }
}

// ── LISTEN / NOTIFY (pub-sub) ───────────────────────────────────────
//
// `listen(pool, channel_name)` pins a single pool connection for the
// listener's lifetime, attaches a listener to it, runs `LISTEN
// "<channel_name>"` (quoted, so that it keeps its case), and returns a
// bounded silt `Channel` of `Notification` records. The connection's
// protocol task hands each notification to the listener as it comes;
// the worker waits for one a tick (200 ms) at a time, so that
// `channel.close(ch)` is observed within one tick.
//
// `notify(executor, channel_name, payload)` runs `SELECT pg_notify($1, $2)`
// against either a pool or tx executor: the channel name is a bind
// parameter, which `pg_notify` takes as it is written.

/// Bounded capacity for listener channels. Notifications are tiny
/// (three short strings) so 64 is a generous buffer without costing
/// meaningful memory; the slow consumer just pauses the worker via
/// backpressure, and if the consumer goes away entirely
/// the worker's send ends on close.
const LISTEN_CHANNEL_CAPACITY: usize = 64;

/// How long the listener worker waits for a notification before it
/// looks whether its channel was closed. Upper bound on the delay
/// between `channel.close(listen_ch)` and the worker dropping the
/// pinned connection back into the pool.
const LISTEN_POLL_TICK_MS: u64 = 200;

/// The most bytes of a channel name PostgreSQL keeps: it cuts a longer
/// name in `LISTEN`, and refuses it in a notification.
const CHANNEL_NAME_MAX: usize = 63;

/// Why `caller` (`postgres.listen`, `postgres.notify`) refuses the
/// channel name `name`, if it does: listen and notify take the same
/// names, so that every name listened on can be notified.
fn refused_channel_name(caller: &str, name: &str) -> Option<Value> {
    if name.is_empty() {
        return Some(other_error(format!(
            "{caller}: a channel name cannot be empty"
        )));
    }
    if name.contains('\0') {
        return Some(other_error(format!(
            "{caller}: a channel name cannot hold a NUL character"
        )));
    }
    (name.len() > CHANNEL_NAME_MAX).then(|| {
        other_error(format!(
            "{caller}: channel name {name:?} is longer than {CHANNEL_NAME_MAX} bytes, \
             the longest PostgreSQL keeps"
        ))
    })
}

/// The channel name `name` as the identifier of `LISTEN` / `UNLISTEN`:
/// in double quotes, a quote in it doubled, so that the server keeps
/// its case, as `pg_notify` does, and takes a keyword.
fn quoted_channel(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Build a silt `Notification` record from a notification of the server.
fn notification_to_record(n: &Notification) -> Value {
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
/// function starts (see `listen`'s io-pool closure), with `told`
/// attached to the connection; we just pump notifications until the
/// channel closes or the connection ends.
fn do_listen_worker(
    conn: PinnedConn,
    channel_name: String,
    mut told: tokio::sync::mpsc::Receiver<Notification>,
    ch: Arc<Channel>,
    scheduler: Arc<Scheduler>,
) {
    let tick = Duration::from_millis(LISTEN_POLL_TICK_MS);
    while !ch.is_closed() {
        match conn.wait(async { tokio::time::timeout(tick, told.recv()).await.ok() }) {
            Some(Some(n)) => {
                let rec = notification_to_record(&n);
                if !scheduler.send_wait(&ch, rec) {
                    break;
                }
            }
            // The connection has ended. We don't surface that on the
            // channel because the silt-side contract is a stream of
            // Notification records, not a Result wrapper.
            Some(None) => break,
            // No notification in this tick: look at the channel again.
            None => {}
        }
    }

    // Best-effort UNLISTEN so the server stops queuing notifications for
    // this backend. Errors here are ignored — the conn is about to go
    // back into the pool, and r2d2 will recycle a broken conn on next
    // checkout anyway. The notifications that come before its answer
    // are dropped meanwhile, so that none holds the connection up.
    let unlisten_sql = format!("UNLISTEN {}", quoted_channel(&channel_name));
    let unlisten = conn.client().batch_execute(&unlisten_sql);
    let mut unlisten = std::pin::pin!(unlisten);
    let _ = conn.wait(std::future::poll_fn(|cx| {
        while let std::task::Poll::Ready(Some(_)) = told.poll_recv(cx) {}
        unlisten.as_mut().poll(cx)
    }));
    conn.told.lock().unwrap_or_else(|e| e.into_inner()).take();
    ch.close(Close::default(), scheduler.wake());
    // `conn` drops here → returns to the pool.
}

/// Blocking worker for `notify`. Uses `SELECT pg_notify($1, $2)` so the
/// channel name and payload are both bind parameters — no escaping
/// needed and dynamic channel names are supported.
fn do_notify(
    target: ExecutorRef,
    (channel_name, payload): (String, String),
    wanted: &Wanted,
) -> Value {
    let sql = "SELECT pg_notify($1, $2)";
    let params: [&(dyn ToSql + Sync); 2] = [&channel_name, &payload];
    match target {
        ExecutorRef::Pool(pool) => {
            let conn = match pool_conn(&pool, wanted) {
                Ok(c) => c,
                Err(e) => return err(e),
            };
            match conn.wait(conn.client().execute(sql, &params)) {
                Ok(_) => ok(Value::Unit),
                Err(e) => err(pg_error_to_variant(&e)),
            }
        }
        ExecutorRef::Tx(cell) => {
            let conn = match tx_conn(&cell, Some(wanted)) {
                Ok(conn) => conn,
                Err(e) => return err(e),
            };
            match conn.conn().wait(conn.conn().client().execute(sql, &params)) {
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
            let text =
                format!("postgres type mismatch on column `{col}`: expected {exp}, got {act}");
            match cast_mends(act) {
                true => format!("{text}; cast it in SQL, for example {col}::text"),
                false => text,
            }
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

/// Whether the message of a `TypeMismatch` whose actual part is
/// `actual` says to cast the column in SQL: not for a NULL, which a
/// cast leaves NULL, nor for what a `json` or `jsonb` column holds (the
/// decoder builds those with the column's type at their head, or as the
/// whole actual part when the plan refuses the column), as casting a
/// document to text is rarely what mends it.
fn cast_mends(actual: &str) -> bool {
    let head = actual.split(':').next().unwrap_or(actual);
    !(actual == "NULL" || matches!(head.trim_end_matches("[]"), "json" | "jsonb"))
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

/// What becomes of a connect that nobody waits for any more: it runs to
/// its end, and the pool it made, which nobody has, is closed (on the
/// pool's thread: its connection says goodbye to the server).
fn pool_given_back(vm: &Vm) -> Leave {
    let runtime = vm.runtime.clone();
    Leave::gives_back(move |made| {
        if let Value::Variant(made) = made
            && made.is(bv::OK)
            && let [handle] = made.fields()
            && let Some(pool) = PoolHandle::take(handle)
        {
            let pool = registry()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pools
                .remove(&pool.0);
            runtime.io_pool.background(pg_timeout_err, move || {
                drop(pool);
                Value::Unit
            });
        }
    })
}

builtins! {
    fn connect(vm, url: &str) -> Result<Step, VmError> {
        let url = url.to_string();
        vm.io("postgres", pg_timeout_err, pool_given_back(vm), move |_| do_connect(url))
    }

    fn connect_with(vm, url: &str, opts: Opts) -> Started {
        let opts = parse_connect_opts(&opts).map_err(pg_connect)?;
        let url = url.to_string();
        Ok(vm.io("postgres", pg_timeout_err, pool_given_back(vm), move |_| {
            do_connect_with(url, opts)
        }))
    }

    fn query(vm, conn: &Value, sql: &str, params: &Value, ty: Type) -> Result<Step, VmError> {
        let impls = vm.decoding_table();
        let row = RowType::of(&QUERY, ty, &impls)?;
        let (conn, sql) = (conn.clone(), sql.to_string());
        with_params(vm, QUERY.name, params, move |vm, (params, unsendable)| {
            let target = resolve_executor(&conn)?;
            let tx = TxHandle::take(&conn).and_then(tx_of);
            let decoding = (row, vm.runtime.clone(), impls);
            let to_fill = ToFill::default();
            let filled = to_fill.clone();
            let run = move |wanted: &Wanted| do_query(target, sql, params, decoding, to_fill, wanted);
            Ok(statement_then(vm, tx, unsendable, run, move |value| filled.then(value)))
        })
    }

    fn execute(vm, conn: &Value, sql: &str, params: &Value) -> Result<Step, VmError> {
        let (conn, sql) = (conn.clone(), sql.to_string());
        with_params(vm, "postgres.execute", params, move |vm, (params, unsendable)| {
            let target = resolve_executor(&conn)?;
            let tx = TxHandle::take(&conn).and_then(tx_of);
            Ok(statement(vm, tx, unsendable, move |wanted| {
                do_execute(target, sql, params, wanted)
            }))
        })
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
        // The task went before it had its transaction: nobody ends it
        // but this.
        let leave = Leave::gives_back(move |began| {
            if let Value::Variant(began) = began
                && began.is(bv::OK)
                && let [handle] = began.fields()
                && let Some(tx) = TxHandle::take(handle)
            {
                abandon_tx(&runtime, tx.0);
            }
        });
        let op = vm.io_op(pg_timeout_err, leave, move |wanted| begin_tx(&pool, wanted));
        Ok(Step::Run(Box::new(Transact {
            wait: Some(op.wait(vm, Limit::Io)),
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
    fn stream(vm, conn: &Value, sql: &str, params: &Value, ty: Type) -> Result<Step, VmError> {
        let impls = vm.decoding_table();
        let row = RowType::of(&STREAM, ty, &impls)?;
        let (conn, sql) = (conn.clone(), sql.to_string());
        with_params(vm, STREAM.name, params, move |vm, params| {
            let decoding = (row, vm.runtime.clone(), impls);
            open_stream(vm, &conn, sql, params, decoding)
        })
    }

    // (A cursor lives inside a transaction: `tx` is a `PgTx`.)
    fn cursor(vm, tx: TxHandle, sql: &str, params: &Value, batch_size: i64, ty: Type) -> Result<Step, VmError> {
        let impls = vm.decoding_table();
        let row = RowType::of(&CURSOR, ty, &impls)?;
        let sql = sql.to_string();
        with_params(vm, CURSOR.name, params, move |vm, (params, unsendable)| {
            let cell = tx.open()?;
            let Ok(batch_size @ 1..) = u64::try_from(batch_size) else {
                return Err(other_error("postgres.cursor: batch_size must be >= 1".to_string()).into());
            };
            let decode = (row, vm.runtime.clone(), impls);
            Ok(statement(vm, tx_of(tx), unsendable, move |wanted| {
                do_cursor_open(tx.0, cell, sql, params, batch_size, decode, wanted)
            }))
        })
    }

    fn cursor_next(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        let tx = lookup_cursor(cursor.0).and_then(|entry| tx_of(TxHandle(entry.tx_id)));
        let rt = vm.runtime.clone();
        let to_fill = ToFill::default();
        let filled = to_fill.clone();
        let run = move |wanted: &Wanted| do_cursor_next(cursor.0, rt, to_fill, wanted);
        statement_then(vm, tx, None, run, move |value| filled.then(value))
    }

    fn cursor_close(vm, cursor: CursorHandle) -> Result<Step, VmError> {
        let tx = lookup_cursor(cursor.0).and_then(|entry| tx_of(TxHandle(entry.tx_id)));
        statement(vm, tx, None, move |wanted| do_cursor_close(cursor.0, wanted))
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
    // the io_pool) so LISTEN errors — e.g. a pool checkout
    // failure — surface as an immediate `Err(PostgresError)` rather than a
    // channel that silently closes. The long-lived worker is spawned only
    // after LISTEN succeeds.
    fn listen(vm, pool: PoolHandle, channel: &str) -> Started {
        let pool = pool.open()?;
        if let Some(refused) = refused_channel_name("postgres.listen", channel) {
            return Err(refused.into());
        }

        // Mint the silt-side channel up front so we can hand the caller an
        // owned Arc synchronously.
        let ch_id = vm.next_channel_id();
        let worker_channel = Channel::new(ch_id, LISTEN_CHANNEL_CAPACITY);
        let nobodys = worker_channel.clone();

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
        let work = move |wanted: &Wanted| -> Value {
            let conn = match pool_conn(&pool, wanted) {
                Ok(c) => c,
                Err(e) => return err(e),
            };
            // The notifications of the connection go to the worker from
            // now on.
            let (listener, told) = tokio::sync::mpsc::channel(LISTEN_CHANNEL_CAPACITY);
            *conn.told.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
            let listen_sql = format!("LISTEN {}", quoted_channel(&channel_name));
            if let Err(e) = conn.wait(conn.client().batch_execute(&listen_sql)) {
                conn.told.lock().unwrap_or_else(|e| e.into_inner()).take();
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
            runtime.io_pool.background(pg_timeout_err, move || {
                do_listen_worker(conn, channel_name, told, worker_ch, scheduler);
                drop(serving);
                Value::Unit
            });
            ok(Value::Channel(worker_channel))
        };

        // A listener that nobody waits for any more, whose LISTEN was
        // done all the same, is nobody's: its channel is closed, so that
        // its worker ends, and gives the connection back.
        let scheduler = vm.scheduler().clone();
        let leave = Leave::gives_back(move |_| {
            let _ = nobodys.close(Close::default(), scheduler.wake());
        });
        Ok(vm.io("postgres", pg_timeout_err, leave, work))
    }

    // Fires a NOTIFY, on a pool or in a transaction. Uses
    // `SELECT pg_notify($1, $2)` so channel names are bind-parameter
    // safe.
    fn notify(vm, conn: &Value, channel: &str, payload: &str) -> Started {
        let target = resolve_executor(conn)?;
        if let Some(refused) = refused_channel_name("postgres.notify", channel) {
            return Err(refused.into());
        }
        let (channel, payload) = (channel.to_string(), payload.to_string());
        let tx = TxHandle::take(conn).and_then(tx_of);
        Ok(statement(vm, tx, None, move |wanted| do_notify(target, (channel, payload), wanted)))
    }
}

/// The transaction of a statement, as the task that waits for the
/// statement needs it when it stops waiting.
struct StatementTx {
    canceller: Canceller,
    cell: Arc<TxCell>,
}

/// The transaction `tx`, if it is open.
fn tx_of(tx: TxHandle) -> Option<StatementTx> {
    let reg = tx_registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.txs.get(&tx.0).map(|entry| StatementTx {
        canceller: entry.canceller.clone(),
        cell: entry.cell.clone(),
    })
}

/// The step of a statement that runs `f` on the I/O pool and waits for
/// its value: one of the transaction of `tx`, if that is given.
///
/// A statement of a transaction that its task stops waiting for while
/// it runs (a deadline or a cancel that the callback set around it, or
/// `SILT_IO_TIMEOUT`) is cancelled in the server, and the transaction
/// counts as aborted from then on: its later statements are
/// `Err(TxnAborted)` at once, and so is its COMMIT. One that its task
/// stops waiting for before it has the connection (another statement
/// of the transaction, or its stream, holds it) gives up: it is not
/// sent, and the transaction goes on. A statement on the pool that
/// nobody waits for finishes in the server.
///
/// A parameter the encoder could not send (`unsendable`) is the error of
/// the program, not the statement's value; the statement was not sent.
fn statement(
    vm: &mut Vm,
    tx: Option<StatementTx>,
    unsendable: Option<Unsendable>,
    f: impl FnOnce(&Wanted) -> Value + Send + 'static,
) -> Result<Step, VmError> {
    statement_then(vm, tx, unsendable, f, |value| Ok(Step::Done(value)))
}

/// [`statement`] that goes on as `then` says with the statement's value
/// (a frame that finishes rows that wait for the program's impls).
fn statement_then(
    vm: &mut Vm,
    tx: Option<StatementTx>,
    unsendable: Option<Unsendable>,
    f: impl FnOnce(&Wanted) -> Value + Send + 'static,
    then: impl FnOnce(Value) -> Result<Step, VmError> + Send + 'static,
) -> Result<Step, VmError> {
    let leave = match tx {
        None => Leave::RunsOut,
        Some(StatementTx { canceller, cell }) => {
            let runtime = vm.runtime.clone();
            // Called where the waiter goes, which must not wait.
            Leave::interrupts(move |wanted| {
                // One that never had the connection gives up there.
                if !wanted.claimed() {
                    let _holder = cell.holder();
                    cell.changed.notify_all();
                    return;
                }
                canceller.mark();
                runtime.io_pool.background(pg_timeout_err, move || {
                    canceller.cancel();
                    Value::Unit
                });
            })
        }
    };
    let op = vm.io_op(pg_timeout_err, leave, f);
    vm.io_wait_then("postgres", op, move |value| {
        match unsendable.and_then(|unsendable| unsendable.take()) {
            Some(why) => Err(VmError::new(why)),
            None => then(value),
        }
    })
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
                let began = op.value(&vm.woken()?);
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
                let op = vm.runtime.io_pool.submit(
                    pg_timeout_err,
                    Vec::new(),
                    Leave::RunsOut,
                    move |_| end_tx(entry, input),
                );
                // Not ended at `SILT_IO_TIMEOUT`: an `Err` must never
                // stand for a transaction that was committed. (A
                // deadline or a cancel does end it; the task then
                // learns nothing.)
                let wait = op.wait(vm, Limit::None);
                self.stage = Stage::End(op);
                Ok(Step::Park(wait))
            }
            Stage::End(op) => Ok(Step::Done(op.value(&vm.woken()?))),
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
fn begin_tx(pool: &PgPool, wanted: &Wanted) -> Value {
    let conn = match pool_conn(pool, wanted) {
        Ok(conn) => conn,
        Err(e) => return err(e),
    };
    if let Err(e) = conn.wait(conn.client().batch_execute("BEGIN")) {
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
    runtime.io_pool.background(pg_timeout_err, move || {
        roll_back_abandoned(entry);
        Value::Unit
    });
}

/// Roll back a transaction that nobody ends. A statement that runs on
/// it holds its connection: the server is asked to cancel it, and the
/// rollback waits for the connection. The connection goes back to the
/// pool once the rollback is done (when the last statement that had it
/// lets it go), or is dropped if the rollback failed, or if a cancel
/// request was sent for it (`Canceller::sent`).
fn roll_back_abandoned(entry: TxEntry) {
    use std::sync::TryLockError;
    let mut conn = match entry.cell.conn.try_lock() {
        Ok(conn) => conn,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            entry.canceller.cancel();
            entry.cell.conn.lock().unwrap_or_else(|e| e.into_inner())
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
    let mut conn = entry.cell.conn.lock().unwrap_or_else(|e| e.into_inner());
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

    /// The record the module builds has the fields of its type, in the
    /// type's order (`Value::builtin_record` says so in a debug build),
    /// and is shown under its name.
    #[test]
    fn the_record_of_the_module_has_its_type_s_fields() {
        let told = make_notification("jobs", "42", 7);
        assert_eq!(
            told.to_string(),
            "Notification {channel: jobs, payload: 42, pid: 7}"
        );
    }

    /// The column an `undefined_column` error names, as the server
    /// words it.
    /// The cast hint goes with every mismatch but a NULL and what a
    /// json or jsonb column holds.
    #[test]
    fn the_cast_hint_is_for_all_but_null_and_documents() {
        for mended in [
            "float8",
            "int4[] with 2 dimensions",
            "NaN",
            "-Infinity",
            "MyType",
            "24:00:00",
        ] {
            assert!(cast_mends(mended), "{mended}");
        }
        for not in [
            "NULL",
            "jsonb",
            "json[]",
            "jsonb: json type mismatch: expected Int, got string",
            "jsonb[]: a value the server sent malformed",
        ] {
            assert!(!cast_mends(not), "{not}");
        }
    }

    #[test]
    fn the_column_of_an_undefined_column_error() {
        assert_eq!(column_named(r#"column "nope" does not exist"#), "nope");
        assert_eq!(column_named("column t.nope does not exist"), "t.nope");
        assert_eq!(
            column_named(r#"column "x" of relation "t" does not exist"#),
            "x"
        );
        assert_eq!(column_named("something else"), "something else");
    }

    #[test]
    fn strip_ssl_params_preserves_rest() {
        let url = "postgres://u:p@h/db?connect_timeout=5&sslmode=verify-full&sslrootcert=/tmp/ca.pem&application_name=x";
        let (stripped, params) = without_ssl_options(url).expect("parse ok");
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
        let (stripped, params) = without_ssl_options(url).expect("parse ok");
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
            let (_, p) = without_ssl_options(&url).expect("parse ok");
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

    /// A server that takes connections and never answers: what each
    /// connection it took had sent it, and whether its client closed it,
    /// as it finds them. `closed` comes back when every connection it
    /// took is closed, and at the latest at `give_up`.
    struct Silent {
        port: u16,
        /// The most connections that were open at once, and how many
        /// were taken in all, once every one is closed.
        closed: std::sync::mpsc::Receiver<(usize, usize)>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Silent {
        fn start() -> Silent {
            use std::io::Read;
            use std::net::TcpListener;
            use std::sync::atomic::Ordering;
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let port = listener.local_addr().expect("addr").port();
            let (done, closed) = std::sync::mpsc::channel();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopped = stop.clone();
            std::thread::spawn(move || {
                let (mut open, mut most, mut taken) = (Vec::new(), 0, 0);
                while !stopped.load(Ordering::SeqCst) || !open.is_empty() {
                    while let Ok((stream, _)) = listener.accept() {
                        stream.set_nonblocking(true).expect("nonblocking");
                        open.push(stream);
                        taken += 1;
                    }
                    most = most.max(open.len());
                    // A connection is open while a read finds nothing.
                    open.retain_mut(|stream| {
                        let mut buf = [0; 512];
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => return false,
                                Ok(_) => continue,
                                Err(e) => return e.kind() == std::io::ErrorKind::WouldBlock,
                            }
                        }
                    });
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = done.send((most, taken));
            });
            Silent { port, closed, stop }
        }

        /// No more connections will come: wait until the ones taken are
        /// closed, `give_up` at most.
        fn closed(self, give_up: Duration) -> Option<(usize, usize)> {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            self.closed.recv_timeout(give_up).ok()
        }
    }

    /// A server that takes the connection and never answers: the
    /// connect is `Timeout` once the URL's `connect_timeout` is over,
    /// login included (the socket's connect itself succeeds at once),
    /// and the attempt has ended then: its connection is closed.
    #[test]
    fn a_connect_that_gets_no_answer_is_a_timeout() {
        let silent = Silent::start();
        let url = format!(
            "postgres://u:p@127.0.0.1:{}/db?sslmode=disable&connect_timeout=1",
            silent.port
        );
        let result = do_connect(url);
        let Value::Variant(outer) = &result else {
            panic!("expected Variant, got {result:?}");
        };
        assert_eq!(outer.name(), "Err", "{result:?}");
        let Some(Value::Variant(error)) = outer.fields().first() else {
            panic!("expected an error variant: {result:?}");
        };
        assert_eq!(error.name(), "Timeout", "{result:?}");
        // (A minute is no time the test waits: the connection is closed
        // when the connect returns.)
        let closed = silent.closed(Duration::from_secs(60));
        assert_eq!(closed, Some((1, 1)), "the attempt's connection stayed open");
    }

    /// A pool of 2 whose connections meet a server that never answers:
    /// it never has more than 2 attempts open at once, each ends at its
    /// limit, and once the pool is gone none is left open.
    #[test]
    fn a_pool_that_meets_a_silent_server_stays_bounded() {
        let silent = Silent::start();
        let config: Config = format!("postgres://u:p@127.0.0.1:{}/db", silent.port)
            .parse()
            .expect("config");
        let manager = Manager {
            config,
            tls: Tls::Off,
            limit: Duration::from_millis(300),
            first: Mutex::new(None),
        };
        let pool = Pool::builder()
            .max_size(2)
            .connection_timeout(Duration::from_secs(2))
            .build_unchecked(manager);
        let pool = Arc::new(pool);
        let getters: Vec<_> = (0..4)
            .map(|_| {
                let pool = pool.clone();
                std::thread::spawn(move || pool.get().is_err())
            })
            .collect();
        for getter in getters {
            assert!(getter.join().expect("join"), "a connection was made");
        }
        drop(pool);
        let closed = silent.closed(Duration::from_secs(60));
        let Some((most, taken)) = closed else {
            panic!("an attempt's connection stayed open");
        };
        assert!(most <= 2, "{most} attempts were open at once");
        assert!(taken >= 2, "the pool tried {taken} times");
    }

    /// A server that logs every client in and answers each simple query
    /// with an empty result: how many connections it took, and how many
    /// queries came, counted as they come.
    struct Counting {
        port: u16,
        taken: Arc<std::sync::atomic::AtomicUsize>,
        queries: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Counting {
        fn start() -> Counting {
            use std::io::{Read, Write};
            use std::sync::atomic::Ordering;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (took, asked) = (taken.clone(), queries.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    took.fetch_add(1, Ordering::SeqCst);
                    let asked = asked.clone();
                    std::thread::spawn(move || {
                        let mut length = [0; 4];
                        // The startup message: its length, then the rest.
                        if stream.read_exact(&mut length).is_err() {
                            return;
                        }
                        let mut startup = vec![0; u32::from_be_bytes(length) as usize - 4];
                        if stream.read_exact(&mut startup).is_err() {
                            return;
                        }
                        // AuthenticationOk, BackendKeyData, ReadyForQuery.
                        let ready: &[u8] = b"Z\0\0\0\x05I";
                        let mut hello =
                            b"R\0\0\0\x08\0\0\0\0K\0\0\0\x0c\0\0\0\x01\0\0\0\x02".to_vec();
                        hello.extend_from_slice(ready);
                        if stream.write_all(&hello).is_err() {
                            return;
                        }
                        loop {
                            let mut tag = [0; 1];
                            if stream.read_exact(&mut tag).is_err()
                                || stream.read_exact(&mut length).is_err()
                            {
                                return;
                            }
                            let mut body = vec![0; u32::from_be_bytes(length) as usize - 4];
                            if stream.read_exact(&mut body).is_err() || tag[0] == b'X' {
                                return;
                            }
                            if tag[0] == b'Q' {
                                asked.fetch_add(1, Ordering::SeqCst);
                                // EmptyQueryResponse, ReadyForQuery.
                                let mut answer = b"I\0\0\0\x04".to_vec();
                                answer.extend_from_slice(ready);
                                if stream.write_all(&answer).is_err() {
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            Counting {
                port,
                taken,
                queries,
            }
        }
    }

    /// A pool opens its first connection only: two statements one after
    /// the other run on that connection, which is checked with one
    /// round trip each time it is taken.
    #[test]
    fn a_pool_opens_its_first_connection_only() {
        use std::sync::atomic::Ordering;
        let server = Counting::start();
        let config: Config = format!("postgres://u@127.0.0.1:{}/db?sslmode=disable", server.port)
            .parse()
            .expect("config");
        let limit = Duration::from_secs(10);
        let first = connect_within(&config, &Tls::Off, limit).expect("the first connection");
        let manager = Manager {
            config,
            tls: Tls::Off,
            limit,
            first: Mutex::new(Some(first)),
        };
        let pool = pool_of(
            manager,
            &ConnectOpts {
                max_pool_size: Some(5),
            },
        );
        for _ in 0..2 {
            let conn = PinnedConn::take(&pool).expect("a connection");
            assert!(!conn.client().is_closed());
        }
        // (Time for connections the pool would open at once.)
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(server.taken.load(Ordering::SeqCst), 1, "connections");
        assert_eq!(server.queries.load(Ordering::SeqCst), 2, "queries");
    }

    /// What a client did with a server without TLS, which answers a
    /// request for TLS with `N`.
    #[derive(Debug, PartialEq)]
    enum Asked {
        /// It asked for no TLS: its first message is its startup.
        NoTls,
        /// It asked, and closed the connection after the answer.
        ThenClosed,
        /// It asked, and went on with its startup message after the
        /// answer.
        ThenStartup,
    }

    /// The fake server of `Asked`, on the connection `stream`.
    fn no_tls_server(mut stream: impl std::io::Read + std::io::Write) -> Asked {
        // A startup message: its length, then protocol version 3.0.
        let startup = |message: &[u8]| message.get(4..8) == Some(&[0, 3, 0, 0][..]);
        let mut first = [0; 8];
        stream.read_exact(&mut first).expect("the first message");
        if first != [0, 0, 0, 8, 4, 210, 22, 47] {
            assert!(
                startup(&first),
                "neither a request for TLS nor a startup: {first:?}"
            );
            return Asked::NoTls;
        }
        stream.write_all(b"N").expect("answer");
        let mut after = [0; 512];
        match stream.read(&mut after).expect("read") {
            0 => Asked::ThenClosed,
            n => {
                assert!(startup(&after[..n]), "{:?}", &after[..n]);
                Asked::ThenStartup
            }
        }
    }

    /// Connect with the connection string `url` of a local port to a
    /// server without TLS: the connect's value, and what the client did.
    fn without_tls(url: impl FnOnce(u16) -> String) -> (Value, Asked) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // (A safety net: the client answers at once.)
            let net = Some(Duration::from_secs(30));
            stream.set_read_timeout(net).expect("timeout");
            no_tls_server(stream)
        });
        let result = do_connect(url(port));
        (result, server.join().expect("the server"))
    }

    /// The URL and the `key=value` form of a connection string to a
    /// local `port`, with the option `sslmode` if one is given.
    fn both_forms(port: u16, sslmode: Option<&str>) -> [String; 2] {
        let url = format!("postgres://u:p@127.0.0.1:{port}/db");
        let kv = format!("host=127.0.0.1 port={port} user=u password=p dbname=db");
        match sslmode {
            Some(mode) => [
                format!("{url}?sslmode={mode}"),
                format!("{kv} sslmode={mode}"),
            ],
            None => [url, kv],
        }
    }

    /// A server without TLS is refused when sslmode requires TLS (the
    /// default, verify-full, among them), before the client sends
    /// anything more, in either form of the connection string.
    #[test]
    #[cfg(feature = "postgres-tls")]
    fn a_server_without_tls_is_refused_when_tls_is_required() {
        for mode in [
            None,
            Some("require"),
            Some("verify-ca"),
            Some("verify-full"),
        ] {
            for form in [0, 1] {
                let (result, asked) = without_tls(|port| both_forms(port, mode)[form].clone());
                assert_eq!(
                    result.to_string(),
                    "Err(postgres TLS error: error performing TLS handshake: server does not support TLS)",
                    "{mode:?}, form {form}"
                );
                assert_eq!(asked, Asked::ThenClosed, "{mode:?}, form {form}");
            }
        }
    }

    /// With sslmode=prefer the client goes on without TLS, as libpq's
    /// `prefer` does, in either form of the connection string.
    #[test]
    #[cfg(feature = "postgres-tls")]
    fn prefer_goes_on_without_tls() {
        for form in [0, 1] {
            let (_, asked) = without_tls(|port| both_forms(port, Some("prefer"))[form].clone());
            assert_eq!(asked, Asked::ThenStartup, "form {form}");
        }
    }

    /// With sslmode=disable the client asks for no TLS, in either form
    /// of the connection string.
    #[test]
    fn disable_asks_for_no_tls() {
        for form in [0, 1] {
            let (_, asked) = without_tls(|port| both_forms(port, Some("disable"))[form].clone());
            assert_eq!(asked, Asked::NoTls, "form {form}");
        }
    }

    /// A Unix socket carries no TLS, whatever sslmode says: the client
    /// asks for none, in either form of the connection string.
    #[test]
    #[cfg(unix)]
    fn a_unix_socket_asks_for_no_tls() {
        let dir = std::env::temp_dir().join(format!("silt-pg-socket-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the socket's directory");
        let socket = dir.join(".s.PGSQL.5432");
        let host = dir.to_str().expect("a UTF-8 path");
        let encoded =
            percent_encoding::utf8_percent_encode(host, percent_encoding::NON_ALPHANUMERIC);
        for mode in [
            None,
            Some("require"),
            Some("verify-full"),
            Some("prefer"),
            Some("disable"),
        ] {
            let option = mode.map(|mode| format!("sslmode={mode}"));
            let forms = [
                format!(
                    "postgres://u:p@{encoded}/db?{}",
                    option.clone().unwrap_or_default()
                ),
                format!(
                    "host={host} user=u password=p dbname=db {}",
                    option.unwrap_or_default()
                ),
            ];
            for (form, url) in forms.into_iter().enumerate() {
                let _ = std::fs::remove_file(&socket);
                let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
                let server = std::thread::spawn(move || {
                    let (stream, _) = listener.accept().expect("accept");
                    let net = Some(Duration::from_secs(30));
                    stream.set_read_timeout(net).expect("timeout");
                    no_tls_server(stream)
                });
                let _ = do_connect(url);
                let asked = server.join().expect("the server");
                assert_eq!(asked, Asked::NoTls, "{mode:?}, form {form}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The TLS options are read from either form of the connection
    /// string, decoded, the last one standing, and cut out of it; the
    /// rest of it stays as it was.
    #[test]
    fn tls_options_are_read_from_either_form() {
        let read = |s: &str| {
            let (kept, params) = without_ssl_options(s).expect("parse ok");
            (kept, params.mode, params.root_cert)
        };
        assert_eq!(
            read("host=h sslmode=verify-ca user=u sslrootcert='/a b.pem' dbname=d"),
            (
                "host=h  user=u  dbname=d".to_string(),
                Some(EffectiveSslMode::VerifyCa),
                Some("/a b.pem".to_string())
            )
        );
        assert_eq!(
            read("host=h sslmode=prefer sslmode = disable"),
            (
                "host=h  ".to_string(),
                Some(EffectiveSslMode::Disable),
                None
            )
        );
        assert_eq!(
            read(
                "postgres://u:p@h/db?sslmode=verify%2Dfull&application_name=x&sslrootcert=%2Fca.pem"
            ),
            (
                "postgres://u:p@h/db?application_name=x".to_string(),
                Some(EffectiveSslMode::VerifyFull),
                Some("/ca.pem".to_string())
            )
        );
        assert_eq!(
            read("postgres://u:p?w@h/db?sslmode=disable"),
            (
                "postgres://u:p?w@h/db".to_string(),
                Some(EffectiveSslMode::Disable),
                None
            )
        );
        assert_eq!(
            read("host=h user=u"),
            ("host=h user=u".to_string(), None, None)
        );
        for s in ["host=h sslmode=allow", "postgres://h/db?sslmode=allow"] {
            assert_eq!(
                without_ssl_options(s).err(),
                Some("unknown sslmode: allow".to_string())
            );
        }
        // What is left parses.
        let (kept, _) = without_ssl_options("host=h sslmode=verify-full dbname=d").expect("ok");
        assert!(kept.parse::<Config>().is_ok(), "{kept}");
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
