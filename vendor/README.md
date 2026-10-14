# Extension sources

The build compiles these sources with `SQLITE_CORE` against the bundled SQLite
headers. Builds do not download extension sources.

- `sqlite/decimal.c`: SQLite 3.53.2, commit
  [`ccc132c5be20ab5c755c97a08d06b1b592fef330`](https://github.com/sqlite/sqlite/blob/ccc132c5be20ab5c755c97a08d06b1b592fef330/ext/misc/decimal.c).
  The file carries SQLite's public-domain notice.
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
