<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

An experimental SQLite client for Rust, TypeScript and the command line. BoxDB
adds logical types and validation to ordinary SQLite databases, with an optional
readable YAML format for storing data in Git. Version 0 does not preserve old
BoxDB file or API compatibility.

## Get started

Install from this checkout with a current Rust toolchain, native build tools and
libclang (Xcode Command Line Tools on macOS; `libclang-dev` on Debian/Ubuntu):

```sh
cargo install --path . --locked
boxdb data.db -e "
  CREATE TABLE accounts (
    id INT64 PRIMARY KEY,
    name TEXT NOT NULL,
    balance NUMERIC(12,2)
  );
  INSERT INTO accounts(name,balance) VALUES('Ada','12.50');
  SELECT * FROM accounts;
"
```

Run `boxdb data.db` for the interactive shell; `.help` lists shell commands.
Use `boxdb --help` for CLI options, including JSON output and format conversion.

## SQL and types

SQLite executes queries directly. Tables created through BoxDB are STRICT by
default; extended types include bounded integers, exact decimals, UUID, JSON,
UTC instants and typed arrays. Bind decimal strings for exact numeric values.
See [type declarations](src/types.rs) and the
[storage and validation contract](docs/storage.md).

Binary files work with ordinary SQLite clients. External writers receive
SQLite constraints but can bypass BoxDB's additional validation. Readable YAML
storage executes in memory and publishes a complete snapshot on commit, with
one writer per file. See [storage formats](docs/storage.md) and
[Litestream replication](docs/replication.md).

## Language clients

For Rust, add this checkout as a path dependency:

```sh
cargo add boxdb --path /absolute/path/to/boxdb
```

The [library source](src/lib.rs) includes a minimal example;
`cargo doc --no-deps --open` opens the API reference.

The [TypeScript client](packages/typescript/README.md) runs in Node.js and Bun
and includes Drizzle, Kysely and a Postgres.js-style adapter. Follow its build
and installation instructions; no prebuilt npm binaries are published yet.

## Development

With Rust, libclang, Node.js 22.18+, Bun and Just installed, run:

```sh
just check
python3 -m unittest discover -s tests -p 'test_performance.py'
```

`just check` builds and checks both clients and runs their tests. Use `just --list`
for individual TypeScript tasks. Agent workflow is documented in [agents.md](agents.md).
See [benchmark commands and methodology](docs/benchmarks.md) and
[UTC instant examples](docs/temporal.md) for specialized workflows.
