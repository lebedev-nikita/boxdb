# SQLite vs BoxDB storage

Both clients use bundled SQLite. By default, both store ordinary SQLite files;
`--boxdb-format yaml` instead measures BoxDB's readable YAML storage against
file-backed rusqlite. Timings include BoxDB's public API, result allocation,
type validation and durable publication.

```sh
cargo bench --locked --bench sqlite_vs_boxdb
cargo bench --locked --bench sqlite_vs_boxdb -- --help
cargo bench --locked --bench sqlite_vs_boxdb -- \
  --rows 1000 --samples 7 --reads 100 --read-repetitions 20 --batch-size 100 --writes 20 \
  --json /tmp/boxdb-benchmark.json
cargo bench --locked --bench sqlite_vs_boxdb -- \
  --rows 1000 --samples 7 --read-repetitions 20 --typed --json /tmp/boxdb-typed-benchmark.json
cargo bench --locked --bench sqlite_vs_boxdb -- \
  --boxdb-format yaml --rows 10000 --samples 5 --read-repetitions 20 \
  --json /tmp/boxdb-yaml-benchmark.json
```

No SQLite CLI installation is needed. Every sample creates a fresh temporary
database; `--directory /existing/path` selects its filesystem. Temporary
databases are removed automatically; requested JSON reports are retained.

## Measurement

[The harness](../benches/sqlite_vs_boxdb.rs) generates identical deterministic
integer/text rows, SQL and expected results outside timers. Both clients have
an INTEGER primary key and an additional explicit ID index. Every workload is
verified, including committed data after reopening.

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

Updates run twice on selected rows, then deletion removes them. `--writes` is
capped at the dataset size. Read workloads run before mutations; every query is
verified and warmed before timing. The warmed set is repeated according to
`--read-repetitions`; latency is divided by the total executed queries. Schema
creation and correctness checks are excluded. Writes include transaction and commit costs, and the initial
preparation of previously unseen SQL.

One full warmup sample per client is discarded. Measured samples alternate
client order. Reports contain raw durations, medians, configuration, SQLite
version and OS/architecture. `BoxDB/SQLite` divides BoxDB's median microseconds
per unit by SQLite's median for the same workload. Above 1 means BoxDB takes
longer. The console and JSON `comparisons[].boxdb_over_sqlite` always show it.

Storage sizes are sampled after insertion: `database_bytes`, `wal_bytes` and
`total_storage_bytes` include the main file and WAL, excluding shared-memory
and lock files. Text payloads include deterministic SHA-256 digests; sizes are
specific to that dataset.

## Comparable settings

