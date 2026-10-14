# Storage and extended types

## Binary SQLite

`Format::Binary` is SQLite's standard file format. SQLite manages pages, indexes,
transactions, locks, recovery, and the `-wal` and `-shm` sidecars. BoxDB opens
binary databases in WAL mode with FULL synchronization, `fullfsync=ON` on macOS,
foreign keys enabled, a five-second busy timeout, and normal locking. Other
SQLite connections may use the file concurrently. Tables created through BoxDB
use STRICT storage and require SQLite 3.37 or later.

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

Ordinary CREATE TABLE statements are STRICT by default, including TEMP tables,
WITHOUT ROWID and CREATE TABLE AS SELECT. An explicit STRICT suffix is accepted,
including either ordering with WITHOUT ROWID. Standard types follow SQLite's
[STRICT conversion rules](https://www.sqlite.org/stricttables.html): lossless
conversions such as text `'42'` into an integer are allowed; incompatible values
are rejected. ANY preserves each value's storage class. A column without a
declared type becomes ANY. Integer declarations accept SQLite INTEGER or an
explicit bit width (see [type declarations](../src/types.rs));
floating-point aliases map to REAL. INT64 and the SQL alias SERIAL map to
INTEGER, including SQLite's rowid alias and AUTOINCREMENT behavior.

The policy applies to table creation through BoxDB. Opening or importing an
existing database preserves its schema and recognizes legacy integer metadata
with its original bit widths. Virtual tables retain their SQLite
module's typing rules. CTAS retains SQLite's inferred physical types; computed
columns without a supported inferred type become ANY. Extended logical metadata
is not inferred from SELECT expressions.

BoxDB's extended type declarations
are translated into physical types and, where applicable, portable CHECKs:

| Logical type | SQLite representation |
| --- | --- |
| INT8, INT16, INT32, UINT8, UINT16, UINT32 | INTEGER with a range CHECK |
| BOOLEAN | INTEGER with a 0/1 CHECK |
| VARCHAR(n) | TEXT with a length CHECK |
| FLOAT4 | REAL with BoxDB's float32 validation |
| BYTEA | BLOB with a storage-class CHECK |
| NUMERIC | Exact decimal text |
| UUID and INSTANT | Text |
| JSON and typed arrays | JSON text; decimal array elements use strings |

Unsigned columns return `Value::UnsignedInteger`; signed columns return
`Value::Integer`. Supported unsigned ranges fit in SQLite's signed INTEGER,
so native comparisons and sorting retain their numeric meaning. Typed arrays
preserve these logical element types.

Typed parameter values are normalized before binding. Prefer `Value::Numeric`
for exact decimals and typed UUID/instant values for canonical representations.
SQLite's ordinary SQL arithmetic and sorting still operate on physical storage
classes. Extended CAST syntax from the former BoxDB executor is not supported.
Use the built-in [decimal functions and collation](extensions.md#exact-decimals)
for exact arithmetic and numeric ordering of decimal text.
Computed columns return SQLite values; direct column projections, including
aliases and views, use their original column's logical type.

TEMP validators are specific to the BoxDB connection. An external SQLite writer
gets portable SQLite constraints but can bypass the additional Rust validators.
BoxDB checks extended values when reading them and validates them on snapshot
import. A SELECT containing an invalid projected typed value returns an error
for the entire result. Foreign-key enforcement must be enabled on each external connection.
This is a validation contract, not protection against deliberate metadata edits.

`INSTANT` is the only temporal column type. It represents an exact instant:
input must include `Z` or an explicit numeric UTC offset. BoxDB shifts offset
instants to UTC and stores fixed-width RFC 3339 text with nine fractional
digits, such as `2024-02-29T12:00:00.000000000Z`. This also makes ordinary SQLite
text ordering chronological for canonical values. Dates without a time or
offset, leap seconds, and precision beyond nanoseconds are rejected. The UTC
year must be between 0000 and 9999. See [instant usage](temporal.md).

SQL writes through BoxDB normalize instant columns and typed instant arrays
using TEMP triggers. Defaults added to existing rows are normalized within the
schema savepoint. Readable imports normalize rows before restoring user triggers.
Generated instant expressions must already produce canonical
UTC text. Other SQLite connections do not run these triggers and should write
canonical UTC values themselves. SQL comparisons with plain text literals still
use SQLite text semantics; typed `Value::Instant` parameters are normalized
before binding.

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
sql: CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT) STRICT
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

Row documents are cached in memory. SQLite change notifications select the rows
to serialize again, including trigger writes, REPLACE deletions and foreign-key
cascades; rollback is reconciled against the current database state. Schema
changes rebuild the cache. Tables without an accessible rowid are reserialized
as a whole when changed. Cached documents add memory proportional to the YAML
snapshot, alongside the in-memory SQLite database.

Publication still writes the complete stream, synchronizes the temporary file,
replaces the destination and synchronizes its directory. Its I/O cost grows with
file size even for a single changed row. Group related writes in BEGIN/COMMIT to
pay for one publication; reading an already open database uses SQLite in memory
without parsing YAML again. Opening a file must parse and restore the stream.

`Database::export` requires no active transaction. Binary export uses SQLite's
backup API to produce a self-contained file with committed WAL pages included.
`Database::read_snapshot` opens a separate SQLite reader to capture committed
binary data while a writer is active. Restore and conversion require a new
path. Previous native BoxDB binary files and JSONL layouts are unsupported.
