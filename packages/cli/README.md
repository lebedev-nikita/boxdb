# BoxDB CLI

The BoxDB command line database, with typed SQLite columns, SQL migrations and
optional readable YAML storage.

Install with [Homebrew](https://brew.sh/) on macOS or Linux:

```sh
brew install lebedev-nikita/tap/boxdb
```

Homebrew installs a standalone binary without Node.js. Apple Silicon macOS and
x86-64 Linux use release binaries; Intel macOS and ARM64 Linux build from source
with Rust and CMake. See the [tap](https://github.com/lebedev-nikita/homebrew-tap#boxdb)
for release updates. Upgrade with `brew update && brew upgrade boxdb`.

Alternatively, install through npm:

```sh
npm install -g @boxdb/cli
```

Then run:

```sh
boxdb data.db -e "CREATE TABLE users(name TEXT); INSERT INTO users VALUES('Ada'); SELECT * FROM users;"
```

Or run without a global installation:

```sh
npx @boxdb/cli --help
```

The npm package requires Node.js 22.18+ on macOS ARM64 (macOS 11+) or Linux x64 or ARM64
with glibc 2.35+ (Ubuntu 22.04+ or 64-bit Raspberry Pi OS Bookworm+).
All three binaries are included in the npm package; no Rust compiler,
installation scripts or additional downloads are needed. Alpine/musl, Windows,
Intel macOS and 32-bit ARM are not supported by this npm release.

To switch from an existing global installation, first install and verify Homebrew's
binary with `"$(brew --prefix)/bin/boxdb" --version`. Then remove the old package
using its original package manager:

```sh
npm uninstall -g @boxdb/cli
# Or, for a Bun global installation:
bun remove -g @boxdb/cli
```

Open a new shell or run `rehash` in zsh (`hash -r` in bash). Check
`command -v boxdb` and `boxdb --version`; the command should resolve through
Homebrew's `bin` directory.

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
