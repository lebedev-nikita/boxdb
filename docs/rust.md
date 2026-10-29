# BoxDB for Rust

Use the Rust core directly for embedded applications and command-line tools.
It bundles BoxDB's patched SQLite engine and extensions.

## Install

```sh
cargo add boxdb
```

Build with a current stable Rust toolchain, native build tools and libclang.
On macOS, install Xcode Command Line Tools; on Debian or Ubuntu, install
`build-essential` and `libclang-dev`. A checkout can also be used as a local path
dependency without Cargo patches.

## Open, write and read

```rust
use boxdb::{Database, Format, Value};

fn main() -> boxdb::Result<()> {
    let mut db = Database::open("data.db", Format::Binary)?;
    db.execute(
        "CREATE TABLE IF NOT EXISTS users(id INT64 PRIMARY KEY, name TEXT NOT NULL)"
    )?;
    db.execute_params(
        "INSERT INTO users(name) VALUES(?)",
        &[Value::Text("Ada".into())],
    )?;
    let results = db.execute("SELECT name FROM users ORDER BY id")?;
    for row in &results[0].rows {
        println!("{row:?}");
    }
    Ok(())
}
```

Use `Database::try_memory()?` for a fallible in-memory connection. To create
readable storage, open a new path with `Format::Readable`. Existing contents
determine the format when reopening.

`execute()` accepts SQL scripts and returns a `Vec<QueryResult>`, one result per
statement. A result contains column names, positional rows and affected-row
counts. `execute_params()` binds values separately from SQL. Scripts are not
automatically atomic; group related work in an explicit transaction.

```rust
db.execute("BEGIN IMMEDIATE")?;
let work = db.execute_params(
    "INSERT INTO users(name) VALUES(?)",
    &[Value::Text("Grace".into())],
);
match work {
    Ok(_) => { db.execute("COMMIT")?; }
    Err(error) => {
        db.execute("ROLLBACK")?;
        return Err(error);
    }
}
```

On failure, account for rollback errors in application error handling.
The core API is synchronous; use an appropriate blocking boundary in an async
application. Open independent connections inside the threads that own them.

## Values and errors

`Value` represents signed and unsigned integers, exact decimals, text, bytes,
booleans, UUIDs, JSON documents, temporal values, arrays and NULL. Bind exact
amounts as `Value::Decimal(String)` or decimal strings in a matching typed
column. Read [SQL types](sql.md), [decimals](native-decimal.md) and
[temporal values](temporal.md) before conversion.

Errors implement `std::error::Error`. Branch on `error.code()`; optional
`result_code()` and `extended_code()` retain engine details. See
[client errors](client-errors.md) for cleanup and retry considerations.

## Reference and operations

The [API reference](https://docs.rs/boxdb) documents the released crate.
In a checkout, run `cargo doc --no-deps --open` for the exact local version.

- [SQL migrations](migrations.md#migrations-from-application-code).
- [Storage and conversion](storage.md).
- [Diagnostics and recovery](recovery.md).
- [Built-in SQL extensions](extensions.md).
