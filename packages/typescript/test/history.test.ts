import { expect, test } from "vitest";
import { BoxDB, Changeset } from "../src/index.js";

const schema = `
  CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT NOT NULL);
  CREATE TABLE history(id INT64 PRIMARY KEY, label TEXT, changes CHANGESET NOT NULL);
  CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
    INSERT INTO history(label, changes) VALUES(boxdb_context('label'), boxdb_changeset());
  END`;

test("commit triggers record transactions and changesets undo them", async () => {
  await using db = await BoxDB.open();
  await db.exec(schema);
  await db.query`INSERT INTO tasks VALUES(${1n}, ${"a"})`;
  {
    await using tx = await db.begin({ context: { label: "rename" } });
    await tx.query`UPDATE tasks SET title=${"b"} WHERE id=${1n}`;
    await tx.commit();
  }
  const rows = (await db.query`SELECT label, changes FROM history ORDER BY id`) as {
    label: unknown;
    changes: unknown;
  }[];
  expect(rows.map((row) => row.label)).toEqual([null, "rename"]);
  expect(rows[1]!.changes).toBeInstanceOf(Changeset);
  const rename = rows[1]!.changes as Changeset;

  {
    await using tx = await db.begin();
    expect(await tx.apply(rename, { invert: true })).toEqual({
      applied: 1,
      skipped: 0,
      conflicts: [],
    });
    await tx.commit();
  }
  expect(await db.query`SELECT title FROM tasks`).toEqual([{ title: "a" }]);
  expect(await db.query`SELECT count(*) AS n FROM history`.get()).toEqual({ n: 3n });

  {
    await using tx = await db.begin({ record: true });
    await tx.query`DELETE FROM tasks`;
    const changes = await tx.changeset();
    expect(changes).toBeInstanceOf(Changeset);
    expect(
      await tx.query`SELECT operation, column_name, old FROM changeset_rows(${changes!}) ORDER BY position`,
    ).toEqual([
      { operation: "delete", column_name: "id", old: 1n },
      { operation: "delete", column_name: "title", old: "a" },
    ]);
    await tx.rollback();
  }
  expect(await db.query`SELECT count(*) AS n FROM history`.get()).toEqual({ n: 3n });

  await db.query`UPDATE tasks SET title=${"edited"}`;
  await expect(db.apply(rename, { invert: true })).rejects.toMatchObject({
    code: "BOXDB_CONSTRAINT_FUNCTION",
  });
  expect(await db.apply(rename, { invert: true, onConflict: "skip" })).toEqual({
    applied: 0,
    skipped: 1,
    conflicts: [{ table: "tasks", operation: "update", kind: "data", key: [1] }],
  });
  expect(await db.query`SELECT title FROM tasks`).toEqual([{ title: "edited" }]);
  await db.query`INSERT INTO history(label, changes) VALUES(${"manual"}, ${rename})`;
  expect(await db.query`SELECT typeof(changes) AS kind FROM history WHERE label='manual'`).toEqual([
    { kind: "blob" },
  ]);
  await expect(db.query`INSERT INTO history(changes) VALUES(${"text"})`).rejects.toMatchObject({
    code: "BOXDB_CONSTRAINT_DATATYPE",
  });
  await expect(
    db.query`INSERT INTO history(changes) VALUES(${new Uint8Array([1, 2, 3])})`,
  ).rejects.toMatchObject({ code: "BOXDB_CONSTRAINT_FUNCTION" });
});

test("recording and context require a transaction and stay transaction-scoped", async () => {
  await using db = await BoxDB.open();
  await db.exec(schema);
  await expect(db.execute("SELECT boxdb_changeset()")).rejects.toMatchObject({
    code: "BOXDB_CONSTRAINT_FUNCTION",
  });
  await using tx = await db.begin({ record: true, context: { label: "first" } });
  expect(await tx.changeset()).toBeNull();
  expect(await tx.query`SELECT boxdb_context('label') AS label`).toEqual([{ label: "first" }]);
  await tx.commit();
  await using next = await db.begin();
  expect(await next.query`SELECT boxdb_context('label') AS label`).toEqual([{ label: null }]);
  await next.rollback();
});
