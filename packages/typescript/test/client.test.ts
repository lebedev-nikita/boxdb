import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { afterEach, describe, expect, it } from "vitest";
import { BoxDBError, Database } from "../src/index.js";

const connections: Database[] = [];
const directories: string[] = [];
function open(path?: string, options?: ConstructorParameters<typeof Database>[1]): Database {
  const db = new Database(path, options);
  connections.push(db);
  return db;
}
function directory(): string {
  const path = mkdtempSync(join(tmpdir(), "boxdb-ts-"));
  directories.push(path);
  return path;
}
afterEach(() => {
  connections.forEach((db) => db.close());
  connections.length = 0;
  directories.forEach((path) => rmSync(path, { recursive: true, force: true }));
  directories.length = 0;
});

describe("query interface", () => {
  it("returns every stored int64 and write count as bigint without a mode switch", () => {
    const db = open();
    db.exec("CREATE TABLE ints(n INT64)");
    expect(db.run("INSERT INTO ints VALUES(?)", [9223372036854775807n])).toEqual({
      changes: 1n,
      lastInsertRowid: 1n,
    });
    expect(
      db.get("SELECT n, 1 AS small, -9223372036854775808 AS min, 1.0 AS real FROM ints"),
    ).toEqual({
      n: 9223372036854775807n,
      small: 1n,
      min: -9223372036854775808n,
      real: 1,
    });
  });
  it("binds safe integer numbers and booleans as integers while retaining real parameters", () => {
    const db = open();
    expect(db.get("SELECT ? AS integer, ? AS flag, ? AS real", [42, true, 1.25])).toEqual({
      integer: 42n,
      flag: 1n,
      real: 1.25,
    });
  });
  it("preserves BoxDB UNIQUE error codes and the native cause", () => {
    const db = open();
    db.exec("CREATE TABLE users(name TEXT UNIQUE); INSERT INTO users VALUES('Ada')");
    expect(() => db.run("INSERT INTO users VALUES(?)", ["Ada"])).toThrowError(
      expect.objectContaining({
        name: "BoxDBError",
        message: "UNIQUE constraint failed: users.name",
        code: "BOXDB_CONSTRAINT_UNIQUE",
        resultCode: 19,
        extendedCode: 2067,
        cause: expect.objectContaining({
          code: "BOXDB_CONSTRAINT_UNIQUE",
          resultCode: 19,
          extendedCode: 2067,
        }),
      }),
    );
  });

  it.each([
    ["INSERT INTO t VALUES(1,'Grace',1)", "BOXDB_CONSTRAINT_PRIMARYKEY", 1555],
    ["INSERT INTO t VALUES(2,NULL,1)", "BOXDB_CONSTRAINT_NOTNULL", 1299],
    ["INSERT INTO t VALUES(2,'Grace',0)", "BOXDB_CONSTRAINT_CHECK", 275],
    ["SELECT * FROM missing", "BOXDB_ERROR", 1],
    ["SELECT FROM", "BOXDB_ERROR", 1],
  ])("exposes structured errors from query: %s", (sql, code, extended) => {
    const db = open();
    db.exec(
      "CREATE TABLE t(id INTEGER PRIMARY KEY,name TEXT NOT NULL,n INTEGER CHECK(n>0)); INSERT INTO t VALUES(1,'Ada',1)",
    );
    expect(() => db.query(sql)).toThrowError(
      expect.objectContaining({
        code,
        resultCode: extended === 1 ? 1 : 19,
        extendedCode: extended,
      }),
    );
  });

  it.each(["binary", "readable"] as const)(
    "preserves deferred foreign-key codes from %s scripts",
    (format) => {
      const db = open(join(directory(), "data.db"), { format });
      db.exec(
        "CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)",
      );
      expect(() => db.exec("INSERT INTO children VALUES(99)")).toThrowError(
        expect.objectContaining({
          code: "BOXDB_CONSTRAINT_FOREIGNKEY",
          resultCode: 19,
          extendedCode: 787,
        }),
      );
    },
  );

  it("distinguishes validation and closed-database failures without engine codes", () => {
    const db = open();
    expect(() => db.prepare("SELECT 1; SELECT 2")).toThrowError(
      expect.objectContaining({
        code: "BOXDB_VALIDATION",
        resultCode: undefined,
        extendedCode: undefined,
      }),
    );
    db.close();
    expect(() => db.get("SELECT 1")).toThrowError(
      expect.objectContaining({ code: "BOXDB_CLOSED", cause: expect.any(Error) }),
    );
  });

  it("preserves BoxDB engine codes when opening a database fails", () => {
    expect(() => new Database(join(directory(), "missing", "data.db"))).toThrowError(
      expect.objectContaining({ code: "BOXDB_CANTOPEN", resultCode: 14, extendedCode: 14 }),
    );
  });

  it("distinguishes busy and read-only BoxDB failures", () => {
    const path = join(directory(), "data.db");
    const writer = open(path);
    writer.exec("CREATE TABLE t(n INTEGER)");
    const db = open(path);
    db.pragma("busy_timeout = 0");
    writer.exec("BEGIN IMMEDIATE");
    try {
      expect(() => db.run("INSERT INTO t VALUES(1)")).toThrowError(
        expect.objectContaining({ code: "BOXDB_BUSY", resultCode: 5, extendedCode: 5 }),
      );
    } finally {
      writer.exec("ROLLBACK");
    }
    db.pragma("query_only = ON");
    expect(() => db.run("INSERT INTO t VALUES(1)")).toThrowError(
      expect.objectContaining({ code: "BOXDB_READONLY", resultCode: 8, extendedCode: 8 }),
    );
  });

  it("provides decimal, Unicode, regexp, vector, full-text and spatial extensions", () => {
    const db = open();
    expect(
      db.get(
        "SELECT decimal_add(?,?) AS amount, text_lower(?) AS word, regexp_substr(?,'\\d+') AS digits",
        ["9007199254740992.01", "0.02", "ПРИВЕТ", "order #123"],
      ),
    ).toEqual({ amount: "9007199254740992.03", word: "привет", digits: "123" });
    db.exec("CREATE TABLE prices(n NUMERIC(12,2),d DECIMAL(12,2))");
    db.run("INSERT INTO prices VALUES(?,?)", ["12.5", "12.5"]);
    expect(db.get("SELECT * FROM prices")).toEqual({ n: "12.5", d: "12.5" });
    db.exec(
      "CREATE VIRTUAL TABLE vectors USING vec0(embedding float[2]); INSERT INTO vectors(rowid,embedding) VALUES(1,'[1,0]'),(2,'[0,1]')",
    );
    expect(db.get("SELECT rowid FROM vectors WHERE embedding MATCH ? AND k=1", ["[1,0]"])).toEqual({
      rowid: 1n,
    });
    db.exec(
      "CREATE VIRTUAL TABLE search USING fts5(body); INSERT INTO search VALUES('hello SQLite'); CREATE VIRTUAL TABLE regions USING rtree(id,min_x,max_x,min_y,max_y); INSERT INTO regions VALUES(7,0,10,0,10)",
    );
    expect(db.get("SELECT body FROM search WHERE search MATCH ?", ["sqlite"])).toEqual({
      body: "hello SQLite",
    });
    expect(db.get("SELECT id FROM regions WHERE min_x<=5 AND max_x>=5")).toEqual({ id: 7n });
  });

  it("executes parameterized writes and queries without preparing explicitly", () => {
    const db = open();
    db.exec("CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE)");
    expect(db.query("INSERT INTO users(name) VALUES (?)", ["Ada"])).toMatchObject({
      rows: [],
      changes: 1n,
      lastInsertRowid: 1n,
    });
    expect(db.all("SELECT * FROM users WHERE name = :name", { name: "Ada" })).toEqual([
      { id: 1n, name: "Ada" },
    ]);
    expect(db.get("SELECT * FROM users WHERE id = ?", [2])).toBeUndefined();
    expect(db.query("INSERT INTO users(name) VALUES(?) RETURNING id", ["Grace"]).rows).toEqual([
      { id: 2n },
    ]);
  });

  it("matches SQLite storage values, aliases, blobs and affected rows", () => {
    const db = open();
    const sqlite = new DatabaseSync(":memory:");
    try {
      const schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, text TEXT, n REAL, b BLOB)";
      db.exec(schema);
      sqlite.exec(schema);
      const insert = "INSERT INTO t VALUES(?,?,?,?)";
      const parameters = [1, "'; DROP TABLE t; --", 1.25, Buffer.from([0, 128, 255])] as const;
      const written = sqlite.prepare(insert).run(...parameters);
      expect(db.prepare(insert).run(parameters)).toEqual({
        changes: BigInt(written.changes),
        lastInsertRowid: BigInt(written.lastInsertRowid),
      });
      const sql = "SELECT id, text AS label, n, b, NULL AS missing FROM t";
      const actual = db.all(sql);
      const reference = sqlite.prepare(sql);
      reference.setReadBigInts(true);
      const expected = reference
        .all()
        .map((row) => ({ ...row, b: Buffer.from(row.b as Uint8Array) }));
      expect(actual).toEqual(expected);
      expect(db.run("UPDATE t SET n = n + 1 WHERE id = ?", [1]).changes).toBe(1n);
      expect(db.run("UPDATE t SET n = n + 1 WHERE id = ?", [9]).changes).toBe(0n);
    } finally {
      sqlite.close();
    }
  });

  it("supports bound arrays, variadic and named statement arguments", () => {
    const db = open();
    const statement = db.prepare("SELECT ? AS a, ? AS b");
    expect(statement.get(1, "x")).toEqual({ a: 1n, b: "x" });
    expect(statement.get([2, "y"])).toEqual({ a: 2n, b: "y" });
    expect(db.prepare("SELECT @value AS a, @value AS b").get({ value: "same" })).toEqual({
      a: "same",
      b: "same",
    });
    expect(db.prepare("SELECT $a AS a, ? AS b").get({ $a: "named" }, 3)).toEqual({
      a: "named",
      b: 3n,
    });
    expect(() => statement.get(1)).toThrow("Too few");
    expect(() => statement.get(1, 2, 3)).toThrow("Too many");
    expect(() => db.get("SELECT :a", { other: 1 })).toThrow("Missing");
    expect(() => db.get("SELECT :a", { a: 1n, extra: 2 })).toThrow("Too many");
    expect(() => db.prepare("SELECT ?").get(undefined)).toThrow(TypeError);
  });

  it("supports raw rows, repeated column names, iterators and pragmas", () => {
    const db = open();
    expect(db.prepare("SELECT 1 AS x, 2 AS x").raw().all()).toEqual([[1n, 2n]]);
    expect([...db.prepare("SELECT 1 AS x UNION ALL SELECT 2").iterate([])]).toEqual([
      { x: 1n },
      { x: 2n },
    ]);
    expect(db.pragma("foreign_keys", { simple: true })).toBe(1n);
    expect(db.pragma("foreign_keys")).toEqual([{ foreign_keys: 1n }]);
  });

  it("retains bigint precision and rejects lossy inputs", () => {
    const db = open();
    expect(db.get("SELECT 9223372036854775807 AS n")).toEqual({ n: 9223372036854775807n });
    expect(db.prepare("SELECT ? AS n").get(9223372036854775807n)).toEqual({
      n: 9223372036854775807n,
    });
    expect(db.prepare("SELECT ? AS n").get(-9223372036854775808n)).toEqual({
      n: -9223372036854775808n,
    });
    expect(() => db.get("SELECT ?", [9223372036854775808n])).toThrow(RangeError);
    expect(() => db.get("SELECT ?", [Number.MAX_SAFE_INTEGER + 1])).toThrow(RangeError);
    expect(db.get("SELECT 1 AS n")).toEqual({ n: 1n });
    expect(db.query("SELECT 1").lastInsertRowid).toBe(0n);
  });

  it("keeps float values and null distinct from integers", () => {
    const db = open();
    expect(db.get("SELECT 1 AS i, 1.0 AS f, NULL AS n")).toEqual({ i: 1n, f: 1, n: null });
    expect(() => db.get("SELECT ?", [Infinity])).toThrow(TypeError);
  });

  it("validates extended types while returning SQLite storage values", () => {
    const db = open();
    db.exec(
      "CREATE TABLE t(active BOOLEAN, payload JSON, amount DECIMAL(12,2), at INSTANT, small UINT8)",
    );
    db.run("INSERT INTO t VALUES(?,?,?,?,?)", [
      true,
      '{"x":1}',
      "12.50",
      "2024-02-29T16:00:00+04:00",
      255,
    ]);
    expect(db.get("SELECT * FROM t")).toEqual({
      active: 1n,
      payload: '{"x":1}',
      amount: "12.50",
      at: "2024-02-29T12:00:00.000000000Z",
      small: 255n,
    });
    expect(() => db.run("UPDATE t SET small=?", [256])).toThrow();
    expect(db.get("SELECT small FROM t")).toEqual({ small: 255n });
  });

  it.each(["binary", "readable"] as const)(
    "persists %s files using the Rust storage implementation",
    (format) => {
      const path = join(directory(), "data.db");
      const db = open(path, { format });
      db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)");
      db.run("INSERT INTO t(name) VALUES(?)", ["Ada"]);
      db.close();
      expect(open(path).all("SELECT * FROM t")).toEqual([{ id: 1n, name: "Ada" }]);
      expect(readFileSync(path).toString().startsWith("SQLite format 3")).toBe(format === "binary");
    },
  );

  it("prepares without executing DDL and rejects multiple statements", () => {
    const db = open();
    const create = db.prepare("CREATE TABLE t(n INTEGER)");
    expect(db.all("SELECT name FROM sqlite_schema WHERE name='t'")).toEqual([]);
    create.run();
    expect(() => db.prepare("SELECT 1; SELECT 2")).toThrow();
    expect(() => db.query("SELECT 1; DELETE FROM t")).toThrow();
    expect(db.get("SELECT 1 AS n; -- trailing comment")).toEqual({ n: 1n });
    expect(() => db.prepare("-- only a comment")).toThrow();
  });

  it("refreshes cached statements after schema changes", () => {
    const db = open();
    db.exec("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)");
    const query = db.prepare("SELECT * FROM t");
    expect(query.get()).toEqual({ n: 1n });
    db.exec("ALTER TABLE t ADD COLUMN s TEXT DEFAULT 'new'");
    expect(query.get()).toEqual({ n: 1n, s: "new" });
  });

  it("closes connections and previously prepared statements", () => {
    const db = open();
    const statement = db.prepare("SELECT 1");
    db.close().close();
    expect(db.open).toBe(false);
    expect(() => statement.get()).toThrow(BoxDBError);
    expect(() => db.prepare("SELECT 1")).toThrow("closed");
  });
});

