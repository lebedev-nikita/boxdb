# Diagnose and recover a database

Keep the source and its SQLite sidecars together. Stop application writers before
recovery and write the result to a new path. Neither repair nor recovery replaces
the source or overwrites an existing destination. Existing destination `-wal`,
`-shm` and `-journal` files also prevent publication.

## Check an existing file

```sh
boxdb check data.db --quick
boxdb check data.db --full --json
```

Checks never create a missing database or initialize BoxDB metadata. Binary
checks use a read-only connection and one read transaction, including committed
WAL contents. They do not change the journal mode or checkpoint the database;
SQLite may manage shared-memory coordination files for a WAL reader.

Quick mode checks SQLite structure and BoxDB metadata. Full mode additionally
checks index consistency, foreign keys, logical column values and BLOB encodings
in ordinary main-database tables. YAML must be parsed and restored even in quick
mode. JSON is validated as an opaque document; no SQL JSON processing is used.

A report has `status: valid | invalid | incomplete`, a `complete` flag, performed
checks and issues. `valid` means the performed checks found no problems, not that
every original value is correct. A changed but still valid value can go
undetected. `complete: false` means validation stopped or some checks could not
run, including when metadata is unusable. Locked or inaccessible database pages
are not classified as proven corruption. The CLI exits unsuccessfully when the
check does not pass. With `--json`, reports go to stdout and errors to stderr.

Issues identify tables, columns, rowids and YAML document/line locations when
available. Serialized rowids are decimal strings to preserve 64-bit precision.
Value errors do not include cell contents. SQLite structural diagnostics retain
the engine's message. Reports are bounded to 1,000 value/foreign-key issues;
SQLite's structural check has its own error limit.

## Rebuild indexes

```sh
boxdb repair data.db repaired.db --rebuild-indexes --json
```

Repair copies readable table data through BoxDB's engine and rebuilds indexes in
a new binary database. It preserves accessible rowids, logical metadata,
AUTOINCREMENT sequences, views and triggers, and recomputes generated columns.
Application triggers are created after loading rows. Virtual tables are rejected.
Unusable schemas, unreadable table data or an invalid final database abort repair.
This operation does not need the source indexes to be readable.

## Salvage available data

```sh
boxdb recover damaged.db recovered.db --json
boxdb recover damaged.yaml recovered.yaml --format readable --json
```

Binary recovery uses SQLite's recovery extension with BoxDB's native codec and
bundled functions. It excludes freelist pages to reduce resurrection of deleted
records. Records that cannot be assigned to a table may appear in `lost_and_found`.
Recovered content can be missing or altered; virtual-table content and damaged
schema objects are not guaranteed to survive. See SQLite's
[recovery limitations](https://sqlite.org/recovery.html).

YAML recovery reads independently delimited documents, requires a valid header,
and restores the available schema and metadata without inferring missing types.
It skips malformed or rejected row documents and creates application triggers
after rows. Rejected fragments are retained verbatim in `<destination>.rejected`,
with document numbers and source line numbers. This file contains source data;
the report only lists locations and reasons. Damage to document boundaries or
required schema/metadata can prevent recovery. Orphaned rows are retained and
reported by the final foreign-key check.

Every recovery returns a full check of its output and `loss_possible: true`, even
if that check passes. `recovered_rows` counts ordinary application rows, including
`lost_and_found`; it cannot establish how many rows were lost. A binary result
that fails checks is retained for further inspection and the CLI exits with an
error. Readable output is published only if validation and conversion succeed.
Review rejected documents, warnings and application invariants before use.

## Application interfaces

Rust uses the same operations:

```rust,no_run
use boxdb::{CheckMode, Database, Format};

let check = Database::check_file("data.db", CheckMode::Full)?;
let repair = Database::repair_file("data.db", "repaired.db")?;
let salvage = Database::recover_file("damaged.db", "recovered.db", Format::Binary)?;
# Ok::<(), boxdb::Error>(())
```

TypeScript exposes `BoxDB.checkFile(path, "full")`,
`BoxDB.repairFile(source, destination)` and
`BoxDB.recoverFile(source, destination, "binary")` as promises. These file
operations run on native workers independently of open client connections.
Go exposes `CheckFile(path, CheckFull)`, `RepairFile(source, destination)` and
`RecoverFile(source, destination, Binary)` as blocking functions returning a
report and an error. File maintenance has no per-call cancellation option.

For all clients, a returned report means the operation ran; inspect `status` and
`complete` before treating the file as valid. For example:

```ts
import { BoxDB } from "@boxdb/client";

const report = await BoxDB.checkFile("data.db", "full");
if (report.status !== "valid" || !report.complete) {
  console.error(report.issues);
}
```

Read [operation state and unusable connections](client-errors.md#operation-state-and-unusable-connections)
before implementing retries or pool recovery. A failed write can have an unknown
outcome; reconnecting does not establish whether it was applied.

For recovery from an intact external copy, use
[Litestream restoration](replication.md), then run `boxdb check restored.db --full`.
BoxDB does not manage backup scheduling, remote storage or retention. Normal
database opening retains its create-if-missing behavior.
