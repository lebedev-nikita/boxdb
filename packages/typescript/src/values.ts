import { Buffer } from "node:buffer";
import type {
  ClientResult,
  NativeChangeset,
  NativeInfo,
  NativeResult,
  NativeValue,
} from "./native.js";
import type { ApplyReport, QueryResult } from "./types.js";

/** Recorded row changes in SQLite's changeset format, as stored in CHANGESET columns. */
export class Changeset {
  readonly bytes: Buffer;
  constructor(bytes: Uint8Array) {
    if (!(bytes instanceof Uint8Array)) throw new TypeError("Changeset requires bytes");
    this.bytes = Buffer.from(bytes);
  }
}
function isNativeChangeset(value: NativeValue): value is NativeChangeset {
  return typeof value === "object" && value !== null && !(value instanceof Uint8Array);
}
export function fromNative(value: NativeValue): unknown {
  return isNativeChangeset(value) ? new Changeset(value.changeset) : value;
}
export function parseReport(value: unknown): ApplyReport {
  if (typeof value !== "string") throw new TypeError("boxdb_apply returned no report");
  return JSON.parse(value) as ApplyReport;
}

export function value(parameter: unknown): NativeValue {
  if (typeof parameter === "bigint") {
    if (parameter < -9223372036854775808n || parameter > 18446744073709551615n)
      throw new RangeError("bigint is outside the BoxDB 64-bit range");
    return parameter;
  }
  if (parameter === null || typeof parameter === "string") return parameter;
  if (typeof parameter === "number") {
    if (!Number.isFinite(parameter)) throw new TypeError("SQL numbers must be finite");
    if (Number.isInteger(parameter) && !Number.isSafeInteger(parameter)) {
      throw new RangeError("Use bigint for integers outside the JavaScript safe range");
    }
    return Number.isInteger(parameter) ? BigInt(parameter) : parameter;
  }
  if (typeof parameter === "boolean") return parameter ? 1n : 0n;
  if (parameter instanceof Changeset) return { changeset: parameter.bytes };
  if (parameter instanceof Uint8Array) return Buffer.from(parameter);
  throw new TypeError(
    "SQL parameters must be null, string, number, bigint, boolean, Uint8Array, or Changeset",
  );
}

export function record(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof Uint8Array) &&
    !(value instanceof Changeset)
  );
}

export function bindParameters(info: NativeInfo, args: unknown[]): NativeValue[] {
  const parameters = args.length === 1 && Array.isArray(args[0]) ? (args[0] as unknown[]) : args;
  const named = record(parameters[0]) ? parameters[0] : undefined;
  const positional = named ? parameters.slice(1) : parameters;
  const used = new Set<string>();
  let position = 0;
  const result = info.parameters.map((name) => {
    if (named && name && !name.startsWith("?")) {
      const key = Object.hasOwn(named, name) ? name : name.slice(1);
      if (!Object.hasOwn(named, key)) throw new RangeError(`Missing SQL parameter: ${name}`);
      used.add(key);
      return value(named[key]);
    }
    if (position >= positional.length) throw new RangeError("Too few SQL parameters");
    return value(positional[position++]);
  });
  if (
    position !== positional.length ||
    (named && Object.keys(named).some((key) => !used.has(key)))
  ) {
    throw new RangeError("Too many SQL parameters");
  }
  return result;
}

export function clientResult(result: NativeResult): ClientResult {
  return { ...result, rows: result.rows.map((row) => row.map(fromNative)) };
}
export function queryResult(result: ClientResult, raw = false): QueryResult {
  const rows = result.rows;
  return {
    columns: result.columns,
    rows: raw
      ? rows
      : rows.map((row) =>
          Object.fromEntries(result.columns.map((name, index) => [name, row[index]])),
        ),
    changes: result.changes,
    lastInsertRowid: result.lastInsertRowid,
  };
}
