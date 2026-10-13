<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

An experimental embedded database for small, typed local datasets. BoxDB has
its own Rust SQL executor, strict columns, binary or Git-friendly storage, and
S3 snapshot replication. SQL compatibility is partial; this is not a production
SQLite replacement.

## Get started

Download the CLI from [GitHub Releases](https://github.com/lebedev-nikita/boxdb/releases),
or build from this checkout with a current Rust toolchain supporting edition 2024
and native C/C++ build tools:

```sh
cargo install --path . --locked
boxdb data.jsonl --format readable -e "
  CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    balance NUMERIC(12,2) DEFAULT 0
  );
  INSERT INTO accounts(name,balance) VALUES('Ada',12.50);
  SELECT * FROM accounts;
"
boxdb data.jsonl --json -e 'SELECT * FROM accounts'
```

Without `-e`, `boxdb data.jsonl` opens a multiline shell or reads SQL from
stdin. End statements with a semicolon; `.help` lists shell commands.
Use `boxdb --help` and `boxdb <command> --help` for CLI options.

Every column requires a type, and tables are strict by default; `STRICT` is
rejected. Writes do not implicitly convert text to numbers or integers to
booleans; use typed values or supported explicit casts. `INTEGER` is 32-bit;
use `BIGINT` for 64-bit integers. Rich types
include exact decimals, UUIDs, JSON, dates, timestamps, and arrays. See
[type validation](src/types.rs) for supported names and limits.

Core SQL statements, constraints, joins, aggregation, and transactions are
implemented. Features such as ALTER TABLE, views, triggers, UPSERT, CTEs,
expression subqueries, and window functions remain unsupported. See the
[public API tests](tests/database.rs) for working query examples.

## Storage and Rust API

Existing files determine their own format; `--format` chooses a new file's
format. Readable files use deterministic UTF-8 JSON Lines with SQL schema records
and stable row IDs for Git diffs. Binary files use a custom BoxDB format;
BoxDB cannot open SQLite database files. Conversion destinations must be new:

```sh
boxdb convert data.jsonl compact.boxdb --format binary
boxdb convert compact.boxdb readable.jsonl --format readable
git add readable.jsonl
```

The entire database lives in memory, and commits atomically replace its file.
A connection holds an exclusive lock until dropped; keep the `.lock` sidecar
in place. Both file size and uncompressed records are limited to 256 MiB.
There is no WAL or shared connection concurrency.

Run `cargo doc --no-deps --open` for the Rust API and a parameterized query
example. The example also lives in [src/lib.rs](src/lib.rs). Statements commit
individually unless enclosed in BEGIN/COMMIT; ROLLBACK discards pending changes.

## S3 backup

The CLI can continuously replicate committed snapshots and restore a published
revision. Follow the [S3 setup and recovery guide](docs/replication.md) for
configuration and recovery limits.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests cover the public Rust API, CLI, reopening both formats, and S3 upload and
restore through a local HTTP server.

Only selected original SQLite cases have been adapted; the full SQLite test
suite has not been ported or run against BoxDB. [Fixture provenance](tests/sqlite/provenance.json)
records source revisions, checksums, and case counts.
[The importer](tools/import_sqlite_tests.py) regenerates the fixtures with
Python 3 and Tcl; running the Rust tests requires neither.
