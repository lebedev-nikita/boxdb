# BoxDB for Go

A `database/sql` driver backed by the BoxDB Rust core through cgo. It uses the
same type validation, SQLite extensions and binary/readable storage as the Rust
and TypeScript clients. Its API follows the standard SQL interfaces used by
[go-sqlite3](https://github.com/mattn/go-sqlite3) and
[go-sql-driver/mysql](https://github.com/go-sql-driver/mysql).

## Install a release

Requires Go 1.23+, a C compiler, and `CGO_ENABLED=1`. Prebuilt native libraries
are available for macOS ARM64 (macOS 11+) and Linux x64 (glibc 2.35+). Download
the native library matching the Go module version, then add the driver to your
application:

```sh
BOXDB_GO_VERSION=0.3.2
BOXDB_GO_PLATFORM=darwin-arm64 # Use linux-x64 for Linux on amd64.
BOXDB_GO_PREFIX="$HOME/.local/share/boxdb/$BOXDB_GO_VERSION/$BOXDB_GO_PLATFORM"
mkdir -p "$BOXDB_GO_PREFIX"
curl --fail --location \
  "https://github.com/lebedev-nikita/boxdb/releases/download/v$BOXDB_GO_VERSION/boxdb-go-$BOXDB_GO_VERSION-$BOXDB_GO_PLATFORM.tar.gz" \
  --output "$BOXDB_GO_PREFIX/native.tar.gz"
tar -xzf "$BOXDB_GO_PREFIX/native.tar.gz" -C "$BOXDB_GO_PREFIX"
export CGO_ENABLED=1
export CGO_LDFLAGS="-L$BOXDB_GO_PREFIX/lib ${CGO_LDFLAGS:-}"
go get github.com/lebedev-nikita/boxdb/packages/go@v0.3.2
```

Keep `CGO_LDFLAGS` set when building, testing or running your application. The
native library is linked into the application; deployment does not need Rust
or a separate BoxDB shared library. Module releases use `packages/go/vX.Y.Z`
Git tags, pointing to the same commit as the corresponding `vX.Y.Z` release.

## Build from this checkout

Requires Go 1.23+, Rust, a C compiler, libclang and Just on macOS or Linux.
See the [repository prerequisites](../../README.md). Keep `CGO_ENABLED=1`;
the native library must be built for the same OS and architecture as Go.

From the repository root:

```sh
just go-build
just go-check go-test
```

In your application's Go module, point the dependency at this built checkout:

```sh
go mod edit -replace github.com/lebedev-nikita/boxdb/packages/go=/absolute/path/to/boxdb/packages/go
go get github.com/lebedev-nikita/boxdb/packages/go
```

Rebuild the native library after changing Rust code or moving to another platform.
The Go module has no third-party Go dependencies.
For an optimized native library, run `go run packages/go/build.go -release`
from the repository root. The build also updates a generated Go fingerprint so
Go's cgo cache cannot silently reuse a previous native library.

## Queries

```go
package main

import (
    "context"
    "fmt"
    "log"

    boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

func main() {
    db, err := boxdb.Open("data.db")
    if err != nil { log.Fatal(err) }
    defer db.Close()

    ctx := context.Background()
    if _, err := db.ExecContext(ctx,
        "CREATE TABLE IF NOT EXISTS users(id INT64 PRIMARY KEY, name TEXT NOT NULL)"); err != nil {
        log.Fatal(err)
    }
    result, err := db.ExecContext(ctx, "INSERT INTO users(name) VALUES(?)", "Ada")
    if err != nil { log.Fatal(err) }
    id, err := result.LastInsertId()
    if err != nil { log.Fatal(err) }
    var name string
    if err := db.QueryRowContext(ctx, "SELECT name FROM users WHERE id=?", id).Scan(&name); err != nil {
        log.Fatal(err)
    }
    fmt.Println(name)
}
```

`Open` eagerly opens the database and returns `*sql.DB`, safe to share across
goroutines. It defaults to one persistent connection so `:memory:` stays isolated
and readable files retain their single writer. Do not increase the pool size for
these modes. Binary files can use additional connections via `SetMaxOpenConns`.
Do not expire the only connection of an in-memory database.

The package also registers the `boxdb` driver for `sql.Open("boxdb", path)`.
That standard constructor opens lazily and uses Go's default pool settings;
set `SetMaxOpenConns(1)` and `SetMaxIdleConns(1)` yourself for memory/readable storage.

For new readable files, use `boxdb.Open("data.yaml", boxdb.WithFormat(boxdb.Readable))`.
Existing contents determine the format when reopening. `Binary` is the default.

## Standard SQL interfaces

Use `QueryContext` with `Rows.Next`, `Rows.Scan`, `Rows.Err` and `Rows.Close`;
`QueryRowContext().Scan()` returns `sql.ErrNoRows` when no row matches.
Column order and duplicate column names are preserved. Write results implement
`LastInsertId` and `RowsAffected`.

`PrepareContext` returns a standard `*sql.Stmt`. Parameters support positional
`?`/`?NNN` and named `:name`, `@name`, `$name` placeholders:

```go
err := db.QueryRowContext(ctx, "SELECT name FROM users WHERE id=:id",
    sql.Named("id", id)).Scan(&name)
```

Values are bound separately from SQL. Missing, extra and unknown named parameters
return errors. Queries and prepared statements require one SQL statement.
`ExecContext` without parameters also accepts SQL scripts; `RowsAffected` refers
to the last statement. Scripts stop on the first error and are not automatically
atomic. Use `BeginTx`, execute through `*sql.Tx`, then `Commit` or `Rollback` for
atomic work. `Begin` and writable `BeginTx` transactions use `BEGIN IMMEDIATE`,
acquiring the writer before transaction work starts. `sql.TxOptions{ReadOnly:
true}` uses a deferred transaction and rejects writes, including scripts that
try to disable `query_only`. Commit, rollback and pool reset clear the read-only
restriction before connection reuse. Default and serializable isolation are
supported; other isolation levels return an error. Raw SQL transaction commands
retain SQLite's own mode semantics.

Contexts interrupt active SQLite queries and lock waits. The default lock wait is
up to five seconds; setting `PRAGMA busy_timeout` replaces the cancellable lock
handler. Opening native databases is synchronous. Results are currently fully
materialized by the Rust core before `Rows` iteration.

Pool session cleanup finishes before the next operation and is independent of
that operation's cancellation. A cancelled request does not discard a healthy
connection or lose its committed in-memory data.

## SQL migrations

```go
migrations, err := boxdb.ReadMigrations(os.DirFS("."), "db/migrations")
if err != nil { return err }
_, err = migrations.Up(ctx, db)
if err != nil { return err }
```

The reader accepts `fs.FS`, including `embed.FS`. Use
`boxdb.NewMigrationSet([]boxdb.MigrationFile{{Filename: filename, SQL: sql}})` for
documents held in memory. `Up`, `Down` and `Status` take a context and a BoxDB
`*sql.DB`, retaining one connection for the whole call. These methods share the
CLI's format and history. See [migration sources, cancellation and rollback](../../docs/migrations.md#migrations-from-application-code).

## Values and errors

Use ordinary Go integers, floats, strings, booleans, `[]byte`, `nil`, `time.Time`,
`sql.Null*` and `driver.Valuer` for parameters. Signed integers preserve the INT64
range. Unsigned parameters (`uint`, `uint8`, `uint16`, `uint32`, `uint64`) bind as
UINT64 without loss across its full range. Non-finite floats return errors.
`time.Time` binds as a UTC INSTANT; the core rounds finer input to microseconds.
See [temporal precision](../../docs/temporal.md#precision-across-language-clients).

Results use SQLite storage values: `int64`, `float64`, `string`, `[]byte` and `nil`.
Unsigned results fitting INT64 are driver `int64` values; larger results are
decimal strings because `database/sql/driver.Value` has no uint64 representation.
Both forms can be scanned into `uint64`.
DECIMAL columns and calculated decimals return exact strings. Bind decimal
strings for exact amounts; NUMERIC is not supported. JSON and arrays remain JSON
strings, UUID remains text, temporal values return canonical strings (INSTANT in UTC, DATETIME without a
time zone), and BOOLEAN returns integer 0/1 (which `Scan` accepts into
`bool`). Serialize arrays explicitly. BoxDB validates logical values on both
reads and writes.

`Decimal`, `UUID`, `Instant`, `Date`, `DateTime` and `Interval` implement
`sql.Scanner` and `driver.Valuer` with string storage. Use matching SQL columns
or explicit casts: the engine validates and normalizes values, while the Go
codecs preserve exact text without converting decimals to float64 or calendar
values to a time zone. Use `sql.Null[T]` for nullable values.

```go
amount := boxdb.Decimal("9007199254740993.01")
_, err = db.ExecContext(ctx, "INSERT INTO payments(amount) VALUES(?)", amount)
var total boxdb.Decimal
err = db.QueryRowContext(ctx, "SELECT SUM(amount) FROM payments").Scan(&total)
```

`JSON[T]{Data: document}` implements the same SQL interfaces and JSON
marshal/unmarshal methods for complete documents. Read the decoded document from
`Data`; decoding into interface values preserves numbers as `json.Number`.
A nil `Data` represents JSON `null`; `sql.Null[boxdb.JSON[T]]` represents SQL NULL.
Serialization and parsing run in Go; SQL JSON inspection and editing remain
unsupported. Custom application types can also implement `sql.Scanner` and
`driver.Valuer`.

BoxDB failures are `*boxdb.Error`, available through `errors.As`. They expose
`Message`, a symbolic `Code` such as `BOXDB_CONSTRAINT_UNIQUE`, and optional
`*int` fields `ResultCode` and `ExtendedCode`. Errors outside the engine have
nil numeric codes. Codes do not depend on parsing the message.

Context cancellation matches `context.Canceled` or `context.DeadlineExceeded`
through `errors.Is`; an interrupted native call also retains its BoxDB error
through `errors.As`. A completed unrelated failure is not replaced by a racing
cancellation. Transaction completion and cleanup can return joined errors.
See the [error contract](../../docs/client-errors.md).

File maintenance uses `boxdb.CheckFile(path, boxdb.CheckFull)`,
`boxdb.RepairFile(source, destination)` and
`boxdb.RecoverFile(source, destination, boxdb.Binary)`. Each returns a report and
an error without opening a database/sql pool. Inspect the report even when the
error is nil: recovery can lose data, and checks can report invalid or incomplete
results. See [diagnostics and recovery](../../docs/recovery.md).
