# Storage and extended types

## Binary SQLite

`Format::Binary` is SQLite's standard file format. SQLite manages pages, indexes,
transactions, locks, recovery, and the `-wal` and `-shm` sidecars. BoxDB opens
binary databases in WAL mode with FULL synchronization, `fullfsync=ON` on macOS,
foreign keys enabled, a five-second busy timeout, and normal locking. Other
connections using the same engine may use the file concurrently. Native values
and their indexes require BoxDB's patched SQLite engine.

Do not copy only a live `.db` file while ignoring its WAL. Use SQLite's backup
API, `boxdb convert`, or [Litestream](replication.md). Binary databases are not
limited to the size of RAM; BoxDB materializes query results. Readable YAML
storage is limited to 256 MiB.

## Extended schema

The ordinary `__boxdb_columns` table records logical types, their version and
physical table declarations. Native types are validated by the engine. BoxDB
validates its metadata against the current schema and installs TEMP validators
for other logical types and native elements in typed arrays. Those
triggers also validate writes caused by SQL expressions, INSERT SELECT and user
triggers. Their errors abort the statement. BoxDB reserves `__boxdb_` names.
The `__boxdb_codec` table stores the database-wide BLOB codec version. Unknown
versions are rejected when opening or restoring a database.

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
| INT8, INT16, INT32 | INTEGER with a range CHECK |
| UINT8, UINT16, UINT32 | Native declarations; standard INTEGER cells in typed columns and indexes |
| UINT64 | Native declaration; `Q` tag followed by eight unsigned big-endian bytes |
| BOOLEAN | INTEGER with a 0/1 CHECK |
| VARCHAR(n) | TEXT with a length CHECK |
| ENUM('a','b',...) | TEXT with a membership CHECK and BINARY collation |
| DOUBLE | REAL with 64-bit floating-point values |
| BYTEA | BLOB with a storage-class CHECK |
| DECIMAL, UUID, INSTANT, DATE, DATETIME, INTERVAL | Single-byte type tag plus BLOB payload interpreted by the patched engine |
| JSON and typed arrays | JSON text; decimal array elements use strings |

Declare string enums inline:

```sql
CREATE TABLE tasks (
    id INT64 PRIMARY KEY,
    status ENUM('todo', 'doing', 'done') NOT NULL DEFAULT 'todo'
);
ALTER TABLE tasks ADD COLUMN priority ENUM('low', 'high') DEFAULT 'low';
```

An enum requires a nonempty list of unique SQL string literals. Labels preserve
case, whitespace and Unicode; an empty string is allowed. Clients bind and return
ordinary strings (`Value::Text` in Rust). NULL is allowed unless the column is
NOT NULL. SQLite's TEXT conversions apply before membership validation, so a
numeric input of `1` matches a declared label `'1'`.

Equality, uniqueness, sorting and indexes use BINARY string comparison; label
declaration order has no sorting significance. Other column collations are
rejected. Membership CHECKs are stored in the schema and apply to external
SQLite writers too, unless CHECK enforcement is explicitly disabled. Typed enum
arrays are supported, with each non-null element checked against the labels.
Enums follow the main-database restriction for logical types. SQL casts to ENUM
are rejected during preparation; named types and ALTER TYPE are unsupported.
Changing a label set requires rebuilding the table in a migration.

Unsigned columns and computed unsigned results return `Value::UnsignedInteger`;
signed values return `Value::Integer`. All four unsigned widths are native SQL
types. UINT8/16/32 column values use standard SQLite INTEGER serial types;
reading a column or its covering index restores its unsigned width. UINT64 uses
a typed BLOB for the full range from zero through 18446744073709551615.
Expressions, ANY columns and materialized results retain tagged values where
column declarations cannot reconstruct the type. Tags are `C`, `S`, `L`, and
`Q` for UINT8/16/32/64, with 1/2/4/8-byte big-endian payloads. Typed arrays
preserve unsigned values without passing through floating point.

Comparisons, ordering and indexes compare unsigned values numerically, including
mixed signed INTEGER, REAL and DECIMAL comparisons. Integer SQL literals above
INT64_MAX are UINT64 values; values above UINT64_MAX are rejected.
`Value::UnsignedInteger` parameters bind as UINT64. Use a SQL CAST to request a
narrower parameter width. Values fitting INT64 may also be stored in INTEGER
columns; larger unsigned values require UINT64 or ANY.

Unsigned `+`, `-`, `*`, `/` and `%` retain the widest unsigned operand's width.
Signed INTEGER operands participate exactly; a negative or out-of-range result
is an error. Division truncates, and division by zero is an error. Mixing DECIMAL
promotes the result to DECIMAL; arithmetic with REAL requires an explicit cast.
Bitwise AND/OR/NOT and logical shifts operate within the unsigned width. Left
shifts discard bits outside that width, and shifts of 64 or more produce zero.
Negative bitwise operands and shift counts are rejected.

SUM of unsigned inputs widens to UINT64, promoting to exact DECIMAL when its
range is exceeded. AVG returns DECIMAL and follows native decimal division
rules. Mixed signed or DECIMAL aggregates promote to DECIMAL. MIN/MAX retain
the selected value's type. Unsigned CAST accepts finite integral REAL values in range and rejects
fractional or overflowing values. CAST to INTEGER rejects values above INT64_MAX;
CAST to REAL explicitly permits floating-point rounding.

Typed parameter values are normalized before binding. Use `Value::Decimal` for
native exact arithmetic.
Native values retain their types in computed results, materialization and sorting.
Other logical values use column-origin metadata for typed projections.

The binary file retains SQLite's standard page and record encoding. Native
values use a one-byte type tag followed by their payload. Ordinary BLOBs always
use the BYTEA tag `b`, including empty values and payloads starting with a type
tag. No cell contains a signature or codec version. Readable YAML uses typed mappings such as
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
version: 3
codec_version: 1
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

`boxdb convert source.db destination.yaml --format readable` converts committed
data without modifying the source. Use `--format binary` for a self-contained
SQLite file, including committed WAL pages. The destination must be new.
The Rust equivalent is `Database::convert(source, destination, format)`.
Conversion validates logical types and foreign keys within one read transaction.
Binary-to-binary conversion uses SQLite's backup API without loading the entire
file into memory; readable storage executes in memory. Previous native BoxDB
binary files and JSONL layouts are unsupported.
