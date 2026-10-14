# Storage and extended types

## Binary SQLite

`Format::Binary` is SQLite's standard file format. SQLite manages pages, indexes,
transactions, locks, recovery, and the `-wal` and `-shm` sidecars. BoxDB opens
binary databases in WAL mode with FULL synchronization, `fullfsync=ON` on macOS,
foreign keys enabled, a five-second busy timeout, and normal locking. Other
connections using the same engine may use the file concurrently. Native values
and their indexes require BoxDB's patched SQLite engine.

Do not copy only a live `.db` file while ignoring its WAL. Use SQLite's backup
API, BoxDB export, or [Litestream](replication.md). Binary databases are not
limited to the size of RAM; BoxDB materializes query results. Snapshot APIs
currently limit imported/exported snapshots to 256 MiB.

## Extended schema

The ordinary `__boxdb_columns` table records logical types, their version and
physical table declarations. Native types are validated by the engine. BoxDB
validates its metadata against the current schema and installs TEMP validators
for other logical types and native elements in typed arrays. Those
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
| DOUBLE | REAL with 64-bit floating-point values |
| BYTEA | BLOB with a storage-class CHECK |
| DECIMAL, UUID, INSTANT, DATE, DATETIME, INTERVAL | Versioned BLOB envelopes interpreted by the patched engine |
| JSON and typed arrays | JSON text; decimal array elements use strings |

Unsigned columns return `Value::UnsignedInteger`; signed columns return
`Value::Integer`. Supported unsigned ranges fit in SQLite's signed INTEGER,
so native comparisons and sorting retain their numeric meaning. Typed arrays
preserve these logical element types.

Typed parameter values are normalized before binding. Use `Value::Decimal` for
native exact arithmetic.
Native values retain their types in computed results, materialization and sorting.
Other logical values use column-origin metadata for typed projections.

The binary file retains SQLite's standard page and record encoding. Native
values use versioned BLOB envelopes; ordinary BLOBs with the reserved prefix
are escaped. Readable YAML uses typed mappings such as
`at: {instant: "2024-02-29T12:00:00.000000Z"}` and `amount: {decimal: "12.50"}`.
UUID uses a 16-byte payload and canonical lowercase text at public interfaces.
See [native decimals](native-decimal.md) and [temporal types](temporal.md).

`CAST` supports the native types and explicit text views. Other extended logical
casts remain unsupported. JSON is opaque validated text; SQL JSON/JSONB
processing functions and operators are disabled.

TEMP validators are specific to the BoxDB connection. An external SQLite writer
can bypass additional Rust validators for other extended types. Native schemas
and indexes require the modified engine.
BoxDB checks extended values when reading them and validates them on snapshot
import. A SELECT containing an invalid projected typed value returns an error
for the entire result. Foreign-key enforcement must be enabled on each external connection.
This is a validation contract, not protection against deliberate metadata edits.

Native temporal columns normalize assignments in the engine, including generated
columns. Native typed array elements normalize through BoxDB TEMP triggers.
See [temporal semantics and examples](temporal.md).

Use BoxDB for typed CREATE/ALTER/DROP operations: it updates schema and metadata
within one SQLite savepoint. Other extended typed declarations apply to main
database tables; native scalar types also support TEMP and attached tables.
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
