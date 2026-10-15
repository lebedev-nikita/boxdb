export type SQLValue = null | number | bigint | string | Buffer;
export type SQLParameter = SQLValue | Uint8Array | boolean;
export type Parameters = readonly SQLParameter[] | Readonly<Record<string, SQLParameter>>;
export interface RunResult {
  changes: bigint;
  lastInsertRowid: bigint;
}
export interface QueryResult extends RunResult {
  columns: string[];
  rows: unknown[];
}
export interface BoxDBOptions {
  signal?: AbortSignal;
  format?: "binary" | "readable";
  readOnly?: boolean;
  busyTimeoutMs?: number;
  maxPendingQueries?: number;
  /** Default transaction lifetime in milliseconds after BEGIN succeeds; defaults to 5000. */
  transactionTimeoutMs?: number;
}
