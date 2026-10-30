Use TDD.

Package publishing scripts are exempt from TDD. Do not add or maintain automated
tests for package publishing scripts.

This library is in development. Backward compatibility is not required; update
APIs, storage contracts, and callers directly without compatibility layers.
Keep SQLite's standard binary page and record encoding.

Keep JSON as opaque validated documents. SQL JSON/JSONB inspection and editing
are out of scope.

Keep user documentation and the BoxDB usage skill
(`.agents/skills/boxdb/SKILL.md`) in sync with public API and behavior changes.

Mechanism over policy: add a core capability only when it cannot be built on
the public API, and keep application policy (undo stacks, conflict semantics,
history layouts) in user code or optional packages.

Patch the SQLite engine only at value, expression and record boundaries through
the checked anchors in `vendor/libsqlite3-sys/boxdb.rs`. Transactions, metadata,
SQL preprocessing and hooks live in Rust.

Keep `sqlite_schema` standard SQLite SQL. BoxDB-only objects such as commit
triggers live in `__boxdb_*` metadata tables, parsed and executed by Rust.

BoxDB guarantees hold for writes through BoxDB connections; external SQLite
writers bypass validation and change recording. Logical metadata features cover
the `main` database.

Register every consumer of SQLite's single preupdate hook in
`history::install_hook`; the Session extension replaces that hook and is not
used.
