import { findBoxDBError } from "@boxdb/client";
import type {
  BoxDB,
  Parameters,
  QueryResult,
  TransactionClient,
  TransactionHandle,
  TransactionOptions as BoxDBTransactionOptions,
} from "@boxdb/client";
import { Context, Effect, Layer, Stream } from "effect";
import * as Reactivity from "effect/reactivity/Reactivity";
import { SqlClient, SqlError, Statement } from "effect/sql";
import type { SqlConnection } from "effect/sql";

/** An Effect `SqlClient` that runs on a borrowed BoxDB client. */
export interface BoxDBSqlClient extends SqlClient.SqlClient {
  readonly client: BoxDB;
  /** Not supported by SQLite. */
  readonly updateValues: never;
}

export const BoxDBSqlClient = Context.Service<BoxDBSqlClient>(
  "@boxdb/effect-adapter/BoxDBSqlClient",
);

export interface BoxDBSqlClientConfig {
  /** Borrowed connection; the adapter never closes it. */
  readonly client: BoxDB;
  readonly spanAttributes?: Record<string, unknown> | undefined;
  readonly transformResultNames?: ((name: string) => string) | undefined;
  readonly transformQueryNames?: ((name: string) => string) | undefined;
}

/** Options for top-level `withTransaction` calls; nested calls use savepoints. */
export const TransactionOptions = Context.Reference<
  Omit<BoxDBTransactionOptions, "signal" | "raw">
>("@boxdb/effect-adapter/TransactionOptions", { defaultValue: () => ({}) });

type Transform = (<A extends object>(rows: ReadonlyArray<A>) => ReadonlyArray<A>) | undefined;

const query = (
  target: () => TransactionClient,
  sql: string,
  params: ReadonlyArray<unknown>,
  raw: boolean,
): Effect.Effect<QueryResult, SqlError.SqlError> =>
  Effect.tryPromise({
    try: (signal) => target().execute(sql, params as Parameters, { signal, raw }),
    catch: (cause) => new SqlError.SqlError({ reason: classify(cause, "execute") }),
  });

/** Routes compiled statements to the root client or the innermost transaction. */
const makeConnection = (target: () => TransactionClient): SqlConnection.Connection => {
  const rows = (sql: string, params: ReadonlyArray<unknown>, transform: Transform) =>
    Effect.map(query(target, sql, params, false), (result) =>
      transform ? transform(result.rows as Array<object>) : result.rows,
    );
  const values = (sql: string, params: ReadonlyArray<unknown>) =>
    Effect.map(
      query(target, sql, params, true),
      (result) => result.rows as ReadonlyArray<ReadonlyArray<unknown>>,
    );
  return {
    execute: rows,
    executeUnprepared: rows,
    executeRaw: (sql, params) => query(target, sql, params, false),
    executeValues: values,
    executeValuesUnprepared: values,
    executeStream: (sql, params, transform) =>
      Stream.flatMap(Stream.fromEffect(rows(sql, params, transform)), Stream.fromIterable),
  };
};

let nextClientId = 0;

/** Handles of each top-level transaction: the root at index 0, then one per savepoint. */
const transactionHandles = new WeakMap<SqlConnection.Connection, Array<TransactionHandle>>();

const control = (operation: string, run: () => Promise<unknown>) =>
  Effect.tryPromise({
    try: run,
    catch: (cause) => new SqlError.SqlError({ reason: classify(cause, operation) }),
  }).pipe(Effect.asVoid);

/**
 * Finishes the handle at `depth` together with the savepoints inside it. A
 * failed commit keeps the handle so the transaction can still roll back.
 */
const finish = (conn: SqlConnection.Connection, depth: number, commit: boolean) =>
  Effect.suspend(() => {
    const handles = transactionHandles.get(conn)!;
    const handle = handles[depth];
    if (handle === undefined) return Effect.void;
    const forget = Effect.sync(() => handles.splice(depth));
    return commit
      ? Effect.andThen(
          control("commit", () => handle.commit()),
          forget,
        )
      : Effect.ensuring(
          control("rollback", () => handle.rollback()),
          forget,
        );
  });

/**
 * BoxDB rejects transaction-control SQL, so transactions and savepoints use
 * transaction handles; statements run on the innermost one.
 */
