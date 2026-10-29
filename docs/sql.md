# SQL and types

BoxDB executes SQL with its bundled SQLite engine. Familiar joins, indexes,
constraints and transactions work alongside validated logical columns.
Create and alter schemas through BoxDB so its metadata stays consistent.

## A practical schema

```sql
CREATE TABLE tasks (
    id INT64 PRIMARY KEY,
    title VARCHAR(200) NOT NULL,
    status ENUM('todo','doing','done') NOT NULL DEFAULT 'todo',
    budget DECIMAL(12,2),
    due_at INSTANT,
    tags TEXT[]
);
CREATE INDEX tasks_status ON tasks(status);
```

Ordinary tables are STRICT by default. Declare each column's type explicitly,
including generated columns and columns added with ALTER TABLE. `INT64 PRIMARY
KEY` behaves as SQLite's INTEGER rowid alias. Use `ANY` when preserving SQLite
storage classes is intentional.

## Choose a type

| Data | Declaration | Practical rule |
| --- | --- | --- |
| Signed integer | `INT8`, `INT16`, `INT32`, `INT64`, `INTEGER` | Explicit widths validate ranges; INTEGER is 64-bit |
| Unsigned integer | `UINT8`, `UINT16`, `UINT32`, `UINT64` | Full unsigned range; arithmetic checks overflow |
| Approximate number | `REAL`, `DOUBLE` | Use for floating-point measurements |
| Exact amount | `DECIMAL`, `DECIMAL(p,s)` | Bind strings; choose precision and scale deliberately |
| Text | `TEXT`, `VARCHAR(n)` | VARCHAR enforces a maximum length |
| Fixed labels | `ENUM('todo','done')` | String values; BINARY text ordering |
| Boolean | `BOOLEAN`, `BOOL` | Validates 0/1 |
| Bytes | `BLOB`, `BYTEA` | Bind bytes through the client |
| Identifier | `UUID` | Canonical UUID values |
| Point in time | `INSTANT` | Offset input; UTC output with microsecond precision |
| Calendar values | `DATE`, `TIME`, `DATETIME` | TIME and DATETIME have no time zone |
| Duration | `INTERVAL` | Separate months, days and microseconds |
| Typed collection | `TEXT[]`, `INT64[][]` | Complete arrays, serialized as JSON where required |
| Opaque document | `JSON` | Validate the document; inspect it in application code |

For declaration details and physical representations, see
[storage and extended types](storage.md). For arithmetic, read
[decimals](native-decimal.md) and [temporal values](temporal.md).

## Bind values

SQL literals are useful for a first example. Application data must use bound
parameters. TypeScript's tagged queries bind interpolated values automatically:

```ts
await db.query`INSERT INTO tasks(title, budget) VALUES(${"Ship the docs"}, ${"12.50"})`;
```

Do not put SQL quotes around interpolations. Use `db.identifier()` for dynamic
identifiers and `db.fragment` for composable SQL. Other clients accept positional
and named bindings; see the matching client guide.

## Differences to watch

- `INT`, `SMALLINT`, `BIGINT`, `FLOAT`, `DOUBLE PRECISION`, `CHARACTER VARYING`
  and `NUMERIC` declarations are unsupported. Use the explicit types above.
- Bind decimal strings; casting a REAL value to DECIMAL preserves its existing
  binary approximation. `/` and `AVG` require terminating exact results; use
  `decimal_div` with an explicit scale and rounding mode otherwise.
- Enum order is text order, not declaration order. To change enum labels, rebuild
  the table in a migration. Named enum types and SQL casts to ENUM are unsupported.
- JSON is an opaque document. No JSONB, JSON SQL operators or document queries.
- `DOUBLE[]` stores a typed array; sqlite-vec uses its own packed vectors.
- Reserve `__boxdb_` names and avoid column names ending in `?`, which marks
  nullable fields in YAML schema exports.
- Non-native logical types require main-database tables. Native scalar types
  also support TEMP and attached tables.

Each client has its own value mapping: TypeScript integer results are `bigint`;
Go can scan unsigned results into `uint64`; Rust exposes typed `Value` variants.
Read the client guide before converting values.
