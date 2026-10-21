---
title: "postgres"
section: "Standard Library"
order: 23
---

# postgres (opt-in feature)

PostgreSQL client backed by an `r2d2`-managed connection pool.
Cooperatively yields on I/O so a silt task that calls `postgres.query`
parks until the result lands and other tasks keep running in the
meantime.

The `postgres` module is **not** built by default. Build silt with
`--features postgres` to enable it, or `--features postgres-tls` to add
`native-tls`-backed TLS; whether a connection uses TLS is chosen by its
`sslmode` (see [TLS](#tls-secure-by-default)).

```sh
cargo build --release --features postgres
```

A statement's parameters are a tuple of plain values, and the rows of
its result are decoded into a record type the program declares (or into
`json.Value`, for rows of a shape the program does not know), as
`json.parse(text, T)` decodes a document. See
[Parameters and rows](#parameters-and-rows).

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `connect` | `(String) -> Result(PgPool, PostgresError)` | Open a connection pool from a `postgresql://` URL (uses r2d2 defaults) |
| `connect_with` | `(String, Map(String, Int)) -> Result(PgPool, PostgresError)` | Like `connect` with a tunable options bag (see [Connect options](#connect-options)) |
| `query` | `(a, String, p, type r) -> Result(List(r), PostgresError)` | Run a statement and decode its rows into a record type (or json.Value) |
| `execute` | `(a, String, p) -> Result(Int, PostgresError)` | Run a statement and return the number of rows it affected |
| `transact` | `(PgPool, Fn(PgTx) -> Result(a, PostgresError)) -> Result(a, PostgresError)` | Pin a single connection for a transaction; callback runs inside BEGIN/COMMIT |
| `close` | `(PgPool) -> ()` | Drop the pool; future ops on it error |
| `stream` | `(a, String, p, type r) -> Result(Channel(Result(r, PostgresError)), PostgresError)` | Stream decoded rows through a bounded channel (backpressured) |
| `cursor` | `(PgTx, String, p, Int, type r) -> Result(PgCursor(r), PostgresError)` | Declare a server-side cursor with batch size |
| `cursor_next` | `(PgCursor(r)) -> Result(List(r), PostgresError)` | Fetch the next batch of decoded rows from a cursor |
| `cursor_close` | `(PgCursor(r)) -> Result((), PostgresError)` | Release a cursor and its underlying connection |
| `listen` | `(PgPool, String) -> Result(Channel(Notification), PostgresError)` | LISTEN on a channel; delivers async notifications |
| `notify` | `(a, String, String) -> Result((), PostgresError)` | NOTIFY a channel with a payload |

The first argument of `query`, `execute`, `stream` and `notify` (`a`
above) is the connection: a `PgPool`, or the `PgTx` of a transaction.
Any other type is a compile error, where the call is written and at each
call of a function or method that passes its own parameter on as the
connection:
``the connection of `postgres.query` must be a PgPool or a PgTx``.

## Parameters and rows

```text
type User { id: Int, name: String, nick: Option(String), born: time.Date }
type Id { id: Int }

let users = postgres.query(db, "SELECT id, name, nick, born FROM users WHERE id > $1", (10,), User)?
let rows  = postgres.query(db, "SELECT * FROM audit", (), json.Value)?
let n     = postgres.execute(db, "DELETE FROM users WHERE id = $1", (id,))?
let ids   = postgres.query(db, "INSERT INTO users (name) VALUES ($1) RETURNING id", (name,), Id)?
```

### Parameters

The parameters of a statement are a tuple, `$1` its first element:
`(id, name)`, `(id,)` for one (`(id)` is `id` itself, and the compile
error says to write `(id,)`), `()` for none. A function or method that
passes its own parameter on as the parameters is checked the same way at
each call.

Each parameter goes to the server as text, and the server reads it as
the type the statement needs at its place, so an `Int` goes into an
`int4`, an `int8` or a `numeric`, a `String` into a `date`, a `uuid`
or an enum, a `Float` into a `float4`; a text the type does not take
(`"abc"` for an `int4`) is the server's `Query` error. The text of a
value:

| Value | Sent as |
| --- | --- |
| `None` | `NULL` (`Some(x)` is sent as `x`) |
| `Int`, `Float`, `Bool`, `String` | its text (a `Float` keeps its point: `2.0`, which no integer column takes) |
| `Bytes` | `bytea`'s hex text, `\x00ff` |
| `Date`, `Time`, `DateTime` | silt's text of it: `2026-10-13`, `01:02:03`; a `DateTime` in UTC, `2026-10-13T01:02:03+00`; a year before 1 as a year BC, `0044-03-15 BC` |
| a `List` | an array (`{"1","2"}`) where the statement's parameter is an array (`id = ANY($1)`), and JSON anywhere else |
| a variant without fields | its name, as a `String` is sent (so `Active` goes into a `text` column, or a postgres enum with the label `Active`); like a `String`, it is no JSON for a `json` or `jsonb` column: there, send it in a record or as a `json.Value` |
| a record, a `Map`, a tuple, a variant with fields, a `json.Value` | JSON, as `json.stringify` writes it (`json.Null` is the JSON `null`, not SQL `NULL`) |
| a value of a type with a written [`json.Encode`](json.md#jsonencode) impl | the `json.Value` its `encode` gives, sent as a `json.Value` is (JSON), wherever the value is in the parameter |

A `DateTime` read from a `timestamptz` (in UTC, see the table of
[Rows](#rows)) and sent back is the same instant whatever the session's
`TimeZone`; a `timestamp` column ignores the offset.

A value that JSON has no form for (a function, a set, a channel, or
`Bytes` inside a record) is an error of the program when the statement
is called (`postgres.query: unsupported value kind Fn`), and nothing is
run. A list of `Bytes` goes into a `bytea[]`; where it is to
be JSON it is the same error, found before the statement is run. In a
`postgres.stream`, which returns before the statement is prepared, that
error is the channel's one element, `Err(Other("postgres.stream:
unsupported value kind Bytes"))`.

### Rows

`postgres.query`, `postgres.stream` and `postgres.cursor` take the type
a row is decoded into as their last argument: a record type, or
`json.Value`, or an alias of one (the type it names). It is checked where
it is known, as for `json.parse` (written at the call, or given to a
function or method that passes it on, at each call of it): any other
type, or a record with a field of a type no column can fill (`Bytes` in a
`Map`, a tuple or a nested record, which only a json document could fill;
an enum, which postgres has no decoder for), is a compile error that names
the field and its record. A record nested in a field is filled from a json
document by `json.parse`'s rules, an enum in it too. A record or enum type
with a written [`json.Decode`](json.md#jsondecode) impl is decoded by the
impl (see [Types with a `json.Decode` impl](#types-with-a-jsondecode-impl)).

| Function | Type argument |
|----------|---------------|
| `postgres.query` | a record type or `json.Value` |
| `postgres.stream` | a record type or `json.Value` |
| `postgres.cursor` | a record type or `json.Value` |

Each filled from the columns the table below names, the fields of the record
type may have these types:

- `Int`, `Float`, `String`, `Bool`
- `Bytes`, as the field, in an `Option` or in a `List` (a `bytea` column)
- `Date`, `Time`, `DateTime`
- `json.Value`, any column as the table's last column says
- `List(T)`, `Option(T)`, `Map(String, T)`
- tuples, such as `(Int, String)`
- non-generic record types, from a `json` or `jsonb` column by `json.parse`'s
  rules (an enum in them too)
- a record or enum type with a written `json.Decode` impl, from a `json` or
  `jsonb` column by the impl
- type aliases of any of these

where `T` is again one of these types. An enum without such an impl, a
`Set`, a generic record, a function or a type parameter has no decoder.

Each field of the record is read from the column of its exact name;
Postgres folds a name it is not given in quotes to lower case, so a
column created as `"Id"` is read through `AS`: `SELECT "Id" AS id`. A
column that no field names is not read. The columns are checked against
the fields once per result, before the statement runs:

- a field with no column is `Err(NoSuchColumn(field))`, an `Option`
  field too;
- a column whose type cannot fill its field is
  `Err(TypeMismatch(column, field type, column type))`, say
  `TypeMismatch("id", "Int", "float8")`: the column's type decides, not
  its values (an `Int` field is not filled from a `float8` column even
  where the value is `2.0`). Its `message()` says how to go on:
  ``postgres type mismatch on column `id`: expected Int, got float8;
  cast it in SQL, for example id::text``. A `NULL` that has no type of
  its own (`SELECT NULL AS v`) is a `text` column: cast it, `NULL::int4`;
- two columns of the name a field reads are
  ``Err(Other("postgres: column `id` appears twice in the result; rename one with AS"))``.

So a statement whose rows do not fit is not run: an `INSERT ...
RETURNING` into a record that does not fit inserts nothing. A cell is
checked as it is read, after the statement has run: an `INSERT ...
RETURNING` whose returned cell does not fit is `Err`, and its rows were
inserted all the same (on a pool they stay; in a transaction the
callback decides). A `NULL` fills an `Option` field with `None`, and
any other field with nothing, `Err(TypeMismatch(column, field type,
"NULL"))`; a `NaN`, an `Infinity` or a date's `infinity` is
`TypeMismatch(column, field type, "NaN")` (or `"Infinity"`,
`"-Infinity"`); an array of more dimensions, or of fewer, than the
field's list has levels is `TypeMismatch(column, "List(List(Int))",
"int4[] with 1 dimension")`. The message of each says to cast the
column in SQL, but that of a `NULL` does not, nor that of anything a
`json` or `jsonb` column holds. A failed decode is never a value: it is
the call's `Err`.

| Column type | Field types it fills | In a `json.Value` row |
| --- | --- | --- |
| `bool` | `Bool` | `json.Bool` |
| `int2`, `int4`, `int8` | `Int`; `Float` (the nearest) | `json.Int` |
| `float4`, `float8` | `Float` (`NaN` and `Infinity` are errors) | `json.Float` |
| `numeric` | `String` (the exact text), `Float` (the nearest) | `json.String`, the exact text |
| `text`, `varchar`, `char(n)`, `name`, `"char"`, an enum | `String` | `json.String` |
| `uuid` | `String` | `json.String` |
| `bytea` | `Bytes` | `json.String`, the hex text `\x00ff` |
| `date` | `Date` | `json.String`, as silt shows a `Date` |
| `time` | `Time` | `json.String`, as silt shows a `Time` |
| `timestamp`, `timestamptz` | `DateTime` (a `timestamptz` in UTC) | `json.String`, as silt shows a `DateTime` |
| `json`, `jsonb` | any field type but `Bytes` and an enum, decoded by `json.parse`'s rules | the document |
| an array | `List(T)` of a `T` its elements fill, `List(Option(T))` where an element may be `NULL`, `List(List(T))` for two dimensions | `json.Array` |
| `void` (what `pg_sleep(1)` gives) | none | `json.Null` |

A domain's column is its base type's. A `json` or `jsonb` column fills a
`String` field only with a JSON string (cast it, `col::text`, for the
document's text), an `Option` field with `None` where the document is
`null`, and no field that holds `Bytes` (in a record it names too); a
document that does not fit its field is `TypeMismatch(column, field
type, "jsonb: <why>")`, with the column's type and why as
`json.decode` says it, ending with the path of the value in the document:
`TypeMismatch("doc", "Meta", "jsonb: json type mismatch: expected Int, got
string at level")`. Every other type (`interval`, `timetz`, `inet`, `money`, ...)
fills no field, and is an error in a `json.Value` row as well: cast it
in SQL, `col::text`.

In a `json.Value` row each row is a `json.Object` of all its columns,
`json.Null` for a `NULL`; two columns of one name are the error above.

### Types with a `json.Decode` impl

One data layer, one override: a record or enum type with a written
`trait json.Decode for T` impl is decoded by its `decode` in a row as in
`json.parse`, and its fields are not looked into (it may hold a `Set`):

- A field whose type is, or holds, such a type is filled from a `json` or
  `jsonb` column (or an array of them), as a record's field is: the impl
  gets its part of the cell's document as a `json.Value`, wherever it is
  (the field itself, an element of a list, a field of a record in the
  document). An `Option` around it stays structural (`NULL` and `null`
  are `None`). Any other column is the plan's `TypeMismatch(column,
  "T", "text")` before the statement runs. The `Err` the impl returns
  is the column's mismatch, worded as the decoder's own errors in a
  document: `TypeMismatch("code", "Code", "jsonb: no code here")`.
  Where the part is inside a cell's document (an element of a
  `List(Code)` field from a `jsonb` column), the message ends with its
  path there: `TypeMismatch("codes", "List(Code)", "jsonb: no code here
  at [0]")`. In an array column (`jsonb[]`) each element is a document
  of its own: the mismatch names the element's type and no index, as the
  decoder's own errors there do, `TypeMismatch("codes", "Code",
  "jsonb[]: no code here")`. The first error in the row, in the order
  of the type's fields, counts, the impl's or a later cell's.
- A row type with such an impl is decoded by the impl from the
  `json.Object` of the row's columns, as a `json.Value` row is (every
  column, by its name): an enum type is a row type so. The `Err` the impl
  returns is the row's `Err(Other(message))`, the `JsonError`'s message:
  `Other("json missing field: n")`.

The impls are the program's code, which the task that called
`postgres.query` or `postgres.cursor_next` runs, after the rows are read
(a failure of an impl is that task's). In a stream, a task of its own
finishes each such row before the worker sends it: an impl's `Err` is
that row's `Err`, and the stream ends there, as at a row the decoder
refuses; an impl that fails ends the stream with its failure, as a
failing stream stage does: `channel.each` raises it, a plain
`channel.receive` sees `None`.

`postgres.execute` gives the number of rows the statement affected (for
a statement that affects no rows, such as `CREATE TABLE`, `0`). A
statement whose rows the program reads, `RETURNING` among them, is a
`postgres.query`.

## Connecting

`postgres.connect` makes the pool's first connection itself and says at
once why one fails: `Connect(msg)` for a port where nothing listens, a
host that is unknown or cannot be reached, or a database that does not
exist; `AuthFailed(msg)` for a role or password the server refuses
(SQLSTATE class `28`); `Tls(msg)` for a failed TLS handshake, or a
server without TLS when `sslmode` requires TLS; `Timeout`
for a server that does not answer within 10 seconds, login included.
A `connect_timeout=N` in the connection string (whole seconds, more than
0; in a URL's query or in the `key=value` form, the last one standing)
sets that time instead; `connect_timeout=0` or less is `Connect(msg)`. The pool opens
its other connections when it needs them, each bounded the same way: an
attempt that gets no answer in that time is given up, and nothing of it
is left behind. Each statement on the pool checks the connection it
takes with a round trip to the server (an empty query), so that a
connection whose session the server has ended is not used: it is
closed, and another is opened. A connection that has been idle for 10
minutes, or open for 30, is closed while it is idle, and opened again
only when a statement needs it. A statement that finds no free connection in the
pool, which has opened as many as `max_pool_size` allows, waits 30
seconds for one, and is then `Timeout`.

## Cooperative I/O

Every function above that talks to the server cooperates with silt's
task scheduler: it hands its work to silt's I/O pool, and the task
that called it waits for the answer while the program's other tasks go
on, whether that task is one of `task.spawn`'s or `main`. From silt's
perspective the call looks synchronous. `postgres.stream` returns its
channel at once; a worker of its own feeds it.

## Transactions

`postgres.transact` pins one pooled connection for the duration of the
callback, issues `BEGIN` up-front, and either `COMMIT`s on `Ok(_)` or
`ROLLBACK`s on `Err(_)` (or on panic). The callback receives a `PgTx`
handle; queries that should participate in the transaction must go
through that handle — calling `postgres.query(pool, ...)` with the
enclosing pool would pick a different connection and miss the
transaction entirely. Nested `postgres.transact` calls are rejected;
use `postgres.execute(tx, "SAVEPOINT ...")` manually instead.

A statement of the transaction that fails aborts it in the server:
the transaction's later statements are `Err(TxnAborted)`, and if the
callback returns `Ok(_)` all the same, `postgres.transact` answers
`Err(TxnAborted)` and nothing is committed (unless a
`ROLLBACK TO SAVEPOINT` brought the transaction back first).

A `task.deadline` that passes, or a `task.cancel`, ends a task that
waits for a statement at once, on the pool and inside a transaction
alike. What becomes of the statement in the server differs:

- A statement that has no connection yet (it waits for one of the pool,
  or for the transaction's while another statement of it runs) is never
  sent: it gives up, nothing is cancelled, and the transaction goes on.
  The same holds for the `BEGIN` of `postgres.transact` and the `LISTEN`
  of `postgres.listen`.
- On the pool, outside a transaction, a statement that has its
  connection is left to finish; its connection then goes back to the
  pool.
- Inside `postgres.transact`, the server is asked to cancel a
  statement that has the connection. When the task that ends is the one that runs `transact`
  (or the callback fails with an error of the program), the
  transaction is rolled back. When the deadline or the cancel is one
  the callback set around the statement itself, the statement fails
  as any failed statement does: the transaction is aborted, its later
  statements are `Err(TxnAborted)` at once, and so is
  `postgres.transact`'s answer if the callback returns `Ok(_)`. The
  same holds where `SILT_IO_TIMEOUT` ends the wait for a statement of
  a transaction. This is for the statements a task waits for:
  `postgres.query`, `postgres.execute`, `postgres.notify` and the
  cursor functions (`cursor`, `cursor_next`, `cursor_close`). A
  deadline or a cancel around `channel.receive` of a stream on the
  transaction ends a wait on the channel, not on a statement: it
  cancels and aborts nothing (a stream on a `PgTx` is never
  cancelled; see below).
  The connection goes back to the pool after the rollback; it is
  closed instead if the rollback fails, or if a cancel request was sent
  for it (a request that reaches the server late would cancel the
  connection's next statement).

Around the callback `postgres.transact` waits too, and a deadline or a
cancel ends the task there at once as well. While it waits for a
connection of the pool or for its `BEGIN`, the transaction is not
begun, or is rolled back as soon as it is. While it waits for its
`COMMIT` or `ROLLBACK`, that goes on and finishes in the server, and
the task does not learn how it ended. `SILT_IO_TIMEOUT` (see the
concurrency guide) ends the wait for a connection with its `BEGIN` and
the wait for each statement with `Err(Timeout)`, but not the wait for
`COMMIT` or `ROLLBACK`: an `Err` from `postgres.transact` never stands
for a transaction that was committed.

## Streaming and cursors

`postgres.stream(conn, sql, params, T)` returns a bounded `Channel`
whose elements are `Result(T, PostgresError)` rows. A background worker
pumps the rows into the channel and closes it when the query completes
(or on error: a statement that fails, a result whose columns do not fit
`T`, a row that cannot be decoded). Slow consumers backpressure the
server side via the channel capacity.

A receiver that has read enough closes the channel (`channel.close`),
and the worker stops. On a pool, a stream that stops before its last
row (closed, or dropped as the program ends) has its statement
cancelled in the server, and its connection is closed rather than given
back to the pool with rows unread. On a `PgTx` the connection is the
transaction's: the rest of the rows is read and dropped before the
transaction's next statement or its `COMMIT` (a cancel would abort the
transaction).

The worker counts as a task of the program. While it fetches rows the
program does not end. When the program ends while the worker waits for
room in the channel (nobody reads it, or its reader was cancelled), the
worker is dropped there, as a task that waits to send is. A stream
opened on a `PgTx` ends with the transaction: its channel is closed
when the transaction commits or rolls back, and the rows it holds can
still be received. While it is open it holds the transaction's
connection: the transaction's other statements wait while its worker
fetches rows, until it has sent the last row or has stopped. While the
worker waits for room in the channel (nobody reads it), a statement of
the transaction does not wait, which would be for ever where the task
that would read the stream is the one that waits for the statement:
it is `Err(Other("postgres: a stream of this transaction is still
open; read or close it first"))` at once. So is a statement that comes
while a reader slower than the stream (another task, say) leaves the
channel full: read the stream to its end, or close it, first. A stream on a `PgTx` whose
reader is gone (cancelled, say) without closing the channel therefore
holds the connection until the transaction ends or the program does.

`postgres.cursor(tx, sql, params, batch_size, T)` is the lower-level
primitive: it `DECLARE`s a server-side cursor inside an open
transaction and returns an opaque `PgCursor(T)` that `cursor_next` can
repeatedly drain in batches of `List(T)`. Its columns are checked
against `T` when it is declared, so that is where a mismatch is an
error. A batch that cannot be decoded (a `NULL` for a field that is no
`Option`, say), or that the server fails, is `cursor_next`'s `Err`, and
ends the cursor, as a stream ends after its `Err`: every later
`cursor_next` is the same error. Always call `cursor_close` (or let the transaction
commit/rollback, which cleans up).

## LISTEN / NOTIFY

`postgres.listen(pool, "channel_name")` returns a `Channel` that
delivers a `postgres.Notification` record for every NOTIFY on that
PostgreSQL channel: its `channel`, its `payload`, and `pid`, the process
id of the session that sent it. The listener holds one of the pool's
connections until its channel is closed: while it is open, the pool has
one connection fewer for statements (with `max_pool_size` 1, a
statement waits the pool's 30 seconds and is `Timeout`).
`postgres.notify(target, channel, payload)` sends a single NOTIFY.

A channel name is used as it is written: its case is kept (`"Orders"`
and `"orders"` are two channels), and it may be a keyword or hold any
character but NUL. Both functions take a name of 1 to 63 bytes, the
most PostgreSQL keeps, without a NUL, and refuse any other before
anything is sent: `Err(Other("postgres.listen: a channel name cannot be
empty"))`, `"... is longer than 63 bytes, the longest PostgreSQL
keeps"`, `"... a channel name cannot hold a NUL character"`.

## Example

```text
import env
import json
import postgres
import time

type User { id: Int, name: String, nick: Option(String), born: time.Date }

type Id { id: Int }

-- In one transaction: a table of its own, two users in it, one of them
-- changed, and the users read back.
fn users(db: postgres.PgPool) -> Result(List(User), postgres.PostgresError) {
  postgres.transact(db) { tx ->
    let _ = postgres.execute(
      tx,
      "CREATE TEMP TABLE users (id int4 PRIMARY KEY, name text NOT NULL, nick text, born date) ON COMMIT DROP",
      (),
    )?
    let ids = postgres.query(
      tx,
      "INSERT INTO users VALUES ($1, $2, $3, $4), ($5, $6, $7, $8) RETURNING id",
      (11, "alice", Some("al"), "1990-05-17", 12, "bob", None, "2001-02-03"),
      Id,
    )?
    println("inserted: {ids}")
    let n = postgres.execute(tx, "UPDATE users SET nick = $1 WHERE id = $2", ("bobby", 12))?
    println("updated: {n}")
    let rows = postgres.query(tx, "SELECT id, born FROM users WHERE id = $1", (11,), json.Value)?
    println("as json: {json.stringify(rows)}")
    postgres.query(tx, "SELECT id, name, nick, born FROM users WHERE id > $1 ORDER BY id", (10,), User)
  }
}

fn main() {
  let url = match env.get("DATABASE_URL") {
    Some(url) -> url
    None -> "postgres://localhost/app"
  }
  match postgres.connect(url) {
    Ok(db) -> {
      match users(db) {
        Ok(found) -> println("users: {found}")
        Err(postgres.TxnAborted) -> println("rolled back (transaction aborted, retry)")
        Err(e) -> println("rolled back: {e.message()}")
      }
      postgres.close(db)
    }
    Err(e) -> println("connect: {e.message()}")
  }
}
```

It prints:

```text
inserted: [Id {id: 11}, Id {id: 12}]
updated: 1
as json: [{"born":"1990-05-17","id":11}]
users: [User {id: 11, name: alice, nick: Some(al), born: 1990-05-17}, User {id: 12, name: bob, nick: Some(bobby), born: 2001-02-03}]
```

## Notes

- `uuid.v7` produces a time-ordered UUID suitable for use as a primary
  key; collisions within the same millisecond are disambiguated with
  random bits per RFC 9562.
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
| `prefer` | opportunistic | no | no |
| `require` | yes | no | no (libpq-compatible encryption-only) |
| `verify-ca` | yes | yes | no |
| `verify-full` | yes | yes | yes |

**New default (silt 0.11+)**: a connection URL that omits `sslmode=`
entirely resolves to `verify-full`. This is a deliberate deviation
from libpq's historical `prefer`, which silently downgraded to
plaintext on handshake failure. A silt program whose URL is just
`postgres://user@host/db` now requires a valid, hostname-matching TLS
cert — or an explicit opt-out via `?sslmode=disable`.

Every mode that requires encryption (the default, `require`,
`verify-ca`, `verify-full`) refuses a server without TLS: the connect
is `Tls(msg)`, and nothing more is sent to the server. Only `prefer`
goes on without encryption.

`sslmode` is read the same way in both forms of the connection string,
a `postgres://` URL's query and `key=value` pairs. A host that is a
Unix socket (a path, as `host=/var/run/postgresql`) never uses TLS,
whatever `sslmode` says, as in libpq: a connection string whose hosts
are all sockets connects without it.

If silt was built **without** the `postgres-tls` feature, a URL that
defaults to `verify-full` (or any `require` / `verify-*`) returns a
clear `Tls(msg)` at connect time rather than silently using
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
bag for tunables that don't belong in the URL. A key that is no option
below, or a value the option cannot have, is `Err(Connect(msg))`, and
nothing is sent to the server.

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
`PostgresError` variants (e.g. `Query(msg, sqlstate)` carries the five-
character SQLSTATE code so constraint-specific branches can match on
`"23505"` for unique violations, `"23503"` for FK violations, etc.).
If you need the full un-redacted text for diagnostics, log it on the
Rust side (e.g. via a custom embedder) — the silt-side `PostgresError` value
is intentionally scrubbed.

## PostgresError variants

All fallible `postgres.*` calls return `Result(T, PostgresError)`. The
variants (declared in silt's stdlib, no user `pg.silt` entries
needed) are:

| Variant | Fields | Raised for |
| --- | --- | --- |
| `Connect(msg)` | `String` | A connect that failed, URL parse, connect options, SQLSTATE class `08` |
| `Tls(msg)` | `String` | TLS handshake / a server without TLS when `sslmode` requires TLS / cert read / connector build |
| `AuthFailed(msg)` | `String` | SQLSTATE class `28`: a role or password the server refuses |
| `Query(msg, sqlstate)` | `String, String` | Any other DbError; `sqlstate` is the 5-char code |
| `TypeMismatch(col, expected, actual)` | `String, String, String` | A column whose type, or a cell whose value, does not fit its field (see [Rows](#rows)) |
| `NoSuchColumn(col)` | `String` | A field with no column in the result; SQLSTATE `42703` (undefined_column), the column the server names |
| `Closed` | — | Connection dropped, or a transaction used after it ended |
| `Timeout` | — | SQLSTATE `57014`, a server that does not answer a connect, no free connection in the pool's 30 s |
| `TxnAborted` | — | SQLSTATE `25P02` (in_failed_sql_transaction) |
| `Other(msg)` | `String` | Catch-all for shapes we can't classify |

`PostgresError` implements the stdlib `Error` trait, so if you don't want
to pattern-match on variants you can always call `err.message()` to
get a formatted user-friendly string.
