import { createRequire } from "node:module";
import type { MigrationFile, MigrationInfo, MigrationStatus } from "./migrations.js";

export const runMigrations = Symbol("runMigrations");
export type NativeMigrationSet = object;

export type NativeValue = null | number | bigint | string | Buffer;
export interface NativeInfo {
  columns: string[];
  parameters: (string | null | undefined)[];
  readonly: boolean;
}
export interface NativeResult {
  columns: string[];
  rows: NativeValue[][];
  changes: bigint;
  lastInsertRowid: bigint;
}
export interface NativeResponse {
  inTransaction: boolean;
  info?: NativeInfo;
  result?: NativeResult;
  migrations?: MigrationInfo[];
  statuses?: MigrationStatus[];
}
export interface NativeConnection {
  call(
    id: number,
    operation: string,
    sql: string,
    parameters: NativeValue[],
    readOnly: boolean,
    migrations?: NativeMigrationSet,
  ): Promise<NativeResponse>;
  cancel(id: number): void;
}
const platform = `${process.platform}-${process.arch}`;
if (platform !== "darwin-arm64" && platform !== "linux-x64") {
  throw new Error(
    `BoxDB does not support ${platform}. Supported: macOS ARM64 and Linux x64 (glibc 2.35+).`,
  );
}
const binding = createRequire(import.meta.url)(`../native/${platform}/boxdb.node`) as {
  NativeMigrationSet: new (files: readonly MigrationFile[]) => NativeMigrationSet;
  NativeDatabase: new (
    path: string,
    format: string,
    readOnly: boolean,
    busyTimeoutMs: number,
  ) => NativeConnection;
};
export const NativeDatabase = binding.NativeDatabase;
export const NativeMigrationSet = binding.NativeMigrationSet;
