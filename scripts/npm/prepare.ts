import { chmodSync, mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { output, readRelease, root } from "./release.js";

const runId = process.argv[2];
if (!runId || !/^\d+$/.test(runId))
  throw new Error("Usage: bun run npm:prepare <successful npm workflow run ID>");
function run(command: string, args: string[]): string {
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
const workflow = JSON.parse(
  run("gh", ["api", `repos/lebedev-nikita/boxdb/actions/runs/${runId}`]),
) as {
  path: string;
  status: string;
  conclusion: string;
  head_sha: string;
};
if (
  workflow.path !== ".github/workflows/npm.yml" ||
  workflow.status !== "completed" ||
  workflow.conclusion !== "success"
) {
  throw new Error(
    "Expected a successful npm packages workflow, including all platform smoke tests",
  );
}
if (workflow.head_sha !== run("git", ["rev-parse", "HEAD"]).trim())
  throw new Error("Workflow source must match the current checkout");
mkdirSync(join(root, "target"), { recursive: true });
const temporary = mkdtempSync(join(root, "target/.npm-download-"));
try {
  run("gh", [
    "run",
    "download",
    runId,
    "--repo",
    "lebedev-nikita/boxdb",
    "--name",
    "boxdb-npm",
    "--dir",
    temporary,
  ]);
  rmSync(output, { recursive: true, force: true });
  renameSync(temporary, output);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
const release = readRelease();
if (release.commit !== workflow.head_sha)
  throw new Error("Downloaded release does not match its source workflow");
console.log(run("bun", ["run", "npm:test"]));
console.log(run("bun", ["run", "npm:publish", "--dry-run"]));
const quote = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
const launcher = join(output, "Publish BoxDB.command");
writeFileSync(
  launcher,
  `#!/bin/bash\nset -euo pipefail\ncd ${quote(root)}\nexec ${quote(process.execPath)} run npm:publish\n`,
);
chmodSync(launcher, 0o755);
console.log(
  `Ready: ${output}\nOpen ${launcher} or run bun run npm:publish to sign in and confirm publication.`,
);
