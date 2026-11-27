#!/usr/bin/env python3
"""Time readable YAML files of several sizes through the BoxDB CLI.

Each size is generated as a binary database, converted to a readable file and
then opened, updated and compacted by separate CLI processes. A second CLI,
such as an installed previous release, can be measured on the same data.
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SCHEMA = """CREATE TABLE items(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  email TEXT NOT NULL,
  amount DECIMAL(12,2) NOT NULL,
  created INSTANT NOT NULL,
  note TEXT
);"""

# About 150 bytes per row in the readable log.
FILL = """WITH RECURSIVE r(n) AS (VALUES({start}) UNION ALL SELECT n+1 FROM r WHERE n<{end})
INSERT INTO items SELECT n, 'user '||n, 'user'||n||'@example.com',
  CAST((n%100000)||'.'||(n%100) AS DECIMAL), CAST('2024-02-29T12:00:00Z' AS INSTANT),
  CASE WHEN n%3=0 THEN NULL ELSE 'note '||hex(randomblob(12)) END FROM r;"""


def run(cli, *args, stdin=None):
    """Run a CLI command; return seconds and peak resident memory in MiB."""
    command = ["/usr/bin/time", "-l", cli, *args] if sys.platform == "darwin" else [cli, *args]
    start = time.perf_counter()
    result = subprocess.run(command, input=stdin, capture_output=True, text=True)
    elapsed = time.perf_counter() - start
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: {result.stderr[-2000:]}")
    match = re.search(r"(\d+)\s+maximum resident set size", result.stderr)
    rss = int(match.group(1)) / 2**20 if match else float("nan")
    return elapsed, rss


def best(repeats, function):
    results = [function() for _ in range(repeats)]
    return min(results, key=lambda r: r[0])


def generate(cli, directory, megabytes):
    """A binary database whose readable log is about `megabytes` large."""
    probe = directory / "probe.db"
    run(cli, str(probe), "-e", SCHEMA + FILL.format(start=1, end=20000))
    log = directory / "probe.yaml"
    run(cli, "convert", str(probe), str(log), "--format", "readable")
    per_row = log.stat().st_size / 20000
    rows = int(megabytes * 2**20 / per_row)
    binary = directory / f"{megabytes}.db"
    run(cli, str(binary), "-e", SCHEMA + FILL.format(start=1, end=rows))
    probe.unlink()
    log.unlink()
    return binary, rows


def updates(rows, count, seed):
    rng = random.Random(seed)
    return [
        f"UPDATE items SET amount=amount+1, note='edited {i}' WHERE id={rng.randint(1, rows)};"
        for i in range(count)
    ]


def measure(cli, label, binary, rows, directory, repeats, history_mb):
    path = directory / f"{label}.yaml"
    if path.exists():
        path.unlink()
    run(cli, "convert", str(binary), str(path), "--format", "readable")
    size = path.stat().st_size
    result = {"format": label, "rows": rows, "bytes": size}
    result["open_s"], result["open_rss_mib"] = best(
        repeats, lambda: run(cli, str(path), "-e", "SELECT count(*) FROM items")
    )
    # Autocommit statements each append one line and synchronize the file.
    single = updates(rows, 100, 1)
    elapsed, _ = run(cli, str(path), "-e", "".join(single))
    result["autocommit_update_ms"] = (elapsed - result["open_s"]) / len(single) * 1000
    batch = updates(rows, 1000, 2)
    elapsed, _ = run(cli, str(path), "-e", "BEGIN;" + "".join(batch) + "COMMIT;")
    result["tx_1000_updates_ms"] = (elapsed - result["open_s"]) * 1000
    if history_mb:
        # Grow the file with transactions of 100 updates until it holds
        # `history_mb` more megabytes of history.
        target = size + history_mb * 2**20
        seed = 3
        while path.stat().st_size < target:
            script = "".join(
                "BEGIN;" + "".join(updates(rows, 100, seed + i)) + "COMMIT;" for i in range(50)
            )
            run(cli, str(path), "-e", script)
            seed += 50
        result["history_bytes"] = path.stat().st_size
        result["history_open_s"], result["history_open_rss_mib"] = best(
            repeats, lambda: run(cli, str(path), "-e", "SELECT count(*) FROM items")
        )
    result["vacuum_s"], _ = run(cli, str(path), "-e", "VACUUM")
    result["after_vacuum_bytes"] = path.stat().st_size
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", default="target/release/boxdb")
    parser.add_argument("--previous", help="CLI of an earlier release to compare")
    parser.add_argument("--sizes", default="20,50,100", help="megabytes of rows")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--history", type=int, default=0, help="megabytes of history to add")
    parser.add_argument("--output", help="write JSON results here")
    args = parser.parse_args()
    results = []
    with tempfile.TemporaryDirectory() as temp:
        directory = Path(temp)
        for megabytes in [int(s) for s in args.sizes.split(",")]:
            binary, rows = generate(args.cli, directory, megabytes)
            for label, cli, history in [("log", args.cli, args.history), ("previous", args.previous, 0)]:
                if not cli:
                    continue
                result = measure(cli, label, binary, rows, directory, args.repeats, history)
                result["target_mib"] = megabytes
                results.append(result)
                print(json.dumps(result), flush=True)
            binary.unlink()
    if args.output:
        Path(args.output).write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
