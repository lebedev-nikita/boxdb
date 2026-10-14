import assert from "node:assert/strict";
import { Database } from "../dist/index.js";
import { drizzle } from "../dist/drizzle.js";
import { adapter } from "../dist/adapters/postgres.js";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { Kysely, SqliteDialect } from "kysely";

const client = new Database();
try {
  assert.deepEqual(
    client.get(
      "SELECT decimal_add(?,?) AS amount, text_lower(?) AS word, regexp_substr(?,'\\d+') AS digits, vec_distance_cosine('[1,0]','[0,1]') AS distance",
      ["0.1", "0.2", "ПРИВЕТ", "order #123"],
    ),
    { amount: "0.3", word: "привет", digits: "123", distance: 1 },
  );
  client.exec("CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)");
  const users = sqliteTable("users", { id: integer().primaryKey(), name: text() });
  const orm = drizzle(client);
  orm.insert(users).values({ name: "Ada" }).run();
  assert.deepEqual(client.query("SELECT * FROM users").rows, [{ id: 1, name: "Ada" }]);
  assert.deepEqual(orm.select().from(users).all(), [{ id: 1, name: "Ada" }]);
  const sql = adapter(client);
  const rows = await sql`SELECT name FROM users WHERE id = ${1}`;
  assert.deepEqual(rows, [{ name: "Ada" }]);
  assert.equal(rows.count, 1);
  assert.deepEqual(await sql`SELECT ${1} AS x, ${2} AS x`.values(), [[1, 2]]);
  const queryBuilder = new Kysely<{ users: { id: number; name: string } }>({
    dialect: new SqliteDialect({ database: client }),
  });
  assert.deepEqual(await queryBuilder.selectFrom("users").selectAll().execute(), [
    { id: 1, name: "Ada" },
  ]);
  await queryBuilder.destroy();
  console.log("BoxDB runtime smoke passed");
} finally {
  client.close();
}
