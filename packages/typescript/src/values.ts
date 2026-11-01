import { Buffer } from "node:buffer";
import type { NativeInfo, NativeResult, NativeValue } from "./native.js";
import type { QueryResult } from "./types.js";

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
  if (parameter instanceof Uint8Array) return Buffer.from(parameter);
  throw new TypeError(
    "SQL parameters must be null, string, number, bigint, boolean, or Uint8Array",
  );
}

export function record(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof Uint8Array)
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

export function queryResult(result: NativeResult, raw = false): QueryResult {
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
