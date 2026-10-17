import { maintainFile } from "./native.js";
import { fromNativeError } from "./error.js";

export type CheckMode = "quick" | "full";
export type CheckStatus = "valid" | "invalid" | "incomplete";
export interface CheckIssue {
  check: string;
  code: string;
  message: string;
  table?: string;
  column?: string;
  /** Decimal string, retaining the full signed 64-bit range. */
  rowid?: string;
  document?: number;
  line?: number;
}
export interface CheckReport {
  status: CheckStatus;
  complete: boolean;
  format: "binary" | "readable" | null;
  mode: CheckMode;
  checks: { name: string; status: CheckStatus }[];
  issues: CheckIssue[];
}
export interface RejectedDocument {
  document: number;
  line: number;
  code: string;
  message: string;
}
export interface RecoveryReport {
  output: CheckReport;
  recovered_rows: number;
  loss_possible: boolean;
  rejected: RejectedDocument[];
  rejected_path: string | null;
  warnings: string[];
}

async function run<T>(
  operation: string,
  source: string,
  destination: string | undefined,
  mode: string,
): Promise<T> {
  if (typeof source !== "string" || source.includes("\0"))
    throw new TypeError("source must be a path without NUL");
  if (destination !== undefined && (typeof destination !== "string" || destination.includes("\0")))
    throw new TypeError("destination must be a path without NUL");
  try {
    return JSON.parse(await maintainFile(operation, source, destination, mode)) as T;
  } catch (error) {
    throw fromNativeError(error);
  }
}

export function checkFile(path: string, mode: CheckMode = "full"): Promise<CheckReport> {
  if (mode !== "quick" && mode !== "full") throw new TypeError("mode must be quick or full");
  return run("check", path, undefined, mode);
}
export function repairFile(source: string, destination: string): Promise<RecoveryReport> {
  return run("repair", source, destination, "binary");
}
export function recoverFile(
  source: string,
  destination: string,
  format: "binary" | "readable" = "binary",
): Promise<RecoveryReport> {
  if (format !== "binary" && format !== "readable")
    throw new TypeError("format must be binary or readable");
  return run("recover", source, destination, format);
}
