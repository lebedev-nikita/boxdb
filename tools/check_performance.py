#!/usr/bin/env python3
"""Compare same-machine BoxDB benchmark reports; exit 1 on a regression."""

import argparse
import json
import math
from pathlib import Path
from statistics import median


CONFIG_FIELDS = (
    "rows", "samples", "reads", "read_repetitions", "batch_size", "writes",
    "sqlite_fullfsync", "sqlite_cached", "sqlite_exclusive", "typed", "boxdb_format",
)
WORKLOAD_UNITS = {
    "insert_transaction": "row", "point_lookup": "query", "range_scan": "query",
    "aggregate": "query", "update_transaction": "row", "update_autocommit": "row",
    "delete_transaction": "row", "reopen_scan": "database",
}


def positive_number(value):
    return type(value) in (int, float) and math.isfinite(value) and value > 0


def validate_comparison(baseline, current):
    for field in ("os", "arch", "sqlite_version", "sqlite_settings"):
        if baseline[field] != current[field]:
            raise ValueError(f"{field} differs between reports")
    for field in CONFIG_FIELDS:
        if baseline["config"][field] != current["config"][field]:
            raise ValueError(f"config.{field} differs between reports")


def latencies(report):
    config = report["config"]
    if config["boxdb_format"] not in ("binary", "yaml"):
        raise ValueError("unknown BoxDB format")
    count = config["samples"]
    if type(count) is not int or count < 5:
        raise ValueError("at least five measured samples are required")
    engine = "BoxDB YAML" if config["boxdb_format"] == "yaml" else "BoxDB binary"
    values = {name: [] for name in WORKLOAD_UNITS}
    counts = {"SQLite": 0, engine: 0}
    expected_units = {
        "insert_transaction": config["rows"],
        "point_lookup": config["reads"] * config["read_repetitions"],
        "range_scan": config["reads"] * config["read_repetitions"],
        "aggregate": config["reads"] * config["read_repetitions"],
        "update_transaction": min(config["writes"], config["rows"]),
        "update_autocommit": min(config["writes"], config["rows"]),
        "delete_transaction": min(config["writes"], config["rows"]),
        "reopen_scan": 1,
    }
    for sample in report["samples"]:
        counts[sample["engine"]] += 1
        names = [timing["workload"] for timing in sample["timings"]]
        if len(names) != len(WORKLOAD_UNITS) or set(names) != set(WORKLOAD_UNITS):
            raise ValueError("each sample must contain all eight workloads exactly once")
        for timing in sample["timings"]:
            name = timing["workload"]
            if (timing["unit"] != WORKLOAD_UNITS[name]
                    or not positive_number(timing["units"])
                    or timing["units"] != expected_units[name]
                    or not positive_number(timing["elapsed_ms"])):
                raise ValueError(f"invalid measurement for {name}")
        if sample["engine"] == engine:
            for timing in sample["timings"]:
                values[timing["workload"]].append(
                    timing["elapsed_ms"] * 1000 / timing["units"]
                )
    if any(actual != count for actual in counts.values()):
        raise ValueError("sample counts do not match config.samples")
    return {name: median(samples) for name, samples in values.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("--max-regression-percent", type=float, default=30.0,
                        help="allowed median latency increase (default: 30 percent)")
    parser.add_argument("--noise-floor-us", type=float, default=0.5,
                        help="minimum allowed increase per unit (default: 0.5 microseconds)")
    args = parser.parse_args()
    for value in (args.max_regression_percent, args.noise_floor_us):
        if not math.isfinite(value) or value < 0:
            parser.error("tolerances must be finite and nonnegative")
    try:
        baseline_report = json.loads(args.baseline.read_text())
        current_report = json.loads(args.current.read_text())
        validate_comparison(baseline_report, current_report)
        baseline = latencies(baseline_report)
        current = latencies(current_report)
    except (OSError, ValueError, KeyError, TypeError, ZeroDivisionError) as error:
        parser.error(str(error))
    failed = False
    print("status workload baseline_us/unit current_us/unit change limit_us/unit")
    for name, before in baseline.items():
        after = current[name]
        limit = before + max(before * args.max_regression_percent / 100, args.noise_floor_us)
        regression = after > limit
        failed |= regression
        print(f"{'REGRESSION' if regression else 'PASS'} {name} "
              f"{before:.3f} {after:.3f} {(after / before - 1) * 100:+.1f}% {limit:.3f}")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
