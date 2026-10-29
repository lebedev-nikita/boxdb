export const repository = "https://github.com/lebedev-nikita/boxdb";
export const site = "https://lebedev-nikita.github.io";
export const base = "/boxdb/";

export const pages = [
  {
    source: "docs/index.md",
    slug: "docs",
    title: "Documentation",
    description: "Start using BoxDB: choose a client, learn SQL types and deploy your database.",
  },
  {
    source: "docs/quickstart.md",
    slug: "docs/quickstart",
    title: "Quickstart",
    description:
      "Create a typed BoxDB database, write two invoices and query an exact decimal total.",
  },
  {
    source: "docs/why-boxdb.md",
    slug: "docs/why-boxdb",
    title: "Why BoxDB?",
    description:
      "Use cases, tradeoffs and current boundaries of BoxDB for single-server applications.",
  },
  {
    source: "docs/sql.md",
    slug: "docs/sql",
    title: "SQL and types",
    description:
      "Declare typed columns, bind application values and understand BoxDB's SQL differences.",
  },
  {
    source: "packages/typescript/README.md",
    slug: "docs/clients/typescript",
    title: "TypeScript",
    description:
      "Install BoxDB for Node.js and Bun; use asynchronous queries, transactions and cancellation.",
  },
  {
    source: "docs/rust.md",
    slug: "docs/clients/rust",
    title: "Rust",
    description: "Install the BoxDB Rust crate and work with typed queries, values and errors.",
  },
  {
    source: "packages/go/README.md",
    slug: "docs/clients/go",
    title: "Go",
    description: "Install BoxDB's database/sql driver with its matching native library and cgo.",
  },
  {
    source: "packages/cli/README.md",
    slug: "docs/clients/cli",
    title: "Command line",
    description: "Install the BoxDB CLI, run SQL and inspect or recover database files.",
  },
  {
    source: "packages/drizzle-adapter/README.md",
    slug: "docs/integrations/drizzle",
    title: "Drizzle",
    description:
      "Use BoxDB with Drizzle queries, transactions, migrations and extended column builders.",
  },
  {
    source: "packages/better-auth-adapter/README.md",
    slug: "docs/integrations/better-auth",
    title: "Better Auth",
    description:
      "Configure BoxDB as a direct Better Auth database adapter and generate an initial schema.",
  },
  {
    source: "packages/db0-adapter/README.md",
    slug: "docs/integrations/db0",
    title: "db0",
    description: "Connect db0 to a BoxDB client or transaction handle.",
  },
  {
    source: "docs/native-decimal.md",
    slug: "docs/decimals",
    title: "Exact decimals",
    description:
      "Exact DECIMAL arithmetic, precision, scale, explicit rounding and storage semantics.",
  },
  {
    source: "docs/temporal.md",
    slug: "docs/temporal",
    title: "Temporal values",
    description: "Work with UTC instants, dates, times, calendar datetimes and intervals in BoxDB.",
  },
  {
    source: "docs/storage.md",
    slug: "docs/storage",
    title: "Storage and validation",
    description:
      "Understand binary SQLite, readable YAML, extended types and validation boundaries.",
  },
  {
    source: "docs/extensions.md",
    slug: "docs/extensions",
    title: "SQL extensions",
    description:
      "Built-in full-text and vector search, Unicode functions, regular expressions and exact arithmetic.",
  },
  {
    source: "docs/migrations.md",
    slug: "docs/migrations",
    title: "Migrations",
    description:
      "Create and run SQL migrations and export schema through the CLI or application clients.",
  },
  {
    source: "docs/client-errors.md",
    slug: "docs/errors",
    title: "Errors and cancellation",
    description:
      "Handle BoxDB error codes, cancellation, transaction cleanup and client value mappings.",
  },
  {
    source: "docs/deployment.md",
    slug: "docs/deployment",
    title: "Deployment and backups",
    description:
      "Deploy BoxDB on persistent local storage, back up committed data and plan upgrades.",
  },
  {
    source: "docs/replication.md",
    slug: "docs/replication",
    title: "Litestream replication",
    description: "Replicate and restore binary BoxDB databases with original Litestream.",
  },
  {
    source: "docs/recovery.md",
    slug: "docs/recovery",
    title: "Diagnostics and recovery",
    description:
      "Check, repair and recover BoxDB files while understanding incomplete checks and possible data loss.",
  },
  {
    source: "docs/benchmarks.md",
    slug: "docs/benchmarks",
    title: "Benchmarks",
    description:
      "Reproduce BoxDB benchmarks and interpret SQLite comparisons and historical measurements.",
  },
  {
    source: "docs/faq.md",
    slug: "docs/faq",
    title: "FAQ",
    description:
      "Answers about BoxDB compatibility, platforms, production status, JSON and storage.",
  },
  {
    source: "CONTRIBUTING.md",
    slug: "docs/contributing",
    title: "Contributing",
    description: "Report bugs, set up BoxDB development and submit focused contributions.",
  },
  {
    source: "SECURITY.md",
    slug: "docs/security",
    title: "Security",
    description:
      "Report a BoxDB security vulnerability privately and understand experimental release support.",
  },
  {
    source: "website/README.md",
    slug: "docs/website",
    title: "Website development",
    description: "Build the BoxDB documentation site and publish it with GitHub Pages.",
  },
  {
    source: "docs/npm-release.md",
    slug: "docs/releases/npm",
    title: "npm releases",
    description: "Build, verify and publish BoxDB's native npm packages.",
  },
  {
    source: "docs/rust-release.md",
    slug: "docs/releases/rust",
    title: "Rust releases",
    description: "Package, verify and publish the BoxDB Rust crate and bundled SQLite crates.",
  },
] as const;

export const pageBySource = new Map<string, (typeof pages)[number]>(
  pages.map((page) => [page.source, page]),
);

export const pageBySlug = new Map<string, (typeof pages)[number]>(
  pages.map((page) => [page.slug, page]),
);
