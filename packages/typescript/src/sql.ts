import type { QueryOptions } from "./database.js";
import type { Parameters, QueryResult, Row, RunResult, SQLParameter, SQLValue } from "./types.js";

export interface CompiledQuery {
  readonly sql: string;
  readonly parameters: readonly SQLParameter[];
}
export class Fragment {
  readonly #compiled: CompiledQuery;
  constructor(sql: string, parameters: readonly SQLParameter[]) {
    this.#compiled = {
      sql,
      parameters: parameters.map((value) =>
        value instanceof Uint8Array ? Buffer.from(value) : value,
      ),
    };
  }
  compile(): CompiledQuery {
    return {
      sql: this.#compiled.sql,
      parameters: this.#compiled.parameters.map((value) =>
        value instanceof Uint8Array ? Buffer.from(value) : value,
      ),
    };
  }
}
export type Interpolation = SQLParameter | Fragment;
export function fragment(strings: TemplateStringsArray, ...values: Interpolation[]): Fragment {
  if (!Array.isArray(strings.raw) || strings.length !== values.length + 1)
    throw new TypeError("Use a tagged SQL template");
  let sql = strings[0]!;
  const parameters: SQLParameter[] = [];
  values.forEach((value, index) => {
    if (value instanceof Fragment) {
      const compiled = value.compile();
      sql += compiled.sql;
      parameters.push(...compiled.parameters);
    } else {
      sql += "?";
      parameters.push(value);
    }
    sql += strings[index + 1]!;
  });
  return new Fragment(sql, parameters);
}
export function identifier(...parts: string[]): Fragment {
  if (
    !parts.length ||
    parts.some((part) => typeof part !== "string" || !part.length || part.includes("\0"))
  )
    throw new TypeError("Identifiers require nonempty components without NUL");
  return new Fragment(parts.map((part) => `"${part.replaceAll('"', '""')}"`).join("."), []);
}
type Execute = (
  sql: string,
  parameters: Parameters,
  options: QueryOptions,
  writeOnly: boolean,
) => Promise<QueryResult<SQLValue[]>>;
export class Query<T = Row> extends Fragment implements PromiseLike<T[]> {
  #result?: Promise<QueryResult<SQLValue[]>>;
  constructor(
    compiled: CompiledQuery,
    private readonly executeQuery: Execute,
  ) {
    super(compiled.sql, compiled.parameters);
  }
  private result(options: QueryOptions = {}, writeOnly = false): Promise<QueryResult<SQLValue[]>> {
    if (!this.#result) {
      const { sql, parameters } = this.compile();
      this.#result = this.executeQuery(sql, parameters, { ...options, raw: true }, writeOnly);
    }
    return this.#result;
  }
  async execute(options: QueryOptions = {}): Promise<T[]> {
    const result = await this.result(options);
    return result.rows.map(
      (row) => Object.fromEntries(result.columns.map((name, index) => [name, row[index]])) as T,
    );
  }
  async get(options: QueryOptions = {}): Promise<T | undefined> {
    return (await this.execute(options))[0];
  }
  async values(options: QueryOptions = {}): Promise<SQLValue[][]> {
    return (await this.result(options)).rows.map((row) => row.slice());
  }
  async run(options: QueryOptions = {}): Promise<RunResult> {
    const result = await this.result(options, true);
    if (result.columns.length)
      throw new TypeError("run() requires a statement without returned rows");
    return { changes: result.changes, lastInsertRowid: result.lastInsertRowid };
  }
  // oxlint-disable-next-line unicorn/no-thenable -- Await starts the lazy query.
  then<A = T[], B = never>(
    resolve?: ((rows: T[]) => A | PromiseLike<A>) | null,
    reject?: ((error: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    return this.execute().then(resolve, reject);
  }
}
