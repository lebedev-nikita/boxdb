# Native decimals

BoxDB's patched SQLite engine supports `DECIMAL` values in arithmetic,
comparison, and ordering. DECIMAL is the only exact decimal type.

```sql
CREATE TABLE prices(amount DECIMAL(20,2));
INSERT INTO prices VALUES('12.50');
SELECT amount + CAST('0.25' AS DECIMAL) FROM prices; -- decimal 12.75
SELECT amount FROM prices ORDER BY amount;          -- numeric order
SELECT decimal_div('1','3',2,'half_even');           -- decimal 0.33
```

Use decimal text, typed Rust parameters, or `CAST('...' AS DECIMAL)` for exact
input. Ordinary SQL `0.1` and JavaScript numbers are binary floating point.
`CAST(0.1 AS DECIMAL)` explicitly converts the actual binary value, producing
`0.1000000000000000055511151231257827021181583404541015625`.

## Arithmetic and equality

`+`, `-`, `*`, unary signs, and `%` operate on finite arbitrary-precision decimals.
INTEGER operands promote exactly. REAL arithmetic requires explicit conversion
with `CAST(... AS DECIMAL)` or conversion of the decimal to REAL. No implicit
float result is used to resolve decimal overflow.

`/` returns an exact terminating decimal. Division by zero and nonterminating
results such as `CAST('1' AS DECIMAL)/3` produce errors. Use
`decimal_div(left,right,scale,mode)` to round division once to an explicit scale,
and `decimal_round(value,scale,mode)` to round an existing value. Supported modes
are `half_even`, `half_up`, `half_down`, `up`, `down`, `ceiling`, and `floor`.
These functions preserve requested trailing zeros and propagate NULL operands.

Decimal equality ignores display scale: `2`, decimal `2.0`, and decimal `2.00`
are equal. Comparisons with REAL use its actual binary value, so decimal `0.1`
is less than REAL `0.1`. Decimal values participate in the numeric ordering
before TEXT and ordinary BLOB values, with SQLite's existing NULL placement.
The same comparison drives predicates, indexes, ORDER BY, grouping, set
operations, uniqueness, and foreign keys.

`DECIMAL(p,s)` checks precision and scale on assignment; excess fractional digits
are rejected rather than rounded. Intermediate expressions are not restricted
to the destination column's scale. Use explicit rounding before assignment.
Public declarations allow precision up to 1000. General decimal arithmetic
is limited to 4096 normalized coefficient digits and an absolute scale of 4096,
with bounded input text, enforced by the [decimal implementation](../vendor/libsqlite3-sys/src/decimal.rs).

`SUM` promotes decimal inputs and preserves exact integer values, including
integer overflow before decimal promotion. `AVG` uses exact division and therefore
rejects nonterminating averages. `MIN` and `MAX` preserve the selected value.
`TOTAL` deliberately retains SQLite's approximate REAL contract. Window aggregates
use the same rules. For fractional RANGE bounds, supply an exact decimal offset,
for example `RANGE BETWEEN CAST('0.5' AS DECIMAL) PRECEDING AND CURRENT ROW`;
an ordinary `0.5` offset is REAL and cannot mix implicitly with decimal arithmetic.

`abs` preserves decimal type. Built-in `round` uses half-up rounding for decimal
input; `decimal_round` provides explicit modes. Bitwise operations retain integer
semantics; use explicit INTEGER casts when integer-only SQL contexts such as
LIMIT require them. Decimal values in typed arrays use strings to preserve digits. SQL JSON
processing functions are disabled.

## Storage and clients

The binary database keeps the standard SQLite page and record format. Decimals
are ordinary BLOB records containing the one-byte tag `d` followed by UTF-8
decimal text. The database-wide codec version is stored in `__boxdb_codec` and
readable snapshot headers. The tag survives stored columns, calculated values,
temporary records, materialized CTEs, and sorting. No extra disk serial type or
in-memory extension subtype is needed.
[SQLite's record format](https://www.sqlite.org/fileformat2.html#record_format)
defines BLOB serial types independently of their payload.

Standard file encoding does **not** promise the same SQL behavior in unmodified
SQLite. Such clients see BLOB values and do not implement decimal affinity,
numeric index ordering, operators, or the BoxDB constraint functions. Use the
BoxDB engine for these databases. Every ordinary BLOB has a `b` tag followed by
the original bytes, so payloads beginning with `d` or any other native tag remain
BYTEA. Ordinary BLOB literals, casts, bind parameters, and function results are
tagged at their API boundary. BoxDB uses a separate private
wire-cell transport for native values and readable imports.

Rust exposes `Type::Decimal` and `Value::Decimal(String)`. Calculated results
are recognized from
their runtime envelopes, not only column-origin metadata. Typed arrays serialize
decimal elements as strings. Rust/CLI typed JSON retains the `Decimal` variant;
TypeScript returns decimal values as strings. Bind TypeScript decimal parameters
as strings and cast them when an expression requires a native decimal value.

The C API retains the five upstream fundamental type codes: decimal storage
reports `SQLITE_BLOB`, while SQL `typeof` reports `decimal`. Decimal text views
and explicit INTEGER/REAL conversions are handled by the patched value accessors.
Standard C column and scalar-function accessors expose logical payloads.
`sqlite3_boxdb_column_blob` and its size accessors expose complete storage cells;
the vendored Rust FFI routes column BLOB reads through these accessors for typed
decoding. Incremental BYTEA reads and writes address payload bytes, preserving
the stored tag.
Generic extensions that gate inputs on `sqlite3_value_numeric_type` still see
BLOB; cast to the representation their API expects. The public decimal SQL
functions are `decimal_round` and `decimal_div`, both returning native decimals.

## Engine architecture and verification

[Cargo](../Cargo.toml) depends directly on `boxdb-sqlite-sys`, the vendored SQLite
3.53.2 bundle. [Checked source transformations](../vendor/libsqlite3-sys/boxdb.rs)
apply at build time to the pinned amalgamation; every patch anchor must match
exactly once. [C helpers](../vendor/libsqlite3-sys/boxdb_decimal.inc) recognize
values and connect VDBE operations to
[Rust decimal arithmetic](../vendor/libsqlite3-sys/src/decimal.rs).
[Aggregate helpers](../vendor/libsqlite3-sys/boxdb_aggregate.inc) keep exact
accumulators. [The shared native type registry](../vendor/libsqlite3-sys/native_types.rs) also
drives UUID and temporal affinities. The original record encoder continues to
serialize BLOB payloads. Readable YAML version 3 records the codec version once
and uses typed mappings, such as
`amount: {decimal: "12.50"}`, preserving requested display scale.

Comparison patches cover in-memory values and serialized-record paths, including
optimized index comparison. SQLite's sorter consumes those record comparators.
This matters because numeric ORDER BY and index searches must agree even when
results spill to temporary files. BoxDB adds precision checks during
[schema translation](../src/schema.rs) and registers explicit rounding functions
on its connections in [src/decimal.rs](../src/decimal.rs). Decimal affinity is
retained in comparison planning and index keys; Bloom-filter hashes agree with
equal INTEGER and REAL values. Comparisons coerce temporary values, preserving
the original operands.

[SQL integration tests](../tests/native_decimal.rs) exercise the Database seam:
operators, comparison, indexed and unindexed plans, constraints, aggregates,
materialization, transports, and snapshots. The same suite checks public Rust
types and serialization. [TypeScript tests](../packages/typescript/test/client.test.ts)
check native transport without JavaScript Number conversion. These public seams
are the acceptance boundary; private flags and patch strings are not the contract.
