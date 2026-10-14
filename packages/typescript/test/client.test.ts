import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { expect, test } from "vitest";
import { Database } from "../src/index.js";

test.each([
  ["INSERT INTO t VALUES(1,'Grace',1)", "BOXDB_CONSTRAINT_PRIMARYKEY", 1555],
  ["INSERT INTO t VALUES(2,NULL,1)", "BOXDB_CONSTRAINT_NOTNULL", 1299],
  ["INSERT INTO t VALUES(2,'Grace',0)", "BOXDB_CONSTRAINT_CHECK", 275],
  ["INSERT INTO t VALUES(2,'Ada',1)", "BOXDB_CONSTRAINT_UNIQUE", 2067],
  ["SELECT * FROM missing", "BOXDB_ERROR", 1],
  ["SELECT FROM", "BOXDB_ERROR", 1],
])("preserves structured errors and native causes: %s", async (sql, code, extended) => {
  await using db = await Database.open();
  await db.exec(
    "CREATE TABLE t(id INTEGER PRIMARY KEY,name TEXT NOT NULL UNIQUE,n INTEGER CHECK(n>0)); INSERT INTO t VALUES(1,'Ada',1)",
  );
  await expect(db.execute(sql)).rejects.toMatchObject({
    name: "BoxDBError",
    code,
    resultCode: extended === 1 ? 1 : 19,
    extendedCode: extended,
    cause: expect.objectContaining({ code }),
  });
});

test("preserves extension functions, full-text, spatial and vector queries", async () => {
  await using db = await Database.open();
  expect(
    await db.query`SELECT CAST(${"9007199254740992.01"} AS DECIMAL)+CAST(${"0.02"} AS DECIMAL) AS amount, text_lower(${"ПРИВЕТ"}) AS word, regexp_substr(${"order #123"},${"\\d+"}) AS digits`.get(),
  ).toEqual({ amount: "9007199254740992.03", word: "привет", digits: "123" });
  await db.exec(
    "CREATE VIRTUAL TABLE vectors USING vec0(embedding float[2]); INSERT INTO vectors(rowid,embedding) VALUES(1,'[1,0]'),(2,'[0,1]'); CREATE VIRTUAL TABLE search USING fts5(body); INSERT INTO search VALUES('hello SQLite'); CREATE VIRTUAL TABLE regions USING rtree(id,min_x,max_x,min_y,max_y); INSERT INTO regions VALUES(7,0,10,0,10)",
  );
  expect(
    await db.query`SELECT rowid FROM vectors WHERE embedding MATCH ${"[1,0]"} AND k=1`.get(),
  ).toEqual({ rowid: 1n });
  expect(await db.query`SELECT body FROM search WHERE search MATCH ${"sqlite"}`.get()).toEqual({
    body: "hello SQLite",
  });
  expect(await db.query`SELECT id FROM regions WHERE min_x<=5 AND max_x>=5`.get()).toEqual({
    id: 7n,
  });
});

test("matches ordinary SQLite values, affected rows and row IDs", async () => {
  await using db = await Database.open();
  const sqlite = new DatabaseSync(":memory:");
  try {
    const schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, text TEXT, n REAL, b BLOB)";
    await db.exec(schema);
    sqlite.exec(schema);
    const sql = "INSERT INTO t VALUES(?,?,?,?)";
    const parameters = [1, "'; DROP TABLE t; --", 1.25, Buffer.from([0, 128, 255])] as const;
    const { changes, lastInsertRowid } = await db.execute(sql, parameters);
    const expectedWrite = sqlite.prepare(sql).run(...parameters);
    expect({ changes, lastInsertRowid }).toEqual({
      changes: BigInt(expectedWrite.changes),
      lastInsertRowid: BigInt(expectedWrite.lastInsertRowid),
    });
    const select = "SELECT id, text AS label, n, b, NULL AS missing FROM t";
    const expectedSelect = sqlite.prepare(select);
    expectedSelect.setReadBigInts(true);
    expect((await db.execute(select)).rows).toEqual(
      expectedSelect.all().map((row) => ({ ...row, b: Buffer.from(row.b as Uint8Array) })),
    );
    expect(await db.query`UPDATE t SET n=n+1 WHERE id=${1}`.run()).toMatchObject({ changes: 1n });
    expect(await db.query`UPDATE t SET n=n+1 WHERE id=${9}`.run()).toMatchObject({ changes: 0n });
  } finally {
    sqlite.close();
  }
});

test("validates named and positional bindings and rejects extra or missing parameters", async () => {
  await using db = await Database.open();
  expect((await db.execute("SELECT @value AS a,@value AS b", { value: "same" })).rows).toEqual([
    { a: "same", b: "same" },
  ]);
  expect((await db.execute("SELECT ? AS a,? AS b", [1, "x"])).rows).toEqual([{ a: 1n, b: "x" }]);
  await expect(db.execute("SELECT ?", [])).rejects.toThrow("Too few");
  await expect(db.execute("SELECT ?", [1, 2])).rejects.toThrow("Too many");
  await expect(db.execute("SELECT :a", { other: 1 })).rejects.toThrow("Missing");
  await expect(db.execute("SELECT :a", { a: 1n, extra: 2 })).rejects.toThrow("Too many");
  expect(await db.query`SELECT 1 AS x,2 AS x`.values()).toEqual([[1n, 2n]]);
});

