default:
    @just --list

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

go-check: go-build
    test -z "$(gofmt -l packages/go/*.go)"
    cd packages/go && go vet ./...

go-test: go-build
    cd packages/go && go test -race -count=1 ./...

check: ts-check ts-test go-check go-test
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --locked
