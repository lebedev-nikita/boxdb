# Change history: changesets and commit triggers

BoxDB can record which rows a transaction changed, store that record as a
`CHANGESET` value, and later apply it or its inverse. The mechanism is generic:
undo and redo, audit history, synchronization or a diff of what a migration
did are all application policies built on the same three pieces.

- A **changeset** holds the primary key, old values and new values of every
  changed row, in [SQLite's changeset format](https://sqlite.org/sessionintro.html).
  BoxDB records it in Rust from its own storage cells, so native types such as
  `DECIMAL`, `UUID` and `INSTANT` survive the round trip.
- A **commit trigger** runs SQL right before a transaction commits, with the
  recorded changeset available through `boxdb_changeset()`.
- `boxdb_apply()` applies a changeset, or its inverse, inside a transaction with
  a declarative conflict policy.

Recording and commit triggers cover tables in the `main` database that have a
PRIMARY KEY. Tables without a primary key, TEMP tables and attached databases
are not recorded. Guarantees apply to writes made through BoxDB connections:
another SQLite client writing the same file bypasses recording, as it bypasses
other connection-level validation.

## Declare a commit trigger

```sql
CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT NOT NULL);
CREATE TABLE history(
    id INT64 PRIMARY KEY,
    label TEXT,
    at INSTANT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    changes CHANGESET NOT NULL
);
CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
    INSERT INTO history(label, changes)
    VALUES(boxdb_context('label'), boxdb_changeset());
END;
```

`BEFORE COMMIT` is the only supported timing. The optional `ON` list names the
main tables whose changes the trigger sees; without it the trigger covers every
main table. The body runs inside the committing transaction, in declaration
order with other commit triggers, and only when its changeset is not empty.
Read-only transactions and transactions that only touch other tables leave no
trace. Writes made by the body are not recorded. If the body fails, the commit
fails with the body's error code and the transaction stays open for repair or
rollback.

Every committing transaction runs the trigger: explicit `COMMIT`, the `RELEASE`
of an outermost savepoint, and single statements executed in autocommit mode.
A rolled-back transaction runs nothing.

Commit triggers are BoxDB objects stored in `__boxdb_commit_triggers`, not in
`sqlite_schema`, so the file remains readable by standard SQLite tooling.
`DROP TRIGGER name` removes one. Renaming a listed table updates the trigger.
Schema dumps and readable YAML storage include commit triggers. Other BoxDB
connections pick up a new declaration with their next write.

## Context and recording functions

| Function | Use |
| --- | --- |
| `boxdb_set_context(key, value)` | Store a value for the current transaction; read it with `boxdb_context(key)`. Requires a transaction. |
| `boxdb_context(key)` | The stored value, or NULL. Context is cleared when the transaction ends. |
| `boxdb_changeset()` | Inside a commit trigger body: the trigger's changeset. Elsewhere: the changes recorded so far in the transaction, or NULL when nothing changed. |
| `boxdb_record(1)` | Record changes in a transaction without a commit trigger, so `boxdb_changeset()` can be called before commit. |
| `boxdb_record(0)` | Stop recording and skip commit triggers for the current transaction, for example during a bulk migration. |
| `boxdb_apply(changeset, options)` | Apply a changeset; see below. |
| `changeset_invert(changeset)` | The changeset that undoes its argument. Patchsets cannot be inverted. |
| `changeset_concat(first, second)` | One changeset equivalent to applying both in order; changes to the same row are merged. |
| `changeset_rows(changeset)` | A table-valued function listing changed fields. |

Recording keeps each touched row's original values in memory until the
transaction ends. A transaction that rewrites millions of rows while commit
triggers exist builds a changeset of the same size; call `boxdb_record(0)`
first when that history is not wanted.

```sql
BEGIN;
SELECT boxdb_set_context('label', 'Rename task');
UPDATE tasks SET title = 'New title' WHERE id = 3;
COMMIT;
```

## Inspect a changeset

`changeset_rows` returns one row per changed field: `seq` (the change's index),
`table`, `operation` (`insert`, `update` or `delete`), `position` (the column's
declaration index), `column_name` (resolved from the current schema when the
table still has at least as many columns, otherwise NULL), `primary_key`,
`old` and `new`. Unchanged columns of an update are omitted.

```sql
SELECT seq, "table", operation, column_name, old, new
FROM changeset_rows((SELECT changes FROM history WHERE id = 2))
ORDER BY seq, position;
```

Values keep their storage cells, so a `DECIMAL` field is an exact decimal and
an `INSTANT` field a UTC instant. Without column metadata they appear in
normalized form, for example `1.5` for a `DECIMAL(12,2)` value stored as `1.50`.

## Apply and undo

`boxdb_apply(changeset, options)` requires an active transaction and returns a
JSON report. `options` is a JSON object, or NULL for defaults:

```json
{ "invert": true, "on_conflict": "abort" }
```

- `invert` applies the inverse, undoing the recorded changes.
- `on_conflict` is `abort` (default), `skip` or `replace`.

Each change is checked against the current row before it is written. A row whose
recorded old values no longer match is a `data` conflict; a missing row to
update or delete is `not_found`; an existing row with an inserted key is
`conflict`; a write SQLite rejects is `constraint`. With `abort` the first
conflict fails the whole apply and nothing is written. With `skip` the
conflicting change is omitted and listed in the report. With `replace` data and
insert conflicts are written anyway, while missing rows and constraint failures
are still omitted. Foreign keys are checked when the transaction commits.

```sql
BEGIN;
SELECT boxdb_apply((SELECT changes FROM history WHERE id = 2), '{"invert": true}');
COMMIT;
```

The report is `{"applied": 1, "skipped": 0, "conflicts": []}`; each conflict
carries `table`, `operation`, `kind` and the row's primary `key`. Conflicts
under `abort` surface as `BOXDB_CONSTRAINT_FUNCTION`; a constraint failure
inside the apply keeps its own code, such as `BOXDB_CONSTRAINT_UNIQUE`.

Applying runs through the same recording as any other write. With the trigger
above, an undo therefore adds its own history row, and undoing that row redoes
the change. History is never rewritten.

## Client APIs

TypeScript transactions accept `context` and `record` options and expose
`changeset()` and `apply()`; `CHANGESET` columns return `Changeset` instances
and `db.apply()` runs an apply in its own transaction. See the
[TypeScript guide](../packages/typescript/README.md#change-history).
Rust exposes `Database::apply_changeset` and the `boxdb::changeset` module for
decoding; Go reads `CHANGESET` columns into `boxdb.Changeset` and calls the SQL
functions through `database/sql`. See the [Rust](rust.md) and
[Go](../packages/go/README.md#change-history) guides.

## Storage

A `CHANGESET` column stores validated changeset bytes as a BLOB; invalid bytes
are rejected on write. Readable YAML storage writes each changeset as a
structured document rather than hexadecimal bytes:

```yaml
  changes:
    changeset:
      - table: tasks
        columns: 2
        key:
        - 0
        op: update
        old:
          0: 3
          1: Old title
        new:
          1: New title
```

Fields are keyed by column position because the changeset format records
positions, not names. Values use the same typed mappings as other YAML cells.

A changeset applies only to a table with the same primary key positions and at
least as many columns as were recorded. Dropping or reordering columns makes
older history for that table unusable; adding columns does not.
