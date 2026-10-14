import type { Database as BetterSQLiteDatabase } from "better-sqlite3";
import { BetterSQLiteSession } from "drizzle-orm/better-sqlite3/session";
import { DefaultLogger } from "drizzle-orm/logger";
import { createTableRelationsHelpers, extractTablesRelationalConfig } from "drizzle-orm/relations";
import { BaseSQLiteDatabase, SQLiteSyncDialect } from "drizzle-orm/sqlite-core";
import type { DrizzleConfig } from "drizzle-orm/utils";
import { Database, type RunResult } from "./index.js";

export type DrizzleDatabase<TSchema extends Record<string, unknown> = Record<string, never>> =
  BaseSQLiteDatabase<"sync", RunResult, TSchema> & { $client: Database };

/** Use Drizzle's SQLite session with BoxDB, without loading better-sqlite3. */
export function drizzle<TSchema extends Record<string, unknown> = Record<string, never>>(
  client: Database,
  config: Omit<DrizzleConfig<TSchema>, "cache"> = {},
): DrizzleDatabase<TSchema> {
  const dialect = new SQLiteSyncDialect(
    config.casing === undefined ? {} : { casing: config.casing },
  );
  const tables = config.schema
    ? extractTablesRelationalConfig(config.schema, createTableRelationsHelpers)
    : undefined;
  const schema =
    tables && config.schema
      ? {
          fullSchema: config.schema,
          schema: tables.tables,
          tableNamesMap: tables.tableNamesMap,
        }
      : undefined;
  const logger = config.logger === true ? new DefaultLogger() : config.logger || undefined;
  // Drizzle types its session against the full better-sqlite3 class; it uses
  // only prepare(), raw(), run/get/all(), and transaction(). Integration tests
  // exercise this narrower protocol, including nested transactions and rows.
  const session = new BetterSQLiteSession(
    client as unknown as BetterSQLiteDatabase,
    dialect,
    schema,
    logger ? { logger } : {},
  );
  const database = new BaseSQLiteDatabase("sync", dialect, session, schema);
  return Object.assign(database, { $client: client }) as DrizzleDatabase<TSchema>;
}
