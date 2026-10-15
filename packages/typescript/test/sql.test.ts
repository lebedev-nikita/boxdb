import { expect, expectTypeOf, test } from "vitest";
import { BoxDB } from "../src/index.js";

test("SQL identifiers quote each explicit component and parameter values stay bound", async () => {
  await using db = await BoxDB.open();
  await db.query`CREATE TABLE ${db.identifier('odd"table')} (${db.identifier('odd"column')} TEXT)`;
  const hostile = "'); DROP TABLE users; --";
  await db.query`INSERT INTO ${db.identifier('odd"table')} VALUES(${hostile})`;
  expect(
    await db.query`SELECT ${db.identifier('odd"column')} FROM ${db.identifier("main", 'odd"table')}`,
  ).toEqual([{ 'odd"column': hostile }]);
  expect(db.identifier("user.name").compile().sql).toBe('"user.name"');
  expect(() => db.identifier("")).toThrow(TypeError);
  expect(() => db.identifier("\0")).toThrow(TypeError);
});

test("a lazy query executes once across all result methods", async () => {
  await using db = await BoxDB.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const query = db.query`INSERT INTO item VALUES(${1}) RETURNING id`;
  expect(await db.query`SELECT count(*) AS n FROM item`.get()).toEqual({ n: 0n });
  expect(await query.values()).toEqual([[1n]]);
  expect(await query.get()).toEqual({ id: 1n });
  const result = await query;
  expect(result).toEqual([{ id: 1n }]);
  expectTypeOf(result).toEqualTypeOf<unknown[]>();
  expect(await db.query`SELECT count(*) AS n FROM item`.get()).toEqual({ n: 1n });
});

test("fragments snapshot blobs and compile does not expose mutable bound storage", async () => {
  await using db = await BoxDB.open();
  const bytes = Buffer.from([1, 2]);
  const fragment = db.fragment`SELECT ${bytes} AS payload`;
  bytes[0] = 9;
  const compiled = fragment.compile();
  (compiled.parameters[0] as Buffer)[1] = 8;
  expect(await db.query`${fragment}`.get()).toEqual({ payload: Buffer.from([1, 2]) });
});

test("a rejected query caches its failure and unexecuted queries reject after close", async () => {
  const db = await BoxDB.open();
  const invalid = db.query`SELECT * FROM missing`;
  await expect(invalid.execute()).rejects.toThrow("no such table");
  await db.exec("CREATE TABLE missing(id INTEGER)");
  await expect(invalid.values()).rejects.toThrow("no such table");
  const pending = db.query`SELECT 1`;
  await db.close();
  await expect(pending.execute()).rejects.toThrow(/closed/);
});
