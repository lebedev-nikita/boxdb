# Why BoxDB?

BoxDB is for developers who want an embedded SQL database with more expressive
types and a small operational footprint. Your application opens a file and runs
SQL. SQLite handles pages, indexes, transactions and locking; BoxDB adds logical
validation and native values to its bundled engine.

## Where it fits

- **Small applications on one server.** Keep application code and database
  together, with TypeScript, Rust, Go or command-line access to the same data.
- **Exact amounts and meaningful dates.** Use DECIMAL arithmetic, UTC instants,
  calendar dates and other native types without rebuilding their semantics in
  every query.
- **Small datasets reviewed in Git.** Choose readable YAML when human-readable
  diffs matter more than concurrent writers or fast full-file publication.
- **An integrated SQL toolkit.** Use built-in full-text search, vector search,
  Unicode functions and regular expressions without loading shared libraries.

## Choose deliberately

| Need | Consider |
| --- | --- |
| Standard SQLite tooling and broad runtime portability | Ordinary SQLite and its established clients |
| Shared remote access, multiple application servers or horizontal write scaling | A server database such as PostgreSQL |
| SQLite plus BoxDB's exact types and validation, on a supported platform | BoxDB binary storage |
| Small, Git-tracked datasets with one writer | BoxDB readable YAML storage |

This is a fit guide, not a performance ranking. Measure your workload with the
[benchmark tools](benchmarks.md) and your own application.

## Current boundaries

BoxDB is experimental version 0 software. API and BoxDB storage compatibility
are not guaranteed. Evaluate upgrade and restore procedures before relying on a
particular version.

- **A patched SQLite engine.** Binary pages and records use SQLite's standard
  encoding. Native values and indexes still require BoxDB's engine. Standard
  `sqlite3` is not a substitute for BoxDB-aware queries or exports.
- **A local database.** No built-in remote server, distributed writes, cloud
  control plane or automatic failover. [Litestream](replication.md) can replicate
  binary databases asynchronously.
- **Platform-specific native clients.** npm releases support macOS ARM64 and
  Linux x64/ARM64 with glibc 2.35+. No browser, Windows, Intel macOS or npm
  Alpine/musl support. Check each client's installation guide.
- **Readable storage has different costs.** It restores the database into memory,
  has a 256 MiB file limit, allows one writer and publishes the entire snapshot
  on commit. Virtual tables require binary or memory storage.
- **JSON is opaque.** Complete documents are validated and processed in your
  application. SQL JSON/JSONB functions, operators and virtual tables are disabled.
- **Results are materialized.** Keep result sets bounded; the TypeScript API has
  no streaming or explicit prepared-statement interface.

Start with the [quickstart](quickstart.md), then read the
[storage contract](storage.md) for the mode you choose.
