import { expect, test } from "vitest";
import { betterAuth, type BetterAuthOptions } from "better-auth";
import { Database } from "@boxdb/client";
import { betterAuthAdapter } from "../src/index.js";

const options: BetterAuthOptions = {
  secret: "boxdb-test-secret-with-at-least-32-characters",
  baseURL: "http://localhost:3000",
  emailAndPassword: { enabled: true },
};

test("Better Auth registers a user, signs in and reads a persisted session", async () => {
  await using db = await Database.open();
  const database = betterAuthAdapter(db);
  const adapter = database(options);
  const schema = await adapter.createSchema!(options);
  await db.exec(schema.code);
  const auth = betterAuth({ ...options, database });
  const registered = await auth.api.signUpEmail({
    body: { name: "Ada", email: "ada@example.com", password: "correct-horse-battery-staple" },
  });
  expect(registered.user.email).toBe("ada@example.com");
  const response = await auth.api.signInEmail({
    body: { email: "ada@example.com", password: "correct-horse-battery-staple" },
    asResponse: true,
  });
  expect(response.status).toBe(200);
  const cookie = response.headers.get("set-cookie")!.split(";")[0]!;
  const session = await auth.api.getSession({ headers: new Headers({ cookie }) });
  expect(session?.user.name).toBe("Ada");
  expect(session?.session.expiresAt).toBeInstanceOf(Date);
  await auth.api.signOut({ headers: new Headers({ cookie }) });
  expect(await auth.api.getSession({ headers: new Headers({ cookie }) })).toBeNull();
});

test("adapter transactions roll back failed operations and preserve mapped fields", async () => {
  await using db = await Database.open();
  const configured: BetterAuthOptions = {
    ...options,
    user: {
      modelName: "people",
      fields: { email: "email_address" },
      additionalFields: { score: { type: "number", defaultValue: 0 } },
    },
  };
  const adapter = betterAuthAdapter(db)(configured);
  await db.exec((await adapter.createSchema!(configured)).code);
  await expect(
    adapter.transaction(async (tx) => {
      await tx.create({
        model: "user",
        data: {
          name: "rollback",
          email: "discard@example.com",
          emailVerified: false,
          createdAt: new Date(),
          updatedAt: new Date(),
        },
      });
      throw new Error("stop");
    }),
  ).rejects.toThrow("stop");
  expect(await adapter.count({ model: "user" })).toBe(0);
  const user = await adapter.create<{
    id: string;
    email: string;
    name: string;
    score: number;
    emailVerified: boolean;
    createdAt: Date;
    updatedAt: Date;
  }>({
    model: "user",
    data: {
      name: "Ada%_",
      email: "ada@example.com",
      emailVerified: false,
      createdAt: new Date(),
      updatedAt: new Date(),
      score: 0,
    },
  });
  expect(
    await adapter.findOne({
      model: "user",
      where: [{ field: "email", value: "ADA@EXAMPLE.COM", mode: "insensitive" }],
    }),
  ).toMatchObject({ id: user.id });
  expect(
    await adapter.findMany({
      model: "user",
      where: [{ field: "name", value: "%_", operator: "contains" }],
    }),
  ).toHaveLength(1);
  expect(
    await adapter.updateMany({
      model: "user",
      where: [{ field: "id", value: user.id }],
      update: { name: "Grace" },
    }),
  ).toBe(1);
  expect(
    await adapter.deleteMany({
      model: "user",
      where: [{ field: "id", value: [user.id], operator: "in" }],
    }),
  ).toBe(1);
  expect(await adapter.count({ model: "user" })).toBe(0);
});

test("guarded increments and single-row consumption remain atomic under contention", async () => {
  await using db = await Database.open();
  const configured: BetterAuthOptions = {
    ...options,
    user: { additionalFields: { score: { type: "number", defaultValue: 0 } } },
  };
  const adapter = betterAuthAdapter(db)(configured);
  await db.exec((await adapter.createSchema!(configured)).code);
  const user = await adapter.create<{
    id: string;
    email: string;
    name: string;
    score: number;
    emailVerified: boolean;
    createdAt: Date;
    updatedAt: Date;
  }>({
    model: "user",
    data: {
      name: "Ada",
      email: "ada@example.com",
      emailVerified: false,
      createdAt: new Date(),
      updatedAt: new Date(),
      score: 0,
    },
  });
  const results = await Promise.all(
    Array.from({ length: 20 }, () =>
      adapter.incrementOne<{ score: number }>({
        model: "user",
        where: [
          { field: "id", value: user.id },
          { field: "score", value: 10, operator: "lt" },
        ],
        increment: { score: 1 },
      }),
    ),
  );
  expect(results.filter(Boolean)).toHaveLength(10);
  expect(
    await adapter.findOne({ model: "user", where: [{ field: "id", value: user.id }] }),
  ).toMatchObject({ score: 10 });
  const consumed = await Promise.all(
    Array.from({ length: 2 }, () =>
      adapter.consumeOne({ model: "user", where: [{ field: "id", value: user.id }] }),
    ),
  );
  expect(consumed.filter(Boolean)).toHaveLength(1);
});

test("numeric IDs and booleans work with bigint storage results", async () => {
  await using db = await Database.open();
  const configured: BetterAuthOptions = {
    ...options,
    advanced: { database: { generateId: "serial" } },
  };
  const adapter = betterAuthAdapter(db)(configured);
  await db.exec((await adapter.createSchema!(configured)).code);
  const user = await adapter.create({
    model: "user",
    data: {
      name: "Ada",
      email: "ada@example.com",
      emailVerified: true,
      createdAt: new Date(),
      updatedAt: new Date(),
    },
  });
  expect(user).toMatchObject({ id: "1", emailVerified: true });
  expect(
    await adapter.findOne({
      model: "user",
      where: [{ field: "id", value: "1", mode: "insensitive" }],
    }),
  ).toMatchObject({ id: "1" });
});

test("string comparisons treat empty suffixes literally", async () => {
  await using db = await Database.open();
  const adapter = betterAuthAdapter(db)(options);
  await db.exec((await adapter.createSchema!(options)).code);
  await adapter.create({
    model: "user",
    data: {
      name: "Ada",
      email: "ada@example.com",
      emailVerified: false,
      createdAt: new Date(),
      updatedAt: new Date(),
    },
  });
  expect(
    await adapter.findMany({
      model: "user",
      where: [{ field: "name", operator: "ends_with", value: "" }],
    }),
  ).toHaveLength(1);
});

test("filter connectors cannot interpolate arbitrary SQL", async () => {
  await using db = await Database.open();
  const adapter = betterAuthAdapter(db)(options);
  await db.exec((await adapter.createSchema!(options)).code);
  await expect(
    adapter.findMany({
      model: "user",
      where: [
        { field: "name", value: "Ada" },
        { field: "name", value: "missing", connector: "OR 1=1 OR" as "OR" },
      ],
    }),
  ).rejects.toThrow(TypeError);
});
