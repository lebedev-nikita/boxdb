#!/usr/bin/env node
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const platform = `${process.platform}-${process.arch}`;
if (platform !== "darwin-arm64" && platform !== "linux-x64") {
  console.error(
    `BoxDB does not support ${platform}. Supported: macOS ARM64 and Linux x64 (glibc 2.35+).`,
  );
  process.exit(1);
}

const binary = fileURLToPath(new URL(`../native/${platform}/boxdb`, import.meta.url));
const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
const signals = ["SIGINT", "SIGTERM", "SIGHUP"] as const;
const forward = (signal: NodeJS.Signals) => {
  child.kill(signal);
};
for (const signal of signals) process.on(signal, forward);
child.on("error", (error) => {
  console.error(`Cannot start BoxDB: ${error.message}. Reinstall @boxdb/cli.`);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  for (const name of signals) process.removeListener(name, forward);
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
