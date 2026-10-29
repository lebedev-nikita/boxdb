---
name: boxdb
description: Use BoxDB from Rust, TypeScript, Go or the CLI, including Drizzle, Better Auth, db0, file diagnostics and recovery. Read the matching client guide before writing application queries, transactions, schemas or migrations. Covers application usage rather than BoxDB engine development.
---

# Use BoxDB

BoxDB is an experimental SQLite client with extended logical types, a patched
SQLite engine and optional readable YAML storage. Read the guide for the chosen
client before writing integration code; APIs and storage contracts may change
during version 0.

Prebuilt CLI, npm client and Go libraries support 64-bit Raspberry Pi OS
Bookworm+ (ARM64, glibc 2.35+). Choose `linux-arm64` for npm and Go binaries,
or `aarch64-unknown-linux-gnu` for the standalone CLI archive.
Follow the client guide for installation.

The [documentation index](https://github.com/lebedev-nikita/boxdb/blob/main/docs/index.md)
routes first-time users to the quickstart, SQL overview and deployment guide.
The same guides are published at https://lebedev-nikita.github.io/boxdb/.

## Choose the client guide

The links point to the BoxDB repository. When a BoxDB checkout is available,
read the same paths locally so the documentation matches the code being used.
Read only the guides relevant to the task.

- TypeScript on Node.js or Bun: [packages/typescript/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/typescript/README.md).
  Use this for installation, bound queries, transaction handles, cancellation,
  connection ownership and bigint values. Queries execute asynchronously.
- Go: [packages/go/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/go/README.md).
  Use this for the cgo build, `database/sql`, connection pooling, contexts and
  Scanner/Valuer types. Use a prebuilt native library matching the Go module
  version, or build it from a checkout.
- Rust: [docs/rust.md](https://github.com/lebedev-nikita/boxdb/blob/main/docs/rust.md)
  and the example and public API in [src/lib.rs](https://github.com/lebedev-nikita/boxdb/blob/main/src/lib.rs).
  In a checkout, use `cargo doc --no-deps --open` for the API reference.
- CLI: [packages/cli/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/cli/README.md).
  Use `boxdb --help` for options and `.help` inside the interactive shell.
- Drizzle: [packages/drizzle-adapter/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/drizzle-adapter/README.md).
  Use the BoxDB adapter, its extended column builders and its migration helper.
- Better Auth: [packages/better-auth-adapter/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/better-auth-adapter/README.md).
  Use the direct adapter; schema generation creates an initial schema rather
  than upgrading an existing one.
- db0: [packages/db0-adapter/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/db0-adapter/README.md).
  Pass a BoxDB database or transaction handle; the connector borrows it.

## Apply BoxDB contracts

- Bind application values separately from SQL. In TypeScript, use tagged
  queries and `identifier()` for dynamic identifiers. Use the client's documented
  parameter forms when executing raw SQL.
- Validate TypeScript query results with a runtime schema, for example Zod,
  before accessing row fields. Results are `unknown[]`; `get()` returns
  `unknown`. Derive application types from validated data. Query APIs accept
  no result-type generic; see the TypeScript guide for validation examples.
- Follow the selected client's value mappings. TypeScript integer results use
  bigint; Go UINT64 results above INT64_MAX use decimal strings that can be
  scanned into uint64.
- Execute transaction work through its transaction handle. TypeScript root
  requests wait for the active transaction; finish it before awaiting root work.
  Adapters borrow the client, so its owner closes it.
- Choose binary storage for concurrent connections or virtual tables. Readable
  YAML executes in memory, has one writer per file and publishes complete
  snapshots on commit; see [storage](https://github.com/lebedev-nikita/boxdb/blob/main/docs/storage.md).
- Keep the standard SQLite file format distinct from SQL compatibility: native
  values and indexes require BoxDB's patched engine. Use BoxDB's conversion or
  SQLite backup facilities for live binary databases, accounting for the WAL.

## SQL differences from SQLite

Read [storage and type declarations](https://github.com/lebedev-nikita/boxdb/blob/main/docs/storage.md#extended-schema)
before writing schemas or migrations. Create and alter typed schemas through
BoxDB so logical metadata stays consistent.

- Declare every column's type explicitly, including generated columns and
  ALTER TABLE additions. Use explicit `ANY` to preserve storage classes.
  Ordinary tables are STRICT by default; lossless conversions such as `'42'`
  to INTEGER remain allowed. CREATE TABLE AS SELECT infers physical types,
  using ANY where necessary, but does not infer extended logical metadata.
- Use `INTEGER` or `INT8/16/32/64`, and `UINT8/16/32/64` for unsigned integers.
  `INT`, `SMALLINT` and `BIGINT` declarations are rejected. `INT64` and `SERIAL`
  map to SQLite INTEGER, including rowid aliases with PRIMARY KEY. Use
  `REAL`/`DOUBLE`, `VARCHAR(n)` and `DECIMAL(p,s)`; `FLOAT`, `DOUBLE PRECISION`,
  `CHARACTER VARYING` and `NUMERIC` are unsupported. Integer widths, VARCHAR
  length, BOOLEAN's 0/1 values and decimal precision/scale are enforced.
- Declare enums inline as `ENUM('todo','doing','done')`, optionally with NOT NULL
  and DEFAULT. Labels must be unique string literals; comparison and ordering
  use BINARY text order, not declaration order. Other collations, named enum
  types, ALTER TYPE and SQL casts to ENUM are unsupported. Rebuild the table
  to change labels. Typed arrays use declarations such as `TEXT[]`, `INT64[][]`
  and `ENUM('todo','done')[]`; serialize complete arrays as JSON in clients that
  bind strings. `DOUBLE[]` is distinct from sqlite-vec's packed vectors.
- Cast expressions explicitly, for example `CAST(? AS DECIMAL)` or
  `CAST(? AS INSTANT)`. Native decimal, UUID, temporal and unsigned casts are
  supported. SQL CAST does not cover every logical column type; see the
  storage guide before adding extended casts.
- Use typed columns and related tables by default. Do not create JSON columns
  unless the developer explicitly requests them. JSON is an opaque validated
  document: serialize and parse in the client.
  JSONB, SQL JSON/JSONB functions, document operators and JSON virtual tables
  are unavailable.
- For exact amounts, bind decimal strings and use `CAST('0.1' AS DECIMAL)` in
  expressions; casting the REAL literal `0.1` preserves its binary approximation.
  Read
  [native decimals](https://github.com/lebedev-nikita/boxdb/blob/main/docs/native-decimal.md)
  before arithmetic: REAL mixing requires explicit conversion, `/` and AVG
  reject nonterminating decimal results, and assignments reject excess scale
  without rounding. Use `decimal_div` and `decimal_round` for explicit rounding.
  Unsigned arithmetic preserves width and rejects negative or overflowing
  results; REAL arithmetic also requires explicit conversion.
- Read [temporal types](https://github.com/lebedev-nikita/boxdb/blob/main/docs/temporal.md)
  before temporal queries. INSTANT accepts an offset and normalizes to UTC;
  DATETIME has no zone. TIME is a time of day without a date or zone: bind
  `HH:MM:SS` with an optional fraction; results use `HH:MM:SS.ffffff`.
  Use Rust `Type::Time` / `Value::Time`, Go `boxdb.Time`, Drizzle `time()`,
  and ordinary strings in TypeScript. Preserve canonical strings for
  microsecond precision. TIME supports comparison, indexing and explicit
  casts from DATETIME/INSTANT; temporal arithmetic and functions such as
  `extract` and `date_trunc` do not accept TIME.
  For other temporal types, use native casts for arithmetic and calls such as
  `extract('year', value)`, `date_trunc('day', value)` and `make_interval(0,1)`.
  SQLite's `date()`, `datetime()` and `strftime()` retain their text contracts.
- Reserve `__boxdb_` names and avoid column names ending in `?` (reserved for
  nullable YAML schema fields). Non-native logical types require main-database
  tables; native scalar types also support TEMP and attached tables.

```sql
CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    status ENUM('todo','doing','done') NOT NULL DEFAULT 'todo',
    budget DECIMAL(12,2),
    due_at INSTANT,
    tags TEXT[]
);
```

## Migrations and schema export

Use `db/migrations` for SQL migrations and `db/schema.yaml` for the YAML schema
dump, relative to the project root, unless the project specifies other paths.
After applying schema changes, refresh the dump from the project root, replacing
`database.yaml` with the actual database path:

```sh
mkdir -p db
boxdb schema dump database.yaml --format yaml > db/schema.yaml
```

Read [migrations](https://github.com/lebedev-nikita/boxdb/blob/main/docs/migrations.md)
for migration commands and schema export details.

## Diagnose and recover files

For corruption, invalid stored values or a failed restore, read
[diagnostics and recovery](https://github.com/lebedev-nikita/boxdb/blob/main/docs/recovery.md)
and [client errors](https://github.com/lebedev-nikita/boxdb/blob/main/docs/client-errors.md).
Use the matching client's file-maintenance API or the CLI:

```sh
boxdb check data.db --full --json
boxdb repair data.db repaired.db --rebuild-indexes --json
boxdb recover damaged.db recovered.db --json
```

- Check an existing source; maintenance does not create a missing database.
  Inspect both report `status` and `complete`, even when the API returns no error.
  `incomplete` means checks could not finish, not proven corruption.
- Repair rebuilds indexes from readable tables into a new binary file. Recovery
  salvages available SQLite or YAML data and always reports possible loss.
  Inspect output validation and warnings before using it. Keep rejected YAML
  fragments in the reported `.rejected` file; they contain source data.
- Use new destinations and preserve the source with its SQLite sidecars. Stop
  writers before recovery. Native values require BoxDB's engine; do not assume
  an ordinary `sqlite3` export/import preserves them.
- Branch on error codes rather than message text. Physical corruption invalidates
  the connection; close it and diagnose the file. An `unknown` operation outcome
  can include applied writes, so do not blindly retry. Keep cleanup failures with
  the original error.
- Use Litestream for binary replication and restoration, then check the restored
  file with BoxDB. Readable YAML needs a separate file-backup strategy.

## Further references

For full-text, vector, Unicode or regexp queries, read
[extensions](https://github.com/lebedev-nikita/boxdb/blob/main/docs/extensions.md).
