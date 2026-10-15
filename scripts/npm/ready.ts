import { existsSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { output, readRelease, root } from "./release.js";

if (existsSync(join(output, "release.json"))) {
  const release = readRelease();
  console.log(`Using verified release ${release.version} from ${release.commit}`);
  process.exit(0);
}
if (existsSync(join(output, "publish-state.json"))) {
  throw new Error("Restore the original release archives before resuming accepted uploads");
}

function run(command: string, args: string[]): string {
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || result.stdout);
  return result.stdout;
}
const commit = run("git", ["rev-parse", "HEAD"]).trim();
const [workflow] = JSON.parse(
  run("gh", [
    "run",
    "list",
    "--repo",
    "lebedev-nikita/boxdb",
    "--workflow",
    "npm.yml",
    "--commit",
    commit,
    "--limit",
    "1",
    "--json",
    "databaseId,status,conclusion",
  ]),
) as { databaseId: number; status: string; conclusion: string }[];
if (!workflow)
  throw new Error(
    "Push this commit to GitHub first so the npm packages workflow can build its release archives",
  );
if (workflow.status !== "completed") {
  const watched = spawnSync(
    "gh",
    [
      "run",
      "watch",
      String(workflow.databaseId),
      "--repo",
      "lebedev-nikita/boxdb",
      "--exit-status",
    ],
    { cwd: root, stdio: "inherit" },
  );
  if (watched.error) throw watched.error;
  if (watched.status !== 0) throw new Error("The npm packages workflow did not pass");
} else if (workflow.conclusion !== "success") {
  throw new Error("The npm packages workflow did not pass");
}
const prepared = spawnSync("bun", ["run", "npm:prepare", String(workflow.databaseId)], {
  cwd: root,
  stdio: "inherit",
});
if (prepared.error) throw prepared.error;
if (prepared.status !== 0) process.exit(prepared.status ?? 1);
