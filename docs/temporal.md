# Native temporal values

BoxDB's patched SQLite provides INSTANT, DATE, DATETIME and INTERVAL values.
They retain their types in expressions, indexes, temporary records and snapshots.
Rust exposes matching Type variants and string-backed Value variants; DATETIME
uses DateTime in Rust serialization. TypeScript and Go return strings.

| Type | Meaning | Input example |
| --- | --- | --- |
| INSTANT | A UTC moment; input includes an offset or Z | `2024-02-29T16:00:00.123456+04:00` |
| DATE | Gregorian date without time or zone | `2024-02-29` |
| DATETIME | Calendar date and time without a zone | `2024-02-29T12:00:00.123456` |
| INTERVAL | Separate signed months, days and microseconds | `1 month 2 days 03:04:05.000006` or `P1M2DT3H4M5.000006S` |

Dates and datetimes have years 0000 through 9999. INSTANT normalizes offsets to
UTC. DATETIME rejects offsets. Named zones and connection-local timezone
settings are not part of these types.

## Precision across language clients

INSTANT, DATETIME and the time component of INTERVAL share microsecond precision
(six fractional second digits); DATE has day precision. Microseconds support
interchange with types such as [Python datetime](https://docs.python.org/3/library/datetime.html#datetime.datetime.resolution)
and [PostgreSQL timestamp/interval](https://www.postgresql.org/docs/current/datatype-datetime.html)
without requiring nanosecond support in every client. The same precision applies
to SQL, typed parameters, arithmetic results and snapshots.

Finer input is rounded to the nearest microsecond, with ties to even:
`.123456500` becomes `.123456`, and `.123457500` becomes `.123458`.
Rounding can carry into the next second or date; results outside the supported
date range are rejected. INTERVAL combines its time quantities before rounding.
INSTANT and DATETIME input accept up to nine fractional digits; greater input
precision and leap seconds are rejected. Canonical output always has six digits.

Clients preserve temporal precision as canonical strings. JavaScript's
[Date](https://tc39.es/ecma262/multipage/numbers-and-dates.html#sec-time-values-and-time-range)
has millisecond precision, so converting a result to Date loses microseconds.

```sql
CREATE TABLE events(id INTEGER PRIMARY KEY, at INSTANT, local_at DATETIME);
INSERT INTO events VALUES(1, '2024-02-29T16:00:00+04:00', '2024-02-29T12:00:00');
SELECT at, local_at FROM events;
-- instant 2024-02-29T12:00:00.000000Z
-- datetime 2024-02-29T12:00:00.000000

SELECT CAST('2024-01-31' AS DATE) + CAST('1 month 1 hour' AS INTERVAL);
-- datetime 2024-02-29T01:00:00.000000
SELECT CAST('2024-03-01' AS DATE) - CAST('2024-02-28' AS DATE);
-- integer 2
```

## Arithmetic

DATE plus/minus INTEGER adds/subtracts calendar days and returns DATE. DATE
plus/minus INTERVAL returns DATETIME. DATETIME plus/minus INTERVAL remains
DATETIME. INSTANT arithmetic uses the UTC calendar and returns INSTANT.
Calendar arithmetic never consults a local timezone, so one day and 24 hours
have the same elapsed length here.

Months are applied first and clamp to the last valid day of the destination
month. Days and then microseconds are applied afterwards. Calendar addition is
not associative: January 31 plus two months can differ from adding one month
twice. Subtracting two DATETIME values or two INSTANT values returns an INTERVAL
containing elapsed days and microseconds. DATE minus DATE returns day count.

INTERVAL supports addition, subtraction, negation, multiplication and division
by numeric scalars. Scaling uses exact rational arithmetic and rounds once to
microseconds with half-even rounding. Fractional months spill into days using
30 days/month; fractional days spill into time using 24 hours/day. Month, day
and time fields otherwise remain separate, including mixed signs.

```sql
SELECT CAST('1 month' AS INTERVAL) * 0.5;
-- interval 0 mons 15 days 00:00:00.000000
SELECT CAST('1 day' AS INTERVAL) = CAST('24 hours' AS INTERVAL);
-- 1
```

SUM and AVG preserve interval fields; window aggregates use the same rules.
Mixed numeric/interval aggregates, unsupported operators, division by zero and
field overflow produce errors. Resource and field bounds live in the
[temporal implementation](../vendor/libsqlite3-sys/src/temporal.rs).

## SQL functions

The engine provides these functions on every connection, including restored
schemas. Inputs retain their native types; cast text explicitly.

| Call | Result |
| --- | --- |
| `extract(field, value)` | Exact DECIMAL calendar/time component |
| `make_interval(years, months, weeks, days, hours, minutes, seconds)` | INTERVAL; omitted trailing arguments default to zero |
| `age(later, earlier)` | Symbolic calendar INTERVAL |
| `age(value)` | INTERVAL from value to the current UTC date at midnight |
| `date_trunc(field, value)` | INSTANT, DATETIME or INTERVAL, with smaller components cleared |

Calls use SQLite's comma-separated arguments. Fields are case-insensitive TEXT,
for example `year`, `month`, `day`, `hour`, `second`, `microseconds`, `epoch` or
ISO `week`/`isoyear`. Allowed fields depend on the input type; the
[function implementation](../vendor/libsqlite3-sys/src/temporal_functions.rs)
defines the complete set. Any supplied NULL returns NULL; invalid fields,
argument types and overflow return errors.

```sql
SELECT extract('second', CAST('2024-02-29T12:34:56.123456' AS DATETIME));
-- decimal 56.123456
SELECT make_interval(0, 1, 0, 2);
-- interval 1 mons 2 days 00:00:00.000000
SELECT age(CAST('2004-06-01' AS DATE), CAST('2004-04-30' AS DATE));
-- interval 1 mons 1 days 00:00:00.000000
SELECT date_trunc('hour', CAST('2024-02-29T12:34:56.123456' AS DATETIME));
-- datetime 2024-02-29T12:00:00.000000
```

`make_interval` accepts positional arguments: its first six fields must be
integers, and seconds may be fractional. Use DECIMAL seconds for exact input;
fractional microseconds follow the shared half-even rule.

`age` uses months and days of the calendar, borrowing partial months using the
earlier date's month length. It is not elapsed duration or an inverse of calendar
addition. DATE is midnight; DATE and DATETIME can be mixed. INSTANT arguments
use UTC and require explicit casts when mixed with calendar values. Unary `age`
uses SQLite's statement clock and is non-deterministic. All other forms are
deterministic and support generated columns and expression indexes.

`date_trunc` keeps INSTANT/DATETIME/INTERVAL types; DATE input becomes DATETIME.
Calendar weeks start on Monday. INTERVAL supports truncation from microseconds
through millennia, excluding weeks; signed fields truncate towards zero.

`extract('epoch', ...)` returns exact seconds. For INTERVAL, this uses the
PostgreSQL convention of 365.25 days per complete year and 30 days per remaining
month, rather than elapsed time on a specific calendar. This differs from the
30-day-month comparison rule. `julian` returns calendar days with up to fifteen
fractional digits, rounded with ties to even.

## Comparison and conversion

Comparison, ORDER BY, grouping, uniqueness and index lookup use the same rules.
DATE compares as midnight with DATETIME. INSTANT and calendar values remain
separate domains unless explicitly cast. INTERVAL ordering/equality uses a
30-day month and a 24-hour day; equal intervals can therefore have different
effects when applied to a calendar date. Cast to TEXT to inspect their fields.

Explicit casts between INSTANT and calendar types use UTC. Casting DATETIME to
DATE discards time; assignment does not silently truncate it. Generated native
columns normalize and validate through the engine. Standard SQLite date,
datetime and strftime functions retain their existing text contracts.
Use casts when an expression needs a native result.

Binary storage retains SQLite's standard record encoding using single-byte BLOB
type tags. Codec versions belong to database metadata. Readable YAML uses typed
mappings. See the [storage contract](storage.md).
