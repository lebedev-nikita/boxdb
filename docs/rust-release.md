# Rust releases

The Cargo release contains three crates at the version in the root `Cargo.toml`:

- `boxdb-sqlite-sys`: BoxDB's bundled SQLite engine and bindings.
- `boxdb-rusqlite`: the rusqlite wrapper with a direct dependency on that engine.
- `boxdb`: the public library and CLI.

Applications install `boxdb` with `cargo add boxdb`. No `[patch]`, Git dependency
or system SQLite is needed. Native build tools and libclang are required.
Versions before 1.0 are experimental and may change the API and storage contract.

## Verify

Use Cargo 1.90 or newer, which supports packaging and publishing multiple crates
in dependency order. Update all Cargo and npm release versions together, run
`just check`, then commit the release changes.

```sh
just cargo-package
```

Cargo packages all three crates and builds their extracted archives using a
temporary registry. This verifies dependencies that have not yet been published.
Archives are written to `target/package`. GitHub's release workflow runs the
same command and attaches all three `.crate` files alongside the CLI binaries.

Before publication, also build a separate application outside this workspace
with only a `boxdb` path dependency. Exercise native types and schema export
through its public Rust API without adding any Cargo patches.

## Publish

Authenticate locally with `cargo login`, using a crates.io API token authorized
to publish these three packages. Keep credentials outside this repository.

```sh
just publish-cargo
```

Cargo verifies and publishes the engine, wrapper and public crate in dependency
order. A release requires a clean Git checkout. If only part of the release is
published, inspect the existing versions and publish only the remaining crates
with `cargo publish --locked -p PACKAGE`.

Finally, verify installation from a separate project with `cargo add boxdb` and
run `cargo install boxdb --locked` in a temporary installation root to check the
published CLI. Publish the corresponding npm archives using the
[npm release workflow](npm-release.md).
