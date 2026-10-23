import {
  chmodSync,
  copyFileSync,
  cpSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { releasePackages, requiredPlatforms } from "./release.js";

const root = resolve(import.meta.dirname, "../..");
const output = join(root, "target/npm");
const platforms = process.argv.includes("--local")
  ? [`${process.platform}-${process.arch}`]
  : requiredPlatforms;
const version = (
  JSON.parse(readFileSync(join(root, "packages/typescript/package.json"), "utf8")) as {
    version: string;
  }
).version;
const cargo = readFileSync(join(root, "Cargo.toml"), "utf8").match(/^version = "([^"]+)"/m)?.[1];
if (!/^\d+\.\d+\.\d+$/.test(version) || version !== cargo)
  throw new Error("Client and Cargo release versions must match (X.Y.Z)");

function run(command: string, args: string[], cwd = root): string {
  const result = spawnSync(command, args, { cwd, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  return result.stdout;
}

// Check all inputs before replacing the previous bundle.
for (const platform of platforms) {
  for (const file of ["boxdb", "boxdb.node"])
    readFileSync(join(root, "target/npm-native", platform, file));
}
for (const { directory: path } of releasePackages) {
  rmSync(join(root, path, "dist"), { recursive: true, force: true });
  run("bunx", [
    "tsc",
    "-p",
    join(path, path.endsWith("cli") ? "tsconfig.json" : "tsconfig.build.json"),
  ]);
}
rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
const packages = [];
for (const { directory: source, name: packageName, binary } of releasePackages) {
  const name = packageName.slice("@boxdb/".length);
  const stage = join(output, "staging", name);
  mkdirSync(stage, { recursive: true });
  const manifest = JSON.parse(readFileSync(join(root, source, "package.json"), "utf8")) as Record<
    string,
    unknown
  >;
  if (manifest.version !== version || manifest.name !== `@boxdb/${name}`)
    throw new Error(`Unexpected ${name} package name/version`);
  delete manifest.devDependencies;
  delete manifest.scripts;
  manifest.files = binary
    ? ["dist", "native", "README.md", "LICENSE", "licenses"]
    : ["dist", "README.md", "LICENSE"];
  writeFileSync(join(stage, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
  cpSync(join(root, source, "dist"), join(stage, "dist"), { recursive: true });
  // The published package has one license directory shared by its JS and native code.
  rmSync(join(stage, "dist/licenses"), { recursive: true, force: true });
  rmSync(join(stage, "dist/LICENSE"), { force: true });
  for (const file of ["README.md"]) copyFileSync(join(root, source, file), join(stage, file));
  copyFileSync(join(root, "LICENSE"), join(stage, "LICENSE"));
  if (binary) {
    mkdirSync(join(stage, "licenses"));
    for (const [path, filename] of [
      ["rusqlite/LICENSE", "rusqlite.txt"],
      ["libsqlite3-sys/LICENSE", "libsqlite3-sys.txt"],
      ["sqlean/LICENSE", "sqlean.txt"],
      ["sqlean/regexp/pcre2/LICENSE", "pcre2.txt"],
      ["sqlite-vec/LICENSE-MIT", "sqlite-vec-MIT.txt"],
      ["sqlite-vec/LICENSE-APACHE", "sqlite-vec-APACHE.txt"],
    ])
      copyFileSync(join(root, "vendor", path!), join(stage, "licenses", filename!));
    for (const platform of platforms) {
      const directory = join(stage, "native", platform);
      mkdirSync(directory, { recursive: true });
      copyFileSync(join(root, "target/npm-native", platform, binary), join(directory, binary));
      if (name === "cli") chmodSync(join(directory, binary), 0o755);
    }
  }
  if (name === "cli") chmodSync(join(stage, "dist/cli.js"), 0o755);
  const [packed] = JSON.parse(
    run("npm", ["pack", stage, "--pack-destination", output, "--ignore-scripts", "--json"]),
  ) as [{ filename: string; integrity: string }];
  const sha256 = createHash("sha256")
    .update(readFileSync(join(output, packed.filename)))
    .digest("hex");
  packages.push({
    name: manifest.name,
    filename: packed.filename,
    integrity: packed.integrity,
    sha256,
  });
}
const commit = run("git", ["rev-parse", "HEAD"]).trim();
writeFileSync(
  join(output, "release.json"),
  JSON.stringify({ version, commit, platforms, packages }, null, 2) + "\n",
);
writeFileSync(
  join(output, "SHA256SUMS"),
  packages.map((p) => `${p.sha256}  ${p.filename}\n`).join(""),
);
console.log(
  `Packed ${platforms.join(", ")} into ${output}${process.argv.includes("--local") ? " (local test bundle; not publishable)" : ""}`,
);
