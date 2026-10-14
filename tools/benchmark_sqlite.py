#!/usr/bin/env python3
"""Build and compare BoxDB with unmodified bundled SQLite (Python 3.11+)."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
from statistics import median
import subprocess
import sys
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
WORKLOAD_UNITS = {
    "insert_transaction": "row", "point_lookup": "query", "range_scan": "query",
    "aggregate": "query", "update_transaction": "row", "update_autocommit": "row",
    "delete_transaction": "row", "reopen_scan": "database",
}


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def command(args, *, cwd=ROOT, env=None):
    return subprocess.check_output(args, cwd=cwd, env=env, text=True).strip()


def logged_command(args, log, *, cwd=ROOT, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    log.write_text(result.stdout, encoding="utf-8")
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}); see {log}\n{result.stdout[-6000:]}")
    return result.stdout


def executable_from_build(output, name):
    for line in output.splitlines():
        if line.startswith("{"):
            event = json.loads(line)
            if (event.get("reason") == "compiler-artifact"
                    and event["target"]["name"] == name and event.get("executable")):
                return Path(event["executable"])
    raise ValueError(f"Cargo did not report an executable for {name}")


def stock_project(target):
    """Isolate Cargo's patch resolution, but reuse workloads and row layout."""
    project = target / "stock-sqlite-benchmark"
    project.mkdir(parents=True, exist_ok=True)
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    dependencies = ("rusqlite", "clap", "serde", "serde_json", "sha2", "hex", "tempfile", "libsqlite3-sys")
    cargo = '[package]\nname = "boxdb-stock-sqlite-benchmark"\nversion = "0.1.0"\nedition = "2024"\n\n[workspace]\n\n[dependencies]\n'
    for name in dependencies:
        versions = {p["version"] for p in lock["package"] if p["name"] == name}
        if len(versions) != 1:
            raise ValueError(f"expected exactly one locked version of {name}")
        options = manifest["dependencies"][name]
        features = options.get("features", []) if isinstance(options, dict) else []
        cargo += f'{name} = {{ version = "={versions.pop()}", features = {json.dumps(features)} }}\n'
    cargo += '\n[[bin]]\nname = "boxdb-stock-sqlite-benchmark"\npath = "main.rs"\n'

    # Keep all variants: simplifying this enum would change row allocation costs.
    values = re.findall(r"(?ms)^pub enum Value \{.*?^\}", (ROOT / "src/types.rs").read_text())
    if len(values) != 1:
        raise ValueError("cannot locate the public Value enum for the stock row layout")
    value = '''use serde::{Deserialize, Serialize};
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
''' + values[0] + '''
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integer(n) => write!(f, "{n}"),
            Self::Text(s) => write!(f, "{s}"),
            _ => unreachable!("this benchmark only generates integer/text values"),
        }
    }
}
'''
    rust_path = lambda path: json.dumps(str(path), ensure_ascii=False)
    main = f'''mod value;
use value::Value;
#[path = {rust_path(ROOT / "benches/support/sqlite.rs")}]
mod sqlite;
use sqlite::Sqlite as Backend;
include!({rust_path(ROOT / "benches/support/workload.rs")});
'''
    for path, contents in ((project / "Cargo.toml", cargo), (project / "main.rs", main),
                           (project / "value.rs", value)):
        if not path.exists() or path.read_text() != contents:
            path.write_text(contents, encoding="utf-8")
    # Preserve the workspace's locked transitive versions while resolving the
    # registry SQLite package instead of its local patch.
    seed = hashlib.sha256((ROOT / "Cargo.lock").read_bytes() + cargo.encode()).hexdigest()
    marker = project / "workspace-lock.sha256"
    if not (project / "Cargo.lock").exists() or not marker.exists() or marker.read_text() != seed:
        shutil.copyfile(ROOT / "Cargo.lock", project / "Cargo.lock")
        marker.write_text(seed)
    return project


