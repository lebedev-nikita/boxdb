# BoxDB for TypeScript

An asynchronous Node.js and Bun SQLite client backed by the BoxDB Rust core.
Each connection owns a native worker: SQL execution and lock waits leave the
JavaScript event loop free. Binary SQLite files, readable YAML storage, extended
column types and built-in SQL extensions use the same core.

## Build

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
prebuilt npm binaries are not published yet. This client does not run in browsers.

## Queries

```ts
import { Database } from "@boxdb/client";

await using db = await Database.open("data.db", { busyTimeoutMs: 5000 });
await db.exec("CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, name TEXT)");
const inserted = await db.query`INSERT INTO users(name) VALUES(${"Ada"})`.run();

const rows = await db.query<{ id: bigint; name: string }>`
  SELECT * FROM ${db.identifier("users")}
  ${db.fragment`WHERE id = ${inserted.lastInsertRowid}`}
  LIMIT ${100}
`;
```

Scalar interpolations become bound parameters; do not put SQL quotes around
them. `identifier("main", "users")` quotes separate components;
`identifier("user.name")` quotes one name containing a dot. Fragments snapshot
parameters and compose without executing SQL.

Queries execute once, lazily. `await query` and `query.execute({ signal })`
return row objects. `query.get({ signal })` returns the first row or `undefined`;
`query.values({ signal })` returns positional rows, preserving duplicate column
names. `query.run({ signal })` returns `{ changes, lastInsertRowid }` and requires
a statement without returned columns. These methods share the execution result
or error; the first execution chooses its signal. `query.compile()` returns a
copy of `{ sql, parameters }` without executing.

For SQL from an adapter or migration tool:

```ts
const result = await db.execute("SELECT name FROM users WHERE id=:id", { id: 1 });
// { rows, columns, changes, lastInsertRowid }
const positional = await db.execute("SELECT 1 AS x, 2 AS x", [], { raw: true });
await db.exec("CREATE INDEX IF NOT EXISTS users_name ON users(name);");
```

`execute` accepts one statement and positional or named bindings. `exec` accepts
scripts without parameters. Query generics describe row types; they do not
validate application schemas. Explicit prepared statements and streaming are
not part of this API.

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
transactions support `deferred` (default), `immediate`, and `exclusive`; nested
transactions use savepoints and have no mode. Use the innermost transaction
handle: root SQL and parent-handle SQL fail promptly while another handle owns
the connection. SQL transaction-control statements must use handles too.

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

## Options, values and diagnostics

`Database.open(path, options)` accepts `format: "binary" | "readable"`,
`readOnly`, `busyTimeoutMs`, `maxPendingQueries`, and `signal`.
Existing contents determine the format. A read-only readable connection reads
an opening snapshot without taking the writer's lock.

Inputs accept `null`, strings, finite numbers, signed 64-bit bigint, boolean and
`Uint8Array`. Safe integer numbers and booleans bind as integers; fractional
numbers bind as REAL. Serialize objects explicitly. Every integer cell, write
count and row ID returns as bigint; REAL remains number. Convert bigint
explicitly when serializing JSON. Unsafe integer inputs throw instead of rounding.

Results retain native decimal values as exact text. DECIMAL arithmetic, comparisons
and ordering run in the engine; NUMERIC is not supported. JSON and arrays remain
strings, INSTANT is UTC text with nanoseconds, and blobs are `Buffer` values. See
the [native decimal guide](../../docs/native-decimal.md) for exact arithmetic.

`BoxDBError` preserves `code`, `resultCode`, `extendedCode` and `cause`. Engine
failures have `BOXDB_*` names, such as `BOXDB_CONSTRAINT_UNIQUE`. Validation,
closed connections and native infrastructure failures have separate categories;
an exhausted queue uses `BOXDB_QUEUE_FULL`. `findBoxDBError(error)` follows causes
and aggregate members without looping on cycles. Invalid arguments throw
`TypeError` or `RangeError`. See the [client error contract](../../docs/client-errors.md).

The optional queue limit bounds waiting operations; cleanup always gets through.
`db.diagnostics` reports pending/active/completed operations and the last queue
wait and execution durations in milliseconds. A duration covers a complete
scheduled operation, including preparation and native execution.

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
