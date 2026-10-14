import { Buffer } from "node:buffer";
import {
  NativeDatabase,
  type NativeConnection,
  type NativeInfo,
  type NativeResult,
  type NativeValue,
} from "./native.js";

export type SQLValue = null | number | bigint | string | Buffer;
export type SQLParameter = SQLValue | Uint8Array | boolean;
export type Parameters = readonly SQLParameter[] | Readonly<Record<string, SQLParameter>>;
export type Row = Record<string, SQLValue>;
export interface RunResult {
  changes: number;
  lastInsertRowid: number | bigint;
}
export interface QueryResult<T = Row> extends RunResult {
  columns: string[];
  rows: T[];
}
export interface DatabaseOptions {
  format?: "binary" | "readable";
  safeIntegers?: boolean;
}
export interface Transaction<A extends unknown[], R> {
  (...args: A): R;
  deferred: (...args: A) => R;
  immediate: (...args: A) => R;
  exclusive: (...args: A) => R;
}

export class BoxDBError extends Error {
  readonly code = "BOXDB_ERROR";
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "BoxDBError";
  }
}

function call<T>(operation: () => T): T {
  try {
    return operation();
  } catch (error) {
    if (error instanceof BoxDBError) throw error;
    throw new BoxDBError(error instanceof Error ? error.message : String(error), { cause: error });
  }
}

function value(parameter: unknown): NativeValue {
  if (parameter === null || typeof parameter === "string" || typeof parameter === "bigint")
    return parameter;
  if (typeof parameter === "number") {
    if (!Number.isFinite(parameter)) throw new TypeError("SQL numbers must be finite");
    if (Number.isInteger(parameter) && !Number.isSafeInteger(parameter)) {
      throw new RangeError("Use bigint for integers outside the JavaScript safe range");
    }
    return parameter;
  }
  if (typeof parameter === "boolean") return parameter ? 1 : 0;
  if (parameter instanceof Uint8Array) return Buffer.from(parameter);
  throw new TypeError(
    "SQL parameters must be null, string, number, bigint, boolean, or Uint8Array",
  );
}

function integer(value: bigint, safeIntegers: boolean): number | bigint {
  if (safeIntegers) return value;
  const number = Number(value);
  if (!Number.isSafeInteger(number))
    throw new RangeError("Use safeIntegers: true to read 64-bit integers without precision loss");
  return number;
}

function record(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof Uint8Array)
  );
}

/** An owning, synchronous BoxDB connection backed by the Rust implementation. */
export class Database {
  readonly name: string;
  readonly memory: boolean;
  readonly readonly = false;
  #connection: NativeConnection;
  #open = true;
  #safeIntegers: boolean;
  #savepoint = 0;

  constructor(path = ":memory:", options: DatabaseOptions = {}) {
    if (typeof path !== "string") throw new TypeError("Database path must be a string");
    if (
      options.format !== undefined &&
      options.format !== "binary" &&
      options.format !== "readable"
    ) {
      throw new TypeError("format must be binary or readable");
    }
    this.name = path;
    this.memory = path === ":memory:";
    this.#safeIntegers = options.safeIntegers ?? false;
    this.#connection = call(() => new NativeDatabase(path, options.format ?? "binary"));
  }

  get open(): boolean {
    return this.#open;
  }
  get inTransaction(): boolean {
    return this.#open && call(() => this.#connection.inTransaction);
  }

  /** Execute exactly one statement, returning rows and write metadata. */
  query<T = Row>(sql: string, parameters: Parameters = []): QueryResult<T> {
    return this.prepare<T>(sql).query(parameters);
  }

  all<T = Row>(sql: string, parameters: Parameters = []): T[] {
    return this.query<T>(sql, parameters).rows;
  }

  get<T = Row>(sql: string, parameters: Parameters = []): T | undefined {
    return this.all<T>(sql, parameters)[0];
  }

  run(sql: string, parameters: Parameters = []): RunResult {
    return this.prepare(sql).run(parameters);
  }

