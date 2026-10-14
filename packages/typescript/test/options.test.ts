import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vitest";
import { Database } from "../src/index.js";

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
