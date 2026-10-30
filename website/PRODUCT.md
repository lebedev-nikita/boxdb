# BoxDB website

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Delegated by the user: Astro and Starlight, built with Bun and Just and published
as static HTML on GitHub Pages. Use the repository's Markdown guides as the
documentation source.

## Users

Developers building small applications on one server, using TypeScript, Rust,
Go or the command line. They need to evaluate BoxDB and run a first query.

## Product Purpose

Make BoxDB understandable, discoverable and usable without reading its source.
Success means a visitor can choose a client, install it, create a database and
find the relevant operational guidance.

## Positioning

“A database for your $5 VPS.” BoxDB combines SQLite with validated logical
types, native exact decimals and temporal values, and optional readable YAML
storage for Git-tracked data.

## Capabilities and Constraints

- Experimental version 0; API and BoxDB storage compatibility are not promised.
- Binary files preserve SQLite's standard page and record encoding. Native
  values and indexes require BoxDB's patched engine.
- TypeScript, Rust, Go and CLI share the Rust core. Drizzle, Better Auth and
  db0 adapters already exist.
- Readable YAML executes in memory, allows one writer and publishes a complete
  snapshot on commit. It does not support virtual tables.
- JSON documents are opaque and validated; SQL JSON inspection is out of scope.
- Hosting must be GitHub Pages. No social media outreach was authorized.

## Brand Commitments

Preserve the BoxDB name, existing cyan B icon and plain developer-oriented voice.
Public documentation is English. Do not invent adoption, performance or
production-readiness claims.

## Evidence on Hand

The repository contains release 0.6.0, executable clients and tests, technical
guides and benchmark methodology. There are no supplied customer testimonials
or adoption statistics.
