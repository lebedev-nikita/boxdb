# Frequently asked questions

## Is BoxDB a database server?

No. It is an embedded SQLite client with a bundled patched engine. Your process
opens a local database. There is no built-in remote protocol or distributed
write system.

## Can I use it in production?

BoxDB is experimental version 0 software. API and BoxDB storage compatibility
are not guaranteed. Evaluate its limitations, pin versions and test recovery
and upgrades with your own data. See [why BoxDB](why-boxdb.md).

## Is the file still SQLite?

Binary files preserve SQLite's standard page and record encoding. Native values
and indexes require BoxDB's patched engine, and external writers can bypass
some logical validation. Use BoxDB-aware queries, conversion and recovery rather
than assuming ordinary `sqlite3` tools understand every value. See [storage](storage.md).

## Does the $5 VPS tagline promise a particular capacity?

No. It describes the intended single-server workflow. Capacity depends on your
queries, data, storage and hardware. Use the [benchmark methodology](benchmarks.md)
and measure your application; there is no universal throughput claim.

## Which client should I use?

Use [TypeScript](../packages/typescript/README.md) for Node.js/Bun,
[Go](../packages/go/README.md) for `database/sql`, [Rust](rust.md) for direct core
access, or the [CLI](../packages/cli/README.md) for inspection and scripts.
They share types, migrations and storage.

## Can BoxDB run in a browser or on Windows?

The npm client is native and does not run in browsers. Published npm binaries
target macOS ARM64 and Linux x64/ARM64 with glibc 2.35+; Windows, Intel macOS,
Alpine/musl and 32-bit ARM are unsupported by that release. Check each client
guide for source-build requirements and standalone release options.

## Why are TypeScript integers bigint?

JavaScript numbers cannot represent the full integer range exactly. Every
integer result, row ID and write count is a `bigint`; REAL remains `number`.
Validate query rows and convert bigint explicitly when serializing JSON.

## Can I query fields inside JSON?

No. JSON is a validated opaque document. Parse and serialize it in application
code. Use typed columns and related tables for fields you query. SQL JSON/JSONB
functions, operators and virtual tables are disabled.

## Should I put my application database in Git?

Readable YAML is useful for small, deliberately versioned datasets. It loads
into memory and writes a complete snapshot on commit, with one writer per file.
Binary storage is the normal choice for application data. Neither format should
be committed if it contains secrets or private user data.

## How do I ask for help or report a bug?

[Open an issue](https://github.com/lebedev-nikita/boxdb/issues/new/choose) with
your version, platform, client and a minimal reproduction. Remove credentials
and private data. For vulnerabilities, use the [security policy](../SECURITY.md).
