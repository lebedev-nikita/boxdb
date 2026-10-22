import { is } from "drizzle-orm";
export { date, dateTime, decimal, instant, interval, json, time, uuid } from "./columns.js";
import { DefaultLogger, type Logger } from "drizzle-orm/logger";
import {
  createTableRelationsHelpers,
  extractTablesRelationalConfig,
  type RelationalSchemaConfig,
  type TablesRelationalConfig,
} from "drizzle-orm/relations";
import {
  BaseSQLiteDatabase,
  SQLiteAsyncDialect,
  SQLiteTimestamp,
  type SQLiteTransactionConfig,
} from "drizzle-orm/sqlite-core";
import {
  SqliteRemoteDatabase,
  type RemoteCallback,
  type AsyncBatchRemoteCallback,
} from "drizzle-orm/sqlite-proxy";
import {
  SQLiteRemoteSession,
  SQLiteProxyTransaction,
  type PreparedQueryConfig,
  type RemotePreparedQuery,
} from "drizzle-orm/sqlite-proxy/session";
import type { DrizzleConfig } from "drizzle-orm/utils";
import { type BoxDB, type TransactionClient, type RunResult } from "@boxdb/client";

export interface DrizzleRunResult extends RunResult {
  rows?: unknown[];
}

export type DrizzleDatabase<TSchema extends Record<string, unknown> = Record<string, never>> =
  BaseSQLiteDatabase<"async", DrizzleRunResult, TSchema> & {
    batch: SqliteRemoteDatabase<TSchema>["batch"];
    $client: BoxDB | TransactionClient;
  };

function positionalRows(rows: unknown[]): unknown[][] {
  return rows.map((row) => {
    if (!Array.isArray(row)) throw new TypeError("Drizzle requires positional rows");
    return row;
  });
}

// Timestamp decoders expect number inputs; integer columns retain bigint.
function timestampDecoder<T extends object>(field: T): T {
  if (!is(field, SQLiteTimestamp)) return field;
  return new Proxy(field, {
    get(target, key, receiver) {
      if (key === "mapFromDriverValue")
        return (value: number | bigint) => {
          const number = typeof value === "bigint" ? Number(value) : value;
          if (!Number.isSafeInteger(number))
            throw new RangeError("Timestamp is outside the safe integer range");
          return target.mapFromDriverValue(number);
        };
      return Reflect.get(target, key, receiver);
    },
  });
}

class Dialect extends SQLiteAsyncDialect {
  override buildRelationalQuery(
    ...args: Parameters<SQLiteAsyncDialect["buildRelationalQuery"]>
  ): ReturnType<SQLiteAsyncDialect["buildRelationalQuery"]> {
    const query = super.buildRelationalQuery(...args);
    type Selection = { field: object; isJson: boolean; selection: Selection[] };
    const visit = (selection: Selection[]) => {
      for (const item of selection) {
        item.field = timestampDecoder(item.field);
        if (item.isJson) visit(item.selection);
      }
    };
    visit(query.selection);
    return query;
  }
}

class Session<
  TFullSchema extends Record<string, unknown>,
  TSchema extends TablesRelationalConfig,
> extends SQLiteRemoteSession<TFullSchema, TSchema> {
  constructor(
    private readonly database: BoxDB | TransactionClient,
    private readonly asyncDialect: SQLiteAsyncDialect,
    private readonly relationalSchema: RelationalSchemaConfig<TSchema> | undefined,
    private readonly options: { logger?: Logger },
  ) {
    const query: RemoteCallback = async (sql, parameters, method) => {
      const result = await database.execute(sql, parameters, { raw: true });
      const rows = positionalRows(result.rows);
      // sqlite-proxy expects one positional row (or undefined) for get().
      return {
        rows: method === "get" ? rows[0]! : rows,
        changes: result.changes,
        lastInsertRowid: result.lastInsertRowid,
      };
    };
    const batch: AsyncBatchRemoteCallback = async (statements) => {
      await using tx = await database.begin();
      const results = [];
      for (const statement of statements) {
        const result = await tx.execute(statement.sql, statement.params, { raw: true });
        const rows = positionalRows(result.rows);
        results.push({
          rows: statement.method === "get" ? rows[0]! : rows,
          changes: result.changes,
          lastInsertRowid: result.lastInsertRowid,
        });
      }
      await tx.commit();
      return results;
    };
    super(query, asyncDialect, relationalSchema, batch, options);
  }

  override prepareQuery<T extends Omit<PreparedQueryConfig, "run">>(
    ...args: Parameters<SQLiteRemoteSession<TFullSchema, TSchema>["prepareQuery"]>
  ): RemotePreparedQuery<T> {
    const [query, fields, method, arrayMode, mapper, metadata, cache] = args;
    return super.prepareQuery<T>(
      query,
      fields?.map((item) => ({ ...item, field: timestampDecoder(item.field) })),
      method,
      arrayMode,
      mapper,
      metadata,
      cache,
    );
  }

  override transaction<T>(
    fn: (tx: SQLiteProxyTransaction<TFullSchema, TSchema>) => Promise<T>,
    config?: SQLiteTransactionConfig,
  ): Promise<T> {
    return (async () => {
      await using client = await this.database.begin(
        config?.behavior ? { mode: config.behavior } : {},
      );
      const session = new Session<TFullSchema, TSchema>(
        client,
        this.asyncDialect,
        this.relationalSchema,
        this.options,
      );
      const result = await fn(
        new Transaction("async", this.asyncDialect, session, this.relationalSchema),
      );
      await client.commit();
      return result;
    })();
  }
}

