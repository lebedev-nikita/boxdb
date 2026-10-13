# SQLite vs binary BoxDB

Run the embedded Rust APIs in Cargo's optimized benchmark profile:

```sh
cargo bench --locked --bench sqlite_vs_boxdb
cargo bench --locked --bench sqlite_vs_boxdb -- --help
cargo bench --locked --bench sqlite_vs_boxdb -- \
  --rows 1000 --samples 5 --reads 100 --batch-size 100 --writes 20 \
  --json /tmp/boxdb-benchmark.json
```

SQLite is bundled through the development-only `rusqlite` dependency; no SQLite
CLI installation is needed. Both engines create fresh temporary files for every
sample. Use `--directory /existing/path` to choose the filesystem. Temporary
databases are removed automatically; the JSON report is kept at the requested path.

## What is measured

[The harness](../benches/sqlite_vs_boxdb.rs) generates identical deterministic
integer/text rows, SQL and expected results before timing. Both databases have
the same primary key and explicit ID index. Every workload is checked against
expected rows outside its timer, including persisted data after reopening.
Text payloads contain an ID prefix and a deterministic SHA-256 hex digest;
compression and file-size results depend on this dataset.

| Workload | Timed work | Latency unit |
| --- | --- | --- |
| `insert_transaction` | Multi-value INSERTs inside one BEGIN/COMMIT | Inserted row |
| `point_lookup` | Equality queries on indexed IDs | Query |
| `range_scan` | Ordered ID ranges returning up to 50 rows | Query |
| `aggregate` | COUNT and SUM grouped by category | Query |
| `update_transaction` | Point UPDATEs inside one BEGIN/COMMIT | Updated row |
| `update_autocommit` | The same UPDATEs with a commit per statement | Updated row |
| `delete_transaction` | Point DELETEs inside one BEGIN/COMMIT | Deleted row |
| `reopen_scan` | Open the committed file and read all remaining rows | Database |

Updates run twice on the same selected rows, then deletion removes those rows.
`--writes` is capped at the dataset size. Read workloads run before mutations;
each query is verified and warmed before measurement. Writes include transaction
and commit costs. Schema creation, SQL generation and correctness checks are
excluded. The file size is measured after insertion, excluding sidecars.

One complete sample per engine warms the process and is discarded. Subsequent
samples alternate engine order. The table reports median microseconds per unit;
`BoxDB/SQLite` above 1 means BoxDB took longer. JSON includes configuration,
engine versions, OS/architecture, raw sample durations and medians.

## How to interpret results

BoxDB explicitly uses `Format::Binary`. SQLite uses `journal_mode=DELETE` and
`synchronous=EXTRA`: commits include syncing, without deferred WAL checkpoints.
See SQLite's [synchronization documentation](https://www.sqlite.org/pragma.html#pragma_synchronous).
These settings enable durable writes but do not make the storage architectures
or failure guarantees identical. BoxDB holds the dataset in memory and publishes
a compressed full snapshot per commit; SQLite updates pages through its rollback
journal. Batching can therefore change the comparison substantially.

Queries parse/prepare SQL on every invocation and materialize all result rows
into owned Rust values. SQLite does not reuse prepared statements because BoxDB
has no equivalent API. This measures the current public APIs, including parsing,
execution, result allocation and persistence; it is not an isolated file-codec
benchmark.

Reopening includes a full scan because SQLite opens files lazily. The operating
system cache is not flushed, so this is a warm-cache comparison, not a cold-disk
test. Run on an idle machine and compare several dataset and batch sizes on the
same filesystem. Results are local measurements, not universal performance
claims; timings are not CI pass/fail thresholds.

CI runs the harness on a small dataset with `cargo test --bench sqlite_vs_boxdb`
to catch SQL or persistence regressions. That smoke test uses the unoptimized
test profile; use `cargo bench` for performance measurements.
