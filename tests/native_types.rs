#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, Type, Value};

#[test]
fn temporal_inputs_round_to_microseconds_consistently() {
    let mut db = Database::memory();
    for (input, expected) in [
        ("123456001", "123456"),
        ("123456500", "123456"),
        ("123456789", "123457"),
        ("123457500", "123458"),
    ] {
        assert_eq!(
            db.execute_params(
                "SELECT CAST(? AS INSTANT),CAST(? AS DATETIME),CAST(? AS INTERVAL)",
                &[
                    Value::Text(format!("2024-02-29T16:00:00.{input}+04:00")),
                    Value::Text(format!("2024-02-29T12:00:00.{input}")),
                    Value::Text(format!("1 month 2 days 03:04:05.{input}")),
                ],
            )
            .unwrap()[0]
                .rows,
            vec![vec![
                Value::Instant(format!("2024-02-29T12:00:00.{expected}Z")),
                Value::DateTime(format!("2024-02-29T12:00:00.{expected}")),
                Value::Interval(format!("1 mons 2 days 03:04:05.{expected}")),
            ]],
            "{input}",
        );
    }
}

#[test]
fn temporal_rounding_carries_across_calendar_and_epoch_boundaries() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST('2024-12-31T23:59:59.999999500Z' AS INSTANT),CAST('2024-12-31T23:59:59.999999500' AS DATETIME),CAST('1969-12-31T23:59:59.999999500Z' AS INSTANT),CAST('-0.0000015 seconds' AS INTERVAL),CAST('0.4 microseconds 0.4 microseconds' AS INTERVAL)").unwrap()[0].rows,
        vec![vec![
            Value::Instant("2025-01-01T00:00:00.000000Z".into()),
            Value::DateTime("2025-01-01T00:00:00.000000".into()),
            Value::Instant("1970-01-01T00:00:00.000000Z".into()),
            Value::Interval("0 mons 0 days -00:00:00.000002".into()),
            Value::Interval("0 mons 0 days 00:00:00.000001".into()),
        ]]);
    for ty in ["INSTANT", "DATETIME"] {
        let zone = if ty == "INSTANT" { "Z" } else { "" };
        assert!(
            db.execute(&format!(
                "SELECT CAST('9999-12-31T23:59:59.999999500{zone}' AS {ty})"
            ))
            .is_err()
        );
    }
}

#[test]
fn temporal_arithmetic_and_aggregates_keep_microsecond_precision() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST('2024-02-29T23:59:59.999999Z' AS INSTANT)+CAST('1 microsecond' AS INTERVAL),CAST('2024-02-29T23:59:59.999999' AS DATETIME)+CAST('1 microsecond' AS INTERVAL),CAST('1970-01-01T00:00:00Z' AS INSTANT)-CAST('1969-12-31T23:59:59.999999Z' AS INSTANT),CAST('1 microsecond' AS INTERVAL)/2,CAST('1 microsecond' AS INTERVAL)*1.5").unwrap()[0].rows,
        vec![vec![
            Value::Instant("2024-03-01T00:00:00.000000Z".into()),
            Value::DateTime("2024-03-01T00:00:00.000000".into()),
            Value::Interval("0 mons 0 days 00:00:00.000001".into()),
            Value::Interval("0 mons 0 days 00:00:00.000000".into()),
            Value::Interval("0 mons 0 days 00:00:00.000002".into()),
        ]]);
    db.execute("CREATE TABLE ticks(v INTERVAL); INSERT INTO ticks VALUES('1 microsecond'),('2 microseconds')").unwrap();
    assert_eq!(
        db.execute("SELECT SUM(v),AVG(v) FROM ticks").unwrap()[0].rows,
        vec![vec![
            Value::Interval("0 mons 0 days 00:00:00.000003".into()),
            Value::Interval("0 mons 0 days 00:00:00.000002".into()),
        ]]
    );
}

#[test]
fn rounded_temporal_values_agree_in_uniqueness_grouping_and_index_lookups() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE moments(v INSTANT UNIQUE); INSERT INTO moments VALUES('2024-02-29T12:00:00.123456001Z'); CREATE INDEX moments_v ON moments(v)").unwrap();
    assert!(
        db.execute("INSERT INTO moments VALUES('2024-02-29T12:00:00.123456499Z')")
            .is_err()
    );
    for plan in ["INDEXED BY moments_v", "NOT INDEXED"] {
        assert_eq!(
            db.execute(&format!(
                "SELECT v FROM moments {plan} WHERE v='2024-02-29T12:00:00.123456499Z'"
            ))
            .unwrap()[0]
                .rows,
            vec![vec![Value::Instant("2024-02-29T12:00:00.123456Z".into())]]
        );
    }
    for (ty, input) in [
        ("INSTANT", "2024-02-29T12:00:00.123456"),
        ("DATETIME", "2024-02-29T12:00:00.123456"),
        ("INTERVAL", "0.123456"),
    ] {
        let unit = match ty {
            "INSTANT" => "Z",
            "INTERVAL" => " seconds",
            _ => "",
        };
        assert_eq!(db.execute(&format!("SELECT COUNT(DISTINCT v) FROM (SELECT CAST('{input}001{unit}' AS {ty}) AS v UNION ALL SELECT CAST('{input}499{unit}' AS {ty}))")).unwrap()[0].rows,
            vec![vec![Value::Integer(1)]]);
    }
}

