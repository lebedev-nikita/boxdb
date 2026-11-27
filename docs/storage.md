# Storage and extended types

## Binary SQLite

`Format::Binary` is SQLite's standard file format. SQLite manages pages, indexes,
transactions, locks, recovery, and the `-wal` and `-shm` sidecars. BoxDB opens
binary databases in WAL mode with FULL synchronization, `fullfsync=ON` on macOS,
foreign keys enabled, a five-second busy timeout, and normal locking. Other
connections using the same engine may use the file concurrently. Native values
and their indexes require BoxDB's patched SQLite engine.

Standalone binary writes acquire SQLite's writer before refreshing type
validators, so concurrent schema changes cannot bypass validation. Validators
are reused while the schema is unchanged. Explicit transactions retain their
requested mode; upgrading an outdated deferred WAL snapshot to a writer can
return `SQLITE_BUSY_SNAPSHOT`. BoxDB does not replay user transactions.

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
READONLY columns are enforced by ordinary schema triggers named `__boxdb_readonly_*`,
so every SQLite connection rejects their updates. BoxDB recreates them after its
DDL and rejects databases whose READONLY triggers changed outside BoxDB with
`BOXDB_METADATA`. Schema dumps show `READONLY` on the column instead.
The `__boxdb_codec` table stores the database-wide BLOB codec version. Unknown
versions are rejected when opening or restoring a database.

