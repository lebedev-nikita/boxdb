default:
    @just --list

# Count tracked project and test files separately, excluding vendored dependencies.
count-lines:
    @echo 'Project files (excluding tests):'
    cloc --vcs=git --exclude-dir=vendor,test,tests
    @echo
    @echo 'Test files:'
    cloc --vcs=git --exclude-dir=vendor --match-d='(^|/)(test|tests)(/|$)'

ts-install:
    bun install --cwd packages/typescript --frozen-lockfile

ts-build: ts-install
    bun run --cwd packages/typescript build

ts-check: ts-build
    bun run --cwd packages/typescript check

ts-test: ts-build
    bun run --cwd packages/typescript test
    cd packages/typescript && node test/runtime.ts
    cd packages/typescript && bun run test/runtime.ts

check: ts-check ts-test
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --locked
