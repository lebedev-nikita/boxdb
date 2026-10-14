import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, vi } from "vitest";
import { Database } from "../src/index.js";

test("begin reserves the writer before its first query by default", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-default-transaction-"));
  try {
    const path = join(directory, "data.db");
    await using db = await Database.open(path);
    await db.exec("CREATE TABLE item(id INTEGER)");
    await using other = await Database.open(path, { busyTimeoutMs: 30 });
    await using tx = await db.begin();
    await expect(other.execute("INSERT INTO item VALUES(1)")).rejects.toMatchObject({
      code: "BOXDB_BUSY",
    });
    await tx.rollback();
    expect((await other.execute("INSERT INTO item VALUES(2)")).changes).toBe(1n);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("readOnly transactions reject writes and release the connection for writers", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER); INSERT INTO item VALUES(1)");
  await using tx = await db.begin({ readOnly: true });
  expect((await tx.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
  await expect(tx.execute("INSERT INTO item VALUES(2)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await tx.commit();
  expect((await db.execute("INSERT INTO item VALUES(3)")).changes).toBe(1n);
  expect((await db.execute("SELECT * FROM item ORDER BY id")).rows).toEqual([
    { id: 1n },
    { id: 3n },
  ]);
});

test("readOnly transactions keep a snapshot while another connection writes", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-read-transaction-"));
  try {
    const path = join(directory, "data.db");
    await using db = await Database.open(path);
    await db.exec(
      "CREATE TABLE item(id INTEGER, at INSTANT); INSERT INTO item VALUES(1,'2026-10-14T10:00:00Z')",
    );
    await using other = await Database.open(path, { busyTimeoutMs: 30 });
    await using tx = await db.begin({ readOnly: true });
    expect((await tx.execute("SELECT id FROM item")).rows).toEqual([{ id: 1n }]);
    await other.execute("INSERT INTO item VALUES(2,'2026-10-14T11:00:00Z')");
    expect((await tx.execute("SELECT id FROM item")).rows).toEqual([{ id: 1n }]);
    await tx.commit();
    expect((await db.execute("SELECT id FROM item ORDER BY id")).rows).toEqual([
      { id: 1n },
      { id: 2n },
    ]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("readOnly transactions enforce writes across query and script entry points", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER); INSERT INTO item VALUES(1)");
  await using tx = await db.begin({ readOnly: true });
  await expect(tx.query`INSERT INTO item VALUES(2) RETURNING id`.get()).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await expect(tx.exec("PRAGMA query_only=OFF; INSERT INTO item VALUES(3)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await tx.execute("PRAGMA query_only=OFF");
  await expect(tx.exec("CREATE TEMP TABLE scratch(id INTEGER)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await expect(
    tx.execute(
      "WITH chosen AS(SELECT id FROM item) UPDATE item SET id=4 WHERE id IN(SELECT id FROM chosen) RETURNING id",
    ),
  ).rejects.toMatchObject({ code: "BOXDB_READONLY" });
  expect((await tx.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
  await tx.rollback();
  expect((await db.execute("INSERT INTO item VALUES(5)")).changes).toBe(1n);
});

test("savepoints inherit readOnly and rollback releases waiting root writes", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER)");
  await using tx = await db.begin({ readOnly: true });
  await using child = await tx.begin();
  await expect(child.execute("INSERT INTO item VALUES(1)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await child.commit();
  await expect(tx.execute("INSERT INTO item VALUES(2)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  const root = db.execute("INSERT INTO item VALUES(3)");
  await tx.rollback();
  await root;
  expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 3n }]);
});

test("readOnly restores an existing query_only setting after commit", async () => {
  await using db = await Database.open();
  await db.exec("CREATE TABLE item(id INTEGER); PRAGMA query_only=ON");
  await using tx = await db.begin({ readOnly: true });
  await tx.execute("SELECT * FROM item");
  await tx.commit();
  await expect(db.execute("INSERT INTO item VALUES(1)")).rejects.toMatchObject({
    code: "BOXDB_READONLY",
  });
  await db.execute("PRAGMA query_only=OFF");
  expect((await db.execute("INSERT INTO item VALUES(2)")).changes).toBe(1n);
});

test("readOnly timeout restores writes before releasing waiting root requests", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    await using db = await Database.open(":memory:", { transactionTimeoutMs: 20 });
    await db.exec("CREATE TABLE item(id INTEGER)");
    await using tx = await db.begin({ readOnly: true });
    await tx.execute("SELECT * FROM item");
    const pending = db.execute("INSERT INTO item VALUES(1)");
    await vi.advanceTimersByTimeAsync(20);
    await pending;
    await expect(tx.commit()).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" });
    expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 1n }]);
  } finally {
    vi.useRealTimers();
  }
});

test.each(["immediate", "exclusive"] as const)(
  "readOnly rejects a conflicting %s mode before acquiring the connection",
  async (mode) => {
    await using db = await Database.open();
    await expect(db.begin({ readOnly: true, mode })).rejects.toBeInstanceOf(TypeError);
    await using tx = await db.begin();
    await tx.commit();
  },
);

test.each([null, 1, "true"])("rejects an invalid readOnly option %j", async (readOnly) => {
  await using db = await Database.open();
  const beginning = db.begin({ readOnly: readOnly as never });
  try {
    await expect(beginning).rejects.toBeInstanceOf(TypeError);
  } finally {
    await (await beginning.catch(() => undefined))?.rollback();
  }
});

test("the client transaction timeout rolls back before releasing waiting root writes", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    await using db = await Database.open(":memory:", { transactionTimeoutMs: 20 });
    await db.exec("CREATE TABLE item(id INTEGER)");
    await using tx = await db.begin();
    await tx.execute("INSERT INTO item VALUES(1)");
    const pending = db.execute("INSERT INTO item VALUES(2)");
    try {
      await vi.advanceTimersByTimeAsync(20);
      await expect(tx.execute("SELECT 1")).rejects.toMatchObject({
        code: "BOXDB_TRANSACTION_TIMEOUT",
      });
      await pending;
      expect(tx.active).toBe(false);
      await expect(tx.commit()).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" });
      expect((await db.execute("SELECT * FROM item")).rows).toEqual([{ id: 2n }]);
    } finally {
      await tx.rollback();
      await pending;
    }
  } finally {
    vi.useRealTimers();
  }
});

test.each([
  { client: undefined, transaction: undefined, deadline: 5000 },
  { client: 100, transaction: 20, deadline: 20 },
  { client: 20, transaction: 100, deadline: 100 },
])(
  "transaction deadlines use the default and allow shorter or longer overrides: %j",
  async ({ client, transaction, deadline }) => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      await using db = await Database.open(
        ":memory:",
        client === undefined ? {} : { transactionTimeoutMs: client },
      );
      await using tx = await db.begin(transaction === undefined ? {} : { timeoutMs: transaction });
      await vi.advanceTimersByTimeAsync(deadline - 1);
      await tx.execute("SELECT 1");
      await vi.advanceTimersByTimeAsync(1);
      await expect(tx.execute("SELECT 1")).rejects.toMatchObject({
        code: "BOXDB_TRANSACTION_TIMEOUT",
      });
      await db.execute("SELECT 1");
      expect(tx.active).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  },
);

test.each([0, -1, 0.5, NaN, Infinity, 2_147_483_648])(
  "rejects unsupported transaction timeout %s at the client and transaction levels",
  async (timeoutMs) => {
    const opening = Database.open(":memory:", { transactionTimeoutMs: timeoutMs });
    try {
      await expect(opening).rejects.toBeInstanceOf(RangeError);
    } finally {
      await (await opening.catch(() => undefined))?.close();
    }
    await using db = await Database.open();
    const beginning = db.begin({ timeoutMs });
    try {
      await expect(beginning).rejects.toBeInstanceOf(RangeError);
    } finally {
      await (await beginning.catch(() => undefined))?.rollback();
    }
  },
);

test("savepoints inherit the outer deadline and root queries wait beyond their release", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    await using db = await Database.open(":memory:", { transactionTimeoutMs: 50 });
    await db.exec("CREATE TABLE item(id INTEGER)");
    await using tx = await db.begin();
    await tx.execute("INSERT INTO item VALUES(1)");
    await vi.advanceTimersByTimeAsync(40);
    await using child = await tx.begin();
    await child.execute("INSERT INTO item VALUES(2)");
    const pending = db.query`SELECT * FROM item`.execute();
    try {
      await child.commit();
      await tx.execute("INSERT INTO item VALUES(3)");
      expect(db.diagnostics.pendingQueries).toBe(1);
      await vi.advanceTimersByTimeAsync(10);
      await expect(tx.commit()).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" });
      expect(await pending).toEqual([]);
    } finally {
      await tx.rollback();
      await pending;
    }
  } finally {
    vi.useRealTimers();
  }
});

