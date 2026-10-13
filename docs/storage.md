# Storage and extended types

## Binary SQLite

`Format::Binary` is SQLite's standard file format. SQLite manages pages, indexes,
transactions, locks, recovery, and the `-wal` and `-shm` sidecars. BoxDB opens
binary databases in WAL mode with FULL synchronization, `fullfsync=ON` on macOS,
foreign keys enabled, a five-second busy timeout, and normal locking. Other
SQLite connections may use the file concurrently. Files containing BoxDB type
metadata require SQLite 3.37 or later because that metadata table is STRICT.

Do not copy only a live `.db` file while ignoring its WAL. Use SQLite's backup
API, BoxDB export, or [Litestream](replication.md). Binary databases are not
limited to the size of RAM; BoxDB materializes query results. Snapshot APIs
currently limit imported/exported snapshots to 256 MiB.

## Extended schema

The ordinary `__boxdb_columns` table records logical types, their version and
physical table declarations. SQLite clients can inspect it without extensions.
BoxDB validates its metadata against the current schema, registers validators
on its connection, and installs TEMP triggers for INSERT and UPDATE. Those
triggers also validate writes caused by SQL expressions, INSERT SELECT and user
triggers. Their errors abort the statement. BoxDB reserves `__boxdb_` names.

Standard SQLite types keep SQLite behavior. BoxDB's extended type declarations
are translated into physical types and, where applicable, portable CHECKs:

| Logical type | SQLite representation |
| --- | --- |
| SMALLINT, INT4 | INTEGER with a range CHECK |
| BOOLEAN | INTEGER with a 0/1 CHECK |
| VARCHAR(n) | TEXT with a length CHECK |
| FLOAT4 | REAL with BoxDB's float32 validation |
| BYTEA | BLOB with a storage-class CHECK |
| NUMERIC, DECIMAL | Exact decimal text |
| UUID and temporal types | Text |
| JSON and typed arrays | JSON text; decimal array elements use strings |

Typed parameter values are normalized before binding. Prefer `Value::Numeric`
for exact decimals and typed UUID/temporal values for canonical representations.
SQLite's ordinary SQL arithmetic and sorting still operate on physical storage
classes. Extended CAST syntax from the former BoxDB executor is not supported.
Computed columns return SQLite values; direct column projections, including
aliases and views, use their original column's logical type.

TEMP validators are specific to the BoxDB connection. An external SQLite writer
gets portable SQLite constraints but can bypass the additional Rust validators.
BoxDB checks extended values when reading them and validates them on snapshot
import. Foreign-key enforcement must be enabled on each external connection.
This is a validation contract, not protection against deliberate metadata edits.

Use BoxDB for typed CREATE/ALTER/DROP operations: it updates schema and metadata
within one SQLite savepoint. Typed declarations apply to main database tables.
External schema edits can leave stale metadata, which BoxDB rejects rather than
silently guessing a new logical schema.

## Independent readable YAML stream

[The readable codec](../src/readable.rs) stores a versioned header, SQL schema
objects, rows with named columns, and AUTOINCREMENT sequences as separate YAML
documents. Each document starts with `---`. A small complete database looks like:

```yaml
---
kind: database
version: 2
application_id: 0
user_version: 0
---
kind: schema
object_type: table
name: notes
table: notes
sql: CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT)
---
kind: row
table: notes
rowid: 1
values:
  body: |-
    First line
    Second line
  id: 1
```

Native cells use YAML numbers, strings and nulls. Quote text that looks like a
number or boolean. BLOBs use a mapping such as `bytes: {blob: '00ff'}`; boolean
scalars are imported as SQLite integers 0/1. Rows retain their accessible SQLite
row IDs; generated columns are recomputed. The representation is independent of
SQLite file bytes.

Comments are accepted on import and omitted when snapshots are regenerated.
Duplicate keys, unknown row columns and malformed cells are rejected.

Readable connections execute SQL in memory and publish a synchronized complete
snapshot by atomic file replacement after committing. Pending transactions stay
private. A readable file holds a separate `.lock` sidecar until closed. On a
publication error, reopen the connection before continuing. Readable snapshots
do not support virtual tables or non-finite floating-point values.

`Database::export` requires no active transaction. Binary export uses SQLite's
backup API to produce a self-contained file with committed WAL pages included.
`Database::read_snapshot` opens a separate SQLite reader to capture committed
binary data while a writer is active. Restore and conversion require a new
path. Previous native BoxDB binary files and JSONL layouts are unsupported.
