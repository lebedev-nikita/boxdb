import type { Database, QueryResult, Row, SQLParameter, SQLValue } from "../index.js";

export type RowList<T extends object[]> = T & {
  readonly count: number;
  readonly columns: readonly { readonly name: string }[];
};

interface CompiledQuery {
  readonly text: string;
  readonly parameters: readonly SQLParameter[];
}

/** An opaque SQL fragment, including quoted identifiers. */
class Fragment {
  readonly #query: CompiledQuery;

  constructor(query: CompiledQuery) {
    this.#query = query;
  }

  static query(fragment: Fragment): CompiledQuery {
    return fragment.#query;
  }
}

type Interpolation = SQLParameter | Fragment;

/** A composable query that executes at most once, when awaited or executed. */
export class PendingQuery<T extends object[] = Row[]>
  extends Fragment
  implements PromiseLike<RowList<T>>
{
  readonly #run: () => Promise<QueryResult<SQLValue[]>>;
  readonly #arrays: boolean;
  #result: Promise<RowList<T>> | undefined;

  constructor(query: CompiledQuery, run: () => Promise<QueryResult<SQLValue[]>>, arrays = false) {
    super(query);
    this.#run = run;
    this.#arrays = arrays;
  }

  execute(): Promise<RowList<T>> {
    this.#result ??= this.#run().then((result) => {
      const output = this.#arrays
        ? result.rows.map((row) => row.slice())
        : result.rows.map((row) =>
            Object.fromEntries(result.columns.map((name, index) => [name, row[index]])),
          );
      return Object.defineProperties(output, {
        count: { value: result.columns.length > 0 ? result.rows.length : result.changes },
        columns: { value: result.columns.map((name) => ({ name })) },
      }) as RowList<T>;
    });
    return this.#result;
  }

  // oxlint-disable-next-line unicorn/no-thenable -- Await intentionally starts this lazy query.
  then<TResult1 = RowList<T>, TResult2 = never>(
    onfulfilled?: ((value: RowList<T>) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2> {
    return this.execute().then(onfulfilled, onrejected);
  }

  catch<TResult = never>(
    onrejected?: ((reason: unknown) => TResult | PromiseLike<TResult>) | null,
  ): Promise<RowList<T> | TResult> {
    return this.execute().catch(onrejected);
  }

  finally(onfinally?: (() => void) | null): Promise<RowList<T>> {
    return this.execute().finally(onfinally);
  }

  values(): PendingQuery<SQLValue[][]> {
    return new PendingQuery(Fragment.query(this), this.#run, true);
  }
}

export interface Sql {
  <T extends object[] = Row[]>(
    strings: TemplateStringsArray,
    ...values: Interpolation[]
  ): PendingQuery<T>;
  (identifier: string): Fragment;
}

/** Adapt an existing BoxDB connection to the Postgres.js tagged-query style. */
export function adapter(db: Database): Sql {
  function sql<T extends object[] = Row[]>(
    strings: TemplateStringsArray | string,
    ...values: Interpolation[]
  ): PendingQuery<T> | Fragment {
    if (typeof strings === "string") {
      if (values.length > 0) throw new TypeError("Identifier helpers accept one string");
      const names = strings.split(".");
      if (names.some((name) => name.length === 0 || name.includes("\0"))) {
        throw new TypeError("SQL identifiers must have nonempty components without NUL");
      }
      return new Fragment({
        text: names.map((name) => `"${name.replaceAll('"', '""')}"`).join("."),
        parameters: [],
      });
    }
    if (
      !Array.isArray(strings) ||
      !Array.isArray(strings.raw) ||
      strings.length !== values.length + 1
    ) {
      throw new TypeError("Use sql as a tagged template, or sql(name) for identifiers");
    }
    const parameters: SQLParameter[] = [];
    let text = strings[0]!;
    for (let index = 0; index < values.length; index++) {
      const value = values[index]!;
      if (value instanceof Fragment) {
        const query = Fragment.query(value);
        text += query.text;
        parameters.push(...query.parameters);
      } else {
        text += "?";
        parameters.push(value);
      }
      text += strings[index + 1]!;
    }
    let result: Promise<QueryResult<SQLValue[]>> | undefined;
    return new PendingQuery<T>({ text, parameters }, () => {
      if (!result) {
        try {
          for (const value of parameters) {
            if (
              value !== null &&
              !["string", "number", "bigint", "boolean"].includes(typeof value) &&
              !(value instanceof Uint8Array)
            ) {
              throw new TypeError(
                "SQL values must be scalar parameters; serialize objects explicitly",
              );
            }
          }
          result = Promise.resolve(db.prepare(text).raw().query(parameters));
        } catch (error) {
          result = Promise.reject(error);
        }
      }
      return result;
    });
  }
  return sql as Sql;
}
