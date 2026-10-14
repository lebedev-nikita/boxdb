# BoxDB adapter for Drizzle

An asynchronous SQLite driver for Drizzle 0.45.x. Install `@boxdb/client`,
`@boxdb/drizzle-adapter`, and `drizzle-orm`.

```ts
import { Database } from "@boxdb/client";
import { createDrizzleDatabase, migrate } from "@boxdb/drizzle-adapter";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

const users = sqliteTable("users", {
  id: integer().primaryKey({ autoIncrement: true }).$type<bigint>(),
  name: text().notNull(),
});
await using client = await Database.open("data.db");
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