#[test]
fn public_rust_temporal_values_validate_and_serialize_at_microsecond_precision() {
    for (ty, input, expected, wire) in [
        (
            Type::Instant,
            Value::Instant("2024-02-29T16:00:00.123456789+04:00".into()),
            Value::Instant("2024-02-29T12:00:00.123457Z".into()),
            serde_json::json!({"Instant":"2024-02-29T12:00:00.123457Z"}),
        ),
        (
            Type::DateTime,
            Value::DateTime("2024-02-29T12:00:00.123456789".into()),
            Value::DateTime("2024-02-29T12:00:00.123457".into()),
            serde_json::json!({"DateTime":"2024-02-29T12:00:00.123457"}),
        ),
        (
            Type::Interval,
            Value::Interval("1 month 2 days 03:04:05.123456789".into()),
            Value::Interval("1 mons 2 days 03:04:05.123457".into()),
            serde_json::json!({"Interval":"1 mons 2 days 03:04:05.123457"}),
        ),
    ] {
        let actual = ty.validate(&input).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(ty.cast(input).unwrap(), expected);
        assert_eq!(serde_json::to_value(&actual).unwrap(), wire);
        assert_eq!(
            serde_json::from_str::<Value>(&serde_json::to_string(&actual).unwrap()).unwrap(),
            expected
        );
    }
}

#[test]
fn uuid_values_are_native_in_columns_and_computed_results() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE ids(id UUID UNIQUE); INSERT INTO ids VALUES('550E8400-E29B-41D4-A716-446655440000')")
        .unwrap();
    assert_eq!(
        db.execute(
            "SELECT id,typeof(id),CAST('550E8400-E29B-41D4-A716-446655440000' AS UUID) FROM ids"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
            Value::Text("uuid".into()),
            Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
        ]]
    );
    assert!(
        db.execute("INSERT INTO ids VALUES('550e8400-e29b-41d4-a716-446655440000')")
            .is_err()
    );
    assert!(db.execute("INSERT INTO ids VALUES('invalid')").is_err());
}

#[test]
fn calendar_arithmetic_uses_months_and_clamps_the_end_of_the_month() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute(
            "SELECT CAST('2024-01-31T12:00:00.000001' AS DATETIME)+CAST('1 month' AS INTERVAL)"
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::DateTime("2024-02-29T12:00:00.000001".into())]]
    );
}

#[test]
fn dates_add_days_and_subtract_to_an_integer_day_count() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST('2024-02-28' AS DATE)+1, CAST('2024-03-01' AS DATE)-CAST('2024-02-28' AS DATE)").unwrap()[0].rows,
        vec![vec![Value::Date("2024-02-29".into()),Value::Integer(2)]]);
}

#[test]
fn intervals_keep_calendar_fields_when_scaled_added_and_negated() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST('1 month' AS INTERVAL)*0.5, CAST('2 months' AS INTERVAL)/4, CAST('1 month 2 days 1 microsecond' AS INTERVAL)+CAST('-1 day 2 microseconds' AS INTERVAL), -CAST('1 month -2 days -00:00:00.000001' AS INTERVAL)").unwrap()[0].rows,
        vec![vec![
            Value::Interval("0 mons 15 days 00:00:00.000000".into()),
            Value::Interval("0 mons 15 days 00:00:00.000000".into()),
            Value::Interval("1 mons 1 days 00:00:00.000003".into()),
            Value::Interval("-1 mons 2 days 00:00:00.000001".into()),
        ]]);
}

