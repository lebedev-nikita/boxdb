import { createRequire } from "node:module";

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
export interface NativeConnection {
  prepare(sql: string): NativeInfo;
  execute(sql: string, parameters: NativeValue[]): NativeResult;
  exec(sql: string): void;
  readonly inTransaction: boolean;
  close(): void;
}
const binding = createRequire(import.meta.url)("../native/boxdb.node") as {
  NativeDatabase: new (path: string, format: string) => NativeConnection;
};
export const NativeDatabase = binding.NativeDatabase;
