import type { Changeset } from "./values.js";

export type SQLValue = null | number | bigint | string | Buffer | Changeset;
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
export type ConflictPolicy = "abort" | "skip" | "replace";
export interface ApplyOptions {
  /** Apply the inverse of the changeset, undoing it. */
  invert?: boolean;
  /** How to treat rows that no longer match the changeset; defaults to abort. */
  onConflict?: ConflictPolicy;
  signal?: AbortSignal;
}
export interface ChangesetConflict {
  table: string;
  operation: "insert" | "update" | "delete";
  kind: "data" | "not_found" | "conflict" | "constraint";
  /** Primary key values of the affected row. */
  key: unknown[];
}
export interface ApplyReport {
  applied: number;
  skipped: number;
  conflicts: ChangesetConflict[];
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