describe("transactions", () => {
  it("commits, rolls back and isolates nested savepoints", () => {
    const db = open();
    db.exec("CREATE TABLE t(n INTEGER UNIQUE)");
    const insert = db.transaction((n: number) => {
      db.run("INSERT INTO t VALUES(?)", [n]);
      return n;
    });
    expect(insert.immediate(1)).toBe(1);
    const outer = db.transaction(() => {
      insert(2);
      expect(() =>
        db.transaction(() => {
          insert(3);
          insert(1);
        })(),
      ).toThrow();
      insert(4);
    });
    outer.exclusive();
    expect(db.all("SELECT * FROM t ORDER BY n")).toEqual([{ n: 1n }, { n: 2n }, { n: 4n }]);
    expect(db.inTransaction).toBe(false);
    expect(() =>
      db.transaction(() => {
        insert(5);
        throw new Error("rollback");
      })(),
    ).toThrow("rollback");
    expect(db.all("SELECT * FROM t WHERE n=5")).toEqual([]);
  });

  it("rolls back readable-file transactions before publication", () => {
    const path = join(directory(), "data.yaml");
    const db = open(path, { format: "readable" });
    db.exec("CREATE TABLE t(n INTEGER)");
    expect(() =>
      db.transaction(() => {
        db.run("INSERT INTO t VALUES(1)");
        throw new Error("stop");
      })(),
    ).toThrow("stop");
    db.close();
    expect(open(path).all("SELECT * FROM t")).toEqual([]);
  });

  it("rejects asynchronous transaction callbacks", () => {
    const db = open();
    expect(() => db.transaction(async () => 1)()).toThrow("synchronous");
    expect(db.inTransaction).toBe(false);
  });
});
