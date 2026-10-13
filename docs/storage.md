# Storage formats

BoxDB has two independent codecs over its typed state. Conversion decodes that
state and encodes it in the destination format. Neither codec uses the other as
an intermediate representation.

Readable storage is deterministic UTF-8 JSONL for inspection and Git diffs.
SQL declarations define column types, row IDs remain stable across edits, and
cells are strings or nulls. Each commit atomically publishes a synchronized
complete snapshot. See [the readable codec](../src/storage/readable.rs).

Binary v3 stores native cells and appends checksummed commits. It has no JSONL,
numeric string casts for scalar integers/floats, hex blobs, or compression.
Versions 1 and 2 are rejected without modifying the file. See
[the binary codec](../src/storage/binary.rs).

| Binary field | Representation |
| --- | --- |
| File header | `BOXDB\0\x03` |
| Commit frame | Kind byte, payload length and its complement, SHA-256 of kind and payload, payload, `COMMIT\0\0` marker |
| Counts and byte lengths | Little-endian unsigned 32-bit integers |
| Row IDs and next row ID | Little-endian unsigned 64-bit integers |
| SQL schemas and names | Length-prefixed UTF-8 |
| Cells | Null/present marker followed by a schema-typed value |
| Integers | Little-endian 16-, 32-, or 64-bit scalars |
| REAL and DOUBLE | Little-endian IEEE 754 32- and 64-bit scalars |
| Boolean | One byte, 0 or 1 |
| BYTEA and UUID | Raw bytes; UUIDs occupy 16 bytes |
| Exact decimals and temporal values | Length-prefixed canonical text |
| Arrays | Element count and recursively typed cells |
| JSON | Binary tags for null, booleans, numbers, strings, arrays and objects |

The first frame is a complete snapshot. Later frames contain inserts, deletions,
and changed column values for stable row IDs. A commit synchronizes the appended
frame before acknowledging success. No-op updates avoid a disk write. Schema
changes and row compaction publish a new snapshot atomically.

Automatic checkpoints bound replay and file growth. A checkpoint replaces the
file when the next append would exceed twice the base snapshot size (with a
1 MiB minimum), 1,024 appended commits, or the 256 MiB file limit. Exports always
produce one compact snapshot; transaction and savepoint changes stay private
until committed.

Opening a file verifies checksums, rebuilds indexes, and validates the final
typed state and constraints. It discards and synchronizes truncation of an
incomplete final frame after a valid committed prefix. Complete corrupt frames
are errors; they are never silently discarded. Import and restore require
complete input and reject incomplete tails.

A connection holds the `.lock` sidecar exclusively until dropped. Writers also
hold `.snapshot.lock` exclusively during publication or recovery.
`Database::read_snapshot(path)` holds that second lock shared while reading and
validating the committed prefix, so replication can run alongside an open
writer. It does not modify incomplete tails. Keep both sidecars in place.

In memory, rows use pages with copy-on-write and stable slots. Automatic primary
and unique indexes use persistent B+ trees, so transactions share unchanged
nodes. Mutations validate changed rows and update affected index keys. Foreign
keys and schema changes use full validation; parent-key indexes accelerate
foreign-key matching where the column types permit it.
