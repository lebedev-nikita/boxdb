# BoxDB CLI

The BoxDB command line database, with typed SQLite columns, SQL migrations and
optional readable YAML storage.

```sh
npm install -g @boxdb/cli
boxdb data.db -e "CREATE TABLE users(name TEXT); INSERT INTO users VALUES('Ada'); SELECT * FROM users;"
```

Or run without a global installation:

```sh
npx @boxdb/cli --help
```

Requires Node.js 22.18+ on macOS ARM64 (macOS 11+) or Linux x64 or ARM64
with glibc 2.35+ (Ubuntu 22.04+ or 64-bit Raspberry Pi OS Bookworm+).
All three binaries are included in the npm package; no Rust compiler,
installation scripts or additional downloads are needed. Alpine/musl, Windows,
Intel macOS and 32-bit ARM are not supported by this npm release.

For Raspberry Pi without Node.js, download the CLI archive from
[GitHub releases](https://github.com/lebedev-nikita/boxdb/releases). Choose
`aarch64-unknown-linux-gnu` for a 64-bit Raspberry Pi OS, extract it and run
`./boxdb`. It requires glibc 2.35+.

Run `boxdb data.db` for the interactive shell, or pipe SQL on stdin. `--help`
lists options and subcommands; `.help` lists shell commands.

Use `boxdb check data.db --full` for diagnostics. `boxdb repair data.db repaired.db
--rebuild-indexes` rebuilds indexes, and `boxdb recover damaged.db recovered.db`
salvages available data. Both write new destinations. See
[recovery contracts and limitations](../../docs/recovery.md).

See the [BoxDB documentation](https://github.com/lebedev-nikita/boxdb#readme)
for storage formats, supported types, migrations and language clients.
