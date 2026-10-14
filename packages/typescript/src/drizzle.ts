import type { Database as BetterSQLiteDatabase } from "better-sqlite3";
import { sql } from "drizzle-orm";
import { BetterSQLiteSession } from "drizzle-orm/better-sqlite3/session";
import { DefaultLogger } from "drizzle-orm/logger";
import { readMigrationFiles, type MigrationConfig } from "drizzle-orm/migrator";
import { createTableRelationsHelpers, extractTablesRelationalConfig } from "drizzle-orm/relations";
import { BaseSQLiteDatabase, SQLiteSyncDialect } from "drizzle-orm/sqlite-core";
import type { DrizzleConfig } from "drizzle-orm/utils";
import { Database, type RunResult } from "./index.js";

export type DrizzleDatabase<TSchema extends Record<string, unknown> = Record<string, never>> =
  BaseSQLiteDatabase<"sync", RunResult, TSchema> & {
    $client: Database;
    $migrate(config: MigrationConfig): void;
  };

class BoxDBDialect extends SQLiteSyncDialect {
  override migrate(...args: Parameters<SQLiteSyncDialect["migrate"]>): void {
    const [migrations, session, config] = args;
    const table = sql.identifier(
      typeof config === "object"
        ? (config.migrationsTable ?? "__drizzle_migrations")
        : "__drizzle_migrations",
    );
    // Drizzle's journal timestamps are integers; BoxDB has no NUMERIC type.
    session.run(sql`CREATE TABLE IF NOT EXISTS ${table} (
      id INTEGER PRIMARY KEY, hash TEXT NOT NULL, created_at INTEGER NOT NULL
    )`);
    const [last] = session.values<[number, string, number]>(
      sql`SELECT id,hash,created_at FROM ${table} ORDER BY created_at DESC LIMIT 1`,
    );
    session.run(sql`BEGIN`);
    try {
      for (const migration of migrations) {
        if (!last || Number(last[2]) < migration.folderMillis) {
          for (const statement of migration.sql) session.run(sql.raw(statement));
          session.run(
            sql`INSERT INTO ${table}(hash,created_at) VALUES(${migration.hash},${migration.folderMillis})`,
          );
        }
      }
      session.run(sql`COMMIT`);
    } catch (error) {
      session.run(sql`ROLLBACK`);
      throw error;
    }
  }
}

/** Use Drizzle's SQLite session with BoxDB, without loading better-sqlite3. */
export function drizzle<TSchema extends Record<string, unknown> = Record<string, never>>(
  client: Database,
  config: Omit<DrizzleConfig<TSchema>, "cache"> = {},
): DrizzleDatabase<TSchema> {
  const dialect = new BoxDBDialect(config.casing === undefined ? {} : { casing: config.casing });
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
  return Object.assign(database, {
    $client: client,
    $migrate(config: MigrationConfig): void {
      dialect.migrate(readMigrationFiles(config), session, config);
    },
  }) as unknown as DrizzleDatabase<TSchema>;
}
