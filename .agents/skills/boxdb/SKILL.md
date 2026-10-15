---
name: boxdb
description: Use BoxDB from Rust, TypeScript, Go or the CLI, including Drizzle, Better Auth and db0 integrations. Read the matching client guide before writing application queries, transactions, schemas or migrations. This skill covers application usage rather than BoxDB engine development.
---

# Use BoxDB

BoxDB is an experimental SQLite client with extended logical types, a patched
SQLite engine and optional readable YAML storage. Read the guide for the chosen
client before writing integration code; APIs and storage contracts may change
during version 0.

## Choose the client guide

The links point to the BoxDB repository. When a BoxDB checkout is available,
read the same paths locally so the documentation matches the code being used.
Read only the guides relevant to the task.

- TypeScript on Node.js or Bun: [packages/typescript/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/typescript/README.md).
  Use this for installation, bound queries, transaction handles, cancellation,
  connection ownership and bigint values. Queries execute asynchronously.
- Go: [packages/go/README.md](https://github.com/lebedev-nikita/boxdb/blob/main/packages/go/README.md).
  Use this for the cgo build, `database/sql`, connection pooling, contexts and
  Scanner/Valuer types. Go native libraries must be built from a checkout.
- Rust: [README.md](https://github.com/lebedev-nikita/boxdb/blob/main/README.md#language-clients)
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
- Create and alter typed schemas through BoxDB. Ordinary table creation is
  STRICT by default. Use `DECIMAL` for exact amounts and bind decimal strings;
  see [native decimals](https://github.com/lebedev-nikita/boxdb/blob/main/docs/native-decimal.md).
- Keep JSON as complete validated documents. Serialize and parse in the client;
  SQL JSON/JSONB inspection and editing are unavailable.
- Follow the selected client's value mappings. TypeScript integer results use
  bigint; Go UINT64 results above INT64_MAX use decimal strings that can be
  scanned into uint64. Preserve canonical temporal strings when microseconds
  matter; see [temporal types](https://github.com/lebedev-nikita/boxdb/blob/main/docs/temporal.md).
- Execute transaction work through its transaction handle. TypeScript root
  requests wait for the active transaction; finish it before awaiting root work.
  Adapters borrow the client, so its owner closes it.
- Choose binary storage for concurrent connections or virtual tables. Readable
  YAML executes in memory, has one writer per file and publishes complete
  snapshots on commit; see [storage](https://github.com/lebedev-nikita/boxdb/blob/main/docs/storage.md).
- Keep the standard SQLite file format distinct from SQL compatibility: native
  values and indexes require BoxDB's patched engine. Use BoxDB's conversion or
  SQLite backup facilities for live binary databases, accounting for the WAL.

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

## Further references

For failure handling, read
[client errors](https://github.com/lebedev-nikita/boxdb/blob/main/docs/client-errors.md)
and branch on error codes rather than message text. For full-text, vector,
Unicode or regexp queries, read
[extensions](https://github.com/lebedev-nikita/boxdb/blob/main/docs/extensions.md).
