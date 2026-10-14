import { Query, fragment, identifier, type Interpolation } from "./sql.js";
import { BoxDBError } from "./error.js";
import type { DatabaseOptions, Parameters, QueryResult, Row } from "./types.js";
import {
  NativeDatabase,
  type NativeConnection,
  type NativeResponse,
  type NativeValue,
} from "./native.js";
import { bindParameters, queryResult } from "./values.js";

export interface QueryOptions {
  signal?: AbortSignal;
  raw?: boolean;
}
export interface TransactionOptions extends QueryOptions {
  mode?: "deferred" | "immediate" | "exclusive";
}

export interface Diagnostics {
  readonly pendingQueries: number;
  readonly activeQueries: number;
  readonly completedQueries: number;
  readonly lastQueueWaitMs: number;
  readonly lastExecutionMs: number;
}
class Queue {
  tail: Promise<void> = Promise.resolve();
  pending = 0;
  active = 0;
  completed = 0;
  waitMs = 0;
  executionMs = 0;
  constructor(private readonly limit = Infinity) {}
  get diagnostics(): Diagnostics {
    return {
      pendingQueries: this.pending,
      activeQueries: this.active,
      completedQueries: this.completed,
      lastQueueWaitMs: this.waitMs,
      lastExecutionMs: this.executionMs,
    };
  }
  run<T>(operation: () => Promise<T>, signal?: AbortSignal, cleanup = false): Promise<T> {
    if (signal?.aborted) return Promise.reject(signal.reason);
    if (!cleanup && this.pending >= this.limit)
      return Promise.reject(new BoxDBError("query queue is full", { code: "BOXDB_QUEUE_FULL" }));
    let waiting = true;
    const enqueued = performance.now();
    this.pending++;
    const leaveQueue = () => {
      if (waiting) {
        waiting = false;
        this.pending--;
      }
    };
    const result = this.tail.then(async () => {
      leaveQueue();
      signal?.throwIfAborted();
      const started = performance.now();
      this.waitMs = started - enqueued;
      this.active++;
      try {
        return await operation();
      } finally {
        this.active--;
        this.completed++;
        this.executionMs = performance.now() - started;
      }
    });
    this.tail = result.then(
      () => {},
      () => {},
    );
    if (!signal) return result;
    return new Promise<T>((resolve, reject) => {
      const cancel = () => {
        if (waiting) {
          leaveQueue();
          reject(signal.reason);
        }
      };
      signal.addEventListener("abort", cancel, { once: true });
      if (signal.aborted) cancel();
      result.then(
        (value) => {
          signal.removeEventListener("abort", cancel);
          resolve(value);
        },
        (error) => {
          signal.removeEventListener("abort", cancel);
          reject(error);
        },
      );
    });
  }
}

class Connection {
  closing = false;
  readonly shutdown = new AbortController();
  inTransaction = false;
  owner: TransactionSession | undefined;
  #native: NativeConnection;
  #requestId = 0;

  constructor(path: string, options: DatabaseOptions) {
    this.#native = new NativeDatabase(
      path,
      options.format ?? "binary",
      options.readOnly ?? false,
      options.busyTimeoutMs ?? 5000,
    );
  }

  async begin(mode: string, signal?: AbortSignal): Promise<void> {
    if (!["deferred", "immediate", "exclusive"].includes(mode)) {
      throw new TypeError("Invalid transaction mode");
    }
    if (this.inTransaction) throw new BoxDBError("connection already has an active transaction");
    try {
      await this.request("control", `BEGIN ${mode}`, [], signal);
      signal?.throwIfAborted();
    } catch (error) {
      if (this.inTransaction) {
        try {
          await this.request("rollback");
        } catch (rollbackError) {
          throw new AggregateError([error, rollbackError], "Transaction and rollback failed");
        }
      }
      throw error;
    }
  }

  async request(
    operation: string,
    sql = "",
    parameters: NativeValue[] = [],
    signal?: AbortSignal,
  ): Promise<NativeResponse> {
    signal?.throwIfAborted();
    const id = ++this.#requestId;
    const cancel = () => this.#native.cancel(id);
    try {
      const pending = this.#native.call(id, operation, sql, parameters);
      signal?.addEventListener("abort", cancel, { once: true });
      const response = await pending;
      this.inTransaction = response.inTransaction;
      return response;
    } catch (cause) {
      if (typeof (cause as { inTransaction?: unknown }).inTransaction === "boolean") {
        this.inTransaction = (cause as { inTransaction: boolean }).inTransaction;
      }
      signal?.throwIfAborted();
      const error = cause as Error & {
        code?: string;
        resultCode?: number;
        extendedCode?: number;
      };
      throw new BoxDBError(error instanceof Error ? error.message : String(cause), {
        cause,
        code:
          typeof error?.code === "string" && error.code.startsWith("BOXDB_")
            ? error.code
            : "BOXDB_INTERNAL",
        ...(typeof error?.resultCode === "number" ? { resultCode: error.resultCode } : {}),
        ...(typeof error?.extendedCode === "number" ? { extendedCode: error.extendedCode } : {}),
      });
    } finally {
      signal?.removeEventListener("abort", cancel);
    }
  }

  async query<T>(
    sql: string,
    parameters: Parameters,
    signal?: AbortSignal,
    writeOnly = false,
    raw = false,
  ): Promise<QueryResult<T>> {
    if (typeof sql !== "string") throw new TypeError("SQL must be a string");
    const { info } = await this.request("prepare", sql, [], signal);
    if (writeOnly && info!.columns.length > 0)
      throw new TypeError(
        "Use query.execute(), query.get(), or query.values() for statements returning rows",
      );
    const bound = bindParameters(info!, [parameters]);
    const { result } = await this.request("execute", sql, bound, signal);
    return queryResult<T>(result!, raw);
  }
}

