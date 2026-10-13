<img src="assets/icon.svg" width="64" height="64" alt="">

# BoxDB

An experimental extended SQLite client for Rust and the command line. Binary
files are ordinary SQLite databases. BoxDB adds logical types and validation;
a separate JSONL format provides readable storage for Git. The project is still
in version 0 and does not preserve old BoxDB file or API compatibility.

## Get started

Install from this checkout with a current Rust toolchain and native build tools:

```sh
cargo install --path . --locked
boxdb data.db -e "
  CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
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
window functions, and SQLite functions. Standard SQLite column types retain
SQLite's behavior; use SQLite `STRICT` tables for its built-in strict typing.
`INTEGER` is 64-bit. BoxDB extensions include `SMALLINT`, `INT4`, `BOOLEAN`,
bounded `VARCHAR`, exact `NUMERIC`/`DECIMAL`, UUID, JSON, temporal types, BYTEA,
and typed arrays.

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
boxdb data.jsonl --format readable -e 'CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT)'
boxdb convert data.db data.jsonl --format readable
boxdb convert data.jsonl restored.db --format binary
```

Conversion destinations must be new. JSONL stores SQL schema declarations and
rows independently of the binary representation. It uses SQLite in memory for
execution and atomically publishes the readable file on commit. A readable file
allows one writer and needs its `.lock` sidecar. Litestream works with binary
SQLite files; see [replication setup](docs/replication.md).

## Rust API

```rust
use boxdb::{Database, Format, Value};

fn main() -> boxdb::Result<()> {
    let mut db = Database::open("data.db", Format::Binary)?;
    db.execute("CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,day DATE)")?;
    db.execute_params("INSERT INTO events(day) VALUES(?)", &[
        Value::Date("2024-02-29".into()),
    ])?;
    let results = db.execute("SELECT day FROM events")?;
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
cargo test --locked --test litestream -- --ignored
cargo bench --locked --bench sqlite_vs_boxdb
```

The Litestream integration test requires the original `litestream` executable.
The same 287 adapted upstream SQLite scenarios run against both BoxDB and
rusqlite; additional tests cover types, schemas and file interoperability.
[Benchmark methodology and reports](docs/benchmarks.md) include BoxDB/SQLite
ratios and retain measurements of previous storage implementations.
