# Migrations and schema export

## SQL migrations

```sh
boxdb migrate new create_accounts
# Edit the SQL file printed by new, then:
boxdb migrate up data.db
boxdb migrate status data.db
boxdb migrate down data.db
```

The default directory is `db/migrations`; use `--dir` to choose another one.
`new` creates a template with a unique, ordered 14-digit UTC version and the
provided name. For example, its contents can be:

```sql
-- migrate:up
CREATE TABLE accounts (
    id INT64 PRIMARY KEY,
    name TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at INSTANT NOT NULL
);

-- migrate:down
DROP TABLE accounts;
```

`up` creates the database if needed and applies pending files in version order.
Use `--format readable` to create YAML storage; existing files are detected
automatically. Each version commits its SQL, logical type metadata and tracking
record together. A failed version rolls back, while previous versions stay
applied. Binary writers use SQLite locking and its five-second busy timeout;
readable files retain their single-writer lock and report contention.
After a readable publication failure, reopen the database before retrying.

`down` rolls back exactly one version. An empty history is a successful no-op.
An empty down section marks a migration as irreversible: attempting its rollback
fails without changing the database. Up sections must contain SQL; use
`SELECT 1;` for an intentional no-op. Both section markers are required.

Do not edit, rename or remove applied files. BoxDB checks their names and SHA-256
checksums, with CRLF line endings normalized to LF. Duplicate versions and new
versions preceding an applied version are rejected before pending SQL runs.
Transaction control, PRAGMA, VACUUM, ATTACH and DETACH are not allowed in either
section; BoxDB owns the connection and transaction. DBMate directive options
such as `transaction:false` are unsupported.

For scripts and CI:

```sh
boxdb migrate status data.db --json --check
boxdb migrate up data.db --json
boxdb migrate down data.db --json
```

`status` inspects committed state without modifying the file or taking a readable
writer lock. Status and down require an existing database. `--check` returns
exit status 1 when migrations are pending. Successful operations return 0;
operation errors return 1, and invalid CLI arguments return 2. JSON output is an
array: status entries have `version`, `name` and `state` (`applied` or `pending`);
up/down entries contain the versions and names changed by that invocation.
Diagnostics go to stderr. See `boxdb migrate --help` and each command's help.

## Migrations from application code

All clients use the same Rust migration runner and `__boxdb_migrations` history
as the CLI. Keep the complete set, including applied versions, so every entry
can be verified. A set is immutable after construction and can be reused with
different databases. Files and embedded documents have identical checksum rules.

### TypeScript

```ts
import { BoxDB, MigrationSet } from "@boxdb/client";

await using db = await BoxDB.open("data.db");
const migrations = await MigrationSet.read("db/migrations");
const applied = await migrations.up(db); // [{ version, name }]
const statuses = await migrations.status(db); // [{ version, name, state }]
const reverted = await migrations.down(db); // { version, name } or null
```

For bundled SQL, use `MigrationSet.fromFiles([{ filename, sql }])`; `filename`
is a bare `VERSION_NAME.sql` name and `sql` is the full document with both
section markers. Construction validates all documents before returning.
Directory reads are asynchronous and ignore entries without a `.sql` extension.

Execution methods accept `{ signal }` and require a root `BoxDB`, rather than a
transaction handle. Each call occupies one queue operation and waits for any
active transaction; finish the transaction before awaiting root migration work.
SQL and lock waits run on the native worker. Ordinary requests on the same
connection wait until the migration call, including rollback cleanup, finishes.
The usual queue limits and draining/aborting close behavior apply.

### Go

```go
migrations, err := boxdb.ReadMigrations(os.DirFS("."), "db/migrations")
if err != nil { return err }
applied, err := migrations.Up(ctx, db)
if err != nil { return err }
_ = applied // []boxdb.MigrationInfo
```

`ReadMigrations` accepts any `fs.FS`, including `embed.FS`.
`NewMigrationSet([]boxdb.MigrationFile{{Filename: "20261015000000_create_accounts.sql", SQL: sql}})`
validates and copies documents from memory. `Status(ctx, db)` returns
`[]MigrationStatus`; `Down(ctx, db)` returns `*MigrationInfo`, or nil for an empty
history. Methods accept a BoxDB `*sql.DB` and pin one connection for the complete
call. Contexts cancel waiting for the pool, writer locks and SQL execution.