#[test]
fn calendar_operators_preserve_result_types_across_dates_datetimes_and_instants() {
    let mut db = Database::memory();
    for (sql, expected) in [
        (
            "CAST('2024-01-31' AS DATE)+CAST('1 month 1 hour' AS INTERVAL)",
            Value::DateTime("2024-02-29T01:00:00.000000".into()),
        ),
        (
            "CAST('2024-02-29T12:00:00' AS DATETIME)-CAST('1 year' AS INTERVAL)",
            Value::DateTime("2023-02-28T12:00:00.000000".into()),
        ),
        (
            "CAST('1 month' AS INTERVAL)+CAST('2024-01-31T00:00:00' AS DATETIME)",
            Value::DateTime("2024-02-29T00:00:00.000000".into()),
        ),
        (
            "CAST('2024-02-29T00:00:00Z' AS INSTANT)+CAST('1 day' AS INTERVAL)",
            Value::Instant("2024-03-01T00:00:00.000000Z".into()),
        ),
        (
            "CAST('2024-03-01T00:00:00Z' AS INSTANT)-CAST('24 hours' AS INTERVAL)",
            Value::Instant("2024-02-29T00:00:00.000000Z".into()),
        ),
        (
            "CAST('2024-03-01T00:00:00.000001' AS DATETIME)-CAST('2024-02-28T00:00:00' AS DATETIME)",
            Value::Interval("0 mons 2 days 00:00:00.000001".into()),
        ),
    ] {
        assert_eq!(
            db.execute(&format!("SELECT {sql}")).unwrap()[0].rows,
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn native_ordering_and_interval_equality_agree_with_indexes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE spans(v INTERVAL UNIQUE); INSERT INTO spans VALUES('1 month')")
        .unwrap();
    assert!(db.execute("INSERT INTO spans VALUES('30 days')").is_err());
    assert_eq!(
        db.execute("SELECT CAST('1 day' AS INTERVAL)=CAST('24 hours' AS INTERVAL)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    db.execute("CREATE TABLE events(id INTEGER PRIMARY KEY,at INSTANT); INSERT INTO events VALUES(1,'2024-01-01T00:00:00Z'),(2,'1969-12-31T23:59:59Z'); CREATE INDEX events_at ON events(at)").unwrap();
    for plan in ["INDEXED BY events_at", "NOT INDEXED"] {
        assert_eq!(
            db.execute(&format!("SELECT id FROM events {plan} ORDER BY at"))
                .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(2)], vec![Value::Integer(1)]]
        );
    }
}

#[test]
fn native_values_survive_binary_and_readable_conversion() {
    let values = vec![
        Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
        Value::Instant("2024-02-29T12:00:00.123457Z".into()),
        Value::Date("2024-02-29".into()),
        Value::DateTime("2024-02-29T12:00:00.123457".into()),
        Value::Interval("1 mons 2 days 03:04:05.000006".into()),
    ];
    let mut db = Database::memory();
    db.execute("CREATE TABLE rich(u UUID,i INSTANT,d DATE,t DATETIME,v INTERVAL)")
        .unwrap();
    db.execute_params("INSERT INTO rich VALUES(?,?,?,?,?)", &values)
        .unwrap();
    for format in [Format::Binary, Format::Readable] {
        let snapshot = files::converted_bytes(&mut db, format).unwrap();
        if format == Format::Readable {
            assert!(
                std::str::from_utf8(&snapshot)
                    .unwrap()
                    .contains("2024-02-29T12:00:00.123457Z")
            );
        }
        let mut restored = files::open_bytes(&snapshot).unwrap();
        assert_eq!(
            restored.execute("SELECT * FROM rich").unwrap()[0].rows,
            vec![values.clone()]
        );
    }
}

#[test]
fn interval_aggregates_and_windows_preserve_calendar_fields() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE spans(id INTEGER PRIMARY KEY,v INTERVAL); INSERT INTO spans VALUES(1,'1 month'),(2,'2 days')").unwrap();
    assert_eq!(
        db.execute("SELECT SUM(v),AVG(v) FROM spans").unwrap()[0].rows,
        vec![vec![
            Value::Interval("1 mons 2 days 00:00:00.000000".into()),
            Value::Interval("0 mons 16 days 00:00:00.000000".into())
        ]]
    );
    assert_eq!(db.execute("SELECT SUM(v) OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM spans ORDER BY id").unwrap()[0].rows,
        vec![vec![Value::Interval("1 mons 0 days 00:00:00.000000".into())],vec![Value::Interval("1 mons 2 days 00:00:00.000000".into())]]);
}

#[test]
fn interval_division_rounds_once_at_microsecond_precision() {
    let mut db = Database::memory();
    let divisor = format!("1.{}", "9".repeat(200));
    assert_eq!(
        db.execute_params(
            "SELECT CAST('1 microsecond' AS INTERVAL)/CAST(? AS DECIMAL)",
            &[Value::Text(divisor)]
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Interval(
            "0 mons 0 days 00:00:00.000001".into()
        )]]
    );
}

