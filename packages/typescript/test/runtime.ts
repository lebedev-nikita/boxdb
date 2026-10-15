import assert from "node:assert/strict";
import { BoxDB, BoxDBError } from "../dist/index.js";

await using db = await BoxDB.open();
assert.deepEqual(
  await db.query`SELECT CAST(${"0.1"} AS DECIMAL)+CAST(${"0.2"} AS DECIMAL) AS amount,text_lower(${"ПРИВЕТ"}) AS word`.get(),
  { amount: "0.3", word: "привет" },
);
await db.exec("CREATE TABLE users(name TEXT UNIQUE)");
{
  await using tx = await db.begin({ mode: "immediate" });
  await tx.query`INSERT INTO users VALUES(${"Ada"})`;
  await tx.commit();
}
{
  await using tx = await db.begin();
  await tx.query`INSERT INTO users VALUES(${"rollback"})`;
}
assert.deepEqual(await db.query`SELECT * FROM users`, [{ name: "Ada" }]);
await assert.rejects(
  db.query`INSERT INTO users VALUES(${"Ada"})`.execute(),
  (error) => error instanceof BoxDBError && error.code === "BOXDB_CONSTRAINT_UNIQUE",
);
const controller = new AbortController();
const reason = new Error("cancelled");
const pending =
  db.query`WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n`.execute(
    { signal: controller.signal },
  );
const timer = setTimeout(() => controller.abort(reason), 30);
try {
  await assert.rejects(pending, (error) => error === reason);
} finally {
  clearTimeout(timer);
}
assert.deepEqual(await db.query`SELECT 42 AS answer`.get(), { answer: 42n });
console.log("BoxDB async runtime smoke passed");
