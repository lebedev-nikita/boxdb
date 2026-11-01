import { BoxDB, findBoxDBError } from "@boxdb/client";
import { Deferred, Effect, Fiber, Layer, Stream } from "effect";
import { Migrator, SqlClient } from "effect/sql";
import { expect, it } from "vitest";
import { BoxDBSqlClient, TransactionOptions, layer } from "../src/index.js";

const run = <A, E>(client: BoxDB, effect: Effect.Effect<A, E, SqlClient.SqlClient>) =>
  Effect.runPromise(Effect.provide(effect, layer({ client })));

it("runs tagged-template queries with bound parameters and bigint integers", async () => {
  await using client = await BoxDB.open();
  const rows = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql`CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)`;
      yield* sql`INSERT INTO users ${sql.insert([{ name: "Ada" }, { name: "Grace" }])}`;
      return yield* sql`SELECT id, name FROM ${sql("users")} WHERE name = ${"Grace"}`;
    }),
  );
  expect(rows).toEqual([{ id: 2n, name: "Grace" }]);
});

it("provides the BoxDB service, borrowing the client without closing it", async () => {
  await using client = await BoxDB.open();
  const borrowed = await Effect.runPromise(
    Effect.provide(
      Effect.gen(function* () {
        return (yield* BoxDBSqlClient).client;
      }),
      Layer.fresh(layer({ client })),
    ),
  );
  expect(borrowed).toBe(client);
  expect(client.open).toBe(true);
});

const names = Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  return (yield* sql<{ name: string }>`SELECT name FROM users ORDER BY id`).map((row) => row.name);
});

const insert = (name: string) =>
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql`INSERT INTO users(name) VALUES(${name})`;
  });

it("commits transactions through BoxDB handles and rolls back failed ones and savepoints", async () => {
  await using client = await BoxDB.open();
  await client.exec("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql.withTransaction(
        Effect.gen(function* () {
          yield* insert("Ada");
          yield* Effect.flip(
            sql.withTransaction(Effect.andThen(insert("nested"), Effect.fail("stop"))),
          );
          yield* sql.withTransaction(insert("Grace"));
        }),
      );
      yield* Effect.flip(sql.withTransaction(Effect.andThen(insert("outer"), Effect.fail("stop"))));
      return yield* names;
    }),
  );
  expect(result).toEqual(["Ada", "Grace"]);
});

it("applies BoxDB transaction options to top-level transactions", async () => {
  await using client = await BoxDB.open();
  await client.exec("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const mode = yield* sql
        .withTransaction(sql`SELECT 1 AS one`)
        .pipe(Effect.provideService(TransactionOptions, { mode: "immediate" }));
      const readOnly = yield* sql
        .withTransaction(insert("Ada"))
        .pipe(Effect.provideService(TransactionOptions, { readOnly: true }), Effect.flip);
      return { mode, readOnly: findBoxDBError(readOnly)?.code };
    }),
  );
  expect(result).toEqual({ mode: [{ one: 1n }], readOnly: "BOXDB_READONLY" });
});

it("rolls back a transaction whose commit fails and keeps the client usable", async () => {
  await using client = await BoxDB.open();
  await client.exec(`
    PRAGMA foreign_keys = ON;
    CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE posts(user_id INTEGER REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED);
  `);
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const error = yield* Effect.flip(
        sql.withTransaction(Effect.andThen(insert("Ada"), sql`INSERT INTO posts VALUES(${42})`)),
      );
      return { reason: error.reason._tag, users: yield* names };
    }),
  );
  expect(result).toEqual({ reason: "ConstraintError", users: [] });
  expect((await client.execute("SELECT count(*) AS n FROM posts")).rows).toEqual([{ n: 0n }]);
});

it("classifies BoxDB failures as Effect SQL error reasons", async () => {
  await using client = await BoxDB.open();
  await client.exec(
    "CREATE TABLE users(id INTEGER PRIMARY KEY, email TEXT UNIQUE, age INTEGER CHECK(age > 0))",
  );
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql`INSERT INTO users(email) VALUES(${"a@example.com"})`;
      const unique = yield* Effect.flip(sql`INSERT INTO users(email) VALUES(${"a@example.com"})`);
      const check = yield* Effect.flip(sql`INSERT INTO users(age) VALUES(${-1})`);
      const syntax = yield* Effect.flip(sql`SELEC 1`);
      return [unique, check, syntax].map((error) => ({
        tag: error.reason._tag,
        code: findBoxDBError(error)?.code,
        constraint: "constraint" in error.reason ? error.reason.constraint : undefined,
      }));
    }),
  );
  expect(result).toEqual([
    { tag: "UniqueViolation", code: "BOXDB_CONSTRAINT_UNIQUE", constraint: "users.email" },
    { tag: "ConstraintError", code: "BOXDB_CONSTRAINT_CHECK", constraint: undefined },
    { tag: "UnknownError", code: "BOXDB_ERROR", constraint: undefined },
  ]);
});

