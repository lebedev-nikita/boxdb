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

## Postgres.js-style adapter

```ts
import { Database } from "@boxdb/client";
import { adapter } from "@boxdb/client/adapters/postgres";

using db = new Database("data.db");
const sql = adapter(db);
const name = "Ada";

const users = await sql<{ id: bigint; name: string }[]>`
  SELECT id, name FROM users WHERE name = ${name}
`;
```

Interpolated values are bound parameters. Write `${name}` without SQL quotes.
For dynamic identifiers, use `sql("users")` or `sql("users.name")`; each dotted
component is quoted separately. Compose queries with nested tagged fragments:

```ts
const condition = sql`WHERE name = ${name}`;
const rows = await sql`SELECT * FROM ${sql("users")} ${condition}`;
console.log(rows.count, rows.columns);
```

Queries are lazy: `await`, `.then()`, `.catch()` or `.finally()` starts execution;
`.execute()` starts it immediately and returns a Promise. Reusing a query object
reuses its result or error. `.values()` returns a lazy view with positional array
rows, preserving column order and duplicate column names, without repeating the
query. Results are arrays with nonenumerable `count` and `columns` properties;
`count` is the returned row count for queries or affected row count for writes,
and `columns` contains `{ name }` objects.

This adapter uses BoxDB's SQLite SQL and value representations. Execution still
runs synchronously on the calling thread when the query starts. The supplied
`db` owns the connection and its synchronous transaction interface.

## Values and errors

Integer results always use `bigint`, including IDs, BOOLEAN storage values,
write counts and `lastInsertRowid`. REAL values use `number`; other storage
values use `null`, `string` and `Buffer`. JSON and arrays remain JSON strings;
DECIMAL/NUMERIC remains exact text. Bind decimal strings for exact amounts.
INSTANT remains canonical UTC text. The `safeIntegers` option and mode-switching
methods are no longer supported.

Parameters accept signed int64 `bigint`, safe integer `number`, finite real
`number`, boolean, `Uint8Array`, string and null. Safe integer numbers and
booleans bind as integers. Unsafe integer numbers and bigint values outside
int64 throw `RangeError`; invalid types and non-finite numbers throw `TypeError`.
Serialize objects explicitly. JSON serialization of bigint results requires
an application-defined conversion, such as a replacer that writes decimal strings.
Result generics describe the expected row shape; they do not validate it.

Database failures throw `BoxDBError` with a symbolic `BOXDB_*` code, optional
numeric `resultCode` and `extendedCode`, and the native exception as `cause`.
For example, a UNIQUE violation has `BOXDB_CONSTRAINT_UNIQUE`, `19` and `2067`.
BoxDB validation, I/O, closed and unavailable connections, and internal failures
have distinct categories; failures outside the engine have no numeric codes.
See the [error contract](../../docs/client-errors.md).

ORMs may wrap the error. Use `findBoxDBError` to inspect direct errors, `cause`
chains and `AggregateError` members:

```ts
import { findBoxDBError } from "@boxdb/client";

try {
  db.run("INSERT INTO users(name) VALUES(?)", ["Ada"]);
} catch (error) {
  const failure = findBoxDBError(error);
  if (failure?.code === "BOXDB_CONSTRAINT_UNIQUE") console.log("Name already exists");
  else throw error;
}
```

## Prepared statements and transactions

```ts
const insert = db.prepare("INSERT INTO users(name) VALUES (?)");
const insertMany = db.transaction((names: string[]) => {
  for (const name of names) insert.run(name);
});
insertMany.immediate(["Grace", "Linus"]);
```

Prepared statements accept variadic parameters, arrays or named parameter
objects. They expose `run`, `all`, `get`, `iterate`, `raw`,
`reader`, `readonly`, and `source`. `raw()` returns arrays instead of row
objects. Queries use Rust's SQLite statement cache; `get` and `iterate` currently
materialize the complete result. This is a compatibility subset, not a complete
replacement for better-sqlite3 (no dynamic extension loading, user functions or backup API).

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
  id: integer().$type<bigint>().primaryKey({ autoIncrement: true }),
  name: text().notNull(),
});
const orm = drizzle(db, { schema: { users } });
orm.insert(users).values({ name: "Ada" }).run();
const rows = orm.select().from(users).all();
```

Use `orm.$migrate({ migrationsFolder: "./drizzle" })` to run Drizzle migrations.
This uses Drizzle's migration engine without the better-sqlite3 result types,
which assume numeric write counts. Integer columns should use `$type<bigint>()`
to match their runtime values. Boolean column mapping is supported; for date
mapping, use a custom decoder that accepts bigint or BoxDB's INSTANT text.
BoxDB accepts `INTEGER` as signed 64-bit and the migration journal's `SERIAL`
as an SQL alias for SQLite INTEGER. Other BoxDB typing rules still apply:
unknown SQLite column types are not automatically accepted in STRICT tables,
and DECIMAL/NUMERIC has BoxDB's exact text semantics. Use TEXT for application strings;
use custom Drizzle column types for BoxDB-specific declarations.

## Built-in extensions

Every connection includes decimal arithmetic, Unicode text functions, regular
expressions, sqlite-vec, FTS5 and R*Tree:

```ts
const db = new Database();
const result = db.get("SELECT decimal_add(?,?) AS amount, text_lower(?) AS name", [
  "0.1", "0.2", "ПРИВЕТ",
]);
// { amount: "0.3", name: "привет" }
```

Bind decimals as strings and vectors as JSON strings or packed `Buffer` values.
Virtual tables require an in-memory or binary database; readable YAML does not
support them. See the [SQL extension guide](../../docs/extensions.md) for examples.

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
