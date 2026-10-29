import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { Root, Link, Image, Code } from "mdast";
import type { Node, Parent } from "unist";
import type { VFile } from "vfile";
import { base, pageBySource, repository } from "./pages";

const root = fileURLToPath(new URL("../../../", import.meta.url));

function websiteLink(url: string, source: string): string {
  const githubSource = `${repository}/blob/main/`;
  if (url.startsWith(githubSource)) {
    const target = new URL(url);
    const page = pageBySource.get(decodeURIComponent(target.pathname.split("/blob/main/")[1]!));
    return page ? `${base}${page.slug}/${target.search}${target.hash}` : url;
  }
  if (/^(?:[a-z][a-z\d+.-]*:|\/|#)/i.test(url)) return url;
  const target = new URL(url, `https://repository.invalid/${source}`);
  const path = decodeURIComponent(target.pathname.slice(1));
  const page = pageBySource.get(path);
  if (page) return `${base}${page.slug}/${target.search}${target.hash}`;
  if (path === "README.md") return `${base}${target.hash === "#readme" ? "" : target.hash}`;
  return `${repository}/blob/main/${path}${target.search}${target.hash}`;
}

export default function remarkRepository() {
  return (tree: Root, file: VFile) => {
    const source = relative(root, resolve(String(file.path))).replaceAll("\\", "/");
    if (!pageBySource.has(source)) return;
    // Starlight supplies the page title; keep one H1 in the published page.
    if (tree.children[0]?.type === "heading" && tree.children[0].depth === 1) tree.children.shift();
    const visit = (node: Node) => {
      if (node.type === "code") {
        const code = node as Code;
        code.lang = code.lang?.split(",")[0];
      }
      if (node.type === "link" || node.type === "image") {
        const link = node as Link | Image;
        link.url = websiteLink(link.url, source);
      }
      if ("children" in node) for (const child of (node as Parent).children) visit(child);
    };
    visit(tree);
  };
}
