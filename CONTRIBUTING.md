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
