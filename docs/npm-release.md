# npm releases

The npm release contains five packages at the same version as `Cargo.toml`:

- `@boxdb/client`
- `@boxdb/cli`
- `@boxdb/drizzle-adapter`
- `@boxdb/better-auth-adapter`
- `@boxdb/db0-adapter`

The client and CLI archives bundle macOS ARM64 and Linux x64 and ARM64
binaries; Linux requires glibc 2.35+. Installations do not run scripts or download binaries.
The CLI exposes `boxdb` through npm's `bin` mechanism. Adapters declare the client
and their integration libraries as peer dependencies.

## Publish through GitHub Actions

Update `Cargo.toml` and the package manifests to the release version, then push
that commit to `main`. Start the **npm packages** workflow with publishing enabled:

```sh
just publish-npm
```

This dispatches `.github/workflows/npm.yml` on `main` with `publish=true`.
Pushing a matching `vX.Y.Z` tag also builds and publishes that version. Ordinary
branch pushes, pull requests and manual runs without `publish=true` only build
and verify packages.

The workflow builds native code on all three platforms, assembles all five
archives, and installs those exact archives on all three platforms with
Node.js 22 and 24 and Bun.
Checks cover SQL, CLI arguments/stdin/errors, adapter queries, Better Auth
registration and sessions, and bundled licenses.

After every verification job passes, the publish job downloads the same
`boxdb-npm` artifact, validates its checksums and source commit, and publishes
with npm OIDC and provenance. It does not rebuild packages. The job uses the
`npm` GitHub environment and requires `id-token: write`; no npm token or local
npm login is needed. All five packages are public with the `latest` tag, with
the client published before its adapters. Concurrent publish jobs are serialized.

A release tag must equal `v` plus the archive version. Already published
versions are skipped only when registry integrity matches the archive;
conflicting versions are rejected. Publishing the packages is not atomic.
For a partial release, rerun the failed publish job from the original workflow
run so it uses the same archives.

## One-time trusted publisher setup

In the repository's GitHub environment settings, create `npm` with deployment
branch policies allowing the `main` branch and `v*` tags. Leave required
reviewers unset for unattended releases.

For each package, configure a GitHub Actions trusted publisher with:

- Repository: `lebedev-nikita/boxdb`
- Workflow filename: `npm.yml`
- Environment: `npm`
- Permission: allow direct `npm publish`

With npm 11.15.0 or newer and package write access, configure this through the
npm CLI. Login and the initial trust request require browser authentication/2FA;
subsequent releases do not:

```sh
npm login --auth-type=web
for package in client cli drizzle-adapter db0-adapter better-auth-adapter; do
  npm trust github "@boxdb/$package" \
    --repository=lebedev-nikita/boxdb --file=npm.yml --environment=npm \
    --allow-publish --yes
  sleep 2
done
```

If npm offers to remember 2FA for five minutes, that covers the remaining
packages. Use `npm trust list @boxdb/client` (and each other package) to inspect
existing bindings before creating them again. Do not revoke unrelated bindings.
A new binding must complete its first successful publish within two days or it
expires; recreate an expired binding before releasing.
See npm's [trusted publishing guide](https://docs.npmjs.com/trusted-publishers/)
and [`npm trust` reference](https://docs.npmjs.com/cli/v11/commands/npm-trust/).

## Inspect archives or publish locally

To inspect archives from a successful workflow at the current checkout's commit:

```sh
just npm-prepare WORKFLOW_RUN_ID
```

This downloads the verified `boxdb-npm` artifact into `target/npm`, checks its
source commit and checksums, runs the installed-package smoke tests locally and
performs `npm publish --dry-run` for each pending archive. The release manifest
records the source commit and hashes of the archives.

For a manual local release, use `just npm-ready` to prepare the archives, then
run `bun run npm:publish` or open `target/npm/Publish BoxDB.command`. This path
requires interactive confirmation and npm login/2FA. It publishes the reviewed
archives without rebuilding. Rerunning skips matching published versions and
resumes waiting for accepted uploads recorded in `target/npm/publish-state.json`.
Keep that state file with the prepared archives until publication is complete.

## Local development

```sh
just npm-test
```

This builds only the host platform, packs all five packages and tests installations
with scripts disabled. Local bundles cannot be published with the release
command. Use `just npm-check` for TypeScript, lint and formatting checks.
