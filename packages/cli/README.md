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

Requires Node.js 22.18+ on macOS ARM64 (macOS 11+) or Linux x64 with glibc 2.35+
(Ubuntu 22.04+). Both binaries are included in the npm package; no Rust compiler,
installation scripts or additional downloads are needed. Alpine/musl, Windows,
Intel macOS and Linux ARM64 are not supported by this npm release.

Run `boxdb data.db` for the interactive shell, or pipe SQL on stdin. `--help`
lists options and subcommands; `.help` lists shell commands.

See the [BoxDB documentation](https://github.com/lebedev-nikita/boxdb#readme)
for storage formats, supported types, migrations and language clients.
