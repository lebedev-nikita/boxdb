import assert from "node:assert/strict";
import { eq } from "drizzle-orm";
import { integer, sqliteTable } from "drizzle-orm/sqlite-core";
import { BoxDB, findBoxDBError } from "@boxdb/client";
import {
  createDrizzleDatabase,
  date,
  dateTime,
  decimal,
  instant,
  interval,
  json,
  uuid,
} from "@boxdb/drizzle-adapter";

const path = process.argv[2];
assert.ok(path, "database path is required");
const typed = sqliteTable("typed", {
  id: integer().primaryKey().$type<bigint>(),
  amount: decimal({ precision: 38, scale: 18 }).notNull(),
  external: uuid().notNull(),
  occurred: instant().notNull(),
  day: date().notNull(),
  local: dateTime().notNull(),
  duration: interval().notNull(),
  document: json().$type<{ ref: string; source: string }>().notNull(),
});
await using client = await BoxDB.open(path);
const db = createDrizzleDatabase(client, { schema: { typed } });
const first = {
  id: 1n,
  amount: "9007199254740993.01",
  external: "550e8400-e29b-41d4-a716-446655440000",
  occurred: "2024-02-29T12:00:00.123457Z",
  day: "2024-02-29",
  local: "2024-02-29T12:00:00.123457",
  duration: "1 mons 0 days 00:00:00.000000",
  document: { ref: "9007199254740993", source: "go" },
};
assert.deepEqual(await db.select().from(typed).get(), first);
const second = {
  ...first,
  id: 2n,
  amount: "12.50",
  external: "650e8400-e29b-41d4-a716-446655440000",
  occurred: "2024-02-29T12:00:00.123458Z",
  local: "2024-02-29T12:00:00.123458",
  duration: "30 days",
};
const third = {
  ...second,
  id: 3n,
  amount: "2.00",
  external: "750e8400-e29b-41d4-a716-446655440000",
  occurred: "2024-02-29T12:00:00.123459Z",
  local: "2024-02-29T12:00:00.123459",
  duration: "29 days",
};
await db.insert(typed).values([second, third]);
assert.deepEqual(await db.select({ id: typed.id }).from(typed).orderBy(typed.amount), [
  { id: 3n },
  { id: 2n },
  { id: 1n },
]);
await assert.rejects(
  db.insert(typed).values({ ...third, id: 4n, amount: "12.5000" }),
  (error) => findBoxDBError(error)?.code === "BOXDB_CONSTRAINT_UNIQUE",
);
await assert.rejects(async () => {
  await using tx = await client.begin();
  await tx.query`UPDATE typed SET amount=${"1.00"} WHERE id=1`;
  await tx.query`UPDATE typed SET document=${"invalid JSON"} WHERE id=1`;
  await tx.commit();
});
assert.equal((await db.select().from(typed).where(eq(typed.id, 1n)).get())?.amount, first.amount);
await db
  .update(typed)
  .set({ document: { ...first.document, source: "typescript" } })
  .where(eq(typed.id, 1n));
