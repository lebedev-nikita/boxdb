import { expect, expectTypeOf, test } from "vitest";
import { BoxDB, type Query, type QueryResult } from "../src/index.js";

test("tagged query rows require validation before use", async () => {
  await using db = await BoxDB.open();
  const query = db.query`SELECT 1 AS id`;
  const rows = await query;
  expectTypeOf(rows).toEqualTypeOf<unknown[]>();
  expectTypeOf(query.execute()).toEqualTypeOf<Promise<unknown[]>>();
  expect(rows).toEqual([{ id: 1n }]);

  // @ts-expect-error Query rows cannot be declared by the caller.
  void db.query<{ id: string }>`SELECT 1 AS id`;
  // @ts-expect-error Query cannot carry an unvalidated application row type.
  expectTypeOf<Query<{ id: string }>>();
  // @ts-expect-error Rows must be validated before accessing fields.
  void rows[0]!.id;
});

test("single and positional rows require validation, including in transactions", async () => {
  await using db = await BoxDB.open();
  const query = db.query`SELECT 1 AS id`;
  expectTypeOf(query.get()).toEqualTypeOf<Promise<unknown>>();
  expectTypeOf(query.values()).toEqualTypeOf<Promise<unknown[]>>();
  expectTypeOf(query.then((rows) => rows)).toEqualTypeOf<Promise<unknown[]>>();
  expect(await query.get()).toEqual({ id: 1n });
  expect(await query.values()).toEqual([[1n]]);
  expect(await db.query`SELECT 1 WHERE 0`.get()).toBeUndefined();

  await using tx = await db.begin();
  expectTypeOf(await tx.query`SELECT 1 AS id`).toEqualTypeOf<unknown[]>();
  expectTypeOf((await tx.execute("SELECT 1 AS id")).rows).toEqualTypeOf<unknown[]>();
  // @ts-expect-error Transactions cannot declare an application row type either.
  void tx.query<{ id: string }>`SELECT 1 AS id`;
  // @ts-expect-error Transactions cannot declare an application row type either.
  await tx.execute<{ id: string }>("SELECT 1 AS id");
});

test("execute returns unvalidated rows in both object and raw modes", async () => {
  await using db = await BoxDB.open();
  const result = await db.execute("SELECT 1 AS id");
  const raw = await db.execute("SELECT 1 AS id", [], { raw: true });
  expectTypeOf(result.rows).toEqualTypeOf<unknown[]>();
  expectTypeOf(raw.rows).toEqualTypeOf<unknown[]>();
  expect(result.rows).toEqual([{ id: 1n }]);
  expect(raw.rows).toEqual([[1n]]);

  // @ts-expect-error Result rows cannot be declared by the caller.
  await db.execute<{ id: string }>("SELECT 1 AS id");
  // @ts-expect-error QueryResult cannot carry an unvalidated application row type.
  expectTypeOf<QueryResult<{ id: string }>>();
});
