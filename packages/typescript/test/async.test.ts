import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";
import { BoxDB } from "../src/index.js";

const connections: BoxDB[] = [];
const directories: string[] = [];

it("opens the primary BoxDB API asynchronously", async () => {
  const db = await BoxDB.open();
  try {
    await db.exec("CREATE TABLE users(name TEXT)");
    await db.execute("INSERT INTO users VALUES(?)", ["Ada"]);
    expect((await db.execute("SELECT name FROM users")).rows[0]).toEqual({ name: "Ada" });
  } finally {
    await db.close();
  }
});
async function open(path?: string): Promise<BoxDB> {
  const db = await BoxDB.open(path);
  connections.push(db);
  return db;
}
afterEach(async () => {
  await Promise.all(connections.splice(0).map((db) => db.close()));
  directories.splice(0).forEach((path) => rmSync(path, { recursive: true, force: true }));
});

it("keeps timers running while a write waits for another connection's lock", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-async-"));
  directories.push(directory);
  const path = join(directory, "data.db");
  await using holder = await BoxDB.open(path);
  await holder.exec("CREATE TABLE users(name TEXT)");
  const db = await open(path);
  const lock = await holder.begin({ mode: "immediate" });
  let timerRan = false;
  const timer = setTimeout(() => {
    timerRan = true;
    void lock.rollback().catch(() => {});
  }, 50);
  try {
    const result = await db.execute("INSERT INTO users VALUES(?)", ["Ada"]);
    expect(timerRan).toBe(true);
    expect(result.changes).toBe(1n);
    expect((await db.execute("SELECT name FROM users")).rows[0]).toEqual({ name: "Ada" });
  } finally {
    clearTimeout(timer);
    await lock.rollback();
  }
});

it("cancels a default transaction waiting for a writer and keeps the connection usable", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-begin-cancel-"));
  directories.push(directory);
  const path = join(directory, "data.db");
  await using holder = await BoxDB.open(path);
  await holder.exec("CREATE TABLE item(id INTEGER)");
  const db = await open(path);
  await using lock = await holder.begin();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 30);
  try {
    await expect(db.begin({ signal: controller.signal })).rejects.toMatchObject({
      name: "AbortError",
    });
  } finally {
    clearTimeout(timer);
    await lock.rollback();
  }
  await using tx = await db.begin();
  await tx.execute("INSERT INTO item VALUES(1)");
  await tx.commit();
  expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
});

it.each([false, true])(
  "cancels a lock wait promptly even when a script changes busy_timeout: %s",
  async (script) => {
    const directory = mkdtempSync(join(tmpdir(), "boxdb-async-"));
    directories.push(directory);
    const path = join(directory, "data.db");
    await using holder = await BoxDB.open(path);
    await holder.exec("CREATE TABLE users(name TEXT)");
    const db = await open(path);
    const lock = await holder.begin({ mode: "immediate" });
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 50);
    const start = performance.now();
    try {
      const pending = script
        ? db.exec("PRAGMA busy_timeout=5000; INSERT INTO users VALUES('Ada')", {
            signal: controller.signal,
          })
        : db.execute("INSERT INTO users VALUES(?)", ["Ada"], { signal: controller.signal });
      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      expect(performance.now() - start).toBeLessThan(1000);
    } finally {
      clearTimeout(timer);
      await lock.rollback();
    }
    expect((await db.execute("SELECT * FROM users")).rows).toEqual([]);
    expect((await db.execute("INSERT INTO users VALUES(?)", ["Grace"])).changes).toBe(1n);
  },
  10000,
);

it("does not execute writes outside a transaction after SQLite interrupts and rolls it back", async () => {
  const db = await open();
  await db.exec("CREATE TABLE numbers(n INTEGER)");
  const tx = await db.begin();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 50);
  try {
    await expect(
      tx.execute(
        "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<100000000) INSERT INTO numbers SELECT n FROM numbers",
        [],
        { signal: controller.signal },
      ),
    ).rejects.toMatchObject({ name: "AbortError" });
    await expect(tx.execute("INSERT INTO numbers VALUES(999)")).rejects.toThrow(
      /transaction.*ended/,
    );
  } finally {
    clearTimeout(timer);
    await tx.rollback();
  }
  expect((await db.execute("SELECT * FROM numbers")).rows).toEqual([]);
});

it("drains accepted queries on close and rejects new queries", async () => {
  const db = await open();
  await db.exec("CREATE TABLE users(name TEXT)");
  const write = db.execute("INSERT INTO users VALUES('Ada')");
  const closed = db.close();
  expect(db.open).toBe(false);
  expect((await write).changes).toBe(1n);
  await closed;
  await db.close();
  await expect(db.execute("SELECT * FROM users")).rejects.toThrow(/closed/);
});

it("validates format options before opening a connection", async () => {
  await expect(BoxDB.open(":memory:", { format: "invalid" as "binary" })).rejects.toBeInstanceOf(
    TypeError,
  );
});

it("cancels queued work with the caller's reason and never executes it", async () => {
  const db = await open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const active = new AbortController();
  const running =
    db.query`WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n`.execute(
      { signal: active.signal },
    );
  const failure = expect(running).rejects.toMatchObject({ name: "AbortError" });
  const queued = new AbortController();
  const reason = new Error("no longer needed");
  const write = db.query`INSERT INTO item VALUES(1)`.execute({ signal: queued.signal });
  queued.abort(reason);
  try {
    await expect(write).rejects.toBe(reason);
  } finally {
    active.abort();
  }
  await failure;
  expect(await db.query`SELECT * FROM item`).toEqual([]);
});

it("aborting an idle transaction rolls back without a cancellable cleanup", async () => {
  const db = await open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const controller = new AbortController();
  const tx = await db.begin({ signal: controller.signal });
  await tx.query`INSERT INTO item VALUES(1)`;
  controller.abort();
  await tx[Symbol.asyncDispose]();
  expect(tx.active).toBe(false);
  expect(await db.query`SELECT * FROM item`).toEqual([]);
});

it("transaction timeout interrupts running SQL, cancels queued transaction writes and releases root writes", async () => {
  const db = await open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  await using tx = await db.begin({ timeoutMs: 50 });
  await tx.execute("INSERT INTO item VALUES(1)");
  const running = tx.execute(
    "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) INSERT INTO item SELECT x FROM n",
  );
  const pending = db.execute("INSERT INTO item VALUES(2)");
  const queued = tx.execute("INSERT INTO item VALUES(3)");
  const failed = Promise.all([
    expect(running).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" }),
    expect(queued).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" }),
  ]);
  try {
    await failed;
    await pending;
    expect(tx.active).toBe(false);
    expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 2n }]);
  } finally {
    await tx.rollback();
    await pending;
  }
});

it("abort close cancels root queries waiting behind a transaction", async () => {
  const db = await open();
  await using tx = await db.begin();
  const pending = db.execute("SELECT 1");
  const failed = expect(pending).rejects.toMatchObject({ code: "BOXDB_CLOSED" });
  await db.close({ mode: "abort" });
  await failed;
  expect(tx.active).toBe(false);
});