Ordinary CREATE TABLE statements are STRICT by default, including TEMP tables,
WITHOUT ROWID and CREATE TABLE AS SELECT. An explicit STRICT suffix is accepted,
including either ordering with WITHOUT ROWID. Standard types follow SQLite's
[STRICT conversion rules](https://www.sqlite.org/stricttables.html): lossless
conversions such as text `'42'` into an integer are allowed; incompatible values
are rejected. Every column declaration requires an explicit type, including
generated columns and ALTER TABLE additions. Use explicit ANY to preserve each
value's storage class. Integer declarations accept SQLite INTEGER or an
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
| DECIMAL, UUID, INSTANT, DATE, TIME, DATETIME, INTERVAL | Single-byte type tag plus BLOB payload interpreted by the patched engine |
| JSON and typed arrays | JSON text; decimal array elements use strings |

TIME uses the `h` tag followed by eight unsigned big-endian bytes counting
microseconds since midnight, from 0 through 86,399,999,999.

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
tag. No cell contains a signature or codec version. Readable YAML writes typed
columns as canonical text, such as `at: 2024-02-29T12:00:00.000000Z` and
`amount: 12.5`; ANY columns use typed mappings such as `{decimal: 12.5}`.
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

## Readable YAML log

[The readable codec](../src/readable.rs) stores a database as a log: a header
line followed by one line per committed transaction. Opening a file applies the
lines in order to an in-memory SQLite database. The file never changes
existing lines; each transaction appends one line and synchronizes the file:

```yaml
- {boxdb: 5}
- [{schema: ["CREATE TABLE notes (id INTEGER PRIMARY KEY,body TEXT,done INTEGER NOT NULL DEFAULT false CHECK (\"done\" IN (0, 1))) STRICT", "CREATE TABLE __boxdb_codec(...)"]}, {insert: __boxdb_codec, rows: [{id: 1, version: 1}]}]
- {insert: notes, rows: [{id: 1, body: buy milk, done: false}, {id: 2, body: call mom, done: false}]}
- [{update: notes, key: {id: 1}, set: {body: buy oat milk}, was: {body: buy milk}}, {delete: notes, rows: [{id: 2, body: call mom, done: false}]}]
- {pragma: {user_version: 2}, was: {user_version: 1}}
```

A line is either one step or a sequence of steps:

- `schema` lists SQL statements as SQLite stores them, including BoxDB's
  translation of logical types. Rows of BoxDB metadata tables are ordinary
  steps, so opening a file does not translate SQL again. A table created by
  the transaction, including `CREATE TABLE ... AS SELECT`, is followed by an
  `insert` of its rows.
- `insert` lists complete rows. Cells a row does not mention take the column
  default, so a NULL cell is written only when the default is not NULL.
- `update` names a row by `key`, the changed values in `set` and their
  previous values in `was`.
- `delete` lists the complete rows it removes.
- `pragma` sets `user_version` or `application_id`; `sequences` sets
  AUTOINCREMENT values that the inserted rows would not restore. Other tables
  SQLite maintains itself, such as ANALYZE statistics, are not stored.

A line holds the net change of its transaction: a row inserted and deleted
again is not written, and rows changed by triggers, REPLACE and foreign-key
actions appear as their results. Rolled-back transactions and savepoints are
not written. Opening a file does not run triggers or foreign-key actions; it
checks foreign keys and typed values once all lines are applied.

Rows are identified by their INTEGER PRIMARY KEY, otherwise by their declared
PRIMARY KEY. The hidden rowid of a table with a declared key is not stored and
can change when the file is opened again, which can change the order of rows a
query returns without ORDER BY. Tables without a key store the rowid
as `rowid`, or `_rowid_` or `oid` when a column shadows it; tables without an
accessible rowid identify rows by all their values.

BOOLEAN and BOOL columns use YAML `true`/`false`. INTEGER and ANY columns retain
numeric 0/1. Columns of a native type (DECIMAL, UUID, UINT64, INSTANT, DATE,
TIME, DATETIME and INTERVAL) hold canonical text such as `amount: 12.5` or
`at: 2024-02-29T12:00:00.000000Z`; BYTEA columns hold base64 such as
`photo: AP8=`. These values are read as text through the column type, so `12.50`
is not rounded through floating point. Opening rejects a value the type does
not accept, such as `true`, a mapping or malformed base64. Writing fails with
`BOXDB_INVALID_VALUE` when a typed column holds a cell of another type.

Other cells use YAML numbers, `null` and strings, which stay on one line:
line breaks are written as `\n` escapes. ANY and untyped columns keep explicit
mappings for BLOBs and native values, such as `{blob: AP8=}` and
`{decimal: 1.25}`; boolean scalars are read as SQLite integers 0/1. Generated
columns are recomputed. JSON documents remain opaque text.

The file is a YAML sequence that ordinary YAML tools read. BoxDB reads each
entry on its own line with a flow-style subset of YAML: mappings, sequences,
plain and quoted scalars, and comments. Anchors, tags and entries spanning
several lines are rejected with `BOXDB_YAML` and their line number. Large files
are parsed on several threads.

### Merging branches

Because a transaction is one line that only appends, two branches of a file
merge by keeping the lines of both. Configure Git to do so for the file:

```gitattributes
data.yaml merge=union
```

Without this attribute Git reports a conflict where both branches appended
lines. Resolve it by keeping the lines of both sides and removing the conflict
markers; a file that still contains markers fails to open with `BOXDB_YAML`.

Opening the merged file applies the base, then the lines of each branch. Each
step checks the state it was written against: an `update` requires its `was`
values, a `delete` requires the complete row, and an `insert` requires a free
key. Changes to different rows or different columns of a row are both applied.
A change that does not apply, such as two updates of the same column, an update
of a deleted row, two inserts with the same key or a value for a dropped column,
fails with `BOXDB_CONFLICT` and the line number. Resolve it by removing or
editing that line. Rows written before another branch added a column receive
the column default. Tables without a declared key identify rows by rowid, so
rows inserted on both branches conflict.

### Compaction

VACUUM rewrites the file as a canonical log of the current content: the
schema, one line per row in key order, indexes, views and triggers. The same
log is written by `boxdb convert`. A file grows with every transaction and
opening it replays the whole history, so compact a file whose history is much
larger than its data. VACUUM also renumbers the rowids of tables without a
key, as it does in SQLite. Compacting on one branch while another branch
appends lines makes their merge fail with conflicts; compact after merging.

### Interrupted writes

A crash during an append can leave an incomplete last line. Readers ignore a
final line without a line break unless it is a complete entry, and a last line
containing NUL bytes. `boxdb check` reports it as `BOXDB_INCOMPLETE_COMMIT`; the
next connection that opens the file for writing removes it. Damage before the
last line is an error.

A readable file holds a separate `.lock` sidecar until closed. On a publication
error, reopen the connection before continuing. Readable storage does not
support virtual tables or non-finite floating-point values, and files are
limited to 256 MiB.

Readable files of BoxDB 0.8 and earlier are rejected with `BOXDB_FORMAT`.
Migrate one through the binary format, which is SQLite's own:

```sh
boxdb convert data.yaml data.db --format binary          # BoxDB 0.8
boxdb convert data.db data-v5.yaml --format readable     # current BoxDB
```

`boxdb convert source.db destination.yaml --format readable` converts committed
data without modifying the source. Use `--format binary` for a self-contained
SQLite file, including committed WAL pages. The destination must be new.
The Rust equivalent is `Database::convert(source, destination, format)`.
Conversion validates logical types and foreign keys within one read transaction.
Binary-to-binary conversion uses SQLite's backup API without loading the entire
file into memory; readable storage executes in memory. Previous native BoxDB
binary files and JSONL layouts are unsupported.
