# BoxDB connector for db0

Install `@boxdb/client`, `@boxdb/db0-adapter`, and `db0` 0.4.x.

```sh
npm install @boxdb/client @boxdb/db0-adapter db0
```

```ts
import { BoxDB } from "@boxdb/client";
import { db0Connector } from "@boxdb/db0-adapter";
import { createDatabase } from "db0";

await using client = await BoxDB.open("data.db");
const db = createDatabase(db0Connector(client));
await db.exec("CREATE TABLE IF NOT EXISTS users(name TEXT)");
await db.sql`INSERT INTO users VALUES(${"Ada"})`;
const result = await db.sql`SELECT * FROM users`;
console.log(result.rows);
```

`prepare` creates a synchronous descriptor; `all`, `get` and `run` execute
asynchronously on the native worker. Each `bind` returns an independent
binding. Write results include `success` plus bigint `changes` and `lastInsertRowid`.
Integer row cells also remain bigint.
db0 `undefined` bindings become SQL NULL.

For transactions or cancellation, pass a BoxDB transaction handle to
`db0Connector`. Commit through that handle. The connector borrows its client;
`dispose` does not close or commit it. Raw transaction-control SQL is rejected.
