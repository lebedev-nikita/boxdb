import { eq, getTableColumns, sql } from "drizzle-orm";
import { integer, sqliteTable } from "drizzle-orm/sqlite-core";
import { expect, expectTypeOf, it } from "vitest";
import { BoxDB } from "@boxdb/client";
import {
  createDrizzleDatabase,
  date,
  dateTime,
  decimal,
  instant,
  interval,
  json,
  uuid,
} from "../src/index.js";

it("declares native decimals and preserves exact amounts through writes, reads and calculations", async () => {
  const invoices = sqliteTable("invoices", {
    id: integer().primaryKey().$type<bigint>(),
    amount: decimal({ precision: 20, scale: 2 }).notNull(),
  });
  expectTypeOf<typeof invoices.$inferSelect.amount>().toEqualTypeOf<string>();
  expect(invoices.amount.getSQLType()).toBe("DECIMAL(20,2)");
  await using client = await BoxDB.open();
  await client.exec(
    `CREATE TABLE invoices(id INTEGER PRIMARY KEY, amount ${invoices.amount.getSQLType()} NOT NULL)`,
  );
  const db = createDrizzleDatabase(client, { schema: { invoices } });
  expect(
    await db.insert(invoices).values({ id: 1n, amount: "9007199254740993.01" }).returning(),
  ).toEqual([{ id: 1n, amount: "9007199254740993.01" }]);
  expect(
    await db.select().from(invoices).where(eq(invoices.amount, "9007199254740993.01")).get(),
  ).toEqual({
    id: 1n,
    amount: "9007199254740993.01",
  });
  expect(
    await db
      .select({ amount: sql<string>`${invoices.amount}+CAST('0.02' AS DECIMAL)` })
      .from(invoices),
  ).toEqual([{ amount: "9007199254740993.03" }]);
  await expect(db.insert(invoices).values({ id: 2n, amount: "1.001" })).rejects.toThrow();
});

it("rejects invalid decimal declarations before generating SQL", () => {
  for (const config of [
    { scale: 2 },
    { precision: 0 },
    { precision: 1001 },
    { precision: 4.5 },
    { precision: 4, scale: -1 },
    { precision: 4, scale: 5 },
    { precision: 4, scale: 1.5 },
  ]) {
    expect(() => sqliteTable("invalid", { amount: decimal(config) })).toThrow(RangeError);
  }
});

it("round trips native identifiers, microsecond calendar values and opaque JSON documents", async () => {
  const events = sqliteTable("events", {
    id: uuid().primaryKey(),
    at: instant().notNull(),
    day: date().notNull(),
    local: dateTime().notNull(),
    duration: interval().notNull(),
    document: json().$type<{ theme: string; items: number[] }>(),
  });
  const columns = Object.values(getTableColumns(events));
  expect(columns.map((column) => column.getSQLType())).toEqual([
    "UUID",
    "INSTANT",
    "DATE",
    "DATETIME",
    "INTERVAL",
    "JSON",
  ]);
  expectTypeOf<typeof events.$inferSelect.document>().toEqualTypeOf<{
    theme: string;
    items: number[];
  } | null>();
  await using client = await BoxDB.open();
  await client.exec(
    `CREATE TABLE events(${columns.map((column) => `"${column.name}" ${column.getSQLType()}`).join(",")})`,
  );
  const db = createDrizzleDatabase(client, { schema: { events } });
  const document = { theme: "dark", items: [1, 2, 3] };
  const input = {
    id: "550E8400E29B41D4A716446655440000",
    at: "2024-02-29T16:00:00.123456789+04:00",
    day: "2024-02-29",
    local: "2024-02-29T12:00:00.123456789",
    duration: "1 month 2 days 3 hours",
    document,
  };
  const expected = {
    id: "550e8400-e29b-41d4-a716-446655440000",
    at: "2024-02-29T12:00:00.123457Z",
    day: "2024-02-29",
    local: "2024-02-29T12:00:00.123457",
    duration: "1 mons 2 days 03:00:00.000000",
    document,
  };
  expect(await db.insert(events).values(input).returning().get()).toEqual(expected);
  expect(await db.query.events.findFirst({ where: eq(events.id, input.id) })).toEqual(expected);
  expect(await db.update(events).set({ document: null }).returning().get()).toEqual({
    ...expected,
    document: null,
  });
  await expect(db.insert(events).values({ ...input, day: "2024-02-30" })).rejects.toThrow();
});
