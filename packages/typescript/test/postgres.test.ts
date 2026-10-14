import { expect, expectTypeOf, it } from "vitest";
import { BoxDBError, Database } from "../src/index.js";
import { adapter, type RowList } from "../src/adapters/postgres.js";

it("adapts an existing connection to parameterized tagged queries", async () => {
  using db = new Database();
  const sql = adapter(db);
  await sql`CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT)`;
  const name = "Ada";
  const inserted = await sql<{ id: bigint; name: string }[]>`
    INSERT INTO users(name) VALUES(${name}) RETURNING id, name
  `;
  expect(inserted).toEqual([{ id: 1n, name: "Ada" }]);
  expectTypeOf(inserted).toEqualTypeOf<RowList<{ id: bigint; name: string }[]>>();
  expect(await sql`SELECT name FROM users WHERE id=${1}`).toEqual([{ name: "Ada" }]);
  expect(db.open).toBe(true);
});

it("quotes dynamic identifiers while keeping interpolated values as parameters", async () => {
  using db = new Database();
  const sql = adapter(db);
  await sql`CREATE TABLE ${sql('odd"table')} (${sql('odd"column')} TEXT)`;
  await sql`INSERT INTO ${sql('odd"table')} VALUES(${"'); DROP TABLE users; --"})`;
  expect(await sql`SELECT ${sql('odd"column')} FROM ${sql('main.odd"table')}`).toEqual([
    { 'odd"column': "'); DROP TABLE users; --" },
  ]);
  await expect(sql`SELECT * FROM ${sql('odd"table; DROP TABLE users; --')}`).rejects.toThrow();
});

it("composes lazy queries and runs each awaited query only once", async () => {
  using db = new Database();
  const sql = adapter(db);
  await sql`CREATE TABLE t(n INTEGER)`;
  const pending = sql`INSERT INTO t VALUES(${1}) RETURNING n`;
  expect(await sql`SELECT COUNT(*) AS n FROM t`).toEqual([{ n: 0n }]);
  const inserted = await pending;
  expect(await pending).toBe(inserted);
  expect(await pending.execute()).toBe(inserted);
  expect(await sql`SELECT COUNT(*) AS n FROM t`).toEqual([{ n: 1n }]);
  const condition = (min: number) => sql`n >= ${min}`;
  const filter = sql`WHERE ${condition(2)} AND n < ${4}`;
  const result =
    await sql`WITH numbers(n) AS (VALUES(${1}),(${2}),(${3}),(${4})) SELECT n FROM numbers ${filter} ORDER BY n`;
  expect(result).toEqual([{ n: 2n }, { n: 3n }]);
});

it("returns row arrays with Postgres.js-style counts and column names", async () => {
  using db = new Database();
  const sql = adapter(db);
  await sql`CREATE TABLE t(n INTEGER)`;
  const inserted = await sql`INSERT INTO t VALUES(${1}),(${2}),(${3})`;
  expect(inserted).toEqual([]);
  expect(inserted.count).toBe(3n);
  expect(inserted.columns).toEqual([]);
  const selected = await sql`SELECT n FROM t WHERE n > ${1} ORDER BY n`;
  expect(selected).toEqual([{ n: 2n }, { n: 3n }]);
  expect(selected.count).toBe(2n);
  expect(selected.columns).toEqual([{ name: "n" }]);
  expect((await sql`UPDATE t SET n=${4} WHERE n=${99}`).count).toBe(0n);
});

it("returns positional values without losing duplicate columns or repeating writes", async () => {
  using db = new Database();
  const sql = adapter(db);
  const query = sql`SELECT ${1} AS x, ${2} AS x`;
  const values = await query.values();
  expect(values).toEqual([[1n, 2n]]);
  expect(values.columns).toEqual([{ name: "x" }, { name: "x" }]);
  expect(values.count).toBe(1n);
  expect(await query).toEqual([{ x: 2n }]);
  await sql`CREATE TABLE t(n INTEGER)`;
  const insert = sql`INSERT INTO t VALUES(${1}) RETURNING n`;
  expect(await insert.values()).toEqual([[1n]]);
  expect(await insert).toEqual([{ n: 1n }]);
  expect(await sql`SELECT COUNT(*) AS n FROM t`).toEqual([{ n: 1n }]);
});

it("propagates and caches query errors through catch, finally and values", async () => {
  using db = new Database();
  const sql = adapter(db);
  const invalid = sql`SELECT * FROM missing`;
  const failure = await invalid.catch((error: unknown) => error);
  expect(failure).toBeInstanceOf(BoxDBError);
  expect(failure).toMatchObject({ code: "BOXDB_ERROR", resultCode: 1, extendedCode: 1 });
  expect(await invalid.values().catch((error: unknown) => error)).toBe(failure);
  expect(await invalid.catch(() => "caught")).toBe("caught");
  let finalized = false;
  await invalid
    .finally(() => {
      finalized = true;
    })
    .catch(() => undefined);
  expect(finalized).toBe(true);
  await sql`CREATE TABLE missing(n INTEGER)`;
  await expect(invalid.values()).rejects.toThrow("no such table");
  const pending = sql`SELECT ${1}`;
  db.close();
  await expect(pending).rejects.toThrow("closed");
});

it("rejects malformed identifiers and calls that are not tagged templates", async () => {
  using db = new Database();
  const sql = adapter(db);
  expect(() => sql("main..table")).toThrow(TypeError);
  expect(() => sql("\0")).toThrow(TypeError);
  const untyped: (...args: unknown[]) => unknown = sql as (...args: unknown[]) => unknown;
  expect(() => untyped(["SELECT 1"])).toThrow(TypeError);
  expect(() => untyped("users", "extra")).toThrow(TypeError);
  // @ts-expect-error Objects must be explicitly serialized, not interpolated as SQL.
  await expect(sql`SELECT ${{ untrusted: "SQL" }}`).rejects.toThrow(TypeError);
  await expect(sql`SELECT '${"quoted"}'`).rejects.toThrow("Too many");
  await expect(sql`SELECT ${1}; SELECT ${2}`).rejects.toThrow();
});