Binary-mode defaults match both clients: WAL, FULL synchronization, `fullfsync=ON`, normal
locking, foreign keys enabled, a five-second busy timeout, and a prepared
statement cache of 512 entries. See SQLite's
[synchronization](https://www.sqlite.org/pragma.html#pragma_synchronous),
[fullfsync](https://www.sqlite.org/pragma.html#pragma_fullfsync), and
[locking](https://www.sqlite.org/pragma.html#pragma_locking_mode) references.
The `--sqlite-*` switches change only the reference client; non-default reports
must be interpreted using their saved configuration.

YAML mode executes SQL in memory and atomically replaces a complete YAML file
on each commit, synchronizing the temporary file and its parent directory. Its
reads do not include file access after opening. These are different storage
architectures, so YAML ratios compare end-to-end behavior rather than client
overhead alone. The SQLite reference settings are unchanged in YAML runs.

Both clients materialize owned Rust rows. BoxDB additionally returns column
names, supports SQL batches, monitors external changes and decodes extended
types. Ordinary SQLite SQL bypasses the BoxDB DDL parser. Prepared SQL and
column-type information are cached; external changes invalidate type caches.

With `--typed`, BoxDB declares the payload as VARCHAR(100), adding metadata and
connection-local validation triggers. The reference uses TEXT with an equivalent
length CHECK. Both enforce the length bound, while BoxDB also preserves its
logical schema and validates values on reads. This measures one inexpensive
type check, not UUID, JSON, decimal, date or array parsing costs.

Reopening includes a full scan because SQLite opens files lazily. The OS cache
is not flushed, so results represent a warm cache. Run on an idle machine and
compare multiple dataset and batch sizes on the same filesystem. Local timings
are not universal performance claims or CI pass/fail thresholds. CI separately
checks correctness on a small dataset in binary and typed YAML modes.

## Binary reports

All three runs use seven measured samples, 100 read queries repeated 20 times
per workload, INSERT batches of 100, and 20 writes on macOS ARM64 with matching
default settings:

- [1,000 rows](../benchmarks/results/sqlite-client.json).
- [10,000 rows](../benchmarks/results/sqlite-client-10000.json).
- [1,000 rows with VARCHAR validation](../benchmarks/results/sqlite-client-typed.json).

| Workload | 1,000 rows | 10,000 rows | 1,000 rows, VARCHAR |
| --- | ---: | ---: | ---: |
| `insert_transaction` | 1.00x | 1.05x | 1.03x |
| `point_lookup` | 1.23x | 1.25x | 1.22x |
| `range_scan` | 1.10x | 1.11x | 1.11x |
| `aggregate` | 1.00x | 1.00x | 1.02x |
| `update_transaction` | 1.05x | 1.18x | 1.26x |
| `update_autocommit` | 0.98x | 1.01x | 1.01x |
| `delete_transaction` | 1.00x | 0.96x | 1.02x |
| `reopen_scan` | 1.15x | 1.12x | 1.24x |

These are BoxDB/SQLite ratios. In the 1,000-row ordinary-schema run, point
lookups take 1.784 µs versus 1.452 µs, an added 0.332 µs.
The largest measured ratio across these runs is 1.26x; richer type parsing can
cost more than the VARCHAR case. Durable write timings also vary with filesystem
flushes and background load.

After insertion, ordinary schemas use 144,208 bytes at 1,000 rows and 1,108,288
bytes at 10,000 rows for either client (main file plus WAL). The typed run uses
152,448 bytes for BoxDB versus 144,208 for SQLite, including BoxDB's metadata.

## YAML reports

The before/after runs use five measured samples, 100 queries repeated 20 times,
INSERT batches of 100, and 20 writes on the same macOS ARM64 filesystem with
the same SQLite reference settings. The before implementation is the YAML
codec from `380942b`, with the YAML benchmark option added.

- 1,000 rows: [before](../benchmarks/results/yaml-before.json),
  [after](../benchmarks/results/yaml-after.json).
- 10,000 rows: [before](../benchmarks/results/yaml-before-10000.json),
  [after](../benchmarks/results/yaml-after-10000.json).

The 10,000-row measurements below include all eight database workloads.
Latency is microseconds per row, query or database as listed above. Speedup
divides the old YAML latency by the new YAML latency; the last two columns
divide each run's YAML latency by its own SQLite reference latency.

| Workload | YAML before µs | YAML after µs | YAML speedup | Before / SQLite | After / SQLite |
| --- | ---: | ---: | ---: | ---: | ---: |
| `insert_transaction` | 5.426 | 3.199 | 1.70x | 3.37x | 2.00x |
| `point_lookup` | 0.832 | 0.931 | 0.89x | 0.56x | 0.62x |
| `range_scan` | 8.972 | 9.794 | 0.92x | 0.92x | 1.04x |
| `aggregate` | 1,293.989 | 1,302.257 | 0.99x | 1.00x | 1.00x |
| `update_transaction` | 2,182.960 | 617.600 | 3.53x | 6.49x | 2.11x |
| `update_autocommit` | 41,259.506 | 11,349.894 | 3.64x | 9.08x | 2.52x |
| `delete_transaction` | 2,016.517 | 568.642 | 3.55x | 8.48x | 2.89x |
| `reopen_scan` | 205,016.333 | 87,242.042 | 2.35x | 78.98x | 38.65x |

At 1,000 rows, opening and scanning falls from 21.489 ms to 8.337 ms (2.58x),
and an autocommit update from 14.050 ms to 10.044 ms (1.40x). The corresponding
YAML/SQLite ratios fall from 32.40x to 12.11x and from 2.58x to 2.17x.
Read workloads already execute in memory; these measurements show no read
speedup. In the 10,000-row run, point and range queries are slightly slower,
by 0.099 µs and 0.822 µs respectively. Opening remains much slower than SQLite,
which loads pages on demand instead of parsing a complete text database.

Import reuses table layouts and prepared INSERTs. Export streams borrowed
SQLite cells; publication retains encoded rows and regenerates only changed
rows before writing the complete file. Both synchronization calls and atomic
replacement are preserved. Single-row commits remain limited by full-file
writes and filesystem synchronization; explicit transactions amortize these
costs. See [readable storage](storage.md#independent-readable-yaml-stream) for
cache memory costs and tables that require complete reserialization.

The YAML files remain 177,060 bytes at 1,000 rows and 1,787,072 bytes at 10,000
rows after insertion. The SQLite reference uses 144,208 and 1,108,288 bytes
respectively, including WAL. This workload measures integer/text rows, not
large BLOBs or expensive logical type validators.

## Previous storage implementations

Earlier reports are retained unchanged for comparison. Their SQLite reference
used a DELETE rollback journal and EXTRA synchronization, whereas the current
clients both use SQLite WAL/FULL. Their ratios describe their own experiments;
changes in the reference settings prevent treating them as a single controlled
before/after speedup measurement.

- [Before](../benchmarks/results/before.json): v1 compressed JSONL, original
  SQLite settings (uncached, normal locking, fullfsync off).
- [First optimization](../benchmarks/results/after.json): v2 native snapshots,
  original SQLite settings.
- [Optimized with historical SQLite settings](../benchmarks/results/optimized-historical-sqlite.json):
  v3 native commit log and executor, original SQLite settings.
- [Optimized with matched native-engine settings](../benchmarks/results/optimized.json):
  v3, SQLite prepared statements, exclusive locking and fullfsync on.
- [10,000 native-engine rows](../benchmarks/results/optimized-10000.json):
  v3 with those matched settings.

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

Saved reports also record compiler, Git base revision, collection time, storage
format and source SHA-256. When source changes were uncommitted, the Git base
alone does not identify the measured implementation. The fingerprint hashes
sorted `src/**/*.rs`, `benches/**/*.rs`, `Cargo.toml` and `Cargo.lock`, concatenating
each relative path, a NUL separator and its contents.