class Queries {
  constructor(
    protected readonly connection: Connection,
    protected readonly queue: Queue,
    private readonly signal?: AbortSignal,
    private readonly root = false,
  ) {}

  protected schedule<T>(
    operation: (signal?: AbortSignal) => Promise<T>,
    options: QueryOptions = {},
  ): Promise<T> {
    if (this.root && this.connection.closing)
      return Promise.reject(new BoxDBError("database is closed", { code: "BOXDB_CLOSED" }));
    if (this.root && this.connection.owner)
      return Promise.reject(new BoxDBError("Use the active transaction handle"));
    if (!this.root && (this.connection.owner as Queries | undefined) !== this)
      return Promise.reject(
        new BoxDBError("transaction is closed or has an active nested transaction"),
      );
    const signals = [this.connection.shutdown.signal, this.signal, options.signal].filter(
      (signal): signal is AbortSignal => signal !== undefined,
    );
    const signal = signals.length === 1 ? signals[0]! : AbortSignal.any(signals);
    return this.queue.run(() => {
      if (this.root && this.connection.owner)
        throw new BoxDBError("Use the active transaction handle");
      if (!this.root && (this.connection.owner as Queries | undefined) !== this)
        throw new BoxDBError("transaction is closed or has an active nested transaction");
      if (!this.root && !this.connection.inTransaction)
        throw new BoxDBError("transaction has ended");
      return operation(signal);
    }, signal);
  }

  readonly fragment = fragment;
  readonly identifier = identifier;
  query<T = Row>(strings: TemplateStringsArray, ...values: Interpolation[]): Query<T> {
    return new Query<T>(
      fragment(strings, ...values).compile(),
      (sql, parameters, options, writeOnly) =>
        this.schedule(
          (signal) =>
            this.connection.query<import("./types.js").SQLValue[]>(
              sql,
              parameters,
              signal,
              writeOnly,
              true,
            ),
          options,
        ),
    );
  }

  execute<T = Row>(
    sql: string,
    parameters: Parameters = [],
    options: QueryOptions = {},
  ): Promise<QueryResult<T>> {
    return this.schedule(
      (signal) => this.connection.query<T>(sql, parameters, signal, false, options.raw),
      options,
    );
  }
  exec(sql: string, options: QueryOptions = {}): Promise<void> {
    return this.schedule(async (signal) => {
      if (typeof sql !== "string") throw new TypeError("SQL must be a string");
      await this.connection.request("exec", sql, [], signal);
    }, options);
  }
}

export interface TransactionClient extends Pick<
  Queries,
  "query" | "execute" | "fragment" | "identifier" | "exec"
