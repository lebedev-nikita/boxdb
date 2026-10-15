import { expect, test } from "vitest";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BoxDB, BoxDBError, MigrationSet } from "../src/index.js";

const files = [
  {
    filename: "20261015000000_create_events.sql",
    sql: "-- migrate:up\nCREATE TABLE events(at INSTANT);\n-- migrate:down\nDROP TABLE events;\n",
  },
];

test("embedded migrations apply once and roll back through the client", async () => {
  const migrations = MigrationSet.fromFiles(files);
  await using db = await BoxDB.open();
  expect(await migrations.status(db)).toEqual([
    { version: "20261015000000", name: "create_events", state: "pending" },
  ]);
  expect(await migrations.up(db)).toEqual([{ version: "20261015000000", name: "create_events" }]);
  expect(await migrations.up(db)).toEqual([]);
  await db.exec("INSERT INTO events VALUES('2024-01-01T00:00:00Z')");
  expect(await db.query`SELECT at FROM events`.get()).toEqual({
    at: "2024-01-01T00:00:00.000000Z",
  });
  expect(await migrations.down(db)).toEqual({ version: "20261015000000", name: "create_events" });
  expect(await migrations.down(db)).toBeNull();
  expect((await migrations.status(db))[0]?.state).toBe("pending");
});

test("cancelling a migration made of short statements rolls it back before releasing queued work", async () => {
  await using db = await BoxDB.open();
  await MigrationSet.fromFiles(files).up(db);
  const migrations = MigrationSet.fromFiles([
    ...files,
    {
      filename: "20261015000001_insert_events.sql",
      sql: `-- migrate:up\n${"INSERT INTO events VALUES('2024-01-01T00:00:00Z');\n".repeat(5000)}-- migrate:down\nDELETE FROM events;\n`,
    },
  ]);
  const controller = new AbortController();
  const reason = new Error("stop migration");
  const pending = migrations.up(db, { signal: controller.signal });
  const check = expect(pending).rejects.toBe(reason);
  const rows = db.query`SELECT COUNT(*) AS n FROM events`.get();
  const timer = setTimeout(() => controller.abort(reason), 20);
  try {
    await check;
    expect(await rows).toEqual({ n: 0n });
    expect((await migrations.status(db))[1]?.state).toBe("pending");
    await db.exec("INSERT INTO events VALUES('2024-01-01T00:00:00Z')");
  } finally {
    clearTimeout(timer);
    await rows;
  }
}, 10000);

test.each(["binary", "readable"] as const)(
  "directory and embedded migrations share %s history",
  async (format) => {
    const directory = await mkdtemp(join(tmpdir(), "boxdb-migrations-"));
    try {
      await writeFile(join(directory, files[0]!.filename), files[0]!.sql);
      await writeFile(join(directory, "README.txt"), "ignored");
      const migrations = await MigrationSet.read(directory);
      await using db = await BoxDB.open(join(directory, "data.db"), { format });
      await migrations.up(db);
      const embedded = MigrationSet.fromFiles([
        { ...files[0]!, sql: files[0]!.sql.replaceAll("\n", "\r\n") },
      ]);
      expect(await embedded.up(db)).toEqual([]);
      expect(await embedded.status(db)).toEqual([
        { version: "20261015000000", name: "create_events", state: "applied" },
      ]);
      await embedded.down(db);
      expect((await migrations.status(db))[0]?.state).toBe("pending");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test("validates the full immutable set before execution and preserves engine errors", async () => {
  expect(() => MigrationSet.fromFiles([files[0]!, files[0]!])).toThrow(BoxDBError);
  expect(() =>
    MigrationSet.fromFiles([{ ...files[0]!, sql: "-- migrate:up\nBEGIN;\n-- migrate:down\n" }]),
  ).toThrow(/BEGIN/);
  const documents = files.map((file) => ({ ...file }));
  const migrations = MigrationSet.fromFiles(documents);
  documents[0]!.sql = "invalid";
  await using db = await BoxDB.open();
  await migrations.up(db);
  const failed = MigrationSet.fromFiles([
    ...files,
    {
      filename: "20261015000001_duplicate.sql",
      sql: "-- migrate:up\nCREATE TABLE names(name TEXT UNIQUE); INSERT INTO names VALUES('Ada'); INSERT INTO names VALUES('Ada');\n-- migrate:down\nDROP TABLE names;",
    },
  ]);
  await expect(failed.up(db)).rejects.toMatchObject({
    code: "BOXDB_CONSTRAINT_UNIQUE",
    resultCode: 19,
    extendedCode: 2067,
    message: expect.stringContaining("20261015000001 up"),
  });
  expect((await failed.status(db)).map((entry) => entry.state)).toEqual(["applied", "pending"]);
  await expect(db.execute("SELECT * FROM names")).rejects.toThrow();
  const changed = MigrationSet.fromFiles([{ ...files[0]!, sql: files[0]!.sql + "-- changed\n" }]);
  await expect(changed.up(db)).rejects.toMatchObject({ code: "BOXDB_VALIDATION" });
});

test("migration requests wait for a root transaction and can be cancelled while queued", async () => {
  await using db = await BoxDB.open();
  const migrations = MigrationSet.fromFiles(files);
  await using tx = await db.begin();
  // @ts-expect-error Transaction handles cannot run migrations.
  await expect(migrations.up(tx)).rejects.toBeInstanceOf(TypeError);
  const controller = new AbortController();
  const reason = new Error("cancel queued migration");
  const pending = migrations.up(db, { signal: controller.signal });
  const rejected = expect(pending).rejects.toBe(reason);
  controller.abort(reason);
  await rejected;
  expect(tx.active).toBe(true);
  const applied = migrations.up(db);
  await tx.rollback();
  expect(await applied).toHaveLength(1);
});

test("rejects invalid UTF-8 files instead of changing their checksum through replacement decoding", async () => {
  const directory = await mkdtemp(join(tmpdir(), "boxdb-migrations-"));
  try {
    await writeFile(
      join(directory, files[0]!.filename),
      Buffer.concat([Buffer.from(files[0]!.sql + "-- "), Buffer.from([0xff])]),
    );
    await expect(MigrationSet.read(directory)).rejects.toMatchObject({ code: "BOXDB_IO" });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test.each(["signal", "close"] as const)("cancels a writer lock wait through %s", async (mode) => {
  const directory = await mkdtemp(join(tmpdir(), "boxdb-migrations-"));
  try {
    const path = join(directory, "data.db");
    await using holder = await BoxDB.open(path);
    await using db = await BoxDB.open(path);
    await using lock = await holder.begin();
    const migrations = MigrationSet.fromFiles(files);
    const controller = new AbortController();
    const reason = new Error("cancel writer wait");
    const pending = migrations.up(db, { signal: controller.signal });
    const rejected =
      mode === "signal"
        ? expect(pending).rejects.toBe(reason)
        : expect(pending).rejects.toMatchObject({ code: "BOXDB_CLOSED" });
    const timer = setTimeout(() => {
      if (mode === "signal") controller.abort(reason);
      else void db.close({ mode: "abort" });
    }, 30);
    try {
      await rejected;
    } finally {
      clearTimeout(timer);
      await lock.rollback();
    }
    if (mode === "signal") expect(await migrations.up(db)).toHaveLength(1);
    else await expect(migrations.up(db)).rejects.toMatchObject({ code: "BOXDB_CLOSED" });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
