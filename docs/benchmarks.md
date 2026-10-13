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
file-size results depend on this dataset.

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
`BoxDB/SQLite` is BoxDB's median latency divided by SQLite's median latency for
the same workload and unit. Above 1 means BoxDB took longer; below 1 means it was
faster. JSON includes configuration, engine versions, OS/architecture, raw sample
durations, medians, and a `comparisons` array with `boxdb_over_sqlite`.

## How to interpret results

BoxDB explicitly uses `Format::Binary`. SQLite uses `journal_mode=DELETE` and
`synchronous=EXTRA`, with these defaults:

- `--sqlite-fullfsync true` enables SQLite's hardware flush on macOS, matching
  Rust's `File::sync_all` behavior there. It has no effect on other platforms.
- `--sqlite-cached true` reuses prepared statements for reads and writes. BoxDB
  caches parsed SQL and bound read plans internally.
- `--sqlite-exclusive true` holds SQLite's file locks for the connection
  lifetime, matching BoxDB's exclusive connection policy.

See SQLite's [synchronization](https://www.sqlite.org/pragma.html#pragma_synchronous),
[fullfsync](https://www.sqlite.org/pragma.html#pragma_fullfsync), and
[locking-mode](https://www.sqlite.org/pragma.html#pragma_locking_mode) documentation.
These settings do not make failure guarantees or storage architectures identical.
BoxDB holds typed rows in memory and synchronizes native changed-cell log records;
SQLite updates pages through its rollback journal. Both include durable commit
costs. Automatic BoxDB checkpoints are included if triggered by the workload.

Both engines materialize results into owned Rust values. The harness measures
public API execution, result allocation and persistence, rather than an isolated
file codec. Prepared reads are warmed before timing; writes include the first
preparation of previously unseen statements.

The original reports used uncached SQLite statements, normal locking and
`fullfsync=OFF`. To rerun that configuration against the current BoxDB:

```sh
cargo bench --locked --bench sqlite_vs_boxdb -- \
  --sqlite-fullfsync false --sqlite-cached false --sqlite-exclusive false \
  --json /tmp/boxdb-historical-sqlite.json
```

On macOS the old configuration performs a stronger flush for BoxDB than for
SQLite. Retaining it allows historical comparison; the default mode makes that
flush setting explicit and also gives SQLite prepared-statement and locking
optimizations.

Reopening includes a full scan because SQLite opens files lazily. The operating
system cache is not flushed, so this is a warm-cache comparison, not a cold-disk
test. Run on an idle machine and compare several dataset and batch sizes on the
same filesystem. Results are local measurements, not universal performance
claims; timings are not CI pass/fail thresholds.

CI runs the harness on a small dataset with `cargo test --bench sqlite_vs_boxdb`
to catch SQL or persistence regressions. That smoke test uses the unoptimized
test profile; use `cargo bench` for performance measurements.

## Saved comparison

All reports retain raw samples and `BoxDB/SQLite` ratios. The 1,000-row runs use
five measured samples, 100 reads, batches of 100, and 20 writes on macOS ARM64:

- [Before](../benchmarks/results/before.json): v1 compressed JSONL at commit
  `e2b2cf4`, with the original SQLite configuration.
- [First optimization](../benchmarks/results/after.json): v2 native snapshots,
  with the original SQLite configuration. This intermediate result is retained.
- [Optimized, historical SQLite](../benchmarks/results/optimized-historical-sqlite.json):
  v3 commit log and optimized executor, with the original SQLite configuration.
- [Optimized, matched settings](../benchmarks/results/optimized.json): v3, with
  the current default flush, cache and locking settings.
- [10,000 rows](../benchmarks/results/optimized-10000.json): the same current
  defaults and five samples, at a larger dataset size.

| Workload | v1 / historical SQLite | v3 / historical SQLite | v3 / matched SQLite |
| --- | ---: | ---: | ---: |
| `insert_transaction` | 21.14x | 5.40x | 0.34x |
| `point_lookup` | 1.97x | 0.07x | 0.58x |
| `range_scan` | 63.26x | 0.30x | 0.41x |
| `aggregate` | 4.35x | 0.23x | 0.28x |
| `update_transaction` | 76.03x | 7.00x | 0.32x |
| `update_autocommit` | 44.93x | 14.69x | 0.24x |
| `delete_transaction` | 138.97x | 13.91x | 0.27x |
| `reopen_scan` | 18.76x | 3.59x | 2.35x |

Compare the first two numeric columns to evaluate the historical ratio change.
The third uses a different SQLite denominator and must not be treated as the
same experiment. At matched settings, BoxDB is faster in seven workloads here;
reopening and scanning remains slower. At 10,000 rows, matched ratios range from
0.21x to 0.76x except reopening at 2.90x.

Reports record the compiler, Git base revision, collection time, binary version,
and source SHA-256. New reports identify uncommitted source changes; their Git
base alone does not describe the measured implementation. The fingerprint hashes
sorted `src/**/*.rs`, `benches/**/*.rs`, `Cargo.toml` and `Cargo.lock`, concatenating
each relative path, a NUL separator and its contents. Historical timing samples
are unchanged; their comparison fields were derived from their saved medians.

File sizes are not directly comparable compression ratios: v1 compressed the
snapshot, while v2 and v3 store native cells without compression. V3 files also
include appended commit frames until a checkpoint. Compare latency and storage
size separately.
