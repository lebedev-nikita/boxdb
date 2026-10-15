Use TDD.

Package publishing scripts are exempt from TDD. Do not add or maintain automated
tests for package publishing scripts.

This library is in development. Backward compatibility is not required; update
APIs, storage contracts, and callers directly without compatibility layers.
Keep SQLite's standard binary page and record encoding.

Keep JSON as opaque validated documents. SQL JSON/JSONB inspection and editing
are out of scope.
