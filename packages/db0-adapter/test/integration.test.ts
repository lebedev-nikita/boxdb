import { expect, test } from "vitest";
import { createDatabase } from "db0";
import { Database } from "@boxdb/client";
import { db0Connector } from "../src/index.js";

test("db0 uses native queries and immutable bound statements", async () => {
  await using client = await Database.open();
  const connector = db0Connector(client);
  const db = createDatabase(connector);
  await db.exec("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT)");
  expect(await db.sql`INSERT INTO users(name) VALUES(${"Ada"})`).toMatchObject({
    success: true,
    changes: 1n,
    lastInsertRowid: 1n,
  });
  const statement = db.prepare("SELECT name FROM users WHERE id=?");
  const first = statement.bind(1);
  const absent = statement.bind(2);
  expect(await first.get()).toEqual({ name: "Ada" });
  expect(await absent.all()).toEqual([]);
  await using tx = await client.begin();
  const scoped = createDatabase(db0Connector(tx));
  await scoped.sql`INSERT INTO users(name) VALUES(${"rollback"})`;
  await tx.rollback();
  expect(await db.sql`SELECT name FROM users`).toEqual({ rows: [{ name: "Ada" }], success: true });
  await connector.dispose?.();
  expect(client.open).toBe(true);
});
