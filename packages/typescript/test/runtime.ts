import assert from "node:assert/strict";
import { Database } from "../dist/index.js";
import { drizzle } from "../dist/drizzle.js";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { Kysely, SqliteDialect } from "kysely";

const client = new Database();
try {
  client.exec("CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)");
  const users = sqliteTable("users", { id: integer().primaryKey(), name: text() });
  const orm = drizzle(client);
  orm.insert(users).values({ name: "Ada" }).run();
  assert.deepEqual(client.query("SELECT * FROM users").rows, [{ id: 1, name: "Ada" }]);
  assert.deepEqual(orm.select().from(users).all(), [{ id: 1, name: "Ada" }]);
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