class Transaction<
  TFullSchema extends Record<string, unknown>,
  TSchema extends TablesRelationalConfig,
> extends SQLiteProxyTransaction<TFullSchema, TSchema> {
  constructor(
    resultType: "async",
    dialect: SQLiteAsyncDialect,
    private readonly scopedSession: Session<TFullSchema, TSchema>,
    schema: RelationalSchemaConfig<TSchema> | undefined,
  ) {
    super(resultType, dialect, scopedSession, schema);
  }
  override transaction<T>(
    fn: (tx: SQLiteProxyTransaction<TFullSchema, TSchema>) => Promise<T>,
  ): Promise<T> {
    return this.scopedSession.transaction(fn);
  }
}

/** Run Drizzle queries and transactions on the connection's native thread. */
export function createDrizzleDatabase<
  TSchema extends Record<string, unknown> = Record<string, never>,
>(
  client: BoxDB | TransactionClient,
  config: Omit<DrizzleConfig<TSchema>, "cache"> = {},
): DrizzleDatabase<TSchema> {
  const dialect = new Dialect(config.casing === undefined ? {} : { casing: config.casing });
  const tables = config.schema
    ? extractTablesRelationalConfig(config.schema, createTableRelationsHelpers)
    : undefined;
  const schema =
    tables && config.schema
      ? { fullSchema: config.schema, schema: tables.tables, tableNamesMap: tables.tableNamesMap }
      : undefined;
  const logger = config.logger === true ? new DefaultLogger() : config.logger || undefined;
  const session = new Session(client, dialect, schema, logger ? { logger } : {});
  const database = new SqliteRemoteDatabase("async", dialect, session, schema);
  return Object.assign(database, { $client: client }) as unknown as DrizzleDatabase<TSchema>;
}

/** Apply generated migrations and their journal entries in one transaction. */
export async function migrate(
  database: Pick<DrizzleDatabase, "$client">,
  config: import("drizzle-orm/migrator").MigrationConfig,
): Promise<void> {
  const { readMigrationFiles } = await import("drizzle-orm/migrator");
  const migrations = readMigrationFiles(config);
  await using tx = await database.$client.begin({ mode: "immediate" });
  const table = tx.identifier(config.migrationsTable ?? "__drizzle_migrations");
  await tx.query`CREATE TABLE IF NOT EXISTS ${table} (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, created_at INTEGER NOT NULL)`;
  const last =
    await tx.query`SELECT created_at FROM ${table} ORDER BY created_at DESC LIMIT 1`.get();
  let lastTimestamp: bigint | undefined;
  if (last !== undefined) {
    if (
      typeof last !== "object" ||
      last === null ||
      !("created_at" in last) ||
      typeof last.created_at !== "bigint"
    )
      throw new TypeError("Invalid Drizzle migration timestamp");
    lastTimestamp = last.created_at;
  }
  for (const migration of migrations) {
    if (lastTimestamp !== undefined && lastTimestamp >= BigInt(migration.folderMillis)) continue;
    for (const statement of migration.sql) await tx.exec(statement);
    await tx.query`INSERT INTO ${table}(hash,created_at) VALUES(${migration.hash},${migration.folderMillis})`;
  }
  await tx.commit();
}
