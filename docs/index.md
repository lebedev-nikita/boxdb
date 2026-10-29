# BoxDB documentation

BoxDB is an experimental embedded SQLite client for applications that live on
one server. It adds validated logical types, native decimal and temporal values,
and optional readable YAML storage. There is no database server to run.

Start with the [quickstart](quickstart.md), then follow your client guide:

| Client | Install | Guide |
| --- | --- | --- |
| TypeScript / Node.js / Bun | `npm install @boxdb/client` | [TypeScript](../packages/typescript/README.md) |
| Rust | `cargo add boxdb` | [Rust](rust.md) |
| Go | Go module and matching native library | [Go](../packages/go/README.md) |
| Command line | `brew install lebedev-nikita/tap/boxdb` or `npm install -g @boxdb/cli` | [CLI](../packages/cli/README.md) |

## Learn the model

- [Why BoxDB?](why-boxdb.md): use cases, tradeoffs and when to choose something else.
- [SQL and types](sql.md): practical declarations, bindings and SQLite differences.
- [Exact decimals](native-decimal.md) and [temporal values](temporal.md).
- [Storage](storage.md): binary SQLite, readable YAML and validation boundaries.
- [Built-in extensions](extensions.md): full-text, vector, Unicode and regexp queries.

## Build an application

- [Drizzle](../packages/drizzle-adapter/README.md),
  [Better Auth](../packages/better-auth-adapter/README.md) and
  [db0](../packages/db0-adapter/README.md) integrations.
- [Migrations and schema export](migrations.md).
- [Client errors](client-errors.md).
- [Deployment and backups](deployment.md).
- [Replication with Litestream](replication.md) and [file recovery](recovery.md).
- [FAQ](faq.md) and [benchmark methodology](benchmarks.md).

## Project status

BoxDB is version 0 software. APIs and BoxDB storage contracts may change without
compatibility layers. Native values and indexes require the bundled patched
engine, even though binary files retain SQLite's standard page and record
encoding. Read [the tradeoffs](why-boxdb.md#current-boundaries) before adoption.

[Contribute](../CONTRIBUTING.md), [report a bug](https://github.com/lebedev-nikita/boxdb/issues/new/choose)
or read [the releases](https://github.com/lebedev-nikita/boxdb/releases).