#[test]
fn explicit_temporal_casts_use_utc_and_assignments_do_not_truncate_dates() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST(CAST('2024-02-29' AS DATE) AS DATETIME),CAST(CAST('2024-02-29T12:00:00' AS DATETIME) AS DATE),CAST(CAST('2024-02-29T12:00:00' AS DATETIME) AS INSTANT),CAST(CAST('2024-02-29T16:00:00+04:00' AS INSTANT) AS DATETIME)").unwrap()[0].rows,
        vec![vec![Value::DateTime("2024-02-29T00:00:00.000000".into()),Value::Date("2024-02-29".into()),Value::Instant("2024-02-29T12:00:00.000000Z".into()),Value::DateTime("2024-02-29T12:00:00.000000".into())]]);
    db.execute("CREATE TABLE days(d DATE)").unwrap();
    assert!(
        db.execute("INSERT INTO days VALUES(CAST('2024-02-29T12:00:00' AS DATETIME))")
            .is_err()
    );
}

#[test]
fn dates_and_midnight_datetimes_have_consistent_calendar_equality() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE calendar(v ANY); INSERT INTO calendar VALUES(CAST('2024-02-29' AS DATE)),(CAST('2024-02-29T00:00:00' AS DATETIME))").unwrap();
    assert_eq!(
        db.execute("SELECT COUNT(DISTINCT v) FROM calendar")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn public_rust_temporal_casts_match_explicit_sql_casts() {
    assert_eq!(
        Type::DateTime
            .cast(Value::Instant("2024-02-29T16:00:00+04:00".into()))
            .unwrap(),
        Value::DateTime("2024-02-29T12:00:00.000000".into())
    );
    for (ty, value) in [
        (Type::Date, Value::Date("2024-02-29".into())),
        (
            Type::DateTime,
            Value::DateTime("2024-02-29T12:00:00.000000".into()),
        ),
        (
            Type::Interval,
            Value::Interval("1 mons 0 days 00:00:00.000000".into()),
        ),
    ] {
        assert_eq!(
            serde_json::from_str::<Type>(&serde_json::to_string(&ty).unwrap()).unwrap(),
            ty
        );
        assert_eq!(
            serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(),
            value
        );
    }
}

#[test]
fn native_identifiers_and_calendar_values_reject_numeric_operations() {
    let mut db = Database::memory();
    for sql in [
        "SELECT abs(CAST('550e8400-e29b-41d4-a716-446655440000' AS UUID))",
        "SELECT CAST(CAST('2024-02-29' AS DATE) AS INTEGER)",
        "SELECT CAST('2024-02-29' AS DATE)&1",
        "SELECT ~CAST('2024-02-29' AS DATE)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
}

#[test]
fn casting_a_native_value_to_its_own_type_is_an_identity() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT CAST(CAST('550e8400-e29b-41d4-a716-446655440000' AS UUID) AS UUID)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Uuid(
            "550e8400-e29b-41d4-a716-446655440000".into()
        )]]
    );
}

#[test]
fn interval_input_supports_iso_and_rejects_ambiguous_or_overflowing_fields() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT CAST('-P1Y2M3DT4H5M6.000007S' AS INTERVAL)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Interval(
            "-14 mons -3 days -04:05:06.000007".into()
        )]]
    );
    for input in [
        "P1Y2Y",
        "P1DT",
        "PT1S2H",
        "2147483648 months",
        "9223372036854775808 microseconds",
        "1e999999999 days",
    ] {
        assert!(
            db.execute_params("SELECT CAST(? AS INTERVAL)", &[Value::Text(input.into())])
                .is_err(),
            "{input}"
        );
    }
}

#[test]
fn native_text_views_and_concatenation_work_in_utf16_databases() {
    let mut db = Database::memory();
    db.execute("PRAGMA encoding='UTF-16le'; CREATE TABLE calendar(v DATETIME); INSERT INTO calendar VALUES('2024-02-29T12:00:00')").unwrap();
    assert_eq!(
        db.execute("SELECT v||'雪' FROM calendar").unwrap()[0].rows,
        vec![vec![Value::Text("2024-02-29T12:00:00.000000雪".into())]]
    );
}

#[test]
fn temporal_casts_preserve_distinct_native_types_and_microseconds() {
    let mut db = Database::memory();
    let rows = db.execute("SELECT CAST('2024-02-29' AS DATE), CAST('2024-02-29T12:00:00.123457' AS DATETIME), CAST('2024-02-29T16:00:00.123457+04:00' AS INSTANT), CAST('1 month 2 days 03:04:05.000006' AS INTERVAL)").unwrap()[0].rows.clone();
    assert_eq!(
        serde_json::to_value(rows).unwrap(),
        serde_json::json!([[
            {"Date":"2024-02-29"},
            {"DateTime":"2024-02-29T12:00:00.123457"},
            {"Instant":"2024-02-29T12:00:00.123457Z"},
            {"Interval":"1 mons 2 days 03:04:05.000006"}
        ]])
    );
}
