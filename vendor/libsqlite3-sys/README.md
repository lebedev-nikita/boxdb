# BoxDB's SQLite engine

This directory vendors `libsqlite3-sys` 0.38.2 and SQLite 3.53.2. The upstream
amalgamation in `sqlite3/sqlite3.c` is unchanged. `boxdb.rs` applies checked source
transformations at build time; each anchor must match exactly once. The package is
published as `boxdb-sqlite-sys`, retaining the Rust library name `libsqlite3_sys`.
BoxDB and `boxdb-rusqlite` depend on it directly; consumers need no Cargo patches.
SQLite extensions link to this engine.

`native_types.rs` is the shared native type registry used by Rust and the build.
`boxdb_native.inc` dispatches casts, operators, comparisons, SQL functions and value views.
`src/temporal.rs` implements calendar arithmetic and exact interval scaling.
`src/temporal_functions.rs` implements extract, make_interval, age and date_trunc.
SQLite JSON1/JSONB is omitted at compile time; BoxDB validates opaque JSON in Rust.

## Adding a native type

Add a Kind and descriptor to [native_types.rs](native_types.rs): its SQL name,
affinity, wire tag and fixed payload size. The build generates C
descriptors, affinity constants and SQLite's declaration/type-check tables from
that registry. Affinities occupy 0x47 through 0x4f and 0x60; the 0x6f comparison
mask excludes SQLite's JUMPIFNULL and NULLEQ bits. Affinity lookup uses the
registry rather than assuming contiguous assignments.

Implement parsing, formatting, casts, operators, comparison keys and hashing in
Rust behind [native.rs](src/native.rs). Comparison and hashing must agree with
indexes, grouping and uniqueness. [boxdb_native.inc](boxdb_native.inc) supplies
the shared C dispatch at SQLite's value, expression and record boundaries;
cross-type comparison domains and special numeric behavior still need explicit
rules. DECIMAL has a dedicated variable-length codec and numeric backend.

Expose the type in BoxDB's Rust Type/Value enums, schema translation and client
value mappings. Native parameters, results and snapshots use a one-byte BLOB
type tag, while the SQLite C API retains its five standard storage classes.
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
ordinary BLOBs use single-byte type tags. UINT8/16/32 use INTEGER serial types
when a column or index declaration supplies their width; OP_Column restores
that width before evaluation. Runtime and untyped values keep their tag, while
UINT64 always uses a tagged BLOB. `src/unsigned.rs` implements checked unsigned
arithmetic and logical shifts. The database codec version is stored
in `__boxdb_codec`, and readable headers also record it. Stock SQLite does not have
compatible query or index semantics. See [the decimal contract](../../docs/native-decimal.md).

`sqlite3_boxdb_value_blob` and `sqlite3_boxdb_value_bytes` expose the wire cell
of a `sqlite3_value`, which BoxDB's preupdate hook uses to record changesets.
Normal BLOB bind/result APIs add the BYTEA tag. BoxDB transports already
encoded cells through the pointer API with the private type `boxdb.wire-blob.v1`
and a borrowed C descriptor `{ const char *data; int length; }`. The patched API
copies the descriptor's bytes immediately and calls a supplied destructor once.
This route is used for typed parameters, native decimal function results, and
readable snapshot import. Standard C column and scalar-function value accessors
expose logical payloads. `sqlite3_boxdb_column_blob` and its size accessors expose
wire cells; the Rust FFI uses these for column BLOB decoding. Incremental BLOB
access skips the BYTEA tag and preserves it on writes.

To upgrade SQLite, update the pinned upstream sources and crate build/bindings,
then adapt the checked transformations. Run the complete Rust and TypeScript
and Go suites, especially `tests/native_decimal.rs` and `tests/native_types.rs`; comparison changes must agree
across scans, indexes, materialized records, and temporary-file sorting.

Upstream bindings retain their MIT [license](LICENSE). SQLite's amalgamation is
in the public domain. BoxDB additions use the repository's MIT license.
