import { eq, sql } from "drizzle-orm";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { expect, it } from "vitest";
import { Database } from "@boxdb/client";
import { createDrizzleDatabase } from "../src/index.js";

const users = sqliteTable("users", {
  id: integer().primaryKey({ autoIncrement: true }).$type<bigint>(),
  name: text().notNull(),
  active: integer({ mode: "boolean" }).notNull(),
  profile: text({ mode: "json" }).$type<{ theme: string }>(),
});

it("runs asynchronous Drizzle queries, conversions, returning, joins and nested transactions", async () => {
  await using client = await Database.open();
  await client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,active INTEGER NOT NULL,profile TEXT)",
  );
  const db = createDrizzleDatabase(client, { schema: { users } });
  const [ada] = await db
    .insert(users)
    .values({ name: "Ada", active: true, profile: { theme: "dark" } })
    .returning()
    .all();
  expect(ada).toEqual({ id: 1n, name: "Ada", active: true, profile: { theme: "dark" } });
  expect(await db.select().from(users).where(eq(users.id, 1n)).get()).toEqual(ada);
  expect(await db.query.users.findFirst({ where: eq(users.id, 1n) })).toEqual(ada);
  await db.transaction(
    async (tx) => {
      await tx.insert(users).values({ name: "Grace", active: false }).run();
      await expect(
        tx.transaction(async (nested) => {
          await nested.insert(users).values({ name: "rollback", active: true }).run();
          throw new Error("stop");
        }),
      ).rejects.toThrow("stop");
    },
    { behavior: "immediate" },
  );
  expect(await db.select({ name: users.name }).from(users).all()).toEqual([
    { name: "Ada" },
    { name: "Grace" },
  ]);
  expect(
    await db
      .update(users)
      .set({ name: "Ada L" })
      .where(eq(users.id, 1n))
      .returning({ name: users.name })
      .get(),
  ).toEqual({ name: "Ada L" });
  expect(
    await db
      .select()
      .from(users)
      .leftJoin(sql`(SELECT 1 AS other_id) AS other`, sql`${users.id} = other.other_id`)
      .all(),
  ).toHaveLength(2);
  expect(db.$client).toBe(client);
});

it("executes Drizzle batches atomically and returns native write metadata", async () => {
  await using client = await Database.open();
  await client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL UNIQUE,active INTEGER NOT NULL,profile TEXT)",
  );
  const db = createDrizzleDatabase(client);
  const result = await db.insert(users).values({ name: "Ada", active: true }).run();
  expect(result.changes).toBe(1n);
  expect(result.lastInsertRowid).toBe(1n);
  await expect(
    db.batch([
      db.insert(users).values({ name: "Grace", active: false }),
      db.insert(users).values({ name: "Ada", active: true }),
    ]),
  ).rejects.toThrow();
  expect(await db.select({ name: users.name }).from(users)).toEqual([{ name: "Ada" }]);
  const [inserted, selected] = await db.batch([
    db.insert(users).values({ name: "Grace", active: false }).returning({ name: users.name }),
    db.select({ name: users.name }).from(users),
  ]);
  expect(inserted).toEqual([{ name: "Grace" }]);
  expect(selected).toEqual([{ name: "Ada" }, { name: "Grace" }]);
});

it("applies generated Drizzle migrations once", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { migrate } = await import("../src/index.js");
  const folder = mkdtempSync(join(tmpdir(), "boxdb-drizzle-"));
  try {
    mkdirSync(join(folder, "meta"));
    writeFileSync(
      join(folder, "meta/_journal.json"),
      JSON.stringify({
        version: "7",
        dialect: "sqlite",
        entries: [{ idx: 0, version: "6", when: 1, tag: "0000_users", breakpoints: true }],
      }),
    );
    writeFileSync(
      join(folder, "0000_users.sql"),
      "CREATE TABLE users(id integer PRIMARY KEY AUTOINCREMENT,name text NOT NULL,active integer NOT NULL,profile text);\n--> statement-breakpoint\nCREATE INDEX users_name ON users(name);",
    );
    await using client = await Database.open();
    const db = createDrizzleDatabase(client);
    await migrate(db, { migrationsFolder: folder });
    await migrate(db, { migrationsFolder: folder });
    expect(await client.query`SELECT COUNT(*) AS n FROM __drizzle_migrations`.get()).toEqual({
      n: 1n,
    });
    expect(
      await db.insert(users).values({ name: "Ada", active: true }).returning({ id: users.id }),
    ).toEqual([{ id: 1n }]);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

it("preserves bigint storage integers while mapping boolean columns", async () => {
  await using client = await Database.open();
  await client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,active INTEGER NOT NULL,profile TEXT)",
  );
  const db = createDrizzleDatabase(client);
  expect(await db.insert(users).values({ name: "Ada", active: true }).returning()).toEqual([
    { id: 1n, name: "Ada", active: true, profile: null },
  ]);
});

it("maps timestamp columns from bigint storage without converting integer IDs", async () => {
  await using client = await Database.open();
  await client.exec("CREATE TABLE events(id INTEGER PRIMARY KEY,at INTEGER,at_ms INTEGER)");
  const events = sqliteTable("events", {
    id: integer().primaryKey().$type<bigint>(),
    at: integer({ mode: "timestamp" }),
    atMs: integer("at_ms", { mode: "timestamp_ms" }),
  });
  const db = createDrizzleDatabase(client, { schema: { events } });
  const date = new Date("2026-10-14T12:00:00.000Z");
  await db.insert(events).values({ id: 9223372036854775807n, at: date, atMs: date });
  const expected = { id: 9223372036854775807n, at: date, atMs: date };
  expect(await db.select().from(events).get()).toEqual(expected);
  expect(await db.query.events.findFirst()).toEqual(expected);
});
