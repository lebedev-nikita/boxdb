import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import sitemap from "@astrojs/sitemap";
import { unified } from "@astrojs/markdown-remark";
import remarkRepository from "./src/content/remark-repository";
import { site, base, repository } from "./src/content/pages";

export default defineConfig({
  site,
  base,
  trailingSlash: "always",
  markdown: { processor: unified({ remarkPlugins: [remarkRepository] }) },
  integrations: [
    starlight({
      title: "BoxDB",
      description:
        "SQLite with validated types, exact decimals and readable storage. For your single-server application.",
      logo: { src: "../assets/icon.svg", alt: "" },
      social: [{ icon: "github", label: "GitHub", href: repository }],
      editLink: { baseUrl: `${repository}/edit/main/` },
      customCss: ["./src/styles/docs.css"],
      expressiveCode: {
        plugins: [
          {
            name: "keyboard-scrollable-code",
            hooks: {
              postprocessRenderedBlock: ({ renderData }) => {
                const visit = (node: typeof renderData.blockAst) => {
                  if (node.tagName === "pre") node.properties.tabIndex = 0;
                  for (const child of node.children) if (child.type === "element") visit(child);
                };
                visit(renderData.blockAst);
              },
            },
          },
        ],
      },
      head: [
        { tag: "meta", attrs: { property: "og:image", content: `${site}${base}social.png` } },
        { tag: "meta", attrs: { name: "twitter:card", content: "summary_large_image" } },
      ],
      sidebar: [
        {
          label: "Start here",
          items: [
            { slug: "docs" },
            { slug: "docs/quickstart" },
            { slug: "docs/why-boxdb" },
            { slug: "docs/sql" },
          ],
        },
        {
          label: "Clients",
          items: [
            { slug: "docs/clients/typescript" },
            { slug: "docs/clients/rust" },
            { slug: "docs/clients/go" },
            { slug: "docs/clients/cli" },
          ],
        },
        {
          label: "Integrations",
          items: [
            { slug: "docs/integrations/drizzle" },
            { slug: "docs/integrations/better-auth" },
            { slug: "docs/integrations/db0" },
          ],
        },
        {
          label: "Core concepts",
          items: [
            { slug: "docs/decimals" },
            { slug: "docs/temporal" },
            { slug: "docs/storage" },
            { slug: "docs/extensions" },
          ],
        },
        {
          label: "Build and operate",
          items: [
            { slug: "docs/migrations" },
            { slug: "docs/errors" },
            { slug: "docs/deployment" },
            { slug: "docs/replication" },
            { slug: "docs/recovery" },
          ],
        },
        {
          label: "Project",
          items: [
            { slug: "docs/faq" },
            { slug: "docs/benchmarks" },
            { slug: "docs/contributing" },
            { slug: "docs/security" },
            { label: "Releases", link: `${repository}/releases` },
            {
              label: "Maintainers",
              collapsed: true,
              items: [
                { slug: "docs/website" },
                { slug: "docs/releases/npm" },
                { slug: "docs/releases/rust" },
              ],
            },
          ],
        },
      ],
    }),
    sitemap(),
  ],
});
