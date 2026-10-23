import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { requiredPlatforms } from "./release.js";

const root = resolve(import.meta.dirname, "../..");
const platform = `${process.platform}-${process.arch}`;
if (!requiredPlatforms.includes(platform)) {
  throw new Error(`Unsupported npm build platform: ${platform}`);
}
const built = spawnSync(
  "cargo",
  ["build", "--release", "--locked", "-p", "boxdb", "-p", "boxdb-node"],
  {
    cwd: root,
    stdio: "inherit",
    env: { ...process.env, MACOSX_DEPLOYMENT_TARGET: "11.0" },
  },
);
if (built.error) throw built.error;
if (built.status !== 0) process.exit(built.status ?? 1);
const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
  cwd: root,
  encoding: "utf8",
});
if (metadata.error) throw metadata.error;
if (metadata.status !== 0) throw new Error(metadata.stderr);
const target = (JSON.parse(metadata.stdout) as { target_directory: string }).target_directory;
const output = join(root, "target/npm-native", platform);
mkdirSync(output, { recursive: true });
copyFileSync(join(target, "release/boxdb"), join(output, "boxdb"));
chmodSync(join(output, "boxdb"), 0o755);
copyFileSync(
  join(
    target,
    process.platform === "darwin" ? "release/libboxdb_node.dylib" : "release/libboxdb_node.so",
  ),
  join(output, "boxdb.node"),
);
console.log(`Built npm binaries: ${output}`);
