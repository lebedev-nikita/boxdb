# Replication with Litestream

BoxDB binary files are ordinary SQLite databases in WAL mode. Use the original
[Litestream](https://litestream.io/) to replicate their data and BoxDB's type
metadata. BoxDB has no built-in replication, S3 credentials, or remote restore
commands. Readable JSONL files are not Litestream databases; convert them to
binary storage when continuous replication is needed.

## Replicate and restore

Install Litestream using its [installation guide](https://litestream.io/install/).
For a local replica:

```sh
litestream replicate data.db file:///absolute/path/to/replica
litestream restore -o restored.db file:///absolute/path/to/replica
boxdb restored.db -e 'SELECT * FROM accounts'
```

Keep the replication process running alongside your application. To sync once,
use `litestream replicate -once data.db file:///absolute/path/to/replica`.
Restore into a new destination. For S3 and other remote storage, configure
credentials and replicas directly in Litestream; see its
[configuration reference](https://litestream.io/reference/config/).

Litestream requires WAL and opens its own SQLite connections. Keep normal
locking; an exclusive connection prevents it from accessing the database.
BoxDB's default busy timeout allows its writes to wait for short checkpoint
locks. Checkpoint tuning belongs to Litestream configuration; see
[tips and caveats](https://litestream.io/tips/).

Replication is asynchronous. Local COMMIT success does not mean the replica has
received that commit, and independent writers on separate replicas do not get
conflict merging. Restore-point granularity depends on available replication
files and retention. See [how Litestream works](https://litestream.io/how-it-works/).

## Verify locally

```sh
cargo test --locked --test litestream -- --ignored --nocapture
```

The test uses the installed original Litestream, a temporary file replica, and
a live BoxDB writer. It replicates two batches, restores a new SQLite file, and
checks typed results and validation after recovery. Local file replication was
verified with Litestream 0.5.17; live cloud storage is not tested by this suite.
