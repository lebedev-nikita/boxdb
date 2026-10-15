# BoxDB adapter for Drizzle

An asynchronous SQLite driver for Drizzle 0.45.x. Install `@boxdb/client`,
`@boxdb/drizzle-adapter`, and `drizzle-orm`.

```sh
npm install @boxdb/client @boxdb/drizzle-adapter drizzle-orm
```

```ts
import { BoxDB } from "@boxdb/client";
import { createDrizzleDatabase, migrate } from "@boxdb/drizzle-adapter";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

const users = sqliteTable("users", {
  id: integer().primaryKey({ autoIncrement: true }).$type<bigint>(),
  name: text().notNull(),
});
await using client = await BoxDB.open("data.db");
const db = createDrizzleDatabase(client, { schema: { users } });
await migrate(db, { migrationsFolder: "./drizzle" });
const rows = await db.insert(users).values({ name: "Ada" }).returning();
```

The driver supports relational queries, joins, prepared Drizzle queries,
`RETURNING`, and atomic `batch`. Drizzle transaction callbacks use BoxDB
transaction handles internally; nested callbacks use savepoints. You can also
pass an existing BoxDB transaction to `createDrizzleDatabase`.

Write metadata and integer cells remain bigint. Use `integer().$type<bigint>()`
for Drizzle integer columns. Boolean columns map to boolean and timestamp
columns map to Date, without converting unrelated integer cells. Migrations
use an INTEGER journal and apply SQL and journal entries in one transaction.

The adapter borrows `$client`; close it yourself or use `await using`.
For cancellation across Drizzle work, open a BoxDB transaction with a signal
and create the Drizzle database from its handle. Base-client ownership rules
apply when mixing Drizzle and direct SQL.

## BoxDB columns

Declare extended types with the adapter's column builders:

```ts
import { decimal, instant, json, uuid } from "@boxdb/drizzle-adapter";

const invoices = sqliteTable("invoices", {
  id: uuid().primaryKey(),
  amount: decimal({ precision: 20, scale: 2 }).notNull(),
  createdAt: instant("created_at").notNull(),
  details: json().$type<{ reference: string }>(),
});
```

`decimal`, `uuid`, `instant`, `date`, `dateTime` and `interval` use strings for
parameters and results. BoxDB validates and normalizes values in the declared
SQL columns; exact decimals and temporal microseconds survive without conversion
to JavaScript Number or Date. Builders expose the native SQL declarations for
schema generation. Create matching columns through BoxDB migrations.

`json` serializes and parses complete documents in the client. `$type<T>()`
describes the application type; it does not validate that shape at runtime.
SQL NULL remains `null`. JSON/JSONB SQL inspection and editing are unsupported.
