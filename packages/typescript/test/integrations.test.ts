import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { eq, sql } from "drizzle-orm";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { Kysely, SqliteDialect, type Generated } from "kysely";
import { expect, it } from "vitest";
import { BoxDBError, Database, findBoxDBError } from "../src/index.js";
import { drizzle } from "../src/drizzle.js";

const users = sqliteTable("users", {
  id: integer().$type<bigint>().primaryKey({ autoIncrement: true }),
  name: text().notNull(),
  active: integer({ mode: "boolean" }).notNull(),
  profile: text({ mode: "json" }).$type<{ theme: string }>(),
});

it("preserves BoxDB codes in direct and wrapped Drizzle errors", () => {
  using client = new Database();
  client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY,name TEXT UNIQUE,active INTEGER,profile TEXT); INSERT INTO users(name) VALUES('Ada')",
  );
  const db = drizzle(client);
  for (const execute of [
    () => db.insert(users).values({ name: "Ada", active: true }).run(),
    () => db.run(sql`INSERT INTO users(name) VALUES('Ada')`),
  ]) {
    let caught: unknown;
    try {
      execute();
    } catch (error) {
      caught = error;
    }
    expect(findBoxDBError(caught)).toMatchObject({
      code: "BOXDB_CONSTRAINT_UNIQUE",
      resultCode: 19,
      extendedCode: 2067,
    });
  }
});

it("preserves structured BoxDB failures through Kysely", async () => {
  const client = new Database();
  client.exec("CREATE TABLE t(n INTEGER UNIQUE); INSERT INTO t VALUES(1)");
  const db = new Kysely<{ t: { n: bigint } }>({ dialect: new SqliteDialect({ database: client }) });
  try {
    const failure = await db
      .insertInto("t")
      .values({ n: 1n })
      .execute()
      .catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(BoxDBError);
    expect(findBoxDBError(failure)).toMatchObject({
      code: "BOXDB_CONSTRAINT_UNIQUE",
      resultCode: 19,
      extendedCode: 2067,
    });
  } finally {
    await db.destroy();
  }
});

it("runs Drizzle queries, conversions, returning, joins and nested transactions", () => {
  using client = new Database();
  client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,active INTEGER NOT NULL,profile TEXT)",
  );
  const db = drizzle(client, { schema: { users } });
  const [ada] = db
    .insert(users)
    .values({ name: "Ada", active: true, profile: { theme: "dark" } })
    .returning()
    .all();
  expect(ada).toEqual({ id: 1n, name: "Ada", active: true, profile: { theme: "dark" } });
  expect(db.select().from(users).where(eq(users.id, 1n)).get()).toEqual(ada);
  expect(db.query.users.findFirst({ where: eq(users.id, 1n) }).sync()).toEqual(ada);
  db.transaction(
    (tx) => {
      tx.insert(users).values({ name: "Grace", active: false }).run();
      expect(() =>
        tx.transaction((nested) => {
          nested.insert(users).values({ name: "rollback", active: true }).run();
          throw new Error("stop");
        }),
      ).toThrow("stop");
    },
    { behavior: "immediate" },
  );
  expect(db.select({ name: users.name }).from(users).all()).toEqual([
    { name: "Ada" },
    { name: "Grace" },
  ]);
  expect(
    db
      .update(users)
      .set({ name: "Ada L" })
      .where(eq(users.id, 1n))
      .returning({ name: users.name })
      .get(),
  ).toEqual({ name: "Ada L" });
  expect(
    db
      .select()
      .from(users)
      .leftJoin(sql`(SELECT 1 AS other_id) AS other`, sql`${users.id} = other.other_id`)
      .all(),
  ).toHaveLength(2);
  expect(db.$client).toBe(client);
});

it("runs Drizzle migrations with bigint results through the BoxDB adapter", () => {
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
    using client = new Database();
    const db = drizzle(client);
    // The adapter uses Drizzle's migration engine with BoxDB result types.
    db.$migrate({ migrationsFolder: folder });
    db.$migrate({ migrationsFolder: folder });
    expect(client.get("SELECT COUNT(*) AS n FROM __drizzle_migrations")).toEqual({ n: 1n });
    expect(
      client.run("INSERT INTO users(name,active) VALUES(?,?)", ["Ada", 1]).lastInsertRowid,
    ).toBe(1n);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

it("uses Kysely's built-in SQLite dialect without a custom driver or type cast", async () => {
  interface Schema {
    users: { id: Generated<bigint>; name: string };
  }
  const client = new Database();
  const db = new Kysely<Schema>({ dialect: new SqliteDialect({ database: client }) });
  try {
    await db.schema
      .createTable("users")
      .addColumn("id", "integer", (c) => c.primaryKey().autoIncrement())
      .addColumn("name", "text", (c) => c.notNull())
      .execute();
    await db.insertInto("users").values({ name: "Ada" }).execute();
    expect(await db.selectFrom("users").selectAll().execute()).toEqual([{ id: 1n, name: "Ada" }]);
    await expect(
      db.transaction().execute(async (tx) => {
        await tx.insertInto("users").values({ name: "rollback" }).execute();
        throw new Error("stop");
      }),
    ).rejects.toThrow("stop");
    const stream = db.selectFrom("users").selectAll().stream();
    const rows = [];
    for await (const row of stream) rows.push(row);
    expect(rows).toEqual([{ id: 1n, name: "Ada" }]);
  } finally {
    await db.destroy();
  }
  expect(client.open).toBe(false);
});
