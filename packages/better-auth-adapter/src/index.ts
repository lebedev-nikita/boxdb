import type { BetterAuthOptions } from "better-auth";
import {
  createAdapterFactory,
  type CleanedWhere,
  type DBAdapterDebugLogOption,
  type DBAdapter,
  type ModelTarget,
} from "better-auth/adapters";
import type { BoxDB, TransactionClient, SQLParameter } from "@boxdb/client";

export interface BetterAuthAdapterOptions {
  usePlural?: boolean;
  debugLogs?: DBAdapterDebugLogOption;
}
const quote = (name: string) => `"${name.replaceAll('"', '""')}"`;
const scalar = (value: unknown): SQLParameter => {
  if (value === undefined) return null;
  if (
    value === null ||
    ["string", "number", "bigint", "boolean"].includes(typeof value) ||
    value instanceof Uint8Array
  )
    return value as SQLParameter;
  throw new TypeError("Better Auth values must be transformed into SQLite scalar values");
};
function predicate(where: CleanedWhere[] = []): { sql: string; parameters: SQLParameter[] } {
  const parameters: SQLParameter[] = [];
  const parts = where.map((condition, index) => {
    let field = quote(condition.field);
    let value = condition.value;
    if (
      condition.mode === "insensitive" &&
      (typeof value === "string" ||
        (Array.isArray(value) && value.every((item) => typeof item === "string")))
    ) {
      field = `lower(${field})`;
      if (typeof value === "string") value = value.toLowerCase();
      if (Array.isArray(value))
        value = value.map((item) =>
          typeof item === "string" ? item.toLowerCase() : item,
        ) as typeof value;
    }
    const connector = condition.connector ?? "AND";
    if (connector !== "AND" && connector !== "OR")
      throw new TypeError("Unsupported filter connector");
    const prefix = index ? ` ${connector} ` : "";
    if (value === null && ["eq", "ne"].includes(condition.operator ?? "eq"))
      return prefix + `${field} IS ${condition.operator === "ne" ? "NOT " : ""}NULL`;
    if (["in", "not_in"].includes(condition.operator ?? "eq")) {
      if (!Array.isArray(value)) throw new TypeError("Membership comparisons require an array");
      if (!value.length) return prefix + (condition.operator === "in" ? "0" : "1");
      parameters.push(...value.map(scalar));
      return (
        prefix +
        `${field} ${condition.operator === "not_in" ? "NOT " : ""}IN (${value.map(() => "?").join(",")})`
      );
    }
    if (["contains", "starts_with", "ends_with"].includes(condition.operator ?? "eq")) {
      if (typeof value !== "string") throw new TypeError("String comparisons require strings");
      parameters.push(value);
      const comparison =
        condition.operator === "contains"
          ? `instr(${field}, ?) > 0`
          : condition.operator === "starts_with"
            ? `substr(${field}, 1, length(?)) = ?`
            : `substr(${field}, length(${field})-length(?)+1) = ?`;
      if (condition.operator !== "contains") parameters.push(value);
      return prefix + comparison;
    }
    const operators = { eq: "=", ne: "!=", lt: "<", lte: "<=", gt: ">", gte: ">=" };
    const operator = operators[(condition.operator ?? "eq") as keyof typeof operators];
    if (!operator) throw new TypeError("Unsupported comparison");
    parameters.push(scalar(value));
    return prefix + `${field} ${operator} ?`;
  });
  return { sql: parts.length ? ` WHERE (${parts.join("")})` : "", parameters };
}
const entries = (data: unknown) =>
  Object.entries(data as Record<string, unknown>).filter(([, value]) => value !== undefined);

