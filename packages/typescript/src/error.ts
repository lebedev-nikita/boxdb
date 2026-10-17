import { record } from "./values.js";

export interface BoxDBErrorOptions extends ErrorOptions {
  code?: string;
  resultCode?: number;
  extendedCode?: number;
  operation?: ErrorOperation;
  outcome?: OperationOutcome;
  inTransaction?: boolean;
  connectionUsable?: boolean;
  cleanupErrors?: readonly BoxDBError[];
}

export type ErrorOperation =
  | "open"
  | "prepare"
  | "read"
  | "write"
  | "commit"
  | "rollback"
  | "publish"
  | "check"
  | "repair"
  | "recover";
export type OperationOutcome = "not_applied" | "pending" | "committed" | "unknown";

export class BoxDBError extends Error {
  readonly code: string;
  readonly resultCode: number | undefined;
  readonly extendedCode: number | undefined;
  readonly operation: ErrorOperation | undefined;
  readonly outcome: OperationOutcome | undefined;
  readonly inTransaction: boolean | undefined;
  readonly connectionUsable: boolean | undefined;
  readonly cleanupErrors: readonly BoxDBError[];
  constructor(message: string, options?: BoxDBErrorOptions) {
    super(message, options);
    this.name = "BoxDBError";
    this.code = options?.code ?? "BOXDB_ERROR";
    this.resultCode = options?.resultCode;
    this.extendedCode = options?.extendedCode;
    this.operation = options?.operation;
    this.outcome = options?.outcome;
    this.inTransaction = options?.inTransaction;
    this.connectionUsable = options?.connectionUsable;
    this.cleanupErrors = options?.cleanupErrors ?? [];
  }
}

interface NativeErrorDetails {
  message: string;
  code: string;
  result_code?: number | null;
  extended_code?: number | null;
  operation?: ErrorOperation | null;
  outcome?: OperationOutcome | null;
  in_transaction?: boolean | null;
  connection_usable?: boolean | null;
  cleanup?: NativeErrorDetails[];
}

function fromDetails(details: NativeErrorDetails, cause?: unknown): BoxDBError {
  return new BoxDBError(details.message, {
    cause,
    code: details.code,
    ...(details.result_code != null ? { resultCode: details.result_code } : {}),
    ...(details.extended_code != null ? { extendedCode: details.extended_code } : {}),
    ...(details.operation != null ? { operation: details.operation } : {}),
    ...(details.outcome != null ? { outcome: details.outcome } : {}),
    ...(details.in_transaction != null ? { inTransaction: details.in_transaction } : {}),
    ...(details.connection_usable != null ? { connectionUsable: details.connection_usable } : {}),
    cleanupErrors: details.cleanup?.map((error) => fromDetails(error)) ?? [],
  });
}

export function fromNativeError(cause: unknown): BoxDBError {
  if (cause instanceof BoxDBError) return cause;
  if (record(cause) && typeof cause.details === "string")
    return fromDetails(JSON.parse(cause.details) as NativeErrorDetails, cause);
  return new BoxDBError(cause instanceof Error ? cause.message : String(cause), {
    cause,
    code: "BOXDB_INTERNAL",
  });
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
