# npm releases

The npm release contains `@boxdb/client` and `@boxdb/cli` at the same version as
`Cargo.toml`. Each archive bundles macOS ARM64 and Linux x64 binaries; Linux
requires glibc 2.35+. Installations do not run scripts or download binaries.
The CLI exposes `boxdb` through npm's `bin` mechanism. Adapters are not part of
this release command.

## Prepare

Push the release commit and wait for the **npm packages** workflow to pass.
It builds native code on both platforms, assembles the two complete archives,
and installs those exact archives on both platforms with Node.js 22 and 24 and
Bun. SQL, CLI arguments/stdin/errors and bundled licenses are tested.
The workflow only uploads artifacts; it never publishes to npm.

From the same commit, with Bun and authenticated `gh` available:

```sh
just npm-prepare WORKFLOW_RUN_ID
```

This downloads the verified `boxdb-npm` artifact into `target/npm`, checks its
source commit and checksums, runs the installed-package smoke tests locally and
performs `npm publish --dry-run` for each archive. It also creates
`target/npm/Publish BoxDB.command` for macOS. The release manifest records the
source commit and hashes of the archives to review.

## Publish

The npm account must own the `boxdb` scope or have publishing permission there.
For the first release, create the free public-package organization `boxdb` on
npm before publishing.

Open `Publish BoxDB.command` or run:

```sh
just npm-publish
```

The command signs in through npm's browser flow if necessary, shows the account,
versions and source commit, and asks for confirmation. Complete npm's 2FA prompts
when requested. Both packages are published publicly with the `latest` tag from
the reviewed archives, without rebuilding. No npm token is stored in this repo
or GitHub Actions.

If publishing stops after one package, rerun the same command. Already published
versions are skipped only when their registry integrity matches the local
archive; conflicting versions are rejected. Publishing both packages is not
atomic. Keep the prepared archives until both are published.

## Local development

```sh
just npm-test
```

This builds only the host platform, packs both packages and tests installations
with scripts disabled. Local bundles cannot be published with the release
command. Use `just npm-check` for TypeScript, lint and formatting checks.
