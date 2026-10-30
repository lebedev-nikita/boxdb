# Client errors

BoxDB owns the public error names in every language. Engine failures use
`BOXDB_*` names such as `BOXDB_CONSTRAINT_UNIQUE`; numeric result codes currently
retain the embedded engine's primary and extended values. Messages are for
people and must not be parsed to identify failures.

| Client | Error | Numeric engine details | Cause lookup |
| --- | --- | --- | --- |
| Rust | `boxdb::Error`, `code()` | `result_code()` / `extended_code()`, returning `Option<i32>` | `std::error::Error::source()` |
| TypeScript | `BoxDBError`, `code` | `resultCode` / `extendedCode`, optional numbers | `cause`; `findBoxDBError(error)` follows causes and aggregate members |
| Go | `*boxdb.Error`, `Code` | `ResultCode` / `ExtendedCode`, optional `*int` | `errors.As`; `errors.Is` for standard errors |

The [Rust error type](../src/error.rs) owns engine-code naming and the serialized
error object used by the [Go bridge](../packages/go/native/src/lib.rs).
[TypeScript](../packages/typescript/src/error.ts) preserves the native code and
exception. A UNIQUE violation has code `BOXDB_CONSTRAINT_UNIQUE`, primary code
`19`, and extended code `2067`. Numeric codes are absent when the engine did not
report the failure; zero is not a placeholder for missing information.
Updating a [read-only column](sql.md#read-only-columns) has code
`BOXDB_CONSTRAINT_READONLY`, primary code `19`, and extended code `1811`.

Validation, filesystem I/O, closed connections, unusable connections after
publication failure, and internal infrastructure failures have separate
`BOXDB_*` categories. Failures raised by BoxDB SQL functions such as
`boxdb_apply` or `boxdb_changeset` use `BOXDB_CONSTRAINT_FUNCTION` unless an
engine code applies. Readable-file lock contention retains its OS cause without
inventing numeric engine codes. Rust retains I/O, parser, JSON and YAML sources;
`with_context()` retains codes and sources. `Database::try_memory()` provides
fallible initialization and is used by both native clients.

## Cancellation and cleanup

[Go](../packages/go/native.go) matches cancellation and deadlines through
`errors.Is(err, context.Canceled)` or `errors.Is(err, context.DeadlineExceeded)`.
When a native operation is interrupted or stops waiting for a lock after the
context ends, the context error and the native BoxDB error are joined. A native
failure unrelated to cancellation is retained rather than replaced merely
because the context has ended. Cancellation before execution has no native
failure to attach.

Go transaction completion retains both the operation and cleanup errors.
A failed session reset invalidates the connection and retains its original
reason. The driver's pool-reset boundary can signal `driver.ErrBadConn` before
an operation; a transaction result does not request automatic retry after an
operation may have executed. See the Go contracts for
[`errors.Join`](https://pkg.go.dev/errors#Join) and
[`ErrBadConn`](https://pkg.go.dev/database/sql/driver#ErrBadConn).

TypeScript transactions use explicit handles and `await using`: unfinished
transactions roll back during disposal. A failed commit keeps a still-active
native transaction available for repair or retry. Disposal retains operation and
rollback failures through JavaScript's explicit resource management protocol.
Transaction deadline expiry uses `BOXDB_TRANSACTION_TIMEOUT`, interrupts running
SQL and rolls back before releasing root requests waiting on the connection.
Expired handles retain the timeout reason for further queries and commit calls.
TypeScript read-only transactions use `BOXDB_READONLY` for forbidden SQL writes. Their
restriction is inherited by savepoints and released before waiting root work
resumes; an explicitly enabled pre-existing `query_only` setting is preserved.
Argument errors use `TypeError` or `RangeError`; both unsafe integer numbers and
bigint outside the combined INT64/UINT64 range (`-2^63` through `2^64 - 1`)
use `RangeError`.

Signals reject cancelled TypeScript requests with the caller's reason. Rollback
and connection-close cleanup are not cancelled by the same signal. SQL runs on
a dedicated native worker, including busy waits. See the client guide for the
transaction ownership and close contracts.

## TypeScript integers and integrations

Integer storage values always return as `bigint`; REAL values return as `number`.
`lastInsertRowid` and write counts also use bigint. There is no precision mode
switch. Safe integer numbers and booleans bind as integers. Applications must
explicitly convert bigint when serializing JSON. Tagged-query result methods
reuse the same native execution and rejected error.

[Drizzle](../packages/drizzle-adapter/README.md) may return `BoxDBError` directly
or put it in an ORM error's `cause`, depending on the API used.
`findBoxDBError()` handles both, as well as aggregates, and avoids cycles.
Use bigint schema types for integer values and the adapter's `migrate()` helper
for an INTEGER migration journal and bigint result metadata.

[Better Auth](../packages/better-auth-adapter/README.md) maps booleans and numeric
fields to its expected application types. [db0](../packages/db0-adapter/README.md)
preserves bigint values and write metadata. All adapters borrow the client.

See the [TypeScript](../packages/typescript/README.md) and
[Go](../packages/go/README.md) guides for usage. Contract coverage lives in the
[Rust tests](../tests/errors.rs), [Go tests](../packages/go/boxdb_test.go), and
[TypeScript tests](../packages/typescript/test/errors.test.ts).
The CLI reports errors on stderr with exit status 1. `--json` emits structured
errors; maintenance reports are written separately to stdout.

## Operation state and unusable connections

Core connection errors retain the operation, outcome, transaction state and
connection usability when known. Rust exposes `operation()`, `outcome()`,
`in_transaction()`, `connection_usable()` and `cleanup_errors()`. TypeScript uses
`operation`, `outcome`, `inTransaction`, `connectionUsable` and `cleanupErrors`;
Go uses `Operation`, `Outcome`, `InTransaction`, `ConnectionUsable` and `Cleanup`.
Missing state remains absent rather than being guessed.

Read/preparation failures report `not_applied`. Failed writes conservatively
report `unknown`, so callers must not blindly retry them. A failed SQL script can
have committed earlier statements; its outcome is `unknown` even if its final
statement was a read. Native cleanup failures remain attached to the primary
error. No write replay is requested by the driver after an operation fails.

`BOXDB_CORRUPT` and `BOXDB_NOTADB` invalidate the normal connection. Further
queries and preparation return `BOXDB_UNAVAILABLE`; Go also marks that connection
invalid for the pool. Constraint and argument errors do not invalidate it.
`BOXDB_METADATA`, `BOXDB_UNSUPPORTED_CODEC` and `BOXDB_FORMAT` distinguish schema
metadata, codec-version and file-format problems from engine corruption.
Invalid projected logical values use `BOXDB_INVALID_VALUE`, keeping stored-data
failures distinct from invalid query arguments.

Readable publication failures invalidate the connection. A failure before file
replacement reports `not_applied`; a failure after replacement, such as directory
synchronization failure, reports `unknown`. Reopen and inspect durable data before
deciding whether to repeat an operation. See [file diagnostics](recovery.md).
