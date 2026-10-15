import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { createInterface } from "node:readline/promises";
import { output, readRelease, registry, root } from "./release.js";

const release = readRelease();
const dryRun = process.argv.includes("--dry-run");

function npm(args: string[]) {
  const result = spawnSync("npm", [...args, `--registry=${registry}`], {
    cwd: root,
    encoding: "utf8",
  });
  if (result.error) throw result.error;
  return result;
}
function interactive(args: string[]) {
  const result = spawnSync("npm", [...args, `--registry=${registry}`], {
    cwd: root,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`npm ${args[0]} failed; rerun this command to resume`);
}

// Inspect the exact archives before asking for credentials or publishing anything.
for (const pkg of release.packages) {
  interactive([
    "publish",
    join(output, pkg.filename),
    "--access=public",
    "--ignore-scripts",
    "--dry-run",
  ]);
}
if (dryRun) process.exit(0);
if (!process.stdin.isTTY)
  throw new Error("Run npm:publish in an interactive terminal for confirmation and npm 2FA");

let identity = npm(["whoami"]);
if (identity.status !== 0) {
  interactive(["login", "--auth-type=web"]);
  identity = npm(["whoami"]);
}
if (identity.status !== 0) throw new Error(identity.stderr);
const username = identity.stdout.trim();
console.log(
  `\nPublish as ${username}: ${release.packages.map((p) => `${p.name}@${release.version}`).join(", ")}`,
);
console.log(`Source commit: ${release.commit}`);

const pending = [];
for (const pkg of release.packages) {
  const existing = npm(["view", `${pkg.name}@${release.version}`, "dist.integrity", "--json"]);
  if (existing.status === 0) {
    if (JSON.parse(existing.stdout) !== pkg.integrity)
      throw new Error(`${pkg.name}@${release.version} already exists with different contents`);
    console.log(`Already published and verified: ${pkg.name}@${release.version}`);
    continue;
  }
  let code: string | undefined;
  try {
    code = (JSON.parse(existing.stdout) as { error?: { code?: string } }).error?.code;
  } catch {
    /* Preserve the original registry error below. */
  }
  if (code !== "E404") throw new Error(existing.stderr);
  pending.push(pkg);
}
if (pending.length === 0) process.exit(0);

const prompt = createInterface({ input: process.stdin, output: process.stdout });
let answer: string;
try {
  answer = await prompt.question("Publish these public packages to npm? [y/N] ");
} finally {
  prompt.close();
}
if (!/^y(es)?$/i.test(answer.trim())) {
  console.log("Cancelled.");
  process.exit(0);
}

for (const pkg of pending) {
  // Recheck after the human confirmation; publish the reviewed bytes without rebuilding.
  readRelease();
  interactive([
    "publish",
    join(output, pkg.filename),
    "--access=public",
    "--ignore-scripts",
    "--tag=latest",
  ]);
  const published = npm(["view", `${pkg.name}@${release.version}`, "dist.integrity", "--json"]);
  if (published.status !== 0 || JSON.parse(published.stdout) !== pkg.integrity) {
    throw new Error(`Cannot verify the published integrity for ${pkg.name}; rerun to check/resume`);
  }
}
console.log(
  "\nPublished and verified:\n" +
    release.packages.map((pkg) => `${pkg.name}@${release.version}`).join("\n"),
);
