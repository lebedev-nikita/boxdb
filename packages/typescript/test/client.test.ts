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
  it("executes parameterized writes and queries without preparing explicitly", () => {
    const db = open();
    db.exec("CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE)");
    expect(db.query("INSERT INTO users(name) VALUES (?)", ["Ada"])).toMatchObject({
      rows: [],
      changes: 1,
      lastInsertRowid: 1,
    });
    expect(db.all("SELECT * FROM users WHERE name = :name", { name: "Ada" })).toEqual([
      { id: 1, name: "Ada" },
    ]);
    expect(db.get("SELECT * FROM users WHERE id = ?", [2])).toBeUndefined();
    expect(db.query("INSERT INTO users(name) VALUES(?) RETURNING id", ["Grace"]).rows).toEqual([
      { id: 2 },
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
      expect(db.prepare(insert).run(parameters)).toEqual(sqlite.prepare(insert).run(...parameters));
      const sql = "SELECT id, text AS label, n, b, NULL AS missing FROM t";
      const actual = db.all(sql);
      const expected = sqlite
        .prepare(sql)
        .all()
        .map((row) => ({ ...row, b: Buffer.from(row.b as Uint8Array) }));
      expect(actual).toEqual(expected);
      expect(db.run("UPDATE t SET n = n + 1 WHERE id = ?", [1]).changes).toBe(1);
      expect(db.run("UPDATE t SET n = n + 1 WHERE id = ?", [9]).changes).toBe(0);
    } finally {
      sqlite.close();
    }
  });

  it("supports bound arrays, variadic and named statement arguments", () => {
    const db = open();
    const statement = db.prepare("SELECT ? AS a, ? AS b");
    expect(statement.get(1, "x")).toEqual({ a: 1, b: "x" });
    expect(statement.get([2, "y"])).toEqual({ a: 2, b: "y" });
    expect(db.prepare("SELECT @value AS a, @value AS b").get({ value: "same" })).toEqual({
      a: "same",
      b: "same",
    });
    expect(db.prepare("SELECT $a AS a, ? AS b").get({ $a: "named" }, 3)).toEqual({
      a: "named",
      b: 3,
    });
    expect(() => statement.get(1)).toThrow("Too few");
    expect(() => statement.get(1, 2, 3)).toThrow("Too many");
    expect(() => db.get("SELECT :a", { other: 1 })).toThrow("Missing");
    expect(() => db.get("SELECT :a", { a: 1, extra: 2 })).toThrow("Too many");
    expect(() => db.prepare("SELECT ?").get(undefined)).toThrow(TypeError);
  });

  it("supports raw rows, repeated column names, iterators and pragmas", () => {
    const db = open();
    expect(db.prepare("SELECT 1 AS x, 2 AS x").raw().all()).toEqual([[1, 2]]);
    expect([...db.prepare("SELECT 1 AS x UNION ALL SELECT 2").iterate([])]).toEqual([
      { x: 1 },
      { x: 2 },
    ]);
    expect(db.pragma("foreign_keys", { simple: true })).toBe(1);
    expect(db.pragma("foreign_keys")).toEqual([{ foreign_keys: 1 }]);
  });

  it("retains bigint precision and rejects lossy inputs", () => {
    const db = open();
    expect(() => db.get("SELECT 9223372036854775807 AS n")).toThrow(RangeError);
    expect(db.prepare("SELECT ? AS n").safeIntegers().get(9223372036854775807n)).toEqual({
      n: 9223372036854775807n,
    });
    expect(db.prepare("SELECT ? AS n").safeIntegers().get(-9223372036854775808n)).toEqual({
      n: -9223372036854775808n,
    });
    expect(() => db.get("SELECT ?", [9223372036854775808n])).toThrow("64-bit range");
    expect(() => db.get("SELECT ?", [Number.MAX_SAFE_INTEGER + 1])).toThrow(RangeError);
    db.defaultSafeIntegers();
    expect(db.get("SELECT 1 AS n")).toEqual({ n: 1n });
    expect(db.query("SELECT 1").lastInsertRowid).toBe(0n);
  });

  it("keeps float values and null distinct from integers", () => {
    const db = open(undefined, { safeIntegers: true });
    expect(db.get("SELECT 1 AS i, 1.0 AS f, NULL AS n")).toEqual({ i: 1n, f: 1, n: null });
    expect(() => db.get("SELECT ?", [Infinity])).toThrow(TypeError);
  });

  it("validates extended types while returning SQLite storage values", () => {
    const db = open();
    db.exec(
      "CREATE TABLE t(active BOOLEAN, payload JSON, amount NUMERIC(12,2), at INSTANT, small UINT8)",
    );
    db.run("INSERT INTO t VALUES(?,?,?,?,?)", [
      true,
      '{"x":1}',
      "12.50",
      "2024-02-29T16:00:00+04:00",
      255,
    ]);
    expect(db.get("SELECT * FROM t")).toEqual({
      active: 1,
      payload: '{"x":1}',
      amount: "12.50",
      at: "2024-02-29T12:00:00.000000000Z",
      small: 255,
    });
    expect(() => db.run("UPDATE t SET small=?", [256])).toThrow();
    expect(db.get("SELECT small FROM t")).toEqual({ small: 255 });
  });

  it.each(["binary", "readable"] as const)(
    "persists %s files using the Rust storage implementation",
    (format) => {
      const path = join(directory(), "data.db");
      const db = open(path, { format });
      db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)");
      db.run("INSERT INTO t(name) VALUES(?)", ["Ada"]);
      db.close();
      expect(open(path).all("SELECT * FROM t")).toEqual([{ id: 1, name: "Ada" }]);
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
    expect(db.get("SELECT 1 AS n; -- trailing comment")).toEqual({ n: 1 });
    expect(() => db.prepare("-- only a comment")).toThrow();
  });

  it("refreshes cached statements after schema changes", () => {
    const db = open();
    db.exec("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)");
    const query = db.prepare("SELECT * FROM t");
    expect(query.get()).toEqual({ n: 1 });
    db.exec("ALTER TABLE t ADD COLUMN s TEXT DEFAULT 'new'");
    expect(query.get()).toEqual({ n: 1, s: "new" });
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
    expect(db.all("SELECT * FROM t ORDER BY n")).toEqual([{ n: 1 }, { n: 2 }, { n: 4 }]);
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
