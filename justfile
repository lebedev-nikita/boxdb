default:
    @just --list

# Count lines in tracked project files, excluding vendored dependencies.
count-lines:
    cloc --vcs=git --exclude-dir=vendor

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
