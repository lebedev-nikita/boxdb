# BoxDB client for Effect SQL

Install `@boxdb/client`, `@boxdb/effect-adapter`, and `effect` 4.1+.

```sh
npm install @boxdb/client @boxdb/effect-adapter effect
```

```ts
import { BoxDB } from "@boxdb/client";
import { layer } from "@boxdb/effect-adapter";
import { Effect, Layer } from "effect";
import { SqlClient } from "effect/sql";

const BoxDBLive = Layer.unwrap(
  Effect.map(
    Effect.acquireRelease(
      Effect.promise(() => BoxDB.open("data.db")),
      (client) => Effect.promise(() => client.close()),
    ),
    (client) => layer({ client }),
  ),
);

const program = Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  yield* sql`CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, name TEXT)`;
  yield* sql.withTransaction(sql`INSERT INTO users ${sql.insert({ name: "Ada" })}`);
  return yield* sql`SELECT * FROM users`;
});

Effect.runPromise(Effect.provide(program, BoxDBLive)).then(console.log);
```

`layer({ client })` provides the generic `SqlClient` and `BoxDBSqlClient`,
whose `client` field is the borrowed BoxDB connection. The adapter never closes
it; own its lifetime as above. `make(config)` builds the client without a layer
and requires `Reactivity`. `transformQueryNames`, `transformResultNames` and
`spanAttributes` work as in other Effect SQL drivers.

Statements run on the client's native worker. Interrupting a fiber cancels its
queued statement or interrupts running SQL. Integer cells are always `bigint`;
`SqlClient.SafeIntegers` has no effect. Values bind as described in the
[client guide](../typescript/README.md#options-values-and-diagnostics).
`stream` reads the complete result before emitting rows, and `updateValues` is
not supported.

## Transactions

BoxDB rejects transaction-control SQL, so `withTransaction` uses BoxDB
transaction handles: top-level calls begin a transaction and nested calls
create savepoints. Waiting for another transaction to finish is interruptible.
A failed commit is rolled back before the error is returned.
`SqlClient.BeginTransaction` is ignored; provide `TransactionOptions` instead:

```ts
import { TransactionOptions } from "@boxdb/effect-adapter";

sql.withTransaction(rename).pipe(
  Effect.provideService(TransactionOptions, { context: { label: "Rename task" } }),
);
```

`TransactionOptions` accepts the client's `begin` options except `signal`:
`mode`, `readOnly`, `timeoutMs`, `context` and `record`. It applies to
top-level transactions only. Read recorded changes with
`SELECT boxdb_changeset()`; see [change history](../../docs/history.md).

## Errors and migrations

Failures are `SqlError` values whose reason keeps the `BoxDBError` as `cause`;
`findBoxDBError(error)` returns it. `BOXDB_CONSTRAINT_UNIQUE` maps to
`UniqueViolation` with the constraint from the engine message, other constraint
failures to `ConstraintError`, busy and locked databases to `LockTimeoutError`,
`BOXDB_TRANSACTION_TIMEOUT` to `StatementTimeoutError`, and closed or unusable
connections to `ConnectionError`. Other failures are `UnknownError`.

`Migrator` from `effect/sql` runs on this client. BoxDB's own
[`MigrationSet`](../../docs/migrations.md#migrations-from-application-code) also
works on the borrowed client.