const makeWithTransaction = (
  client: BoxDB,
  transactionService: Context.Service<
    SqlClient.TransactionConnection,
    SqlClient.TransactionConnection.Service
  >,
  spanAttributes: ReadonlyArray<readonly [string, unknown]>,
) =>
  SqlClient.makeWithTransaction({
    transactionService,
    spanAttributes,
    // Begins here rather than in `begin`, so waiting for the writer stays interruptible.
    acquireConnection: Effect.fnUntraced(function* (restore) {
      const options = yield* TransactionOptions;
      const handles: Array<TransactionHandle> = [];
      const conn = makeConnection(() => handles.at(-1)!);
      transactionHandles.set(conn, handles);
      // Rolls back a transaction that began just as acquisition was interrupted.
      yield* Effect.addFinalizer(() => Effect.ignore(finish(conn, 0, false)));
      const controller = new AbortController();
      yield* restore(
        control("begin", async () => {
          handles.push(await client.begin({ ...options, signal: controller.signal }));
        }),
      ).pipe(Effect.onInterrupt(() => Effect.sync(() => controller.abort())));
      return conn;
    }),
    begin: () => Effect.void,
    savepoint: (conn) => {
      const handles = transactionHandles.get(conn)!;
      return control("savepoint", async () => handles.push(await handles.at(-1)!.begin()));
    },
    commit: (conn) => finish(conn, 0, true),
    onCommitFailure: (conn) => finish(conn, 0, false),
    rollback: (conn) => finish(conn, 0, false),
    rollbackSavepoint: (conn, id) => finish(conn, id, false),
    releaseSavepoint: (conn, id) => finish(conn, id, true),
  });

export const make = (
  config: BoxDBSqlClientConfig,
): Effect.Effect<BoxDBSqlClient, never, Reactivity.Reactivity> =>
  Effect.gen(function* () {
    const root = makeConnection(() => config.client);
    const transactionService = SqlClient.TransactionConnection(nextClientId++);
    const spanAttributes: ReadonlyArray<readonly [string, unknown]> = [
      ...Object.entries(config.spanAttributes ?? {}),
      ["db.system.name", "sqlite"],
    ];
    const client = yield* SqlClient.make({
      transactionService,
      driver: "boxdb",
      acquirer: Effect.succeed(root),
      borrower: (f) => f(root),
      compiler: Statement.makeCompilerSqlite(config.transformQueryNames),
      spanAttributes,
      transformRows: config.transformResultNames
        ? Statement.defaultTransforms(config.transformResultNames).array
        : undefined,
    });
    return Object.assign(client as BoxDBSqlClient, {
      client: config.client,
      withTransaction: makeWithTransaction(config.client, transactionService, spanAttributes),
    });
  });

/** Provides both `BoxDBSqlClient` and the generic `SqlClient`. */
export const layer = (
  config: BoxDBSqlClientConfig,
): Layer.Layer<BoxDBSqlClient | SqlClient.SqlClient> =>
  Layer.effectContext(
    Effect.map(make(config), (client) =>
      Context.make(BoxDBSqlClient, client).pipe(Context.add(SqlClient.SqlClient, client)),
    ),
  ).pipe(Layer.provide(Reactivity.layer));

const UNIQUE_PREFIX = "UNIQUE constraint failed:";

/** Maps BoxDB codes and SQLite result codes to Effect SQL error reasons. */
const classify = (cause: unknown, operation: string): SqlError.SqlErrorReason => {
  const error = findBoxDBError(cause);
  const props = { cause, message: error?.message, operation };
  if (error === undefined) return new SqlError.UnknownError(props);
  if (error.code === "BOXDB_CONSTRAINT_UNIQUE") {
    const index = error.message.indexOf(UNIQUE_PREFIX);
    const constraint = index === -1 ? "" : error.message.slice(index + UNIQUE_PREFIX.length).trim();
    return new SqlError.UniqueViolation({ ...props, constraint: constraint || "unknown" });
  }
  switch (error.code) {
    case "BOXDB_CLOSED":
    case "BOXDB_UNAVAILABLE":
      return new SqlError.ConnectionError(props);
    case "BOXDB_TRANSACTION_TIMEOUT":
      return new SqlError.StatementTimeoutError(props);
  }
  switch (error.resultCode) {
    case 3:
      return new SqlError.AuthorizationError(props);
    case 5:
    case 6:
      return new SqlError.LockTimeoutError(props);
    case 19:
      return new SqlError.ConstraintError(props);
    case 23:
      return new SqlError.AuthenticationError(props);
  }
  return new SqlError.UnknownError(props);
};
