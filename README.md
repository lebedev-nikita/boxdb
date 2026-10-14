<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

An experimental extended SQLite client for Rust and the command line. Binary
files are ordinary SQLite databases. BoxDB adds logical types and validation;
a separate YAML stream provides readable storage for Git. The project is still
in version 0 and does not preserve old BoxDB file or API compatibility.

## Get started

Install from this checkout with a current Rust toolchain, native build tools and
libclang (Xcode Command Line Tools on macOS; `libclang-dev` on Debian/Ubuntu):

```sh
cargo install --path . --locked
boxdb data.db -e "
  CREATE TABLE accounts (
    id INT64 PRIMARY KEY,
    name TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    balance NUMERIC(12,2)
  );
  INSERT INTO accounts(name,balance) VALUES('Ada','12.50');
  SELECT * FROM accounts;
"
sqlite3 data.db 'SELECT * FROM accounts'
```

Binary storage uses bundled SQLite with WAL, FULL synchronization, a busy
timeout, and normal locking. Ordinary SQLite clients can read and write the same
file while BoxDB is open. See [storage and types](docs/storage.md).

Without `-e`, BoxDB opens a multiline shell or reads SQL from stdin. End
statements with a semicolon; `.help` lists shell commands. `--json` emits typed
JSON results. Use `boxdb --help` for all options.

## SQL and types

SQLite executes SQL directly, including joins, views, triggers, UPSERT, CTEs,
window functions, and SQLite functions. Tables created through BoxDB are
`STRICT` automatically; explicitly writing `STRICT` is also valid. Standard
columns use SQLite's strict typing and lossless conversion rules. Use `ANY`
for mixed storage classes; columns without a declared type default to `ANY`.
Integer declarations require a bit width: `INT8`, `INT16`, `INT32`, `INT64`,
`UINT8`, `UINT16`, or `UINT32`. `INT8` means eight bits. Unsized integer names
and the former byte-width aliases are rejected. `UINT64` is deferred;
see [issue #1](https://github.com/lebedev-nikita/boxdb/issues/1).
BoxDB extensions also include `BOOLEAN`, bounded `VARCHAR`, exact
`NUMERIC`/`DECIMAL`, UUID, JSON, UTC `INSTANT`, BYTEA, and typed arrays.

BoxDB checks extended columns on its own connection, including expression
updates, INSERT SELECT, and trigger writes. Some rules also have portable
SQLite CHECK constraints. External SQLite clients do not run BoxDB's additional
validators; invalid extended values are detected when BoxDB reads those cells.
Change typed schemas through BoxDB so their metadata stays synchronized.

Extended values have ordinary SQLite representations. Exact decimals are stored
as text: use decimal strings or `Value::Numeric`, avoiding floating-point SQL
literals. SQL arithmetic, comparison and computed result types follow SQLite's
storage semantics. See [src/types.rs](src/types.rs) and [type tests](tests/types.rs).

## Readable storage and conversion

```sh
boxdb notes.yaml --format readable -e 'CREATE TABLE notes(id INT64 PRIMARY KEY,body TEXT)'
boxdb convert data.db export.yaml --format readable
boxdb convert export.yaml restored.db --format binary
```

Conversion destinations must be new. YAML documents separated by `---` store
schema declarations and rows with named columns. Text can use multiline block
scalars; numbers, nulls and BLOBs retain their SQLite storage classes. See the
[readable format example](docs/storage.md#independent-readable-yaml-stream).

Readable storage uses SQLite in memory for execution and atomically publishes
the complete YAML stream on commit. A readable file allows one writer and needs
its `.lock` sidecar. Litestream works with binary SQLite files; see
[replication setup](docs/replication.md).

## Rust API

```rust
use boxdb::{Database, Format, Value};

fn main() -> boxdb::Result<()> {
    let mut db = Database::open("data.db", Format::Binary)?;
    db.execute("CREATE TABLE IF NOT EXISTS events(id INT64 PRIMARY KEY,at INSTANT)")?;
    db.execute_params("INSERT INTO events(at) VALUES(?)", &[
        Value::Instant("2024-02-29T16:00:00+04:00".into()),
    ])?;
    let results = db.execute("SELECT at FROM events")?;
    println!("{:?}", results);
    Ok(())
}
```

The interface uses `Database`, `QueryResult` and typed `Value`s; it does not
implement the full rusqlite Rust API. `cargo doc --no-deps --open` documents the
library. Export requires an idle connection with no active transaction.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -p 'test_performance.py'
cargo test --locked --test litestream -- --ignored
cargo bench --locked --bench sqlite_vs_boxdb
cargo bench --locked --bench sqlite_vs_boxdb -- --boxdb-format yaml
```

The Litestream integration test requires the original `litestream` executable.
The same 287 adapted upstream SQLite scenarios run against rusqlite and BoxDB
in memory, binary and YAML modes, with persistence checks after reopening.
Additional tests cover types, schemas, interrupted writes and interoperability.
[Benchmark methodology and reports](docs/benchmarks.md) include BoxDB/SQLite
ratios and retain measurements of previous storage implementations.
