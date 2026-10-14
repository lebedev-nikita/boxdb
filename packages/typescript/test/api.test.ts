import { expect, test } from "vitest";
import { Database } from "../src/index.js";

test("tagged queries compose identifiers and fragments with bound values", async () => {
  await using db = await Database.open();
  const query = db.query<{
    value: string;
  }>`SELECT ${"O'Reilly"} AS ${db.identifier("value")} ${db.fragment`WHERE ${1} = ${1}`}`;
  expect(query.compile()).toEqual({
    sql: 'SELECT ? AS "value" WHERE ? = ?',
    parameters: ["O'Reilly", 1, 1],
  });
  expect(await query).toEqual([{ value: "O'Reilly" }]);
  expect(await query.get()).toEqual({ value: "O'Reilly" });
  expect(await query.values()).toEqual([["O'Reilly"]]);
});

test("using transactions own their connection and roll back unfinished work", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER PRIMARY KEY)");
  {
    await using tx = await db.begin();
    await tx.query`INSERT INTO item VALUES (${1})`;
    await expect(db.query`SELECT * FROM item`.execute()).rejects.toThrow(/transaction/i);
    {
      await using child = await tx.begin();
      await child.query`INSERT INTO item VALUES (${2})`;
    }
    await tx.commit();
  }
  {
    await using tx = await db.begin();
    await tx.query`INSERT INTO item VALUES (${3})`;
  }
  expect(await db.query`SELECT * FROM item`).toEqual([{ id: 1n }]);
});

test("a failed commit can be repaired and retried", async () => {
  await using db = await Database.open();
  await db.exec(
    "CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
  );
  await using tx = await db.begin();
  await tx.query`INSERT INTO child VALUES (${7})`;
  await expect(tx.commit()).rejects.toThrow();
  expect(tx.active).toBe(true);
  await tx.query`INSERT INTO parent VALUES (${7})`;
  await tx.commit();
  expect(await db.query`SELECT * FROM child`).toEqual([{ id: 7n }]);
});

test("close drains a transaction while allowing its handle to commit", async () => {
  const db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const tx = await db.begin();
  await tx.query`INSERT INTO item VALUES (1)`;
  const closing = db.close();
  await expect(db.query`SELECT 1`.execute()).rejects.toThrow(/closed/);
  await tx.commit();
  await closing;
  expect(tx.active).toBe(false);
});

test("open respects pre-aborted signals", async () => {
  const reason = new Error("cancel open");
  await expect(Database.open(":memory:", { signal: AbortSignal.abort(reason) })).rejects.toBe(
    reason,
  );
});

test("abort close interrupts running work and rolls back an active transaction", async () => {
  const db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const tx = await db.begin();
  const pending =
    tx.query`WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) INSERT INTO item SELECT x FROM n`.execute();
  const rejected = expect(pending).rejects.toThrow();
  await new Promise((resolve) => setTimeout(resolve, 20));
  await db.close({ mode: "abort" });
  await rejected;
  expect(tx.active).toBe(false);
});

test("SQL cannot bypass transaction handle ownership", async () => {
  await using db = await Database.open();
  await expect(db.exec("/* comment */ BEGIN")).rejects.toThrow();
  await using tx = await db.begin();
  await expect(tx.exec("COMMIT")).rejects.toThrow();
  expect(tx.active).toBe(true);
  await tx.rollback();
});

test("run rejects a returning write before it changes data", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  await expect(db.query`INSERT INTO item VALUES(1) RETURNING id`.run()).rejects.toThrow();
  expect(await db.query`SELECT * FROM item`).toEqual([]);
});

test("queue capacity rejects excess work and exposes waiting and execution durations", async () => {
  await using db = await Database.open(":memory:", { maxPendingQueries: 1 });
  const controller = new AbortController();
  const busy =
    db.query`WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n`.execute(
      { signal: controller.signal },
    );
  const failed = expect(busy).rejects.toMatchObject({ name: "AbortError" });
  await new Promise((resolve) => setTimeout(resolve, 20));
  const queued = db.query`SELECT 7 AS answer`.execute();
  await expect(db.query`SELECT 8`.execute()).rejects.toMatchObject({ code: "BOXDB_QUEUE_FULL" });
  expect(db.diagnostics.pendingQueries).toBe(1);
  controller.abort();
  await failed;
  expect(await queued).toEqual([{ answer: 7n }]);
  expect(db.diagnostics.pendingQueries).toBe(0);
  expect(db.diagnostics.lastQueueWaitMs).toBeGreaterThan(0);
  expect(db.diagnostics.lastExecutionMs).toBeGreaterThanOrEqual(0);
});

test("a queued nested begin prevents the parent from committing over its handle", async () => {
  await using db = await Database.open();
  await using tx = await db.begin();
  const nested = tx.begin();
  const committed = tx.commit();
  const rejected = expect(committed).rejects.toThrow(/nested|transaction/i);
  await using child = await nested;
  await rejected;
  expect(tx.active).toBe(true);
  expect(child.active).toBe(true);
  await child.rollback();
  await tx.commit();
  expect(await db.query`SELECT 7 AS answer`.get()).toEqual({ answer: 7n });
});
