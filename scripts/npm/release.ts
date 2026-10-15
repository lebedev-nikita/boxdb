import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

export const root = resolve(import.meta.dirname, "../..");
export const output = join(root, "target/npm");
export const registry = "https://registry.npmjs.org/";

interface Package {
  name: string;
  filename: string;
  sha256: string;
  integrity: string;
}
export function readRelease() {
  const release = JSON.parse(readFileSync(join(output, "release.json"), "utf8")) as {
    version: string;
    commit: string;
    platforms: string[];
    packages: Package[];
  };
  if (!/^\d+\.\d+\.\d+$/.test(release.version) || !/^[a-f0-9]{40}$/.test(release.commit)) {
    throw new Error("Invalid release version/commit");
  }
  if ([...release.platforms].sort().join(",") !== "darwin-arm64,linux-x64") {
    throw new Error(
      "Publishing requires both macOS ARM64 and Linux x64 binaries; local bundles cannot be published",
    );
  }
  if (
    release.packages
      .map((p) => p.name)
      .sort()
      .join(",") !== "@boxdb/cli,@boxdb/client"
  ) {
    throw new Error("Expected exactly @boxdb/client and @boxdb/cli");
  }
  for (const pkg of release.packages) {
    const expected = `${pkg.name.replace("@", "").replace("/", "-")}-${release.version}.tgz`;
    if (pkg.filename !== expected) throw new Error(`Unexpected archive: ${pkg.filename}`);
    const bytes = readFileSync(join(output, pkg.filename));
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    const integrity = "sha512-" + createHash("sha512").update(bytes).digest("base64");
    if (sha256 !== pkg.sha256 || integrity !== pkg.integrity)
      throw new Error(`Archive checksum mismatch: ${pkg.filename}`);
  }
  return release;
}
