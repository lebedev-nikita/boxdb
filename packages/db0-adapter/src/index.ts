import type { Connector, Primitive, Statement } from "db0";
import type { Database, TransactionClient } from "@boxdb/client";

/** Borrow a BoxDB connection or transaction; disposal does not close its owner. */
export function db0Connector(
  client: Database | TransactionClient,
): Connector<Database | TransactionClient> {
  const statement = (sql: string, bound: Primitive[] = []): Statement => {
    const parameters = (supplied: Primitive[]) =>
      (supplied.length ? supplied : bound).map((value) => value ?? null);
    return {
      bind: (...values) => statement(sql, values),
      all: async (...values) => (await client.execute(sql, parameters(values))).rows,
      get: async (...values) => (await client.execute(sql, parameters(values))).rows[0],
      run: async (...values) => {
        const { changes, lastInsertRowid } = await client.execute(sql, parameters(values));
        return { success: true, changes, lastInsertRowid };
      },
    };
  };
  return {
    name: "boxdb",
    dialect: "sqlite",
    getInstance: () => client,
    exec: (sql) => client.exec(sql),
    prepare: (sql) => statement(sql),
    dispose: async () => {},
  };
}
