# BoxDB's SQLite engine

This directory vendors `libsqlite3-sys` 0.38.2 and SQLite 3.53.2. The upstream
amalgamation in `sqlite3/sqlite3.c` is unchanged. `boxdb.rs` applies checked source
transformations at build time; each anchor must match exactly once. Cargo patches
all workspace users of `libsqlite3-sys` to this engine, including rusqlite and
sqlite-vec.

`boxdb_decimal.inc` integrates decimal affinity, arithmetic, comparison, casts,
and value accessors into SQLite. The internal decimal precision checker is a
built-in, so saved schemas can be restored directly through this engine. `boxdb_aggregate.inc` implements exact decimal
accumulators. `src/decimal.rs` provides bounded arbitrary-precision arithmetic
using BigDecimal and BigInt. This fork requires Rust's standard library and the
bundled build. System SQLite, SQLCipher, Gecko, and loadable-extension builds are
rejected instead of silently dropping native decimal support.

The standard SQLite page and record format is preserved. Native decimals and
escaped ordinary BLOBs use versioned BLOB envelopes. Stock SQLite does not have
compatible query or index semantics. See [the decimal contract](../../docs/native-decimal.md).

Normal BLOB bind/result APIs escape reserved payloads. BoxDB transports already
encoded cells through the pointer API with the private type `boxdb.wire-blob.v1`
and a borrowed C descriptor `{ const char *data; int length; }`. The patched API
copies the descriptor's bytes immediately and calls a supplied destructor once.
This route is used for typed parameters, native decimal function results, and
readable snapshot import. `sqlite3_column_blob` returns wire cells for BoxDB's
Rust decoder; scalar-function value accessors expose logical payloads.

To upgrade SQLite, update the pinned upstream sources and crate build/bindings,
then adapt the checked transformations. Run the complete Rust and TypeScript
suites, especially `tests/native_decimal.rs`; comparison changes must agree
across scans, indexes, materialized records, and temporary-file sorting.

Upstream bindings retain their MIT [license](LICENSE). SQLite's amalgamation is
in the public domain. BoxDB additions use the repository's MIT license.
