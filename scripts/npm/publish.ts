import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { createInterface } from "node:readline/promises";
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
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

const statePath = join(output, "publish-state.json");
const accepted = (
  existsSync(statePath) ? JSON.parse(readFileSync(statePath, "utf8")) : {}
) as Record<string, string>;
type Package = (typeof release.packages)[number];
const packageId = (pkg: Package) => `${pkg.name}@${release.version}`;

function recordAccepted(pkg: Package) {
  accepted[packageId(pkg)] = pkg.integrity;
  writeFileSync(statePath + ".tmp", JSON.stringify(accepted, null, 2) + "\n");
  renameSync(statePath + ".tmp", statePath);
}

function isPublished(pkg: Package): boolean {
  const existing = npm([
    "view",
    packageId(pkg),
    "dist.integrity",
    "--json",
    "--prefer-online",
    "--fetch-retries=0",
    "--fetch-timeout=15000",
  ]);
  if (existing.status === 0) {
    if (JSON.parse(existing.stdout) !== pkg.integrity)
      throw new Error(`${packageId(pkg)} already exists with different contents`);
    return true;
  }
  let code: string | undefined;
  try {
    code = (JSON.parse(existing.stdout) as { error?: { code?: string } }).error?.code;
  } catch {
    /* Preserve the original registry error below. */
  }
  if (code !== "E404") throw new Error(existing.stderr);
  return false;
}

async function waitForPublication(pkg: Package) {
  const deadline = Date.now() + 10 * 60 * 1000;
  while (!isPublished(pkg)) {
    if (Date.now() >= deadline) {
      throw new Error(
        `npm accepted ${packageId(pkg)}, but it is still unavailable after 10 minutes. Rerun this command to resume waiting; the accepted archive will not be uploaded again.`,
      );
    }
    console.log(`npm is processing ${packageId(pkg)}; checking again in 10 seconds...`);
    await delay(10_000);
  }
}

const pending = [];
const processing = [];
for (const pkg of release.packages) {
  const receipt = accepted[packageId(pkg)];
  if (receipt && receipt !== pkg.integrity)
    throw new Error(
      `npm already accepted different archives for ${packageId(pkg)}; restore the original release bundle`,
    );
  if (isPublished(pkg)) {
    console.log(`Already published and verified: ${packageId(pkg)}`);
    continue;
  }
  if (receipt) {
    console.log(`Already accepted by npm: ${packageId(pkg)}; skipping its upload`);
    processing.push(pkg);
    continue;
  }
  pending.push(pkg);
}

// npm's dry run also rejects published versions, so inspect only pending uploads.
for (const pkg of pending) {
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
  `\nPublish as ${username}: ${pending.map((p) => packageId(p)).join(", ") || "no remaining uploads"}`,
);
console.log(`Source commit: ${release.commit}`);

if (pending.length > 0) {
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
  recordAccepted(pkg);
  processing.push(pkg);
}
for (const pkg of processing) await waitForPublication(pkg);
console.log(
  "\nPublished and verified:\n" +
    release.packages.map((pkg) => `${pkg.name}@${release.version}`).join("\n"),
);