def fingerprint():
    digest = hashlib.sha256()
    files = [ROOT / name for name in ("Cargo.toml", "Cargo.lock", "build.rs", "tools/benchmark_sqlite.py")]
    for directory in ("src", "benches", "vendor"):
        files.extend(path for path in (ROOT / directory).rglob("*")
                     if path.is_file() and path.suffix in (".rs", ".c", ".h", ".inc", ".toml"))
    for path in sorted(files):
        digest.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def build(output):
    print("Building both release executables before measurement...", flush=True)
    env = dict(os.environ, LIBSQLITE3_SYS_USE_PKG_CONFIG="0")
    root_metadata = json.loads(command(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], env=env))
    # A path dependency's build script embeds its checkout path. Keep this cache
    # separate from benchmark builds of other revisions/worktrees.
    checkout = hashlib.sha256(str(ROOT).encode()).hexdigest()[:12]
    target = Path(root_metadata["target_directory"]) / "sqlite-comparison-build" / checkout
    project = stock_project(target)
    env["CARGO_TARGET_DIR"] = str(target / "target")
    host = next(line.removeprefix("host: ") for line in command(["rustc", "-Vv"]).splitlines()
                if line.startswith("host: "))
    stock_metadata = json.loads(command(["cargo", "metadata", "--format-version", "1", "--filter-platform", host], cwd=project, env=env))
    sqlite = next(p for p in stock_metadata["packages"] if p["name"] == "libsqlite3-sys")
    if not sqlite["source"] or not sqlite["source"].startswith("registry+"):
        raise ValueError("stock SQLite must come from the registry, outside BoxDB's Cargo patch")
    stock_source = Path(sqlite["manifest_path"]).parent / "sqlite3/sqlite3.c"
    vendored_source = ROOT / "vendor/libsqlite3-sys/sqlite3/sqlite3.c"
    if stock_source.read_bytes() != vendored_source.read_bytes():
        raise ValueError("stock and BoxDB must use the same original SQLite amalgamation")
    logs = output / "build"
    logs.mkdir()
    boxdb_build = logged_command(
        ["cargo", "bench", "--locked", "--bench", "sqlite_vs_boxdb", "--no-run", "--message-format=json"],
        logs / "boxdb.log", env=env,
    )
    stock_build = logged_command(
        ["cargo", "build", "--release", "--locked", "--message-format=json"],
        logs / "stock.log", cwd=project, env=env,
    )
    binaries = output / "bin"
    binaries.mkdir()
    executables = {}
    for label, text, name in (("boxdb", boxdb_build, "sqlite_vs_boxdb"),
                              ("stock", stock_build, "boxdb-stock-sqlite-benchmark")):
        source = executable_from_build(text, name)
        destination = binaries / (label + source.suffix)
        shutil.copy2(source, destination)
        executables[label] = destination
    metadata = {
        "collected_at": datetime.now(timezone.utc).isoformat(),
        "git_revision": command(["git", "rev-parse", "HEAD"]),
        "working_tree_dirty": bool(command(["git", "status", "--porcelain"])),
        "source_sha256": fingerprint(),
        "rustc": command(["rustc", "-Vv"]),
        "cargo": command(["cargo", "-V"]),
        "stock_dependency_source": sqlite["source"],
        "libsqlite3_sys_version": sqlite["version"],
        "sqlite_amalgamation_sha256": hashlib.sha256(stock_source.read_bytes()).hexdigest(),
        "stock_source_matches_vendored_amalgamation": True,
        "method": "Separate executables; shared workloads and SQLite adapter; alternating samples after discarded warmup; median latency per unit.",
        "build_environment": {name: env.get(name, "") for name in
                              ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CFLAGS", "LIBSQLITE3_FLAGS",
                               "LIBSQLITE3_SYS_USE_PKG_CONFIG", "CARGO_BUILD_TARGET")},
    }
    return executables, metadata


def measure(executable, label, config, path):
    args = [str(executable), "--engine", "sqlite" if label == "stock" else "boxdb",
            "--boxdb-format", config["format"], "--samples", "1", "--warmup", "false",
            "--directory", config["directory"], "--json", str(path)]
    for option in ("rows", "reads", "read_repetitions", "batch_size", "writes"):
        args.extend(["--" + option.replace("_", "-"), str(config[option])])
    if config["typed"]:
        args.append("--typed")
    logged_command(args, path.with_suffix(".log"))
    report = json.loads(path.read_text())
    expected_engine = "SQLite" if label == "stock" else (
        "BoxDB YAML" if config["format"] == "yaml" else "BoxDB binary")
    if len(report["samples"]) != 1 or report["samples"][0]["engine"] != expected_engine:
        raise ValueError(f"{path}: expected exactly one {expected_engine} sample")
    timings = report["samples"][0]["timings"]
    if len(timings) != 8 or {t["workload"] for t in timings} != set(WORKLOAD_UNITS):
        raise ValueError(f"{path}: incomplete workload sample")
    for timing in timings:
        elapsed = timing["elapsed_ms"]
        if (timing["unit"] != WORKLOAD_UNITS[timing["workload"]] or timing["units"] <= 0
                or not math.isfinite(elapsed) or elapsed <= 0):
            raise ValueError(f"{path}: invalid timing")
    return report


def compare(stock, boxdb):
    for field in ("sqlite_version", "sqlite_settings", "os", "arch", "value_size", "value_alignment"):
        if stock[field] != boxdb[field]:
            raise ValueError(f"{field} differs between engines")
    if stock["decimal_cast_type"] != "real" or boxdb["decimal_cast_type"] != "decimal":
        raise ValueError("engine probes did not identify stock SQLite and native BoxDB DECIMAL")
    comparisons = []
    for name, unit in WORKLOAD_UNITS.items():
        per_unit = []
        for report in (stock, boxdb):
            timings = [next(t for t in sample["timings"] if t["workload"] == name)
                       for sample in report["samples"]]
            per_unit.append(median(t["elapsed_ms"] * 1000 / t["units"] for t in timings))
        before, after = per_unit
        comparisons.append({"workload": name, "unit": unit, "stock_us_per_unit": before,
                            "boxdb_us_per_unit": after, "boxdb_over_sqlite": after / before,
                            "change_percent": (after / before - 1) * 100,
                            "added_us_per_unit": after - before})
    return comparisons


