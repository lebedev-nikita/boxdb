import { record } from "./values.js";

export interface BoxDBErrorOptions extends ErrorOptions {
  code?: string;
  resultCode?: number;
  extendedCode?: number;
}

export class BoxDBError extends Error {
  readonly code: string;
  readonly resultCode: number | undefined;
  readonly extendedCode: number | undefined;
  constructor(message: string, options?: BoxDBErrorOptions) {
    super(message, options);
    this.name = "BoxDBError";
    this.code = options?.code ?? "BOXDB_ERROR";
    this.resultCode = options?.resultCode;
    this.extendedCode = options?.extendedCode;
  }
}

/** Find the first BoxDB failure in an error's causes or aggregate members. */
export function findBoxDBError(error: unknown): BoxDBError | undefined {
  const pending = [error];
  const seen = new Set<unknown>();
  while (pending.length > 0) {
    const current = pending.pop();
    if (current instanceof BoxDBError) return current;
    if (!record(current) || seen.has(current)) continue;
    seen.add(current);
    if (current instanceof AggregateError) pending.push(...[...current.errors].reverse());
    if (current.cause !== undefined) pending.push(current.cause);
  }
  return undefined;
}
