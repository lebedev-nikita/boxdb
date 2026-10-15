import { afterAll, beforeAll, expect, test } from "vitest";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "../..");
const directory = mkdtempSync(join(tmpdir(), "boxdb-npm-"));

function run(command: string, args: string[], input?: string) {
  const result = spawnSync(command, args, {
    cwd: directory,
    encoding: "utf8",
    input,
    env: { ...process.env, NODE_PATH: "" },
  });
  if (result.error) throw result.error;
  return result;
}

beforeAll(() => {
  writeFileSync(join(directory, "package.json"), '{"private":true,"type":"module"}');
  for (const name of ["cli", "client"]) {
    const version = JSON.parse(readFileSync(join(root, "packages/typescript/package.json"), "utf8"))
      .version as string;
    const archive = join(root, `target/npm/boxdb-${name}-${version}.tgz`);
    const installed = run("npm", [
      "install",
      archive,
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
    ]);
    expect(installed.status, installed.stderr).toBe(0);
  }
});

test("installed client runs SQL in Node.js and Bun without installation scripts", () => {
  const script = join(directory, "client.mjs");
  writeFileSync(
    script,
    `
    import assert from 'node:assert/strict';
    import { BoxDB } from '@boxdb/client';
    const db = await BoxDB.open('client.db');
    try {
      await db.exec('CREATE TABLE IF NOT EXISTS users(name TEXT)');
      const tx = await db.begin();
      await tx.query\`INSERT INTO users VALUES(\${'Ada'})\`.run();
      await tx.commit();
      assert.deepEqual(await db.query\`SELECT DISTINCT name FROM users\`, [{ name: 'Ada' }]);
      assert.deepEqual(await db.query\`SELECT CAST('0.1' AS DECIMAL) + CAST('0.2' AS DECIMAL) AS amount\`.get(), { amount: '0.3' });
    } finally { await db.close(); }
  `,
  );
  for (const runtime of ["node", "bun"]) {
    const result = run(runtime, [script]);
    expect(result.status, `${runtime}: ${result.stderr}`).toBe(0);
  }
});

afterAll(() => rmSync(directory, { recursive: true, force: true }));

test("installed CLI forwards arguments and reads a database in the caller's directory", () => {
  const executable = join(directory, "node_modules/.bin/boxdb");
  const version = run(executable, ["--version"]);
  expect(version.status, version.stderr).toBe(0);
  expect(version.stdout.trim()).toBe("boxdb 0.1.0");
  const created = run(executable, [
    "database with spaces.db",
    "--json",
    "-e",
    "CREATE TABLE users(name TEXT); INSERT INTO users VALUES('Ada'); SELECT name FROM users;",
  ]);
  expect(created.status, created.stderr).toBe(0);
  expect(JSON.parse(created.stdout)).toMatchObject({
    columns: ["name"],
    rows: [[{ Text: "Ada" }]],
  });
  expect(readFileSync(join(directory, "database with spaces.db")).subarray(0, 16).toString()).toBe(
    "SQLite format 3\0",
  );
  const queried = run(executable, [
    "database with spaces.db",
    "--json",
    "-e",
    "SELECT name FROM users",
  ]);
  expect(queried.status, queried.stderr).toBe(0);
  expect(JSON.parse(queried.stdout).rows).toEqual([[{ Text: "Ada" }]]);
});

test("installed CLI preserves stdin, SQL errors and argument error exit codes", () => {
  const executable = join(directory, "node_modules/.bin/boxdb");
  const piped = run(executable, [":memory:", "--json"], "SELECT 42 AS answer;\n");
  expect(piped.status, piped.stderr).toBe(0);
  expect(JSON.parse(piped.stdout).rows).toEqual([[{ Integer: 42 }]]);
  const sqlError = run(executable, [":memory:", "-e", "SELECT * FROM missing_table"]);
  expect(sqlError.status).toBe(1);
  expect(sqlError.stderr).toContain("missing_table");
  const argumentError = run(executable, ["--invalid-option"]);
  expect(argumentError.status).toBe(2);
  expect(argumentError.stderr).toContain("--invalid-option");
});

test("installed packages include their license and bundled extension notices", () => {
  for (const name of ["cli", "client"]) {
    const installed = join(directory, `node_modules/@boxdb/${name}`);
    expect(readFileSync(join(installed, "LICENSE"), "utf8")).toContain("MIT License");
    for (const license of [
      "sqlean.txt",
      "pcre2.txt",
      "sqlite-vec-MIT.txt",
      "sqlite-vec-APACHE.txt",
    ]) {
      expect(readFileSync(join(installed, "licenses", license), "utf8").length).toBeGreaterThan(
        100,
      );
    }
  }
});
