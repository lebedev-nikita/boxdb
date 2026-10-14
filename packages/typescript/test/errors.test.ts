import { expect, it } from "vitest";
import { BoxDBError, findBoxDBError } from "../src/index.js";

it("finds BoxDB errors through causes and aggregates without looping on cycles", () => {
  const failure = new BoxDBError("duplicate", {
    code: "BOXDB_CONSTRAINT_UNIQUE",
    resultCode: 19,
    extendedCode: 2067,
  });
  const cycle: { cause?: unknown } = {};
  cycle.cause = cycle;
  expect(findBoxDBError(failure)).toBe(failure);
  expect(findBoxDBError(new Error("query", { cause: failure }))).toBe(failure);
  expect(
    findBoxDBError(new AggregateError([cycle, new Error("wrapper", { cause: failure })])),
  ).toBe(failure);
  expect(findBoxDBError(cycle)).toBeUndefined();
  expect(findBoxDBError(null)).toBeUndefined();
});
