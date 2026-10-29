# BoxDB website

The public site is built with Astro and Starlight and hosted at
[lebedev-nikita.github.io/boxdb](https://lebedev-nikita.github.io/boxdb/).
It is static HTML with local Pagefind search; no backend, analytics account or
runtime API is needed.

## Develop

Use Bun and Node.js 22.23.4+ (Node.js 24 LTS is recommended for site development).
From the repository root:

```sh
just docs-dev
just docs-build
```

Astro owns framework compilation. Astro files are type-checked with the official
content mapper and a site-local TypeScript 7.1 beta; the stable TypeScript version
used by the library packages is unchanged. The site uses the existing Bun
workspace and formatter rather than migrating the library toolchain. No database
client or native Rust build is required to build the website.

## Edit documentation

Edit the original Markdown in `docs/`, the package READMEs, `CONTRIBUTING.md` or
`SECURITY.md`. The content collection reads those files directly, adds page
metadata and rewrites repository links to site routes where available. Source
code links remain on GitHub. There are no generated Markdown copies to maintain.

Register a new guide in [src/content/pages.ts](src/content/pages.ts), add it to the
sidebar in [astro.config.ts](astro.config.ts), and run `just docs-build`.
Update the BoxDB usage skill when public behavior changes. The landing page is
[src/pages/index.astro](src/pages/index.astro).

## Publish

The [Pages workflow](../.github/workflows/pages.yml) builds on pull requests and
deploys pushes to `main`. GitHub Pages must use **GitHub Actions** as its publishing
source. It can also be deployed manually with the workflow's **Run workflow**
action on `main`. The deployment environment is `github-pages`.

The configured `/boxdb/` base path is required for project Pages hosting. Update
the site/base constants, canonical URLs, sitemap and repository homepage together
if the public domain changes.
