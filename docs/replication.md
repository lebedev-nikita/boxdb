# S3 setup and recovery

Use this guide to back up an existing BoxDB file and restore a stored
snapshot. Both storage formats are supported. The replication process can run
while a database writer remains open.

## Configure access

Use a dedicated bucket prefix for each database and one active publisher per
prefix. Grant GetObject and PutObject access on the prefix and ListBucket access
on the bucket: [S3 returns 403 instead of 404 for missing keys without
ListBucket](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html).

Configure a static access-key profile in the [shared AWS credentials
file](https://docs.aws.amazon.com/sdkref/latest/guide/file-format.html), then select
it and the bucket's region:

```sh
export AWS_PROFILE=backup
export AWS_REGION=eu-central-1
```

Environment keys (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally
`AWS_SESSION_TOKEN`) take precedence over profiles; partial credentials are an
error. Use `AWS_SHARED_CREDENTIALS_FILE` to select another credentials file.
SSO, role assumption, and metadata credential providers are not implemented.

## Replicate and restore

Replace `my-backups` with your bucket. To stream committed snapshots, run:

```sh
boxdb replicate data.jsonl --bucket my-backups --prefix accounts
```

The process retries failed uploads until stopped. Add `--once` for one
synchronization; failure then exits with an error. For a local S3-compatible
service, specify its endpoint and region:

```sh
boxdb replicate data.jsonl --bucket backups --prefix accounts --once \
  --endpoint http://localhost:9000 --region us-east-1
```

Restore into a new destination; the snapshot retains its original storage format:

```sh
boxdb restore restored.boxdb --bucket my-backups --prefix accounts
```

To recover an older snapshot, set `REVISION` to its revision hash from replication
output and add `--revision "$REVISION"`. Use the same endpoint and region when
restoring from an S3-compatible service. See `boxdb replicate --help` and
`boxdb restore --help` for options.

## Recovery limits

Restore without `--revision` uses the last successfully published snapshot.
An explicit revision can be restored if its manifest and blocks are complete,
even if updating the latest pointer failed. Multiple commits between polls can
coalesce, so changes after the last observed snapshot can be absent. Restore
verifies snapshot integrity before creating the destination. BoxDB does not
manage retention or remote garbage collection; binary compression can reduce
upload reuse compared with readable storage.

The client uses signed requests and path-style endpoints. Configure the correct
region and endpoint; HTTP redirects and S3 Express directory buckets are not
supported. BoxDB backups are incompatible with Litestream backups.

The [replication implementation](../src/replication.rs) defines the object layout
and publication protocol. [Local server tests](../tests/replication.rs) verify
signatures, incremental uploads, retries, conflicts, corruption detection, and
restore. Live AWS operation remains unverified.