it("interrupts running SQL when the fiber is interrupted", async () => {
  await using client = await BoxDB.open();
  const started = Date.now();
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const slow = sql`
        WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000000)
        SELECT count(*) FROM n`;
      const timedOut = yield* Effect.timeoutOption(slow, "50 millis");
      const [next] = yield* sql`SELECT 1 AS one`;
      return { timedOut, next };
    }),
  );
  expect(result.timedOut._tag).toBe("None");
  expect(result.next).toEqual({ one: 1n });
  expect(Date.now() - started).toBeLessThan(5000);
});

it("returns values, raw results, streams and transformed names", async () => {
  await using client = await BoxDB.open();
  await client.exec("CREATE TABLE users(user_id INTEGER PRIMARY KEY, full_name TEXT)");
  const camel = (name: string) => name.replace(/_(\w)/g, (_, c: string) => c.toUpperCase());
  const snake = (name: string) => name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
  const result = await Effect.runPromise(
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql`INSERT INTO users ${sql.insert({ fullName: "Ada" })}`;
      const raw = yield* sql`INSERT INTO users(full_name) VALUES(${"Grace"})`.raw;
      return {
        rows: yield* sql`SELECT * FROM users ORDER BY ${sql("userId")}`,
        values: yield* sql`SELECT user_id, full_name FROM users ORDER BY user_id`.values,
        raw,
        stream: Array.from(
          yield* Stream.runCollect(sql`SELECT full_name FROM users ORDER BY user_id`.stream),
        ),
      };
    }).pipe(
      Effect.provide(layer({ client, transformResultNames: camel, transformQueryNames: snake })),
    ),
  );
  expect(result).toEqual({
    rows: [
      { userId: 1n, fullName: "Ada" },
      { userId: 2n, fullName: "Grace" },
    ],
    values: [
      [1n, "Ada"],
      [2n, "Grace"],
    ],
    raw: { rows: [], columns: [], changes: 1n, lastInsertRowid: 2n },
    stream: [{ fullName: "Ada" }, { fullName: "Grace" }],
  });
});

it("runs Effect SQL migrations", async () => {
  await using client = await BoxDB.open();
  const migrate = Migrator.make({});
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const applied = yield* migrate({
        loader: Migrator.fromRecord({
          "0001_users": sql`CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)`,
        }),
      });
      const again = yield* migrate({
        loader: Migrator.fromRecord({
          "0001_users": sql`CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)`,
        }),
      });
      yield* insert("Ada");
      return { applied, again, users: yield* names };
    }),
  );
  expect(result).toEqual({ applied: [[1, "users"]], again: [], users: ["Ada"] });
});

it("interrupts a transaction waiting for another transaction to finish", async () => {
  await using client = await BoxDB.open();
  await client.exec("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
  const result = await run(
    client,
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const started = yield* Deferred.make<void>();
      const release = yield* Deferred.make<void>();
      const holder = yield* Effect.forkChild(
        sql.withTransaction(
          Effect.andThen(Deferred.succeed(started, undefined), Deferred.await(release)),
        ),
      );
      yield* Deferred.await(started);
      const waitStarted = Date.now();
      const waited = yield* Effect.timeoutOption(sql.withTransaction(insert("late")), "50 millis");
      const elapsed = Date.now() - waitStarted;
      yield* Deferred.succeed(release, undefined);
      yield* Fiber.join(holder);
      return { waited: waited._tag, fast: elapsed < 1000, users: yield* names };
    }),
  );
  expect(result).toEqual({ waited: "None", fast: true, users: [] });
});

it("reports queries on a closed client as connection errors", async () => {
  const client = await BoxDB.open();
  await client.close();
  const error = await run(
    client,
    Effect.flatMap(SqlClient.SqlClient, (sql) => Effect.flip(sql`SELECT 1`)),
  );
  expect([error.reason._tag, findBoxDBError(error)?.code]).toEqual([
    "ConnectionError",
    "BOXDB_CLOSED",
  ]);
});
