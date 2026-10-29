import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { docsSchema } from "@astrojs/starlight/schema";
import { pageBySlug, pageBySource, pages, repository } from "./content/pages";

const markdown = glob({
  base: new URL("../../", import.meta.url),
  pattern: pages.map((page) => page.source),
  generateId: ({ entry }) => {
    const page = pageBySource.get(entry);
    if (!page) throw new Error(`Unregistered documentation source: ${entry}`);
    return page.slug;
  },
});

export const collections = {
  docs: defineCollection({
    schema: docsSchema(),
    loader: {
      name: "boxdb-repository-docs",
      load: (context) =>
        markdown.load({
          ...context,
          parseData: (entry) => {
            const page = pageBySlug.get(entry.id);
            if (!page) throw new Error(`Unregistered documentation page: ${entry.id}`);
            return context.parseData({
              ...entry,
              data: Object.assign(entry.data, {
                title: page.title,
                description: page.description,
                editUrl: `${repository}/edit/main/${page.source}`,
              }),
            });
          },
        }),
    },
  }),
};
