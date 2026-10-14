# BoxDB's SQLite engine

This directory vendors `libsqlite3-sys` 0.38.2 and SQLite 3.53.2. The upstream
amalgamation in `sqlite3/sqlite3.c` is unchanged. `boxdb.rs` applies checked source
transformations at build time; each anchor must match exactly once. Cargo patches
all workspace users of `libsqlite3-sys` to this engine, including rusqlite and
sqlite-vec.

`native_types.rs` is the shared native type registry used by Rust and the build.
`boxdb_native.inc` dispatches casts, operators, comparisons, SQL functions and value views.
`src/temporal.rs` implements calendar arithmetic and exact interval scaling.
`src/temporal_functions.rs` implements extract, make_interval, age and date_trunc.
SQLite JSON1/JSONB is omitted at compile time; BoxDB validates opaque JSON in Rust.

## Adding a native type

Add a Kind and descriptor to [native_types.rs](native_types.rs): its SQL name,
contiguous affinity, wire tag and fixed payload size. The build generates C
descriptors, affinity constants and SQLite's declaration/type-check tables from
that registry. Affinities currently occupy 0x47 through 0x4f, including DECIMAL.

Implement parsing, formatting, casts, operators, comparison keys and hashing in
Rust behind [native.rs](src/native.rs). Comparison and hashing must agree with
indexes, grouping and uniqueness. [boxdb_native.inc](boxdb_native.inc) supplies
the shared C dispatch at SQLite's value, expression and record boundaries;
cross-type comparison domains and special numeric behavior still need explicit
rules. DECIMAL has a dedicated variable-length codec and numeric backend.

Expose the type in BoxDB's Rust Type/Value enums, schema translation and client
value mappings. Native parameters, results and snapshots use the common BLOB
envelope, while the SQLite C API retains its five standard storage classes.
Test behavior through Database, including scans versus indexes and both snapshot
formats. Public Rust values and serialization form the other integration boundary.

## Engine integration

`boxdb_decimal.inc` integrates decimal affinity, arithmetic, comparison, casts,
and value accessors into SQLite. The internal decimal precision checker is a
built-in, so saved schemas can be restored directly through this engine. `boxdb_aggregate.inc` implements exact decimal and interval accumulators. `src/decimal.rs` provides bounded arbitrary-precision arithmetic
using BigDecimal and BigInt. This fork requires Rust's standard library and the
bundled build. System SQLite, SQLCipher, Gecko, and loadable-extension builds are
rejected instead of silently dropping native decimal support.

The standard SQLite page and record format is preserved. Native values and
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
and Go suites, especially `tests/native_decimal.rs` and `tests/native_types.rs`; comparison changes must agree
across scans, indexes, materialized records, and temporary-file sorting.

Upstream bindings retain their MIT [license](LICENSE). SQLite's amalgamation is
in the public domain. BoxDB additions use the repository's MIT license.