### Rust

```rust
use boxdb::migrations::{MigrationFile, MigrationSet};

let migrations = MigrationSet::from_files([MigrationFile {
    filename: "20261015000000_create_accounts.sql".into(),
    sql: include_str!("../db/migrations/20261015000000_create_accounts.sql").into(),
}])?;
let applied = migrations.up(&mut db)?;
```

`MigrationSet::read(directory)` loads files instead. Call `up`, `down` or `status`
against an idle `Database`; `status_file` inspects a file without opening a writer.

An error or cancellation rolls back the active version; earlier committed
versions remain applied. Inspect `status` and retry `up` to continue. Cancellation
cannot undo a completed commit. Cleanup does not use the cancelled signal or
context. Errors retain BoxDB codes and engine details, with migration version and
direction added to execution diagnostics. Reopen after a readable publication
failure, as with other writes. Drizzle's migration helper retains its own format
and journal; these methods operate on BoxDB SQL migrations.

## Schema-only export

```sh
boxdb schema dump data.db > db/schema.sql
boxdb schema dump data.db --format=sql > db/schema.sql
boxdb schema dump data.db --format=yaml > db/schema.yaml
```

SQL is the default. Both formats retain tables, indexes, views, triggers,
constraints, logical type metadata and applied migration records. Application
rows and AUTOINCREMENT sequence values are excluded. Dumping reads a consistent
committed snapshot without modifying the source; virtual tables are unsupported.

SQL contains standard physical SQLite declarations plus INSERT statements for
BoxDB's type and migration metadata. Restore it through an ordinary SQLite
client into a new database:

```sh
sqlite3 restored.db < db/schema.sql
boxdb restored.db -e 'SELECT * FROM accounts'
```

YAML exports a single `boxdb-schema` document with logical BoxDB declarations:

```yaml
format: boxdb-schema
version: 1
tables:
  accounts:
    columns:
      id: INTEGER PRIMARY KEY
      email: TEXT NOT NULL
      active: BOOLEAN NOT NULL DEFAULT TRUE
      status: ENUM('pending', 'active', 'disabled') NOT NULL DEFAULT 'pending'
      created_at: INSTANT NOT NULL
    constraints:
      - check: length(email) > 0
indexes:
  accounts_active_email:
    type: unique
    table: accounts
    expressions:
      - text_lower(email)
    where: active = TRUE
```

All tables are STRICT; YAML schema export rejects non-strict tables. Column
mapping order preserves SQL column order. Objects are sorted by name, and
empty optional sections are omitted. Index `type` is `regular` or `unique`;
`expressions` can include `COLLATE`, `ASC` and `DESC`.
Views contain a SQL `query`; triggers contain `table`, `timing`, `event`, optional
`update_of`/`when`, and a SQL `body` without its enclosing BEGIN/END.

Nonzero `application_id` and `user_version` appear in `pragmas`. Applied
`migrations` are an ordered list of records with `version`, `name` and `checksum`.
Internal tables, generated type checks and physical codec metadata are omitted.
See the [complete example](../tests/fixtures/schema.yaml)
for logical types, generated columns, views, triggers and migration records.

YAML schema dumps are export-only documents for inspection and version-control
diffs. They cannot be opened as databases or imported through `boxdb convert`.
The readable database stream is a separate format containing schemas and rows;
`boxdb convert` converts complete databases between binary and readable storage.
SQL dumps can bootstrap an empty database; keep the original migration files
when continuing migrations on a database restored from SQL. The library exposes
`Database::dump_schema` and `Database::dump_schema_file` with `SchemaFormat`.

## Why stock DBMate is insufficient

DBMate's [SQLite driver](https://github.com/amacneil/dbmate/blob/main/pkg/driver/sqlite/sqlite.go)
executes SQL through ordinary SQLite, bypassing BoxDB's type translation and
metadata updates. Its dump uses SQLite's
[`.schema` command](https://sqlite.org/cli.html#querying_the_database_schema),
which omits the metadata rows needed to recover logical types. BoxDB reuses its
SQL section markers, but executes migrations through its own typed connection.
The shell's existing `.schema` command remains physical schema inspection.
