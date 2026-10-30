# Contributing to BoxDB

BoxDB is experimental. Small, focused changes and reproducible bug reports are
welcome. For a large feature or a change to SQL/storage semantics, open an issue
first so the intended behavior can be discussed.

## Report a problem

[Choose an issue template](https://github.com/lebedev-nikita/boxdb/issues/new/choose).
Include the BoxDB version, OS/architecture, client, storage format, minimal code
or SQL, expected result and actual result. Use synthetic data rather than a
private database. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Work locally

Install current stable Rust, native build tools and libclang, Go 1.23+ with cgo,
Node.js 22.18+, Bun, Just and Python 3.11+. On macOS, use Xcode Command Line
Tools; on Debian/Ubuntu, install `build-essential` and `libclang-dev`.

```sh
git clone https://github.com/lebedev-nikita/boxdb.git
cd boxdb
just check
```

`just check` verifies Rust, both language clients, adapters and cross-language
behavior. Run `just --list` for focused commands. Documentation site commands
and deployment instructions live in [website/README.md](website/README.md).

## Design rules

These rules decide where a change belongs. They came out of designing change
history (changesets and commit triggers) and apply to every feature.

- **Mechanism over policy.** The core exposes primitives that cannot be built
  from outside: engine hooks, metadata, typed values. Application policy, such
  as an undo stack, a conflict strategy or a history table layout, stays in user
  code or in an optional package. Ask whether a user could build the feature
  with the public API; if so, it is a recipe or a package, not a core feature.
- **Engine boundary.** The SQLite amalgamation is patched only at value,
  expression and record boundaries, through checked anchors that must match
  exactly once. Everything about transactions, SQL preprocessing, metadata and
  hooks lives in Rust, where it is testable and survives SQLite upgrades.
- **Standard `sqlite_schema`.** Objects SQLite cannot parse, such as
  `BEFORE COMMIT` triggers, never enter `sqlite_schema`; they live in
  `__boxdb_*` metadata tables. Files stay readable by standard SQLite tools.
- **Connection-level guarantees.** Validation and change recording hold for
  writes through BoxDB connections. External SQLite writers bypass them, and
  the documentation says so instead of promising file-level guarantees.
- **Main database first.** Logical metadata features cover `main`; TEMP and
  attached databases are added when a use case needs them. Grammar leaves room
  for that: commit triggers parse qualified table names and reject non-main
  schemas, timing is explicit (`BEFORE COMMIT`), and the `WHEN` slot is unused.
- **One preupdate hook.** SQLite has a single preupdate hook per connection and
  the Session extension takes it over, so change recording is implemented in
  Rust on the hook that `history::install_hook` dispatches to every consumer.

## Submit a change

- For code behavior, start with a failing regression test at the public interface,
  then implement the change. Documentation does not require TDD. Package publishing
  scripts are exempt from automated tests.
- Keep SQLite's standard binary page and record encoding. JSON remains an opaque
  validated document; SQL JSON/JSONB processing is out of scope.
- Version 0 does not require backward compatibility. Update affected APIs,
  callers and contracts directly.
- Update the relevant user guide and [.agents/skills/boxdb/SKILL.md](.agents/skills/boxdb/SKILL.md)
  when public behavior changes. Documentation lives in repository Markdown and is
  published on the site from those same files.
- Explain the problem, resulting behavior and checks in your pull request. Keep
  unrelated cleanup separate.

By submitting a contribution, you agree to license it under the repository's
[MIT license](LICENSE).
