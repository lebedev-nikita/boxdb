"""Contract tests for the benchmark report checker, through its CLI."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
WORKLOADS = {
    "insert_transaction": ("row", 1000),
    "point_lookup": ("query", 2000),
    "range_scan": ("query", 2000),
    "aggregate": ("query", 2000),
    "update_transaction": ("row", 20),
    "update_autocommit": ("row", 20),
    "delete_transaction": ("row", 20),
    "reopen_scan": ("database", 1),
}


def report(latency=10.0, storage="binary", typed=False):
    engine = "BoxDB YAML" if storage == "yaml" else "BoxDB binary"
    return {
        "sqlite_version": "test-version",
        "os": "test-os",
        "arch": "test-arch",
        "sqlite_settings": "matched-settings",
        "config": {
            "rows": 1000, "samples": 7, "reads": 100,
            "read_repetitions": 20, "batch_size": 100, "writes": 20,
            "sqlite_fullfsync": True, "sqlite_cached": True,
            "sqlite_exclusive": False, "typed": typed,
            "boxdb_format": storage, "directory": None, "json": None,
        },
        "samples": [
            {
                "engine": client,
                "timings": [
                    {"workload": name, "unit": unit, "units": units,
                     "elapsed_ms": latency * units / 1000}
                    for name, (unit, units) in WORKLOADS.items()
                ],
            }
            for _ in range(7)
            for client in ("SQLite", engine)
        ],
    }


class PerformanceCheck(unittest.TestCase):
    def check_reports(self, baseline, current, *options):
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("base.json", "current.json")]
            for path, data in zip(paths, (baseline, current)):
                path.write_text(json.dumps(data))
            return subprocess.run(
                [sys.executable, str(ROOT / "tools/check_performance.py"),
                 *map(str, paths), *options],
                text=True, capture_output=True, check=False,
            )

    def test_every_sql_workload_rejects_a_slowdown_in_every_storage_profile(self):
        for storage in ("binary", "yaml"):
            for typed in (False, True):
                for workload in WORKLOADS:
                    with self.subTest(storage=storage, typed=typed, workload=workload):
                        baseline = report(storage=storage, typed=typed)
                        current = report(storage=storage, typed=typed)
                        for sample in current["samples"]:
                            if sample["engine"] != "SQLite":
                                for timing in sample["timings"]:
                                    if timing["workload"] == workload:
                                        timing["elapsed_ms"] *= 1.5
                        result = self.check_reports(baseline, current)
                        self.assertEqual(result.returncode, 1, result.stderr)
                        self.assertIn(f"REGRESSION {workload}", result.stdout)

    def test_different_workload_settings_are_not_comparable(self):
        baseline = report()
        current = report()
        current["config"]["rows"] = 10000
        result = self.check_reports(baseline, current)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("config.rows differs", result.stderr)

    def test_nonfinite_or_negative_tolerances_are_rejected(self):
        for option in ("--max-regression-percent", "--noise-floor-us"):
            for value in ("nan", "inf", "-1"):
                with self.subTest(option=option, value=value):
                    result = self.check_reports(report(), report(), option, value)
                    self.assertEqual(result.returncode, 2, result.stdout)

    def test_unchanged_and_faster_reports_pass_with_different_output_paths(self):
        for latency in (10.0, 8.0, 12.9):
            with self.subTest(latency=latency):
                baseline = report()
                current = report(latency)
                current["config"]["json"] = "/another/current.json"
                current["config"]["directory"] = "/another/temp-directory"
                result = self.check_reports(baseline, current)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.count("PASS "), 8)

    def test_one_outlier_does_not_change_the_median(self):
        current = report()
        for timing in current["samples"][1]["timings"]:
            timing["elapsed_ms"] *= 100
        result = self.check_reports(report(), current)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_small_absolute_changes_are_allowed_by_the_noise_floor(self):
        result = self.check_reports(report(1.0), report(1.4))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_custom_tolerance_can_detect_a_smaller_regression(self):
        result = self.check_reports(report(), report(11.0),
                                    "--max-regression-percent", "5")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_platform_and_sqlite_settings_must_match(self):
        for field in ("os", "arch", "sqlite_version", "sqlite_settings"):
            with self.subTest(field=field):
                current = report()
                current[field] = "different"
                result = self.check_reports(report(), current)
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertIn(f"{field} differs", result.stderr)

    def test_incomplete_or_invalid_measurements_never_pass(self):
        for defect in ("missing workload", "missing sample", "too few samples",
                       "zero duration", "negative duration", "nan duration",
                       "zero units", "wrong unit", "duplicate workload"):
            with self.subTest(defect=defect):
                current = report()
                timing = current["samples"][1]["timings"][0]
                if defect == "missing workload":
                    current["samples"][1]["timings"].pop()
                elif defect == "missing sample":
                    current["samples"].pop()
                elif defect == "too few samples":
                    current["config"]["samples"] = 1
                    current["samples"] = current["samples"][:2]
                elif defect == "zero duration":
                    timing["elapsed_ms"] = 0
                elif defect == "negative duration":
                    timing["elapsed_ms"] = -1
                elif defect == "nan duration":
                    timing["elapsed_ms"] = float("nan")
                elif defect == "zero units":
                    timing["units"] = 0
                elif defect == "wrong unit":
                    timing["unit"] = "query"
                else:
                    current["samples"][1]["timings"].append(timing.copy())
                # Both reports can be equally broken; matching isn't sufficient.
                result = self.check_reports(current, current)
                self.assertEqual(result.returncode, 2, result.stdout)


if __name__ == "__main__":
    unittest.main()
