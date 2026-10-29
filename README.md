<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

**A database for your $5 VPS.**

SQLite with validated logical types, native exact decimals and temporal values,
for Rust, TypeScript, Go and the command line. Keep application data in a binary
SQLite file, or choose readable YAML for small Git-tracked datasets.

[Website & documentation](https://lebedev-nikita.github.io/boxdb/) ·
[Quickstart](docs/quickstart.md) ·
[Releases](https://github.com/lebedev-nikita/boxdb/releases) ·
[API reference](https://docs.rs/boxdb)

> **Experimental, version 0.** APIs and BoxDB storage contracts may change.
> Binary files keep SQLite's standard page and record encoding; native values
> and indexes require BoxDB's patched engine.

## Try it

Requires Node.js 22.18+ on macOS ARM64 or Linux x64/ARM64 with glibc 2.35+
(Ubuntu 22.04+ or 64-bit Raspberry Pi OS Bookworm+).

```sh
npm install -g @boxdb/cli
boxdb data.db -e "
  CREATE TABLE invoices(id INT64 PRIMARY KEY, amount DECIMAL(12,2));
  INSERT INTO invoices(amount) VALUES('0.10'),('0.20');
  SELECT SUM(amount) AS total FROM invoices;
"
```

The exact total is `0.3`. Bind decimal strings for exact input.
Run `boxdb data.db` for the interactive shell, or follow the
[quickstart](docs/quickstart.md) to query, inspect and back up a database.
Standalone binaries are available in [releases](https://github.com/lebedev-nikita/boxdb/releases).

## Choose your client

| Client | Installation | Documentation |
| --- | --- | --- |
| TypeScript / Node.js / Bun | `npm install @boxdb/client` | [Client guide](packages/typescript/README.md) |
| Rust | `cargo add boxdb` | [Client guide](docs/rust.md) · [API](https://docs.rs/boxdb) |
| Go | Go module plus matching native library | [Installation and database/sql](packages/go/README.md) |
| CLI | `npm install -g @boxdb/cli` | [Command-line guide](packages/cli/README.md) |

For TypeScript integrations, use the [Drizzle](packages/drizzle-adapter/README.md),
[Better Auth](packages/better-auth-adapter/README.md) or [db0](packages/db0-adapter/README.md)
adapter. For AI-assisted projects, use the [BoxDB usage skill](.agents/skills/boxdb/SKILL.md).

## Why BoxDB?

- **Types with meaning.** Bounded signed/unsigned integers, enums, exact decimals,
  UUIDs, UTC instants, calendar values, intervals and typed arrays. Ordinary tables
  are STRICT by default.
- **A shared core.** The same validation, SQL migrations and storage across clients.
  The TypeScript client executes SQL on a native worker.
- **Built-in SQL tools.** FTS5, R*Tree, vector search, Unicode text and regular
  expressions, without runtime extension loading.
- **Two storage choices.** Binary SQLite for application data; readable YAML for
  small datasets you want to review as text.

BoxDB is intended for single-server applications. It has no built-in remote
server or distributed writes. Readable storage executes in memory, has one writer
per file and writes a complete snapshot on commit. JSON is a validated opaque
document; SQL JSON/JSONB processing is disabled.

Read [the fit and tradeoffs](docs/why-boxdb.md) before adoption. See
[benchmark methodology](docs/benchmarks.md) to measure your workload.

## Documentation

Start at the [documentation index](docs/index.md), or go directly to:

- [SQL and types](docs/sql.md), [exact decimals](docs/native-decimal.md) and [temporal values](docs/temporal.md).
- [Storage and validation](docs/storage.md) and [SQL extensions](docs/extensions.md).
- [Migrations and schema export](docs/migrations.md).
- [Deployment and backups](docs/deployment.md), [Litestream replication](docs/replication.md),
  [diagnostics and recovery](docs/recovery.md) and [client errors](docs/client-errors.md).
- [FAQ](docs/faq.md).

## Contribute

See [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites, checks and pull requests.
With the development toolchain installed, run `just check`; use `just --list`
for focused commands. Build from source with `cargo install --path . --locked`.
See [npm releases](docs/npm-release.md) and [Rust releases](docs/rust-release.md)
for package publication, and [website development](website/README.md) for GitHub Pages.

[Report a bug or request a feature](https://github.com/lebedev-nikita/boxdb/issues/new/choose).
Report vulnerabilities privately through the [security policy](SECURITY.md).
BoxDB is [MIT licensed](LICENSE).