> {
  begin(options?: QueryOptions): Promise<TransactionHandle>;
}
export interface TransactionHandle extends TransactionClient {
  readonly active: boolean;
  commit(): Promise<void>;
  rollback(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}
let savepoint = 0;
class TransactionSession extends Queries implements TransactionHandle {
  #active = true;
  #finishing: Promise<void> | undefined;
  readonly finished: Promise<void>;
  #release!: () => void;
  #cancel = () => {
    void this.rollback().catch(() => {});
  };
  constructor(
    connection: Connection,
    queue: Queue,
    private readonly transactionSignal?: AbortSignal,
    private readonly parent?: TransactionSession,
    private readonly savepointName?: string,
  ) {
    super(connection, queue, transactionSignal);
    this.finished = new Promise((resolve) => {
      this.#release = resolve;
    });
    connection.owner = this;
    transactionSignal?.addEventListener("abort", this.#cancel, { once: true });
    if (transactionSignal?.aborted) this.#cancel();
  }
  get active(): boolean {
    return this.#active && this.connection.inTransaction;
  }
  begin(options: QueryOptions = {}): Promise<TransactionHandle> {
    const signal =
      this.transactionSignal && options.signal
        ? AbortSignal.any([this.transactionSignal, options.signal])
        : (this.transactionSignal ?? options.signal);
    return this.schedule(async () => {
      const name = `boxdb_${++savepoint}`;
      await this.connection.request("control", `SAVEPOINT ${name}`, [], signal);
      return new TransactionSession(this.connection, this.queue, signal, this, name);
    }, options);
  }
  commit(): Promise<void> {
    if (!this.active || (this.connection.owner as Queries | undefined) !== this || this.#finishing)
      return Promise.reject(
        new BoxDBError("transaction is closed or has an active nested transaction"),
      );
    return this.finish(true);
  }
  rollback(): Promise<void> {
    if (this.#finishing) return this.#finishing.catch(() => this.rollback());
    if (!this.#active) return Promise.resolve();
    return this.finish(false);
  }
  private release(): void {
    this.#active = false;
    this.transactionSignal?.removeEventListener("abort", this.#cancel);
    this.connection.owner = this.parent;
    this.#release();
  }
  private finish(commit: boolean): Promise<void> {
    const operation = this.queue.run(
      async () => {
        if (!this.#active) return;
        if (commit && this.connection.owner !== this)
          throw new BoxDBError("transaction has an active nested transaction");
        if (!commit) {
          while (this.connection.owner && (this.connection.owner as Queries | undefined) !== this) {
            const child = this.connection.owner;
            if (this.connection.inTransaction && child.savepointName)
              await this.connection.request(
                "control",
                `ROLLBACK TO ${child.savepointName}; RELEASE ${child.savepointName}`,
              );
            child.release();
          }
        }
        try {
          if (this.connection.inTransaction) {
            if (commit)
              await this.connection.request(
                "control",
                this.savepointName ? `RELEASE ${this.savepointName}` : "COMMIT",
                [],
                this.transactionSignal,
              );
            else if (this.savepointName)
              await this.connection.request(
                "control",
                `ROLLBACK TO ${this.savepointName}; RELEASE ${this.savepointName}`,
              );
            else await this.connection.request("rollback");
          } else if (commit) throw new BoxDBError("transaction has ended");
          this.release();
        } catch (error) {
          if (!this.connection.inTransaction) this.release();
          throw error;
        }
      },
      undefined,
      true,
    );
    this.#finishing = operation;
    void operation.then(
      () => {
        this.#finishing = undefined;
      },
      () => {
        this.#finishing = undefined;
      },
    );
    return operation;
  }
  async [Symbol.asyncDispose](): Promise<void> {
    await this.rollback();
  }
}

/** A connection owned by a dedicated native thread. */
export class Database extends Queries {
  readonly name: string;
  readonly memory: boolean;

  private constructor(path: string, options: DatabaseOptions) {
    super(new Connection(path, options), new Queue(options.maxPendingQueries), undefined, true);
    this.name = path;
    this.memory = path === ":memory:";
  }

  static async open(path = ":memory:", options: DatabaseOptions = {}): Promise<Database> {
    if (typeof path !== "string") throw new TypeError("Database path must be a string");
    if (
      options.format !== undefined &&
      options.format !== "binary" &&
      options.format !== "readable"
    ) {
      throw new TypeError("format must be binary or readable");
    }
    if (
      options.busyTimeoutMs !== undefined &&
      (!Number.isInteger(options.busyTimeoutMs) ||
        options.busyTimeoutMs < 0 ||
        options.busyTimeoutMs > 0xffffffff)
    )
      throw new RangeError("busyTimeoutMs must be an unsigned 32-bit integer");
    if (
      options.maxPendingQueries !== undefined &&
      (!Number.isSafeInteger(options.maxPendingQueries) || options.maxPendingQueries < 1)
    )
      throw new RangeError("maxPendingQueries must be a positive safe integer");
    options.signal?.throwIfAborted();
    const db = new Database(path, options);
    try {
      await db.connection.request("open", "", [], options.signal);
      options.signal?.throwIfAborted();
    } catch (error) {
      await db.close().catch(() => {});
      throw error;
    }
    return db;
  }

  get diagnostics(): Diagnostics {
    return this.queue.diagnostics;
  }

  get open(): boolean {
    return !this.connection.closing;
  }

  begin(options: TransactionOptions = {}): Promise<TransactionHandle> {
    return this.schedule(async (signal) => {
      await this.connection.begin(options.mode ?? "deferred", signal);
      return new TransactionSession(this.connection, this.queue, signal);
    }, options);
  }

  #closing: Promise<void> | undefined;
  close(options: { mode?: "drain" | "abort" } = {}): Promise<void> {
    if (options.mode && !["drain", "abort"].includes(options.mode))
      return Promise.reject(new TypeError("Invalid close mode"));
    if (options.mode === "abort") {
      this.connection.shutdown.abort(
        new BoxDBError("database closed by abort", { code: "BOXDB_CLOSED" }),
      );
      if (this.connection.owner) void this.connection.owner.rollback().catch(() => {});
    }
    if (this.#closing) return this.#closing;
    this.connection.closing = true;
    this.#closing = (async () => {
      await this.queue.tail;
      while (this.connection.owner) await this.connection.owner.finished;
      await this.connection.request("close");
    })();
    return this.#closing;
  }
  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }
}
