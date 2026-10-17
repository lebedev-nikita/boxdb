# Extension sources

The build compiles these sources with `SQLITE_CORE` against the bundled SQLite
headers. Builds do not download extension sources.

- `sqlean/text` and `sqlean/regexp`: sqlean 0.28.4, commit
  [`17f8f14bdaaea86012420224e179eabdb53a90b7`](https://github.com/nalgeon/sqlean/tree/17f8f14bdaaea86012420224e179eabdb53a90b7/src).
  BoxDB omits the `concat`, `concat_ws`, `ltrim`, `rtrim` and `octet_length`
  registrations in `text/extension.c` so SQLite retains its built-in functions.
  Their `text_*` registrations and C implementations remain available. Other
  sources are unchanged. See `sqlean/LICENSE`; PCRE2's license is in
  `sqlean/regexp/pcre2/LICENSE`.
- sqlite-vec is compiled by the pinned Cargo dependency. `sqlite-vec/` contains
  its [0.1.9 license files](https://github.com/asg017/sqlite-vec/tree/v0.1.9),
  included in the TypeScript package alongside the other extension licenses.

The patched SQLite engine is vendored in `libsqlite3-sys/`; see its
[build and source notes](libsqlite3-sys/README.md).

`sqlite-recovery/` contains `sqlite3recover.c`, `sqlite3recover.h` and `dbdata.c`
from the official [SQLite 3.53.2 source archive](https://sqlite.org/2026/sqlite-src-3530200.zip),
under SQLite's public-domain blessing in each file. `dbdata.c` returns physical
BLOB cells through BoxDB's wire transport, preserving native tags and BYTEA.
`sqlite3recover.c` configures its private output connection with BoxDB's bundled
functions and foreign-key checking disabled during salvage. The engine enables
`SQLITE_ENABLE_DBPAGE_VTAB` for recovery. Page and record encodings are unchanged.
