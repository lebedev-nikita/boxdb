default:
    @just --list

# Compare all storage profiles with unmodified bundled SQLite.
[positional-arguments]
bench-sqlite *args:
    @python3 tools/benchmark_sqlite.py "$@"

# Count tracked project, test, and vendored SQLite files separately.
count-lines:
    @echo 'Project files (excluding tests):'
    cloc --vcs=git --exclude-dir=vendor,test,tests
    @echo
    @echo 'Test files:'
    cloc --vcs=git --exclude-dir=vendor --match-d='(^|/)(test|tests)(/|$)'
    @echo
    @echo 'Vendor SQLite (vendor/libsqlite3-sys):'
    cloc --vcs=git --match-d='(^|/)vendor/libsqlite3-sys(/|$)' --force-lang=C,inc

ts-install:
    bun install --frozen-lockfile

# Serve the documentation site from the repository Markdown sources.
docs-dev: ts-install
    bun run --cwd website dev

# Check and build the static GitHub Pages site and local search index.
docs-build: ts-install
    bun run --cwd website build

ts-build: ts-install
    bun run build

ts-check: ts-build
    bun run check

ts-test: ts-build
    bun run test
    cd packages/typescript && node .tmp-runtime/runtime.js
    cd packages/typescript && bun run .tmp-runtime/runtime.js

go-build:
    go run packages/go/build.go

# Build and test npm archives for this machine; not a publishable release.
npm-test: ts-install
    bun run npm:build
    bun run npm:pack --local
    bun run npm:test

npm-check: ts-install
    bun run npm:check

# Download and verify a successful npm packages workflow from this commit.
[positional-arguments]
npm-prepare run-id:
    bun run npm:prepare "$1"

# Reuse the reviewed bundle, or obtain it from this commit's successful CI run.
npm-ready: npm-check
    bun run scripts/npm/ready.ts

# Build, verify and publish main through GitHub Actions trusted publishing.
publish-npm:
    gh workflow run npm.yml --repo lebedev-nikita/boxdb --ref main --field publish=true

# Package and verify the Rust library and both bundled SQLite implementation crates.
cargo-package:
    cargo package --locked -p boxdb-sqlite-sys -p boxdb-rusqlite -p boxdb

# Publish the Rust crates in dependency order using local Cargo credentials.
publish-cargo:
    cargo publish --locked -p boxdb-sqlite-sys -p boxdb-rusqlite -p boxdb

go-check: go-build
    test -z "$(gofmt -l packages/go/*.go)"
    cd packages/go && go vet ./...

go-test: go-build
    cd packages/go && go test -race -count=1 ./...

# Exercise native types across migrations, both clients, and SQLite/YAML conversion.
client-types-test: ts-build go-build
    cargo test --locked --test client_types -- --ignored

check: ts-check ts-test go-check go-test client-types-test
    cargo fmt --all --check
    rustfmt --edition 2024 --check benches/support/workload.rs
    cargo clippy --no-deps --workspace --exclude boxdb-rusqlite --exclude boxdb-sqlite-sys --all-targets --locked -- -D warnings
    cargo test --locked
    python3 -m unittest discover -s tests -p 'test_*.py'
