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
export interface NativeResponse {
  inTransaction: boolean;
  info?: NativeInfo;
  result?: NativeResult;
}
export interface NativeConnection {
  call(
    id: number,
    operation: string,
    sql: string,
    parameters: NativeValue[],
  ): Promise<NativeResponse>;
  cancel(id: number): void;
}
const binding = createRequire(import.meta.url)("../native/boxdb.node") as {
  NativeDatabase: new (
    path: string,
    format: string,
    readOnly: boolean,
    busyTimeoutMs: number,
  ) => NativeConnection;
};
export const NativeDatabase = binding.NativeDatabase;