test("retains integer precision while preserving float and null storage values", async () => {
  await using db = await Database.open(":memory:");
  expect(
    await db.query`SELECT ${9223372036854775807n} AS high, ${-9223372036854775808n} AS low, 1.0 AS f,NULL AS n`.get(),
  ).toEqual({ high: 9223372036854775807n, low: -9223372036854775808n, f: 1, n: null });
  await expect(db.query`SELECT ${9223372036854775808n}`.execute()).rejects.toThrow("64-bit range");
  await expect(db.query`SELECT ${Number.MAX_SAFE_INTEGER + 1}`.execute()).rejects.toBeInstanceOf(
    RangeError,
  );
  await expect(db.query`SELECT ${Infinity}`.execute()).rejects.toBeInstanceOf(TypeError);
  expect(await db.query`SELECT 9223372036854775807 AS n`.get()).toEqual({
    n: 9223372036854775807n,
  });
});

test("validates BoxDB extended types and returns their SQLite representations", async () => {
  await using db = await Database.open();
  await db.exec(
    "CREATE TABLE t(active BOOLEAN, payload JSON, amount DECIMAL(12,2), at INSTANT, small UINT8)",
  );
  await db.query`INSERT INTO t VALUES(${true},${'{"x":1}'},${"12.50"},${"2024-02-29T16:00:00+04:00"},${255})`;
  expect(await db.query`SELECT * FROM t`.get()).toEqual({
    active: 1n,
    payload: '{"x":1}',
    amount: "12.5",
    at: "2024-02-29T12:00:00.000000Z",
    small: 255n,
  });
  await expect(db.query`UPDATE t SET small=${256}`.execute()).rejects.toThrow();
  expect(await db.query`SELECT small FROM t`.get()).toEqual({ small: 255n });
});

test.each(["binary", "readable"] as const)(
  "persists committed %s data and discards unfinished transactions",
  async (format) => {
    const directory = mkdtempSync(join(tmpdir(), "boxdb-client-"));
    try {
      const path = join(directory, "data.db");
      {
        await using db = await Database.open(path, { format });
        await db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY,name TEXT,payload BLOB)");
        {
          await using tx = await db.begin();
          await tx.query`INSERT INTO t VALUES(${9007199254740993n},${"Ada"},${Buffer.from([0, 255])})`;
          await tx.commit();
        }
        {
          await using tx = await db.begin();
          await tx.query`INSERT INTO t VALUES(${2},${"rollback"},NULL)`;
        }
      }
      await using reopened = await Database.open(path);
      expect(await reopened.query`SELECT * FROM t`).toEqual([
        { id: 9007199254740993n, name: "Ada", payload: Buffer.from([0, 255]) },
      ]);
      expect(readFileSync(path).toString().startsWith("SQLite format 3")).toBe(format === "binary");
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  },
);

test("opening failures preserve SQLite codes", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-client-"));
  try {
    await expect(Database.open(join(directory, "missing", "data.db"))).rejects.toMatchObject({
      code: "BOXDB_CANTOPEN",
      resultCode: 14,
      extendedCode: 14,
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("single statement execution rejects scripts and refreshes changed schemas", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)");
  expect(await db.query`SELECT * FROM t`.get()).toEqual({ n: 1n });
  await db.exec("ALTER TABLE t ADD COLUMN s TEXT DEFAULT 'new'");
  expect(await db.query`SELECT * FROM t`.get()).toEqual({ n: 1n, s: "new" });
  await expect(db.execute("SELECT 1; DELETE FROM t")).rejects.toThrow();
  expect((await db.execute("SELECT 1 AS n; -- trailing comment")).rows).toEqual([{ n: 1n }]);
});

test("integer results and write metadata follow the main bigint contract", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE ints(n INT64)");
  expect(await db.query`INSERT INTO ints VALUES(${9223372036854775807n})`.run()).toEqual({
    changes: 1n,
    lastInsertRowid: 1n,
  });
  expect(
    await db.query`SELECT n,1 AS small,-9223372036854775808 AS min,1.0 AS real FROM ints`.get(),
  ).toEqual({ n: 9223372036854775807n, small: 1n, min: -9223372036854775808n, real: 1 });
  expect(await db.query`SELECT ${42} AS integer,${true} AS flag,${1.25} AS real`.get()).toEqual({
    integer: 42n,
    flag: 1n,
    real: 1.25,
  });
});

test("native decimals retain exact arithmetic, numeric ordering and rounded results", async () => {
  await using db = await Database.open();
  await db.exec(
    "CREATE TABLE prices(amount DECIMAL(20,2) UNIQUE); INSERT INTO prices VALUES('10'),('2.00')",
  );
  expect(await db.query`SELECT amount FROM prices ORDER BY amount`).toEqual([
    { amount: "2" },
    { amount: "10" },
  ]);
  expect(
    await db.query`SELECT CAST(${"9007199254740992.01"} AS DECIMAL)+CAST(${"0.02"} AS DECIMAL) AS amount`.get(),
  ).toEqual({ amount: "9007199254740992.03" });
  expect(await db.query`SELECT decimal_div('1','3',2,'half_even') AS amount`.get()).toEqual({
    amount: "0.33",
  });
  await expect(db.query`INSERT INTO prices VALUES(${"2.0"})`.execute()).rejects.toMatchObject({
    code: "BOXDB_CONSTRAINT_UNIQUE",
  });
});