/** Better Auth's native database adapter, borrowing a BoxDB connection. */
export function betterAuthAdapter(
  client: BoxDB | TransactionClient,
  config: BetterAuthAdapterOptions = {},
): (options: BetterAuthOptions) => DBAdapter {
  return (options) =>
    createAdapterFactory({
      config: {
        adapterId: "boxdb",
        adapterName: "BoxDB",
        usePlural: config.usePlural ?? false,
        debugLogs: config.debugLogs ?? false,
        supportsJSON: false,
        supportsDates: false,
        supportsBooleans: false,
        supportsArrays: false,
        supportsNumericIds: true,
        customTransformOutput: ({ data, fieldAttributes }) => {
          if (typeof data === "bigint" && fieldAttributes.type === "boolean") return data !== 0n;
          if (typeof data === "bigint" && fieldAttributes.type === "number") {
            const number = Number(data);
            if (!Number.isSafeInteger(number))
              throw new RangeError(
                "Better Auth numeric fields must fit JavaScript's safe integer range",
              );
            return number;
          }
          return data;
        },
        transaction: async (callback) => {
          await using tx = await client.begin();
          const result = await callback(betterAuthAdapter(tx, config)(options));
          await tx.commit();
          return result;
        },
      },
      adapter: ({ getModelName, getFieldName, getDefaultModelName }) => {
        const select = async <T>(
          model: string,
          where: CleanedWhere[] = [],
          limit = 1,
          offset = 0,
          sort?: { field: string; direction: "asc" | "desc" },
        ): Promise<T[]> => {
          const compiled = predicate(where);
          const sql = `SELECT * FROM ${quote(model)}${compiled.sql}${sort ? ` ORDER BY ${quote(sort.field)} ${sort.direction === "desc" ? "DESC" : "ASC"}` : ""} LIMIT ? OFFSET ?`;
          return (await client.execute<T>(sql, [...compiled.parameters, limit, offset])).rows;
        };
        const id = (model: string, modelKey?: string) =>
          quote(getFieldName({ model: modelKey ?? getDefaultModelName(model), field: "id" }));
        const update = async <T>(
          model: string,
          where: CleanedWhere[],
          data: unknown,
          single: boolean,
          modelKey?: string,
        ): Promise<T[]> => {
          const fields = entries(data);
          if (!fields.length) return select<T>(model, where, single ? 1 : -1);
          const compiled = predicate(where);
          const filter = single
            ? ` WHERE ${id(model, modelKey)} IN (SELECT ${id(model, modelKey)} FROM ${quote(model)}${compiled.sql} LIMIT 1)`
            : compiled.sql;
          return (
            await client.execute<T>(
              `UPDATE ${quote(model)} SET ${fields.map(([key]) => `${quote(key)}=?`).join(",")}${filter} RETURNING *`,
              [...fields.map(([, value]) => scalar(value)), ...compiled.parameters],
            )
          ).rows;
        };
        return {
          create: async <T extends Record<string, unknown>>({
            model,
            data,
          }: {
            model: string;
            data: T;
          }): Promise<T> => {
            const fields = entries(data);
            const sql = fields.length
              ? `INSERT INTO ${quote(model)} (${fields.map(([key]) => quote(key)).join(",")}) VALUES (${fields.map(() => "?").join(",")}) RETURNING *`
              : `INSERT INTO ${quote(model)} DEFAULT VALUES RETURNING *`;
            return (
              await client.execute<T>(
                sql,
                fields.map(([, value]) => scalar(value)),
              )
            ).rows[0]!;
          },
          findOne: async <T>({
            model,
            where,
          }: {
            model: string;
            where: CleanedWhere[];
          }): Promise<T | null> => (await select<T>(model, where))[0] ?? null,
          findMany: async <T>({
            model,
            where,
            limit,
            offset,
            sortBy,
          }: {
            model: string;
            where?: CleanedWhere[] | undefined;
            limit: number;
            offset?: number | undefined;
            sortBy?: { field: string; direction: "asc" | "desc" } | undefined;
          }): Promise<T[]> => select<T>(model, where, limit, offset, sortBy),
          update: async <T>({
            model,
            modelKey,
            where,
            update: data,
          }: ModelTarget & { where: CleanedWhere[]; update: T }): Promise<T | null> =>
            (await update<T>(model, where, data, true, modelKey))[0] ?? null,
          updateMany: async ({ model, where, update: data }) =>
            (await update(model, where, data, false)).length,
          delete: async ({ model, modelKey, where }) => {
            const compiled = predicate(where);
            await client.execute(
              `DELETE FROM ${quote(model)} WHERE ${id(model, modelKey)} IN (SELECT ${id(model, modelKey)} FROM ${quote(model)}${compiled.sql} LIMIT 1)`,
              compiled.parameters,
            );
          },
          consumeOne: async <T>({
            model,
            modelKey,
            where,
          }: ModelTarget & { where: CleanedWhere[] }): Promise<T | null> => {
            const compiled = predicate(where);
            return (
              (
                await client.execute<T>(
                  `DELETE FROM ${quote(model)} WHERE ${id(model, modelKey)} IN (SELECT ${id(model, modelKey)} FROM ${quote(model)}${compiled.sql} LIMIT 1) RETURNING *`,
                  compiled.parameters,
                )
              ).rows[0] ?? null
            );
          },
          incrementOne: async <T>({
            model,
            modelKey,
            where,
            increment,
            set,
          }: ModelTarget & {
            where: CleanedWhere[];
            increment: Record<string, number>;
            set?: Record<string, unknown> | undefined;
          }): Promise<T | null> => {
            const compiled = predicate(where);
            const deltas = Object.entries(increment);
            const fields = entries(set ?? {});
            const assignments = [
              ...deltas.map(([key]) => `${quote(key)}=coalesce(${quote(key)},0)+?`),
              ...fields.map(([key]) => `${quote(key)}=?`),
            ];
            if (!assignments.length) return (await select<T>(model, where))[0] ?? null;
            return (
              (
                await client.execute<T>(
                  `UPDATE ${quote(model)} SET ${assignments.join(",")} WHERE ${id(model, modelKey)} IN (SELECT ${id(model, modelKey)} FROM ${quote(model)}${compiled.sql} LIMIT 1) RETURNING *`,
                  [
                    ...deltas.map(([, delta]) => scalar(delta)),
                    ...fields.map(([, value]) => scalar(value)),
                    ...compiled.parameters,
                  ],
                )
              ).rows[0] ?? null
            );
          },
          deleteMany: async ({ model, where }) => {
            const compiled = predicate(where);
            return Number(
              (
                await client.execute(
                  `DELETE FROM ${quote(model)}${compiled.sql}`,
                  compiled.parameters,
                )
              ).changes,
            );
          },
          count: async ({ model, where }) => {
            const compiled = predicate(where);
            const result = await client.execute<{ count: number | bigint }>(
              `SELECT count(*) AS count FROM ${quote(model)}${compiled.sql}`,
              compiled.parameters,
            );
            return Number(result.rows[0]!.count);
          },
          createSchema: async ({ tables, file }) => {
            const serial = options.advanced?.database?.generateId === "serial";
            const statements = Object.entries(tables).map(([model, table]) => {
              const columns = [
                `${id(getModelName(model), model)} ${serial ? "INTEGER PRIMARY KEY AUTOINCREMENT" : "TEXT PRIMARY KEY NOT NULL"}`,
              ];
              for (const [field, attributes] of Object.entries(table.fields)) {
                if (field === "id") continue;
                const type =
                  attributes.type === "number"
                    ? "REAL"
                    : attributes.type === "boolean"
                      ? "INTEGER"
                      : "TEXT";
                let column = `${quote(getFieldName({ model, field }))} ${attributes.references && serial ? "INTEGER" : type}${attributes.required ? " NOT NULL" : ""}${attributes.unique ? " UNIQUE" : ""}`;
                if (attributes.references)
                  column += ` REFERENCES ${quote(getModelName(attributes.references.model))} (${quote(getFieldName({ model: attributes.references.model, field: attributes.references.field }))}) ON DELETE ${attributes.references.onDelete === "restrict" ? "RESTRICT" : attributes.references.onDelete === "set null" ? "SET NULL" : "CASCADE"}`;
                columns.push(column);
              }
              return `CREATE TABLE ${quote(getModelName(model))} (${columns.join(",")});`;
            });
            return {
              code: statements.join("\n"),
              path: file ?? "boxdb-auth-schema.sql",
              overwrite: false,
            };
          },
        };
      },
    })(options);
}
