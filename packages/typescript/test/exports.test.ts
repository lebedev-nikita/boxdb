import { expect, test } from "vitest";
import * as client from "../src/index.js";

test("exports BoxDB as its named and default client without a Database alias", async () => {
  expect(client).not.toHaveProperty("Database");
  expect(client).toHaveProperty("BoxDB", client.default);
  await using db = await client.default.open();
  expect(await db.query`SELECT 42 AS value`.get()).toEqual({ value: 42n });
});
