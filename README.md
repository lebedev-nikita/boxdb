<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

A database for your $5 VPS.

An experimental SQLite client for Rust, TypeScript, Go and the command line. BoxDB
adds logical types and validation to ordinary SQLite databases, with an optional
readable YAML format for storing data in Git. Version 0 does not preserve old
BoxDB file or API compatibility.

## Get started

Install the CLI with Node.js 22.18+ on macOS ARM64 or Linux x64 with glibc 2.35+
(Ubuntu 22.04+):

```sh
npm install -g @boxdb/cli
boxdb data.db -e "
  CREATE TABLE accounts (
    id INT64 PRIMARY KEY,
    name TEXT NOT NULL,
    balance DECIMAL(12,2)
  );
  INSERT INTO accounts(name,balance) VALUES('Ada','12.50');
  SELECT * FROM accounts;
"
```

Run `boxdb data.db` for the interactive shell; `.help` lists shell commands.
Use `boxdb --help` for CLI options, including JSON output and format conversion.
See the [CLI README](packages/cli/README.md) for installation and usage.

To build from this checkout, use `cargo install --path . --locked` with a current
Rust toolchain, native build tools and libclang (Xcode Command Line Tools on
macOS; `libclang-dev` on Debian/Ubuntu).

## Migrations and schema export

```sh
boxdb migrate new create_accounts
# Edit the generated SQL file, then:
boxdb migrate up data.db
boxdb migrate status data.db --check
boxdb migrate down data.db
boxdb schema dump data.db > schema.sql
boxdb schema dump data.db --format=yaml > schema.yaml
```

Migrations run through BoxDB and preserve extended types. Each version commits
atomically; applied files are checked for changes. Schema dumps include type and
migration metadata without application rows. SQL is the default export format;
YAML exports a logical schema with nullable columns marked as `name?`;
it is separate from readable database storage. See [migration and restoration usage](docs/migrations.md).

## SQL and types

SQLite executes queries directly. Tables created through BoxDB are STRICT by
default; extended types include bounded integers, string enums, exact decimals,
UUID, JSON, UTC instants, calendar dates/datetimes, intervals and typed arrays. DECIMAL,
UUID, INSTANT, DATE, DATETIME and INTERVAL are native engine values with typed
comparison and indexing. JSON is validated opaque text; JSON/JSONB SQL functions
and operators are disabled.
Table column names cannot end in `?`, which marks nullable columns in YAML schema exports.
Bind decimal strings for exact input. See [native decimals](docs/native-decimal.md).
See [temporal arithmetic](docs/temporal.md), [type declarations](src/types.rs) and the
[storage and validation contract](docs/storage.md).

FTS5, R*Tree, exact decimal SQL functions, Unicode text functions, regular
expressions and vector search are built in for every client. No shared-library
loading is needed. See [extension examples and storage limits](docs/extensions.md).

Binary files use SQLite's standard page and record format. Native type queries
and indexes require BoxDB's patched engine. External writers can bypass BoxDB's
additional validation for other extended types. Readable YAML
storage executes in memory and publishes a complete snapshot on commit, with
one writer per file. See [storage formats](docs/storage.md) and
[Litestream replication](docs/replication.md).

Use `boxdb check data.db --full` to diagnose an existing file. Explicit `repair`
and `recover` commands write to new destinations; see
[diagnostics and recovery](docs/recovery.md) for their limits and client APIs.

## Language clients

Install the experimental Rust library from crates.io:

```sh
cargo add boxdb
```

The [library source](src/lib.rs) includes a minimal example;
`cargo doc --no-deps --open` opens the API reference.
The crate bundles BoxDB's SQLite engine and extensions; building requires native
build tools and libclang. A local path dependency also works without Cargo patches.

The [TypeScript client](packages/typescript/README.md) runs in Node.js and Bun
with a nonblocking API. Follow its build and installation instructions, or
install it with `npm install @boxdb/client`. Separate adapters have their own guides:

- [Drizzle](packages/drizzle-adapter/README.md): queries, transactions, migrations and extended column types.
- [Better Auth](packages/better-auth-adapter/README.md): authentication storage and initial schema generation.
- [db0](packages/db0-adapter/README.md): connector setup and transaction handles.

The [Go client](packages/go/README.md) implements `database/sql`, with context
cancellation, prepared statements and transactions. It uses the Rust core through
cgo; follow its native build and local module installation instructions.

For AI agents, the [BoxDB skill](.agents/skills/boxdb/SKILL.md) routes usage tasks
to the appropriate client guide and explains BoxDB-specific constraints.

## Development

With Rust, libclang, Go 1.23+, a C compiler, Node.js 22.18+, Bun, Just and Python 3.11+ installed, run:

```sh
just check
```

`just check` builds and checks all clients and runs their tests, including a
cross-language migration and conversion scenario. Use `just client-types-test`
to run that scenario alone, or `just --list` for individual client tasks.
See [benchmark commands and methodology](docs/benchmarks.md) and
[UTC instant examples](docs/temporal.md) for specialized workflows.
See [npm releases](docs/npm-release.md) to build, verify and publish the npm packages.
See [Rust releases](docs/rust-release.md) to package and publish the Cargo crates.
