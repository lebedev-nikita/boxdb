# BoxDB for TypeScript

A synchronous Node.js and Bun client backed by the BoxDB Rust library through
Node-API. SQL execution, extended type validation, SQLite files and readable
YAML storage all use the same Rust implementation.

## Build from this checkout

Requires Node.js 22.18+, Bun, Rust, native build tools and libclang, as described
in the [repository README](../../README.md). No prebuilt npm binaries are
published yet. Builds produce a native addon for the current OS and architecture;
this package does not run in browsers.

From the repository root:

```sh
just ts-build
just ts-check ts-test
```

Without Just, run `bun install --cwd packages/typescript --frozen-lockfile`, then
`bun run --cwd packages/typescript build`. For an optimized native addon, run
`bun run --cwd packages/typescript build:release`.

After building, add the local package to your application with
`bun add /absolute/path/to/boxdb/packages/typescript` and import `@boxdb/client`.
Rebuild the addon when changing the Rust core or moving to another platform.

## Queries

```ts
import { Database } from "@boxdb/client";

using db = new Database("data.db");
db.exec("CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, name TEXT)");
db.run("INSERT INTO users(name) VALUES (?)", ["Ada"]);

const result = db.query("SELECT * FROM users WHERE name = :name", { name: "Ada" });
console.log(result.rows);
```

`query` executes one statement and returns `rows`, `columns`, `changes` and
`lastInsertRowid`. `all`, `get` and `run` are shortcuts for rows, the first row
(or `undefined`), and write metadata. `exec` executes SQL scripts without bound
parameters. Use `close()` or `using` to release the connection (`using` requires
runtime support or TypeScript compilation).

For readable storage, pass `{ format: "readable" }` to the constructor. Existing
file contents determine the format when reopening, just as in Rust.

## Values and errors

Results use SQLite storage values: `null`, `number`, `string`, `Buffer`, and
`bigint` when safe-integer mode is enabled. JSON and arrays remain JSON strings;
NUMERIC remains exact decimal text; INSTANT remains UTC text with nanoseconds;
BOOLEAN returns 0 or 1. BoxDB still validates extended values on reads and writes.
Input parameters also accept `Uint8Array` and boolean (bound as 0 or 1).
Serialize objects explicitly, for example with `JSON.stringify`.

Integer results outside JavaScript's safe range throw `RangeError` instead of
silently losing precision. Use `new Database(path, { safeIntegers: true })`,
`db.defaultSafeIntegers()` for future statements, or `statement.safeIntegers()`
to read all SQLite integers as `bigint`. Bind large integers as `bigint`; SQLite
accepts signed 64-bit integers. A large `lastInsertRowid` is always preserved as
`bigint`, even outside safe-integer mode. SQLite REAL values remain numbers.

Result generics, such as `db.all<User>(sql)`, describe the expected row shape;
they do not validate that shape. Native failures throw `BoxDBError` with
`code: "BOXDB_ERROR"` and the original `cause`. Argument and precision errors
throw `TypeError` or `RangeError`. SQLite extended error codes are not exposed yet.

## Prepared statements and transactions

```ts
const insert = db.prepare("INSERT INTO users(name) VALUES (?)");
const insertMany = db.transaction((names: string[]) => {
  for (const name of names) insert.run(name);
});
insertMany.immediate(["Grace", "Linus"]);
```

Prepared statements accept variadic parameters, arrays or named parameter
objects. They expose `run`, `all`, `get`, `iterate`, `raw`, `safeIntegers`,
`reader`, `readonly`, and `source`. `raw()` returns arrays instead of row
objects. Queries use Rust's SQLite statement cache; `get` and `iterate` currently
materialize the complete result. This is a compatibility subset, not a complete
replacement for better-sqlite3 (no extensions, user functions or backup API).

Transactions commit on return and roll back on exceptions. Nested transactions
use savepoints. Callbacks must be synchronous; async callbacks and returned
thenables are rejected. Long queries block the current JavaScript thread.

## Drizzle

Install the optional peer `drizzle-orm` 0.45.x. The BoxDB adapter reuses Drizzle's
synchronous SQLite session without loading or requiring better-sqlite3:

```ts
import { drizzle } from "@boxdb/client/drizzle";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

const users = sqliteTable("users", {
  id: integer().primaryKey({ autoIncrement: true }),
  name: text().notNull(),
});
const orm = drizzle(db, { schema: { users } });
orm.insert(users).values({ name: "Ada" }).run();
const rows = orm.select().from(users).all();
```

The official `drizzle-orm/better-sqlite3/migrator` works with this adapter.
BoxDB accepts `INTEGER` as signed 64-bit and the migration journal's `SERIAL`
as an SQL alias for SQLite INTEGER. Other BoxDB typing rules still apply:
unknown SQLite column types are not automatically accepted in STRICT tables,
and NUMERIC has BoxDB's exact text semantics. Use TEXT for application strings;
use custom Drizzle column types for BoxDB-specific declarations.

## Kysely

Kysely's built-in dialect accepts the client without a custom adapter:

```ts
import { Kysely, SqliteDialect } from "kysely";

const kysely = new Kysely<MySchema>({
  dialect: new SqliteDialect({ database: db }),
});
```

`kysely.destroy()` closes the underlying connection. Integration tests cover
Kysely 0.29.6, Drizzle 0.45.4, migration replay, transactions and type conversions.
