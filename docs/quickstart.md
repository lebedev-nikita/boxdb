# Your first BoxDB database

Create a typed database, write two invoices and calculate an exact total.
Use a new directory so the example starts with an empty database.

## Install

The npm CLI requires Node.js 22.18+ on macOS ARM64 or Linux x64/ARM64 with
glibc 2.35+ (Ubuntu 22.04+ or 64-bit Raspberry Pi OS Bookworm+).

```sh
npm install -g @boxdb/cli
mkdir boxdb-quickstart
cd boxdb-quickstart
```

For a standalone binary, use [GitHub releases](https://github.com/lebedev-nikita/boxdb/releases).
For source builds and platform details, see the [CLI guide](../packages/cli/README.md).

## Create, insert and query

```sh
boxdb invoices.db -e "
  CREATE TABLE invoices (
    id INT64 PRIMARY KEY,
    customer TEXT NOT NULL,
    amount DECIMAL(12,2) NOT NULL
  );
  INSERT INTO invoices(customer,amount)
    VALUES('Ada','0.10'),('Grace','0.20');
  SELECT customer,amount FROM invoices ORDER BY id;
  SELECT SUM(amount) AS total FROM invoices;
"
```

The final query returns the exact decimal `0.3`. Quote decimal inputs so they
do not first become approximate floating-point numbers. The values are validated
against the column's precision and scale; excess scale is rejected, not silently
rounded. See [decimal arithmetic](native-decimal.md).

`invoices.db` persists in the current directory. Run a new process to read it:

```sh
boxdb invoices.db -e 'SELECT COUNT(*) AS invoices FROM invoices;'
```

This returns `2`. Run `boxdb invoices.db` to open the interactive shell.
Enter `.help` for shell commands or `.quit` to leave.

## Inspect and back up

```sh
boxdb check invoices.db --full
boxdb convert invoices.db invoices-backup.db --format binary
boxdb convert invoices.db invoices.yaml --format readable
```

Each destination must be new. Conversion preserves the source. Open
`invoices.yaml` in a text editor to inspect the readable representation, or query
it with `boxdb invoices.yaml -e 'SELECT SUM(amount) FROM invoices;'`.
Existing contents select the format automatically.

Binary storage is the application default: WAL, durable commits and concurrent
connections. Readable storage is useful for small Git-tracked datasets; it
executes in memory and writes a complete YAML snapshot on commit. It supports
one writer per file and has a 256 MiB limit. See [storage](storage.md).

## Use BoxDB from your application

Choose [TypeScript](../packages/typescript/README.md), [Rust](rust.md), or
[Go](../packages/go/README.md). All clients share the same core and files.
Use bound parameters for application values.

For a growing application, put schema changes in [SQL migrations](migrations.md)
and follow the [deployment guide](deployment.md). BoxDB is experimental:
read the [current boundaries](why-boxdb.md#current-boundaries) before adopting it.
