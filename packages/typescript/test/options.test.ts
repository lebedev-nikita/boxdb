import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, vi } from "vitest";
import { Database } from "../src/index.js";

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
      expect(await reader.query`SELECT * FROM item`).toEqual([{ id: 7n }]);
      await expect(reader.query`INSERT INTO item VALUES(8)`.execute()).rejects.toThrow(
        /readonly|read.only/i,
      );
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