def markdown(report):
    config, metadata = report["config"], report["metadata"]
    version = report["runs"][0]["stock"]["sqlite_version"]
    lines = ["# BoxDB / Unmodified SQLite", "",
             f'SQLite {version}; {config["rows"]} rows; {config["samples"]} measured samples per engine.',
             f'Collected: {metadata["collected_at"]}; Git: {metadata["git_revision"]}'
             + (" (working tree modified)." if metadata["working_tree_dirty"] else "."), "",
             metadata["method"], "",
             "Positive change means BoxDB takes longer. All latencies are microseconds per unit.",
             "YAML reads execute in memory; YAML commits and opening include whole-file serialization."]
    for run in report["runs"]:
        lines.extend(["", f'## {run["format"]} / {"VARCHAR" if run["typed"] else "ordinary"}', "",
                      "| Workload | Unit | SQLite µs | BoxDB µs | Ratio | Change | Added µs |",
                      "| --- | --- | ---: | ---: | ---: | ---: | ---: |"])
        for c in run["comparisons"]:
            lines.append(f'| {c["workload"]} | {c["unit"]} | {c["stock_us_per_unit"]:.3f} | '
                         f'{c["boxdb_us_per_unit"]:.3f} | {c["boxdb_over_sqlite"]:.2f}x | '
                         f'{c["change_percent"]:+.1f}% | {c["added_us_per_unit"]:+.3f} |')
    return "\n".join(lines) + "\n"


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option, default in (("rows", 1000), ("samples", 7), ("reads", 100),
                            ("read-repetitions", 20), ("batch-size", 100), ("writes", 20)):
        parser.add_argument("--" + option, type=positive_int, default=default)
    parser.add_argument("--directory", type=Path, default=Path(tempfile.gettempdir()),
                        help="existing parent directory for temporary databases")
    parser.add_argument("--output", type=Path,
                        help="new report directory (default: target/performance/stock-sqlite-TIMESTAMP)")
    parser.add_argument("--boxdb-format", choices=("all", "binary", "yaml"), default="all")
    parser.add_argument("--schema", choices=("all", "ordinary", "varchar"), default="all")
    args = parser.parse_args()
    if args.rows > 2**31 - 1:
        parser.error("--rows must not exceed 2147483647")
    output = (args.output or ROOT / "target/performance" /
              ("stock-sqlite-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))).resolve()
    if not args.directory.is_dir():
        parser.error("--directory must be an existing directory")
    try:
        output.mkdir(parents=True, exist_ok=False)
        raw = output / "raw"
        raw.mkdir()
        executables, metadata = build(output)
        report = {"schema_version": 1, "metadata": metadata,
                  "config": {**vars(args), "directory": str(args.directory.resolve()), "output": str(output)},
                  "runs": []}
        write_json(output / "metadata.json", metadata)
        formats = ("binary", "yaml") if args.boxdb_format == "all" else (args.boxdb_format,)
        schemas = (False, True) if args.schema == "all" else (args.schema == "varchar",)
        profiles = [(storage, typed) for storage in formats for typed in schemas]
        for index, (storage, typed) in enumerate(profiles):
            name = storage + ("-varchar" if typed else "-ordinary")
            config = {**report["config"], "format": storage, "typed": typed}
            merged = {}
            for iteration in range(args.samples + 1):
                order = ("stock", "boxdb") if (index + iteration) % 2 == 0 else ("boxdb", "stock")
                print(f"{name}: {'warmup' if iteration == 0 else f'sample {iteration}/{args.samples}'}", flush=True)
                for label in order:
                    path = raw / f"{name}-{label}-{iteration}.json"
                    sample = measure(executables[label], label, config, path)
                    if iteration == 0:
                        merged[label] = {**sample, "samples": []}
                        merged[label]["config"] = {
                            **sample["config"], "samples": args.samples, "warmup": True, "json": None,
                        }
                        # Per-process summaries describe only its one sample;
                        # the final comparisons below use all measured samples.
                        merged[label].pop("summaries")
                        merged[label].pop("comparisons")
                    else:
                        for field in ("sqlite_version", "sqlite_settings", "os", "arch", "value_size", "value_alignment", "decimal_cast_type"):
                            if sample[field] != merged[label][field]:
                                raise ValueError(f"{field} changed between samples")
                        merged[label]["samples"].extend(sample["samples"])
            run = {"format": storage, "typed": typed, **merged,
                   "comparisons": compare(merged["stock"], merged["boxdb"])}
            report["runs"].append(run)
        write_json(output / "summary.json", report)
        text = markdown(report)
        (output / "summary.md").write_text(text, encoding="utf-8")
        print(text)
        print(f"Reports: {output}")
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    raise SystemExit(main())
