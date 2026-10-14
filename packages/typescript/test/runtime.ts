import assert from "node:assert/strict";
import { BoxDBError, Database } from "../dist/index.js";
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
  assert.throws(
    () => client.get("SELECT * FROM missing"),
    (error: unknown) => {
      assert.ok(error instanceof BoxDBError);
      assert.equal(error.code, "BOXDB_ERROR");
      assert.equal(error.resultCode, 1);
      assert.equal(error.extendedCode, 1);
      assert.ok(error.cause instanceof Error);
      return true;
    },
  );
  const users = sqliteTable("users", { id: integer().$type<bigint>().primaryKey(), name: text() });
  const orm = drizzle(client);
  orm.insert(users).values({ name: "Ada" }).run();
  assert.deepEqual(client.query("SELECT * FROM users").rows, [{ id: 1n, name: "Ada" }]);
  assert.deepEqual(orm.select().from(users).all(), [{ id: 1n, name: "Ada" }]);
  const sql = adapter(client);
  const rows = await sql`SELECT name FROM users WHERE id = ${1}`;
  assert.deepEqual(rows, [{ name: "Ada" }]);
  assert.equal(rows.count, 1n);
  assert.deepEqual(await sql`SELECT ${1} AS x, ${2} AS x`.values(), [[1n, 2n]]);
  const queryBuilder = new Kysely<{ users: { id: bigint; name: string } }>({
    dialect: new SqliteDialect({ database: client }),
  });
  assert.deepEqual(await queryBuilder.selectFrom("users").selectAll().execute(), [
    { id: 1n, name: "Ada" },
  ]);
  await queryBuilder.destroy();
  console.log("BoxDB runtime smoke passed");
} finally {
  client.close();
}
