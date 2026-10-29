import { base, pages, repository, site } from "../content/pages";

export function GET() {
  const content = [
    "# BoxDB",
    "",
    "> Experimental embedded SQLite client with validated logical types, native exact decimals and temporal values, and optional readable YAML storage.",
    "",
    "Version 0 APIs and BoxDB storage contracts may change. Binary pages and records retain SQLite's standard encoding; native values and indexes require BoxDB's patched engine. JSON is an opaque validated document; SQL JSON/JSONB inspection is disabled.",
    "",
    "## Usage skill",
    "",
    `- [BoxDB skill](${repository}/blob/main/.agents/skills/boxdb/SKILL.md): Client routing, SQL differences, transactions and file maintenance. Read it before integration work.`,
    "",
    "## Documentation",
    "",
    ...pages.map((page) => `- [${page.title}](${site}${base}${page.slug}/): ${page.description}`),
    "",
    "## Source",
    "",
    `- [Repository](${repository})`,
    `- [Releases](${repository}/releases)`,
    "",
  ].join("\n");
  return new Response(content, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
