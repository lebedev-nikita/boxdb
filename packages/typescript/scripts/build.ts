import { copyFileSync, mkdirSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));

function run(command: string, args: string[]): void {
  const result = spawnSync(command, args, { cwd: packageRoot, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

const release = process.argv.includes("--release");
run("cargo", [
  "build",
  "--manifest-path",
  "native/Cargo.toml",
  "--locked",
  ...(release ? ["--release"] : []),
]);
const metadata = spawnSync(
  "cargo",
  ["metadata", "--manifest-path", "native/Cargo.toml", "--no-deps", "--format-version", "1"],
  { cwd: packageRoot, encoding: "utf8" },
);
if (metadata.error) throw metadata.error;
if (metadata.status !== 0) throw new Error(metadata.stderr);
const { target_directory: targetDirectory } = JSON.parse(metadata.stdout) as {
  target_directory: string;
};
const library =
  process.platform === "win32"
    ? "boxdb_node.dll"
    : process.platform === "darwin"
      ? "libboxdb_node.dylib"
      : "libboxdb_node.so";
copyFileSync(
  `${targetDirectory}/${release ? "release" : "debug"}/${library}`,
  `${packageRoot}native/boxdb.node`,
);
rmSync(`${packageRoot}dist`, { recursive: true, force: true });
mkdirSync(`${packageRoot}dist`, { recursive: true });
copyFileSync(
  fileURLToPath(new URL("../../../LICENSE", import.meta.url)),
  `${packageRoot}dist/LICENSE`,
);
mkdirSync(`${packageRoot}dist/licenses`, { recursive: true });
for (const [source, name] of [
  ["sqlean/LICENSE", "sqlean.txt"],
  ["sqlean/regexp/pcre2/LICENSE", "pcre2.txt"],
  ["sqlite-vec/LICENSE-MIT", "sqlite-vec-MIT.txt"],
  ["sqlite-vec/LICENSE-APACHE", "sqlite-vec-APACHE.txt"],
]) {
  copyFileSync(
    fileURLToPath(new URL(`../../../vendor/${source}`, import.meta.url)),
    `${packageRoot}dist/licenses/${name}`,
  );
}
run("bunx", ["tsc", "-p", "tsconfig.build.json"]);
run("bunx", ["tsc", "-p", "tsconfig.runtime.json"]);
