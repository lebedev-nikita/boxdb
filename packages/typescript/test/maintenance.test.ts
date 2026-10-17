import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";
import { BoxDB, BoxDBError } from "../src/index.js";

it("checks and rebuilds files without opening a normal client connection", async () => {
  const directory = await mkdtemp(join(tmpdir(), "boxdb-maintenance-"));
  try {
    const path = join(directory, "data.db");
    const db = await BoxDB.open(path);
    await db.exec("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)");
    await db.close();
    const before = await readFile(path);
    const report = await BoxDB.checkFile(path);
    expect(report.status).toBe("valid");
    expect(report.complete).toBe(true);
    const repaired = await BoxDB.repairFile(path, join(directory, "repaired.db"));
    expect(repaired.recovered_rows).toBe(1);
    const recovered = await BoxDB.recoverFile(path, join(directory, "recovered.db"));
    expect(recovered.loss_possible).toBe(true);
    expect(await readFile(path)).toEqual(before);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it("preserves native failure state without invalidating a connection for a constraint", async () => {
  await using db = await BoxDB.open();
  await db.exec("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)");
  try {
    await db.exec("INSERT INTO items VALUES(1)");
    expect.unreachable();
  } catch (error) {
    expect(error).toBeInstanceOf(BoxDBError);
    const failure = error as BoxDBError;
    expect(failure.operation).toBe("write");
    expect(failure.connectionUsable).toBe(true);
    expect(failure.inTransaction).toBe(false);
  }
  await db.exec("INSERT INTO items VALUES(2)");
});

it("blocks further requests after physical corruption", async () => {
  const directory = await mkdtemp(join(tmpdir(), "boxdb-corruption-"));
  try {
    const path = join(directory, "data.db");
    const original = await BoxDB.open(path);
    await original.exec("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)");
    const root =
      (await original.query`SELECT rootpage FROM sqlite_schema WHERE name='items'`.get()) as {
        rootpage: bigint;
      };
    const page = (await original.query`PRAGMA page_size`.get()) as { page_size: bigint };
    await original.close();
    const bytes = await readFile(path);
    bytes[(Number(root!.rootpage) - 1) * Number(page!.page_size)] = 255;
    await writeFile(path, bytes);
    await using db = await BoxDB.open(path);
    await expect(db.query`SELECT * FROM items`).rejects.toMatchObject({
      code: "BOXDB_CORRUPT",
      connectionUsable: false,
    });
    await expect(db.query`SELECT 1`).rejects.toMatchObject({ code: "BOXDB_UNAVAILABLE" });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
