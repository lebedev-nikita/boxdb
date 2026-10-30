# BoxDB for TypeScript

An asynchronous Node.js and Bun SQLite client backed by the BoxDB Rust core.
Each connection owns a native worker: SQL execution and lock waits leave the
JavaScript event loop free. Binary SQLite files, readable YAML storage, extended
column types and built-in SQL extensions use the same core.

## Install

```sh
npm install @boxdb/client
```

Requires Node.js 22.18+ or Bun on macOS ARM64 (macOS 11+) or Linux x64 or ARM64
with glibc 2.35+ (Ubuntu 22.04+ or 64-bit Raspberry Pi OS Bookworm+).
All three native addons are bundled in the npm package;
installation needs no Rust compiler, install scripts or extra downloads.
Alpine/musl, Windows, Intel macOS and 32-bit ARM are not supported by this npm
release. This client does not run in browsers.

## Build from source

Requires Node.js 22.18+, Bun, Rust, native build tools and libclang; see the
[repository README](../../README.md). From the repository root:

```sh
bun install --frozen-lockfile
bun run build
bun run check
bun run test
```

`just ts-build` and `just ts-check ts-test` also build and verify all packages.
For an optimized addon, run `bun run --cwd packages/typescript build:release`,
then build the adapter packages. Builds target the current OS and architecture;
the npm release builds are described in the [release guide](https://github.com/lebedev-nikita/boxdb/blob/main/docs/npm-release.md).

## Queries

Query rows are `unknown[]`. Validate them before use with a schema library such
as [Zod](https://zod.dev/basics):

```sh
npm install zod
```

```ts
import { BoxDB, Changeset } from "@boxdb/client";
import { z } from "zod";

const User = z.object({ id: z.bigint(), name: z.string() });

await using db = await BoxDB.open("data.db", { busyTimeoutMs: 5000 });
await db.exec("CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, name TEXT)");
const inserted = await db.query`INSERT INTO users(name) VALUES(${"Ada"})`.run();

const rows = User.array().parse(
  await db.query`
  SELECT * FROM ${db.identifier("users")}
  ${db.fragment`WHERE id = ${inserted.lastInsertRowid}`}
  LIMIT ${100}
  `,
); // { id: bigint; name: string }[] after runtime validation
```

Scalar interpolations become bound parameters; do not put SQL quotes around
them. `identifier("main", "users")` quotes separate components;
`identifier("user.name")` quotes one name containing a dot. Fragments snapshot
parameters and compose without executing SQL.

Queries execute once, lazily. `await query` and `query.execute({ signal })`
return row objects as `unknown[]`. `query.get({ signal })` returns the first row
or `undefined`, typed as `unknown`. `query.values({ signal })` returns positional
rows as `unknown[]`, preserving duplicate column names. Validate each result
against the shape selected by the query. `query.run({ signal })` returns
`{ changes, lastInsertRowid }` and requires a statement without returned columns.
These methods share the execution result
or error; the first execution chooses its signal. `query.compile()` returns a
copy of `{ sql, parameters }` without executing.

For SQL from an adapter or migration tool:

```ts
const result = await db.execute("SELECT name FROM users WHERE id=:id", { id: 1 });
// { rows, columns, changes, lastInsertRowid }
const names = User.pick({ name: true }).array().parse(result.rows);
const positional = await db.execute("SELECT 1 AS x, 2 AS x", [], { raw: true });
await db.exec("CREATE INDEX IF NOT EXISTS users_name ON users(name);");
```

`execute` accepts one statement and positional or named bindings and returns
`rows: unknown[]` in both object and raw modes. `exec` accepts scripts without
parameters. Queries accept no result type parameter; derive application types from
validated data. The client has no dependency on a validation library. Explicit
prepared statements and streaming are not part of this API.

## Transactions and cancellation

Use TypeScript 5.2+; transpile `await using` for runtimes without native syntax
support (including Node.js 22).

```ts
await using tx = await db.begin({ mode: "immediate", signal });
await tx.query`INSERT INTO users(name) VALUES(${"Grace"})`;
{
  await using nested = await tx.begin();
  await nested.query`INSERT INTO users(name) VALUES(${"temporary"})`;
  // Exiting without commit rolls back this savepoint.
}
await tx.commit();
```

Commit is explicit. Disposal and `rollback()` discard unfinished work. Root
transactions use `immediate` by default, acquiring the writer before transaction
work starts. Explicit `deferred` and `exclusive` modes remain available.

Use `db.begin({ readOnly: true })` for a deferred transaction that rejects SQL
writes with `BOXDB_READONLY`. In binary WAL databases it can read a consistent
snapshot while another connection writes. The native guard covers queries and
scripts, including attempts to disable `query_only`; the previous connection
setting is restored after the transaction. Read-only connections automatically
start read-only transactions. Combining read-only with an immediate or exclusive
mode throws `TypeError`.

```ts
await using tx = await db.begin({ readOnly: true });
const total = z.object({ count: z.bigint() }).parse(
  await tx.query`SELECT count(*) AS count FROM users`.get(),
);
await tx.commit();
```

Nested transactions use savepoints and inherit the outer read-only restriction
and deadline. They have no independent mode. Use the innermost transaction
handle for work inside a transaction; parent-handle SQL fails while a nested
handle owns the connection. Root requests, including `db.begin()`, wait for the
entire transaction to finish. Root SQL then executes independently in autocommit,
in submission order. SQL transaction-control statements must use handles too.

Transactions have a five-second lifetime by default, starting after `BEGIN`
succeeds. Set `transactionTimeoutMs` on the client or override it with `timeoutMs`
on `db.begin()`; both accept integers from 1 to 2147483647 milliseconds.
Savepoints inherit the outer deadline and do not extend it. Expiry interrupts
running SQL, cancels queued transaction work and rolls back. Waiting root
requests resume only after cleanup; further queries or commits through the
expired handle fail with `BOXDB_TRANSACTION_TIMEOUT`.

```ts
await using db = await BoxDB.open(":memory:", { transactionTimeoutMs: 10_000 });
await using tx = await db.begin({ timeoutMs: 2_000 });
const pending = db.query`SELECT 1 AS answer`.execute();
await tx.commit();
await pending; // Executes outside the completed transaction.
```

Awaiting a root request before finishing your own transaction waits until its
timeout rolls it back. Use `tx` for operations needed before commit or rollback.
The deadline starts cancellation; freeing the connection still requires native
SQL interruption and rollback to complete.

`tx.active` reports whether its transaction still exists. A failed commit leaves
an active SQLite transaction available for repair or retry. If SQLite itself
rolls back a transaction after an interrupt, further work through that handle
fails instead of escaping into autocommit.

```ts
const controller = new AbortController();
const pending = db.query`SELECT * FROM users`.execute({ signal: controller.signal });
controller.abort(new Error("request no longer needed"));
await pending; // Rejects with the cancellation reason if cancellation wins.
```

Signals cancel queued requests before execution and interrupt running SQL or
lock waits. A transaction inherits its signal; aborting an idle transaction also
rolls it back. Rollback cleanup cannot be cancelled by that signal. Cancellation
cannot undo a statement that has already committed; use a transaction for atomic
multi-statement work.

`close()` rejects new root requests, drains accepted work and waits for active
transactions to finish. Their handles can still commit or roll back.
`close({ mode: "abort" })` cancels pending and running work, rolls back active
transactions and waits for cleanup. An existing draining close can be escalated
to abort. `await using db` performs a draining close.

## Change history

Transactions can record their row changes and run commit triggers; see
[change history](../../docs/history.md) for the SQL side.

```ts
await using tx = await db.begin({ context: { label: "Rename task" } });
await tx.query`UPDATE tasks SET title = ${"New title"} WHERE id = ${3n}`;
await tx.commit(); // A BEFORE COMMIT trigger can store boxdb_changeset().

const row = z.object({ changes: z.instanceof(Changeset) }).parse(
  await db.query`SELECT changes FROM history ORDER BY id DESC LIMIT 1`.get(),
);
const report = await db.apply(row.changes, { invert: true }); // Undo in its own transaction.
```

`begin({ context })` stores values readable through `boxdb_context(key)` for
the transaction's lifetime; `begin({ record: true })` records changes so
`tx.changeset()` returns a `Changeset` or `null` before commit. `tx.apply()`
applies a changeset inside the transaction and `db.apply()` in a new immediate
transaction; both accept `invert` and `onConflict` (`abort`, `skip` or
`replace`) and return `{ applied, skipped, conflicts }`. CHANGESET columns
return `Changeset` instances, which also bind as parameters; `changeset.bytes`
holds the encoded changeset.

## Options, values and diagnostics

`BoxDB.open(path, options)` accepts `format: "binary" | "readable"`,
`readOnly`, `busyTimeoutMs`, `transactionTimeoutMs`, `maxPendingQueries`, and `signal`.
Existing contents determine the format. A read-only readable connection reads
an opening snapshot without taking the writer's lock.

Inputs accept `null`, strings, finite numbers, bigint, boolean and `Uint8Array`.
Bigint parameters accept the combined INT64/UINT64 range (`-2^63` through
`2^64 - 1`); positive values above INT64_MAX bind as UINT64. Use
`CAST(? AS UINT8/16/32)` to select a narrower unsigned width.
Safe integer numbers and booleans bind as integers; fractional numbers bind as
REAL. Serialize objects explicitly. Every signed or unsigned integer cell, write
count and row ID returns as bigint; REAL remains number. Convert bigint
explicitly when serializing JSON. Unsafe integer inputs throw instead of rounding.

Results retain native decimal values as exact text. DECIMAL arithmetic, comparisons
and ordering run in the engine; NUMERIC is not supported. UUID and temporal values
return canonical strings: INSTANT uses UTC, TIME has no date or time zone,
DATETIME has no time zone, and temporal
strings preserve microsecond precision. JSON and arrays remain opaque strings;
SQL JSON processing is disabled. Blobs are `Buffer` values. See the
[native decimal guide](../../docs/native-decimal.md) and
[temporal types](../../docs/temporal.md).

`BoxDBError` preserves `code`, `resultCode`, `extendedCode` and `cause`. Engine
failures have `BOXDB_*` names, such as `BOXDB_CONSTRAINT_UNIQUE`. Validation,
closed connections and native infrastructure failures have separate categories;
an exhausted queue uses `BOXDB_QUEUE_FULL`. `findBoxDBError(error)` follows causes
and aggregate members without looping on cycles. Invalid arguments throw
`TypeError` or `RangeError`. See the [client error contract](../../docs/client-errors.md).

The optional queue limit bounds waiting operations, including root requests
waiting for a transaction; cleanup always gets through.
`db.diagnostics` reports pending/active/completed operations and the last queue
wait and execution durations in milliseconds. A duration covers a complete
scheduled operation, including preparation and native execution.

## File diagnostics and recovery

For existing files, use `await BoxDB.checkFile(path, "full")`,
`await BoxDB.repairFile(source, destination)` or
`await BoxDB.recoverFile(source, destination, "binary")` independently of a live
client. Reports distinguish invalid data from incomplete checks. Recovery can
lose data even when its output passes validation; see
[diagnostics and recovery](../../docs/recovery.md).

## SQL migrations

```ts
import { MigrationSet } from "@boxdb/client";

const migrations = await MigrationSet.read("db/migrations");
await migrations.up(db);
```

Use `MigrationSet.fromFiles([{ filename, sql }])` to bundle SQL in the application.
The same set exposes `status(db)` and `down(db)`; execution methods accept
`{ signal }`. These methods use the CLI's format and history, execute on the
native worker and wait for active transactions on the root client. See
[migration sources, cancellation and rollback](../../docs/migrations.md#migrations-from-application-code).

## Integrations

Adapters are independent npm packages with their libraries as peer dependencies.
They borrow the client and do not close it:

- [Drizzle adapter](../drizzle-adapter/README.md): asynchronous queries, transactions,
  batches and migrations.
- [Better Auth adapter](../better-auth-adapter/README.md): direct database adapter,
  schema generation and native atomic credential/counter operations.
- [db0 adapter](../db0-adapter/README.md): a SQLite connector accepting a database
  or transaction handle.

Every connection includes decimal arithmetic, Unicode text functions, regular
expressions, sqlite-vec, FTS5 and R*Tree; see the
[SQL extension guide](../../docs/extensions.md). Virtual tables require memory
or binary storage.
