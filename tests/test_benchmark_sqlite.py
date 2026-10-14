"""Run the stock SQLite comparison through its public CLI."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
WORKLOADS = {
    "insert_transaction", "point_lookup", "range_scan", "aggregate",
    "update_transaction", "update_autocommit", "delete_transaction", "reopen_scan",
}


class StockSqliteBenchmark(unittest.TestCase):
    def test_just_command_selects_one_profile_and_forwards_workload_settings(self):
        with tempfile.TemporaryDirectory(prefix="boxdb benchmark ") as directory:
            output = Path(directory) / "report 'quoted'"
            result = subprocess.run(
                ["just", "--justfile", str(ROOT / "justfile"), "bench-sqlite",
                 "--output", str(output), "--directory", directory,
                 "--boxdb-format", "binary", "--schema", "ordinary",
                 "--rows", "10", "--samples", "1", "--reads", "2",
                 "--read-repetitions", "3", "--batch-size", "4", "--writes", "50"],
                cwd=directory, text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            report = json.loads((output / "summary.json").read_text())
            self.assertEqual(len(report["runs"]), 1)
            run = report["runs"][0]
            self.assertEqual((run["format"], run["typed"]), ("binary", False))
            for engine in (run["stock"], run["boxdb"]):
                self.assertEqual(engine["config"]["samples"], 1)
                self.assertEqual(len(engine["samples"]), 1)
                timings = {t["workload"]: t for t in engine["samples"][0]["timings"]}
                self.assertEqual(timings["insert_transaction"]["units"], 10)
                self.assertEqual(timings["point_lookup"]["units"], 6)
                self.assertEqual(timings["update_transaction"]["units"], 10)
            self.assertEqual(len(list((output / "raw").glob("*.json"))), 4)

    def test_command_runs_both_engines_and_reports_all_storage_profiles(self):
        with tempfile.TemporaryDirectory(prefix="boxdb benchmark ") as directory:
            output = Path(directory) / "report"
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools/benchmark_sqlite.py"),
                 "--output", str(output), "--directory", directory,
                 "--rows", "30", "--samples", "2", "--reads", "3",
                 "--read-repetitions", "2", "--batch-size", "7", "--writes", "4"],
                cwd=directory, text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            report = json.loads((output / "summary.json").read_text())
            self.assertTrue(report["metadata"]["stock_source_matches_vendored_amalgamation"])
            self.assertTrue(report["metadata"]["stock_dependency_source"].startswith("registry+"))
            self.assertEqual(
                {(run["format"], run["typed"]) for run in report["runs"]},
                {("binary", False), ("binary", True), ("yaml", False), ("yaml", True)},
            )
            for run in report["runs"]:
                with self.subTest(format=run["format"], typed=run["typed"]):
                    self.assertEqual(run["stock"]["decimal_cast_type"], "real")
                    self.assertEqual(run["boxdb"]["decimal_cast_type"], "decimal")
                    self.assertEqual(run["stock"]["sqlite_version"], run["boxdb"]["sqlite_version"])
                    self.assertEqual(len(run["stock"]["samples"]), 2)
                    self.assertEqual(len(run["boxdb"]["samples"]), 2)
                    self.assertEqual(run["stock"]["config"]["samples"], 2)
                    self.assertEqual(run["boxdb"]["config"]["samples"], 2)
                    self.assertEqual({c["workload"] for c in run["comparisons"]}, WORKLOADS)
                    for comparison in run["comparisons"]:
                        self.assertGreater(comparison["stock_us_per_unit"], 0)
                        self.assertGreater(comparison["boxdb_us_per_unit"], 0)
                        self.assertGreater(comparison["boxdb_over_sqlite"], 0)
            markdown = (output / "summary.md").read_text()
            self.assertIn("Unmodified SQLite", markdown)
            self.assertIn("Change", markdown)
            self.assertIn("reopen_scan", markdown)
            self.assertEqual(len(list((output / "raw").glob("*.json"))), 24)


if __name__ == "__main__":
    unittest.main()
