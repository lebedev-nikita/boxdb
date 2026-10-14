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

test.each(["commit", "rollback"] as const)(
  "root writes wait for transaction %s and execute outside it",
  async (finish) => {
    await using db = await Database.open();
    await db.exec("CREATE TABLE item(id INTEGER)");
    await using tx = await db.begin();
    await tx.execute("INSERT INTO item VALUES(1)");
    const pending = db.execute("INSERT INTO item VALUES(2)");
    const written = expect(pending).resolves.toMatchObject({ changes: 1n });
    void written.catch(() => {});
    try {
      expect((await tx.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
      expect(db.diagnostics.pendingQueries).toBe(1);
      await tx[finish]();
      await written;
      expect((await db.execute("SELECT * FROM item ORDER BY id")).rows).toEqual(
        finish === "commit" ? [{ id: 1n }, { id: 2n }] : [{ id: 2n }],
      );
    } finally {
      await tx.rollback();
      await written;
    }
  },
);

test("root requests queued behind begin execute in order after the transaction", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  const beginning = db.begin();
  const first = db.exec("INSERT INTO item VALUES(2)");
  const second = db.query`SELECT * FROM item ORDER BY id`.execute();
  await using tx = await beginning;
  try {
    await tx.execute("INSERT INTO item VALUES(1)");
    await tx.commit();
    await first;
    expect(await second).toEqual([{ id: 1n }, { id: 2n }]);
  } finally {
    await tx.rollback();
    await Promise.all([first, second]);
  }
});

test("root transaction requests also wait for the current transaction to finish", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  await using first = await db.begin();
  const beginning = db.begin();
  try {
    await first.execute("INSERT INTO item VALUES(1)");
    await first.rollback();
    await using second = await beginning;
    await second.execute("INSERT INTO item VALUES(2)");
    await second.commit();
    expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 2n }]);
  } finally {
    await first.rollback();
    await (await beginning).rollback();
  }
});

test("waiting root queries consume capacity and can be cancelled without cancelling the transaction", async () => {
  await using db = await Database.open(":memory:", { maxPendingQueries: 1 });
  await db.exec("CREATE TABLE item(id INTEGER)");
  await using tx = await db.begin();
  const controller = new AbortController();
  const reason = new Error("cancel waiting root query");
  const pending = db.execute("INSERT INTO item VALUES(2)", [], { signal: controller.signal });
  const rejected = expect(pending).rejects.toBe(reason);
  await expect(db.execute("SELECT 1")).rejects.toMatchObject({ code: "BOXDB_QUEUE_FULL" });
  controller.abort(reason);
  await rejected;
  expect(db.diagnostics.pendingQueries).toBe(0);
  await tx.execute("INSERT INTO item VALUES(1)");
  await tx.commit();
  expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
});

test("draining close executes accepted root queries after the transaction finishes", async () => {
  const db = await Database.open(":memory:", { maxPendingQueries: 1 });
  try {
    await using tx = await db.begin();
    const pending = db.execute("SELECT 7 AS answer");
    const closed = db.close();
    await tx.commit();
    expect((await pending).rows).toEqual([{ answer: 7n }]);
    await closed;
  } finally {
    await db.close({ mode: "abort" });
  }
});

test("using transactions own their connection and roll back unfinished work", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER PRIMARY KEY)");
  {
    await using tx = await db.begin();
    await tx.query`INSERT INTO item VALUES (${1})`;
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
