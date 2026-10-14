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
[TypeScript](../packages/typescript/src/index.ts) preserves the native code and
exception. A UNIQUE violation has code `BOXDB_CONSTRAINT_UNIQUE`, primary code
`19`, and extended code `2067`. Numeric codes are absent when the engine did not
report the failure; zero is not a placeholder for missing information.

Validation, filesystem I/O, closed connections, unusable connections after
publication failure, and internal infrastructure failures have separate
`BOXDB_*` categories. Readable-file lock contention retains its OS cause without
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

TypeScript transactions preserve callback failures and combine them with
rollback failures in `AggregateError`. Argument errors use `TypeError` or
`RangeError`; both unsafe integer numbers and bigint outside signed int64 use
`RangeError`.

## TypeScript integers and integrations

Integer storage values always return as `bigint`; REAL values return as `number`.
`lastInsertRowid`, write counts, and Postgres-style result counts also use bigint.
There is no precision mode switch. Safe integer numbers and booleans bind as
integers. Applications must explicitly convert bigint when serializing JSON.

Kysely and the [Postgres-style adapter](../packages/typescript/src/adapters/postgres.ts)
preserve BoxDB query errors directly. Reusing a tagged query, including its
`.values()` view, preserves the same rejected error object.

[Drizzle](../packages/typescript/src/drizzle.ts) may return `BoxDBError` directly
or put it in an ORM error's `cause`, depending on the API used.
`findBoxDBError()` handles both, as well as `AggregateError`, and avoids cycles.
Use bigint schema types for integer values and `orm.$migrate(config)` for
Drizzle migrations with BoxDB's bigint result metadata.

See the [TypeScript](../packages/typescript/README.md) and
[Go](../packages/go/README.md) guides for usage. Contract coverage lives in the
[Rust tests](../tests/errors.rs), [Go tests](../packages/go/boxdb_test.go), and
[TypeScript tests](../packages/typescript/test/errors.test.ts).
The CLI currently reports human-readable errors on stderr with exit status 1.
