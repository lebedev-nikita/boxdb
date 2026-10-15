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

Embedded applications use `boxdb::migrations::MigrationSet`: read a directory,
then call `up`, `down` or `status` against a `Database`. `status_file` inspects a
file without opening a writer. Migration execution requires an idle connection.

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

YAML uses the existing readable database stream as a schema-only snapshot, with
named fields for metadata rows. Restore it through conversion into a new path:

```sh
boxdb convert db/schema.yaml restored.db --format binary
```

Keep the original migration files when continuing migrations on a restored
schema. These dumps bootstrap an empty database; use `boxdb convert` for a
complete database with data. The library exposes
`Database::dump_schema` and `Database::dump_schema_file` with `SchemaFormat`.

## Why stock DBMate is insufficient

DBMate's [SQLite driver](https://github.com/amacneil/dbmate/blob/main/pkg/driver/sqlite/sqlite.go)
executes SQL through ordinary SQLite, bypassing BoxDB's type translation and
metadata updates. Its dump uses SQLite's
[`.schema` command](https://sqlite.org/cli.html#querying_the_database_schema),
which omits the metadata rows needed to recover logical types. BoxDB reuses its
SQL section markers, but executes migrations through its own typed connection.
The shell's existing `.schema` command remains physical schema inspection.