test("a transaction deadline starts after begin acquires the connection", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    await using db = await Database.open(":memory:", { transactionTimeoutMs: 100 });
    await using first = await db.begin();
    const beginning = db.begin({ timeoutMs: 20 });
    try {
      await vi.advanceTimersByTimeAsync(90);
      await first.commit();
      await using second = await beginning;
      await vi.advanceTimersByTimeAsync(19);
      await second.execute("SELECT 1");
      await vi.advanceTimersByTimeAsync(1);
      await expect(second.commit()).rejects.toMatchObject({ code: "BOXDB_TRANSACTION_TIMEOUT" });
    } finally {
      await first.rollback();
      await (await beginning).rollback();
    }
  } finally {
    vi.useRealTimers();
  }
});

test.each(["binary", "readable"] as const)(
  "readOnly opens %s data without permitting writes",
  async (format) => {
    const directory = mkdtempSync(join(tmpdir(), "boxdb-options-"));
    try {
      const path = join(directory, "data.db");
      const writer = await Database.open(path, { format });
      await writer.exec("CREATE TABLE item(id INTEGER); INSERT INTO item VALUES(7)");
      await writer.close();
      await using reader = await Database.open(path, { readOnly: true });
      await using tx = await reader.begin();
      expect(await tx.query`SELECT * FROM item`).toEqual([{ id: 7n }]);
      await expect(tx.query`INSERT INTO item VALUES(8)`.execute()).rejects.toThrow(
        /readonly|read.only/i,
      );
      await tx.commit();
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  },
);

test("busyTimeoutMs bounds lock waiting without blocking timers", async () => {
  const directory = mkdtempSync(join(tmpdir(), "boxdb-busy-"));
  try {
    const path = join(directory, "data.db");
    await using holder = await Database.open(path);
    await holder.exec("CREATE TABLE item(id INTEGER)");
    await using db = await Database.open(path, { busyTimeoutMs: 30 });
    await using _tx = await holder.begin({ mode: "immediate" });
    const start = performance.now();
    await expect(db.query`INSERT INTO item VALUES(1)`.execute()).rejects.toMatchObject({
      code: "BOXDB_BUSY",
    });
    expect(performance.now() - start).toBeLessThan(500);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}, 10000);
