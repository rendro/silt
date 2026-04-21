---
title: "postgres"
section: "Standard Library"
order: 19
---

# postgres (opt-in feature)

PostgreSQL client backed by an `r2d2`-managed connection pool.
Cooperatively yields on I/O so a silt task that calls `postgres.query`
parks until the result lands and other tasks keep running in the
meantime.

The `postgres` module is **not** built by default. Build silt with
`--features postgres` to enable it, or `--features postgres-tls` to add
`native-tls`-backed TLS support for `postgresql+tls://` URLs.

```sh
cargo build --release --features postgres
```

Pair the builtins below with a silt-side `pg.silt` package that
declares the companion types (`PgPool`, `PgTx`, `PgError`, `Value`,
`QueryResult`, `ExecResult`, `PgCursor`, and `Notification`). The
built-in functions reference those types by name; the typechecker
unifies them against whatever your `pg.silt` library defines.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `connect` | `(String) -> Result(PgPool, PgError)` | Open a connection pool from a `postgresql://` URL (uses r2d2 defaults) |
| `connect_with` | `(String, Map(String, Int)) -> Result(PgPool, PgError)` | Like `connect` with a tunable options bag (see [Connect options](#connect-options)) |
| `query` | `(PgPool \| PgTx, String, List(Value)) -> Result(QueryResult, PgError)` | Run a SELECT-style statement and materialize rows |
| `execute` | `(PgPool \| PgTx, String, List(Value)) -> Result(ExecResult, PgError)` | Run an INSERT/UPDATE/DELETE and return affected-row count |
| `transact` | `(PgPool, Fn(PgTx) -> Result(a, PgError)) -> Result(a, PgError)` | Pin a single connection for a transaction; callback runs inside BEGIN/COMMIT |
| `close` | `(PgPool) -> ()` | Drop the pool; future ops on it error |
| `stream` | `(PgPool \| PgTx, String, List(Value)) -> Result(Channel(Row), PgError)` | Stream rows through a bounded channel (backpressured) |
| `cursor` | `(PgTx, String, List(Value), Int) -> Result(PgCursor, PgError)` | Declare a server-side cursor with batch size |
| `cursor_next` | `(PgCursor) -> Result(List(Map(String, Value)), PgError)` | Fetch the next batch of rows from a cursor |
| `cursor_close` | `(PgCursor) -> Result((), PgError)` | Release a cursor and its underlying connection |
| `listen` | `(PgPool, String) -> Result(Channel(Notification), PgError)` | LISTEN on a channel; delivers async notifications |
| `notify` | `(PgPool \| PgTx, String, String) -> Result((), PgError)` | NOTIFY a channel with a payload |
| `uuidv7` | `() -> String` | Generate a time-ordered UUIDv7 (RFC 9562) |

## Cooperative I/O

Every fallible op above (except `uuidv7`) cooperates with silt's task
scheduler: when called from inside a `task.spawn`'d task, it submits
the request to silt's I/O pool and yields the task slot until the
response arrives. From silt's perspective the call looks synchronous.
Called from the main task it runs synchronously on the calling thread.

## Transactions

`postgres.transact` pins one pooled connection for the duration of the
callback, issues `BEGIN` up-front, and either `COMMIT`s on `Ok(_)` or
`ROLLBACK`s on `Err(_)` (or on panic). The callback receives a `PgTx`
handle; queries that should participate in the transaction must go
through that handle — calling `postgres.query(pool, ...)` with the
enclosing pool would pick a different connection and miss the
transaction entirely. Nested `postgres.transact` calls are rejected;
use `postgres.execute(tx, "SAVEPOINT ...")` manually instead.

## Streaming and cursors

`postgres.stream` returns a bounded `Channel` whose elements are
`Result(Map(String, Value), PgError)` rows. A background worker pumps
the cursor into the channel and closes it when the query completes
(or on error). Slow consumers backpressure the server side via the
channel capacity.

`postgres.cursor` is the lower-level primitive: it `DECLARE`s a
server-side cursor inside an open transaction and returns an opaque
`PgCursor` that `cursor_next` can repeatedly drain in batches. Always
call `cursor_close` (or let the transaction commit/rollback, which
cleans up).

## LISTEN / NOTIFY

`postgres.listen(pool, "channel_name")` returns a `Channel` that
delivers a `Notification` record for every NOTIFY on that PostgreSQL
channel. The underlying worker owns a dedicated connection, so LISTEN
does not consume a slot from the regular query pool.
`postgres.notify(target, channel, payload)` sends a single NOTIFY.

## Example

```text
-- Pair with a user-side `pg.silt` that declares the Value ADT
-- (VInt/VStr/VBool/VFloat/VNull/VList), PgPool, PgTx, PgError,
-- QueryResult, ExecResult, PgCursor, and Notification.
import pg
import postgres

fn main() {
  match postgres.connect("postgresql://localhost/app") {
    Ok(pool) -> {
      -- Transactional INSERT + SELECT.
      let result = postgres.transact(pool, fn(tx) {
        let _ = postgres.execute(
          tx,
          "INSERT INTO users (id, name) VALUES ($1, $2)",
          [VStr(postgres.uuidv7()), VStr("alice")],
        )?
        postgres.query(tx, "SELECT count(*) FROM users", [])
      })
      match result {
        Ok(rows) -> println("committed: {rows}")
        Err(e) -> println("rolled back: {e}")
      }
      postgres.close(pool)
    }
    Err(e) -> println("connect err: {e}")
  }
}
```

## Notes

- The `Value` parameter ADT (`VInt`, `VStr`, `VBool`, `VFloat`, `VNull`,
  `VList`) is declared in the user's `pg.silt`, not here — the builtin
  module references it by name only.
- `postgres.uuidv7` produces a time-ordered UUID suitable for use as a
  primary key; collisions within the same millisecond are disambiguated
  with random bits per RFC 9562.
- `postgres-tls` pulls in `native-tls` / `postgres-native-tls` and
  therefore depends on the system TLS stack (OpenSSL / Schannel /
  SecureTransport depending on platform).

## TLS: secure-by-default

The `sslmode=` parameter in the connection URL controls certificate
verification. Silt explicitly **breaks with libpq defaults** here to
fail closed against surprise MITM paths:

| `sslmode=` | Encryption | Cert validity | Hostname check |
| --- | --- | --- | --- |
| *(omitted)* | **yes** | **yes** | **yes** (equivalent to `verify-full`) |
| `disable` | no | — | — |
| `prefer` / `allow` | opportunistic | no | no |
| `require` | yes | no | no (libpq-compatible encryption-only) |
| `verify-ca` | yes | yes | no |
| `verify-full` | yes | yes | yes |

**New default (silt 0.11+)**: a connection URL that omits `sslmode=`
entirely resolves to `verify-full`. This is a deliberate deviation
from libpq's historical `prefer`, which silently downgraded to
plaintext on handshake failure. A silt program whose URL is just
`postgres://user@host/db` now requires a valid, hostname-matching TLS
cert — or an explicit opt-out via `?sslmode=disable`.

If silt was built **without** the `postgres-tls` feature, a URL that
defaults to `verify-full` (or any `require` / `verify-*`) returns a
clear `ConnectionError` at connect time rather than silently using
plaintext.

**`sslmode=require` remains encryption-only**: when you explicitly
write `sslmode=require` in the URL, silt keeps libpq semantics
(encryption on, cert/hostname validation off). It is an explicit
opt-in to the weaker mode. Prefer `verify-full` unless you have a
concrete reason otherwise.

**Recommended**:
- `postgres://user:pw@host/db` (nothing after the path) — safe default.
- `postgres://user:pw@host/db?sslmode=verify-full` — explicit, same behaviour.
- `postgres://user:pw@host/db?sslmode=disable` — local dev / Unix socket.
- Use `verify-ca` only when DNS / hostname configuration makes
  `verify-full` impractical.

## Connect options

`postgres.connect_with(url, opts)` accepts a `Map(String, Int)` options
bag for tunables that don't belong in the URL. Unknown keys are
silently ignored so new knobs can be added without a breaking change.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `max_pool_size` | `Int` (> 0) | r2d2 default (`10`) | Upper bound on the number of pooled connections. Raise for highly concurrent silt programs that otherwise block waiting for a free connection. |

```text
-- explicit 32-connection pool
let pool = postgres.connect_with(
  "postgres://app@db/app",
  #{"max_pool_size": 32}
)?

-- Same as postgres.connect(url):
let pool = postgres.connect_with("postgres://app@db/app", #{})?
```

## Error detail redaction

PostgreSQL error responses routinely embed user row values in their
`DETAIL:`, `WHERE:`, and `HINT:` follow-on fields — for example, a
UNIQUE violation reports `DETAIL: Key (email)=(alice@example.com)
already exists.`. A silt web handler that echoes the `Err(_)` value
into a 5xx response body would otherwise leak that email to
unauthenticated callers.

Silt strips those follow-on fields before the error crosses the VM
boundary into silt. The primary short message and SQLSTATE code
remain intact so callers can still pattern-match on the typed
`PgError` variants (e.g. `PgQuery(msg, sqlstate)` carries the five-
character SQLSTATE code so constraint-specific branches can match on
`"23505"` for unique violations, `"23503"` for FK violations, etc.).
If you need the full un-redacted text for diagnostics, log it on the
Rust side (e.g. via a custom embedder) — the silt-side `PgError` value
is intentionally scrubbed.

## PgError variants

All fallible `postgres.*` calls return `Result(T, PgError)`. The
variants (declared in silt's stdlib, no user `pg.silt` entries
needed) are:

| Variant | Fields | Raised for |
| --- | --- | --- |
| `PgConnect(msg)` | `String` | Pool checkout, URL parse, SQLSTATE class `08` |
| `PgTls(msg)` | `String` | TLS handshake / cert read / connector build |
| `PgAuthFailed(msg)` | `String` | SQLSTATE class `28` (invalid auth) |
| `PgQuery(msg, sqlstate)` | `String, String` | Any other DbError; `sqlstate` is the 5-char code |
| `PgTypeMismatch(col, expected, actual)` | `String, String, String` | Row decode failures |
| `PgNoSuchColumn(col)` | `String` | SQLSTATE `42703` (undefined_column) |
| `PgClosed` | — | Connection dropped mid-query |
| `PgTimeout` | — | SQLSTATE `57014` or transport timeout |
| `PgTxnAborted` | — | SQLSTATE `25P02` (in_failed_sql_transaction) |
| `PgUnknown(msg)` | `String` | Catch-all for shapes we can't classify |

`PgError` implements the stdlib `Error` trait, so if you don't want
to pattern-match on variants you can always call `err.message()` to
get a formatted user-friendly string.
