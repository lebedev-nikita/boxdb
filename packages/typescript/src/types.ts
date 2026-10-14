export type SQLValue = null | number | bigint | string | Buffer;
export type SQLParameter = SQLValue | Uint8Array | boolean;
export type Parameters = readonly SQLParameter[] | Readonly<Record<string, SQLParameter>>;
export type Row = Record<string, SQLValue>;
export interface RunResult {
  changes: bigint;
  lastInsertRowid: bigint;
}
export interface QueryResult<T = Row> extends RunResult {
  columns: string[];
  rows: T[];
}
export interface DatabaseOptions {
  signal?: AbortSignal;
  format?: "binary" | "readable";
  readOnly?: boolean;
  busyTimeoutMs?: number;
  maxPendingQueries?: number;
}
