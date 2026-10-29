# Deployment and backups

BoxDB is an embedded library. Deploy your application and place its database on
persistent local storage. No separate BoxDB service or network endpoint is needed.
The project is experimental; pin versions and rehearse upgrades with copies of
your own data.

## Choose storage

Use **binary storage** for application data, concurrent connections, FTS5 and
vector indexes. It opens in WAL mode with FULL synchronization and a five-second
default busy timeout. SQLite still serializes writers; keep write transactions
short and measure contention on your workload.

Use **readable YAML** for small datasets where text review and Git diffs matter.
It executes in memory, permits one writer per file and publishes a complete
snapshot per commit. Its file limit is 256 MiB. Group writes in transactions;
do not use it for virtual tables. See the [storage contract](storage.md).

## Deploy the application

1. Pin BoxDB and its client/adapter versions. For Go, match the native archive
   to the Go module release and build with cgo enabled.
2. Put the database on a persistent local volume writable by the application.
   For binary files, allow creation of SQLite's `-wal` and `-shm` sidecars.
3. Apply [migrations](migrations.md) as a controlled deployment step. Keep applied
   migration files unchanged and retain the complete migration history.
4. Open a bounded number of connections. Go memory/readable databases need a
   one-connection pool; TypeScript transactions must use their transaction handle.
5. Close clients during shutdown and allow transaction cleanup to finish.

Check the client's supported OS and architecture before choosing a container
image. npm builds use glibc 2.35+ on Linux; Alpine/musl is unsupported by the npm
release. Never put a database on an ephemeral container filesystem unless losing
it on replacement is intentional.

## Make and restore a snapshot

```sh
boxdb convert data.db backup.db --format binary
boxdb check backup.db --full
```

`backup.db` must be a new path. Conversion reads committed data consistently and
includes committed WAL pages. Do not copy only the live `.db` file while ignoring
its WAL. Preserve snapshots outside the application's disk and test that your
application can open a restored copy.

For continuous binary replication, run [Litestream](replication.md) alongside
the application. Replication is asynchronous: a local commit does not imply that
the replica has received it. Readable YAML needs its own file-backup strategy.

To inspect a restored file:

```sh
boxdb check restored.db --full --json
```

Inspect the report's status and completeness before returning it to service.
If a file is damaged, preserve the source and its sidecars and read
[diagnostics and recovery](recovery.md). Recovery may lose data.

## Upgrade deliberately

Read [release notes](https://github.com/lebedev-nikita/boxdb/releases), retain a
recoverable backup and verify the new version against a copy before deploying.
Version 0 does not promise compatibility with earlier APIs or BoxDB storage
contracts. A newer engine opening one sample file is not an upgrade guarantee
for every schema and query.