  prepare<T = Row>(sql: string): Statement<T> {
    if (typeof sql !== "string") throw new TypeError("SQL must be a string");
    const info = call(() => this.#connection.prepare(sql));
    return new Statement<T>(sql, info, this.#safeIntegers, (parameters) =>
      call(() => this.#connection.execute(sql, parameters)),
    );
  }

  /** Execute a SQL script. Use bound parameters for application queries. */
  exec(sql: string): this {
    call(() => this.#connection.exec(sql));
    return this;
  }

  pragma<T = Row>(sql: string, options: { simple?: boolean } = {}): T[] | SQLValue | undefined {
    const result = this.query(`PRAGMA ${sql}`);
    if (options.simple) {
      const column = result.columns[0];
      return column === undefined ? undefined : result.rows[0]?.[column];
    }
    return result.rows as T[];
  }

  defaultSafeIntegers(enabled = true): this {
    this.#safeIntegers = enabled;
    return this;
  }

  transaction<A extends unknown[], R>(fn: (...args: A) => R): Transaction<A, R> {
    if (typeof fn !== "function") throw new TypeError("Transaction callback must be a function");
    if (Object.prototype.toString.call(fn) === "[object AsyncFunction]") {
      throw new TypeError("Transaction callbacks must be synchronous");
    }
    const wrap =
      (mode: "deferred" | "immediate" | "exclusive") =>
      (...args: A): R => {
        const nested = this.inTransaction;
        const savepoint = `boxdb_ts_${++this.#savepoint}`;
        this.exec(nested ? `SAVEPOINT ${savepoint}` : `BEGIN ${mode}`);
        try {
          const result = fn(...args);
          if (
            result !== null &&
            (typeof result === "object" || typeof result === "function") &&
            "then" in result
          ) {
            throw new TypeError("Transaction callbacks must be synchronous");
          }
          this.exec(nested ? `RELEASE ${savepoint}` : "COMMIT");
          return result;
        } catch (error) {
          if (this.inTransaction) {
            try {
              this.exec(nested ? `ROLLBACK TO ${savepoint}; RELEASE ${savepoint}` : "ROLLBACK");
            } catch (rollbackError) {
              throw new AggregateError([error, rollbackError], "Transaction and rollback failed");
            }
          }
          throw error;
        }
      };
    return Object.assign(wrap("deferred"), {
      deferred: wrap("deferred"),
      immediate: wrap("immediate"),
      exclusive: wrap("exclusive"),
    });
  }

  close(): this {
    this.#connection.close();
    this.#open = false;
    return this;
  }

  [Symbol.dispose](): void {
    this.close();
  }
}

/** The better-sqlite3 subset used by Kysely and the BoxDB Drizzle adapter. */
export class Statement<T = Row> {
  readonly source: string;
  readonly readonly: boolean;
  readonly reader: boolean;
  #info: NativeInfo;
  #execute: (parameters: NativeValue[]) => NativeResult;
  #safeIntegers: boolean;
  #raw = false;

  constructor(
    sql: string,
    info: NativeInfo,
    safeIntegers: boolean,
    execute: (parameters: NativeValue[]) => NativeResult,
  ) {
    this.source = sql;
    this.readonly = info.readonly;
    this.reader = info.columns.length > 0;
    this.#info = info;
    this.#execute = execute;
    this.#safeIntegers = safeIntegers;
  }

  #bind(args: unknown[]): NativeValue[] {
    const parameters = args.length === 1 && Array.isArray(args[0]) ? (args[0] as unknown[]) : args;
    const named = record(parameters[0]) ? parameters[0] : undefined;
    const positional = named ? parameters.slice(1) : parameters;
    const used = new Set<string>();
    let position = 0;
    const result = this.#info.parameters.map((name) => {
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

  #result(args: unknown[]): QueryResult<T> {
    const result = this.#execute(this.#bind(args));
    const rows = result.rows.map((row) =>
      row.map((cell) => (typeof cell === "bigint" ? integer(cell, this.#safeIntegers) : cell)),
    );
    return {
      columns: result.columns,
      rows: (this.#raw
        ? rows
        : rows.map((row) =>
            Object.fromEntries(result.columns.map((name, index) => [name, row[index]])),
          )) as T[],
      changes: result.changes,
      lastInsertRowid: integer(
        result.lastInsertRowid,
        this.#safeIntegers || !Number.isSafeInteger(Number(result.lastInsertRowid)),
      ),
    };
  }

  query(...parameters: unknown[]): QueryResult<T> {
    return this.#result(parameters);
  }
  all(...parameters: unknown[]): T[] {
    return this.#result(parameters).rows;
  }
  get(...parameters: unknown[]): T | undefined {
    return this.#result(parameters).rows[0];
  }
  run(...parameters: unknown[]): RunResult {
    if (this.reader)
      throw new TypeError("Use query(), all(), or get() for statements returning rows");
    const { changes, lastInsertRowid } = this.#result(parameters);
    return { changes, lastInsertRowid };
  }
  *iterate(...parameters: unknown[]): IterableIterator<T> {
    yield* this.all(...parameters);
  }
  raw(enabled?: true): Statement<SQLValue[]>;
  raw(enabled: false): Statement<T>;
  raw(enabled: boolean): Statement<T | SQLValue[]>;
  raw(enabled = true): Statement<T | SQLValue[]> {
    this.#raw = enabled;
    return this as Statement<T | SQLValue[]>;
  }
  safeIntegers(enabled = true): this {
    this.#safeIntegers = enabled;
    return this;
  }
}

export default Database;
