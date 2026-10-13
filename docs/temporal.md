# UTC timestamps

BoxDB has one temporal type, `TIMESTAMP`, representing an exact instant rather
than a local date/time. Its meaning follows
[Temporal.Instant](https://tc39.es/proposal-temporal/docs/instant.html).
`DATE`, `TIME`, `TIMESTAMPTZ`, and the SQL `WITH/WITHOUT TIME ZONE` aliases are
unsupported. Existing BoxDB schemas using those removed logical types are not
compatible; version 0 does not provide automatic migration.

Supply an RFC 3339 timestamp with `Z` or an explicit numeric UTC offset:

```sql
CREATE TABLE events(id INT64 PRIMARY KEY, at TIMESTAMP);
INSERT INTO events VALUES(1, '2026-10-13T16:00:00.123456789+04:00');
SELECT at FROM events;
-- 2026-10-13T12:00:00.123456789Z
```

The offset is applied to preserve the instant, then discarded. Positive offsets
are subtracted from the local clock time; negative offsets are added. UTC
normalization can cross midnight, month, and year boundaries. Named time zones
and timestamps without an explicit offset are unsupported.

In Rust, bind `Value::Timestamp`:

```rust
db.execute_params("INSERT INTO events VALUES(?,?)", &[
    Value::Integer(2),
    Value::Timestamp("2026-10-13T08:30:00.123456789-03:30".into()),
])?;
```

Both examples identify the same instant. BoxDB stores nine fractional digits,
including `.000000000` for whole seconds. See the
[storage contract](storage.md#extended-schema) for precision, validation,
generated columns, and ordinary SQLite client behavior.

For equality and range queries, use typed timestamp parameters or canonical UTC
text. An offset-bearing plain SQL string literal is still a text value; SQLite
does not automatically convert it during comparison. SQL date arithmetic also
retains SQLite's existing behavior. Duration, calendar interval, and named-zone
operations are outside the current timestamp type.
