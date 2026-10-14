use boxdb::{Database, Format, Value};

#[test]
fn make_interval_combines_calendar_fields_and_rounds_seconds() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute(
            "SELECT make_interval(),make_interval(1,2,3,4,5,6,CAST('7.1234565' AS DECIMAL))"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Interval("0 mons 0 days 00:00:00.000000".into()),
            Value::Interval("14 mons 25 days 05:06:07.123456".into()),
        ]]
    );
}

#[test]
fn extract_returns_exact_decimal_for_calendar_and_time_fields() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT extract('year',CAST('2024-02-29' AS DATE)),extract('second',CAST('2024-02-29T12:34:56.123456' AS DATETIME)),extract('epoch',CAST('1969-12-31T23:59:59.999999Z' AS INSTANT)),extract('month',CAST('2 years 13 months' AS INTERVAL))")
            .unwrap()[0].rows,
        vec![vec![
            Value::Decimal("2024".into()),
            Value::Decimal("56.123456".into()),
            Value::Decimal("-0.000001".into()),
            Value::Decimal("1".into()),
        ]]
    );
}

#[test]
fn extract_supports_iso_calendar_and_signed_interval_components() {
    let mut db = Database::memory();
    for (field, ty, input, expected) in [
        ("millennium", "DATE", "2021-01-03", "3"),
        ("century", "DATE", "2021-01-03", "21"),
        ("decade", "DATE", "2021-01-03", "202"),
        ("quarter", "DATE", "2021-01-03", "1"),
        ("isoyear", "DATE", "2021-01-03", "2020"),
        ("week", "DATE", "2021-01-03", "53"),
        ("day", "DATE", "2021-01-03", "3"),
        ("doy", "DATE", "2021-01-03", "3"),
        ("dow", "DATE", "2021-01-03", "0"),
        ("isodow", "DATE", "2021-01-03", "7"),
        ("julian", "DATETIME", "2006-01-01T12:00:00", "2453737.5"),
        ("hour", "DATETIME", "2024-02-29T12:34:56.123456", "12"),
        ("minute", "DATETIME", "2024-02-29T12:34:56.123456", "34"),
        (
            "milliseconds",
            "DATETIME",
            "2024-02-29T12:34:56.123456",
            "56123.456",
        ),
        (
            "microseconds",
            "DATETIME",
            "2024-02-29T12:34:56.123456",
            "56123456",
        ),
        ("timezone", "INSTANT", "2024-02-29T16:00:00+04:00", "0"),
        ("timezone_hour", "INSTANT", "2024-02-29T16:00:00+04:00", "0"),
        (
            "timezone_minute",
            "INSTANT",
            "2024-02-29T16:00:00+04:00",
            "0",
        ),
        ("year", "INTERVAL", "-2 years -13 months", "-3"),
        ("month", "INTERVAL", "-2 years -13 months", "-1"),
        ("quarter", "INTERVAL", "-2 years -13 months", "-1"),
        ("week", "INTERVAL", "-40 days", "-5"),
        ("day", "INTERVAL", "-40 days", "-40"),
        ("hour", "INTERVAL", "-80 hours -20 minutes", "-80"),
        ("minute", "INTERVAL", "-80 hours -20 minutes", "-20"),
        ("SECONDS", "INTERVAL", "-56.123456 seconds", "-56.123456"),
        (
            "milliseconds",
            "INTERVAL",
            "-56.123456 seconds",
            "-56123.456",
        ),
        (
            "microseconds",
            "INTERVAL",
            "-56.123456 seconds",
            "-56123456",
        ),
        ("decade", "INTERVAL", "-123 years", "-12"),
        ("century", "INTERVAL", "-123 years", "-1"),
        ("millennium", "INTERVAL", "-1234 years", "-1"),
        ("epoch", "INTERVAL", "1 year", "31557600"),
    ] {
        assert_eq!(
            db.execute_params(
                &format!("SELECT extract(?,CAST(? AS {ty}))"),
                &[Value::Text(field.into()), Value::Text(input.into())]
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Decimal(expected.into())]],
            "{field}, {ty}, {input}"
        );
    }
}

#[test]
fn age_preserves_calendar_months_and_borrows_from_the_earlier_month() {
    let mut db = Database::memory();
    for (left, right, expected) in [
        (
            "2001-04-10T00:00:00",
            "1957-06-13T00:00:00",
            "525 mons 27 days 00:00:00.000000",
        ),
        (
            "2004-06-01T00:00:00",
            "2004-04-30T00:00:00",
            "1 mons 1 days 00:00:00.000000",
        ),
        (
            "2004-04-30T00:00:00",
            "2004-06-01T00:00:00",
            "-1 mons -1 days 00:00:00.000000",
        ),
        (
            "2024-03-01T00:00:00.000001",
            "2024-02-28T23:59:59.999999",
            "0 mons 1 days 00:00:00.000002",
        ),
        (
            "2023-02-28T00:00:00",
            "2023-01-31T00:00:00",
            "0 mons 28 days 00:00:00.000000",
        ),
    ] {
        assert_eq!(
            db.execute_params(
                "SELECT age(CAST(? AS DATETIME),CAST(? AS DATETIME))",
                &[Value::Text(left.into()), Value::Text(right.into())]
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Interval(expected.into())]],
            "{left}, {right}"
        );
    }
    assert_eq!(db.execute("SELECT age(CAST('2024-03-01' AS DATE),CAST('2024-02-01T00:00:00' AS DATETIME)),age(CAST('2024-03-01T04:00:00+04:00' AS INSTANT),CAST('2024-02-01T00:00:00Z' AS INSTANT))").unwrap()[0].rows,
        vec![vec![Value::Interval("1 mons 0 days 00:00:00.000000".into()); 2]]);
}

#[test]
fn date_trunc_preserves_native_types_and_truncates_signed_fields() {
    let mut db = Database::memory();
    for (field, expected) in [
        ("microseconds", "2024-02-29T12:34:56.123456"),
        ("milliseconds", "2024-02-29T12:34:56.123000"),
        ("second", "2024-02-29T12:34:56.000000"),
        ("minute", "2024-02-29T12:34:00.000000"),
        ("hour", "2024-02-29T12:00:00.000000"),
        ("day", "2024-02-29T00:00:00.000000"),
        ("week", "2024-02-26T00:00:00.000000"),
        ("month", "2024-02-01T00:00:00.000000"),
        ("quarter", "2024-01-01T00:00:00.000000"),
        ("year", "2024-01-01T00:00:00.000000"),
        ("decade", "2020-01-01T00:00:00.000000"),
        ("century", "2001-01-01T00:00:00.000000"),
        ("millennium", "2001-01-01T00:00:00.000000"),
    ] {
        assert_eq!(
            db.execute_params(
                "SELECT date_trunc(?,CAST('2024-02-29T12:34:56.123456' AS DATETIME))",
                &[Value::Text(field.into())]
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::DateTime(expected.into())]],
            "{field}"
        );
    }
    for (field, expected) in [
        ("microseconds", "14 mons -2 days -03:04:05.123456"),
        ("milliseconds", "14 mons -2 days -03:04:05.123000"),
        ("second", "14 mons -2 days -03:04:05.000000"),
        ("minute", "14 mons -2 days -03:04:00.000000"),
        ("hour", "14 mons -2 days -03:00:00.000000"),
        ("day", "14 mons -2 days 00:00:00.000000"),
        ("month", "14 mons 0 days 00:00:00.000000"),
        ("quarter", "12 mons 0 days 00:00:00.000000"),
        ("year", "12 mons 0 days 00:00:00.000000"),
        ("decade", "0 mons 0 days 00:00:00.000000"),
        ("century", "0 mons 0 days 00:00:00.000000"),
        ("millennium", "0 mons 0 days 00:00:00.000000"),
    ] {
        assert_eq!(
            db.execute_params(
                "SELECT date_trunc(?,CAST('1 year 2 months -2 days -03:04:05.123456' AS INTERVAL))",
                &[Value::Text(field.into())]
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Interval(expected.into())]],
            "{field}"
        );
    }
    assert_eq!(db.execute("SELECT date_trunc('week',CAST('2021-01-03' AS DATE)),date_trunc('day',CAST('2024-03-01T00:30:00+04:00' AS INSTANT))").unwrap()[0].rows,
        vec![vec![Value::DateTime("2020-12-28T00:00:00.000000".into()),Value::Instant("2024-02-29T00:00:00.000000Z".into())]]);
}

#[test]
fn unary_age_uses_the_current_utc_date_and_is_not_deterministic() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT age(CAST(current_date AS DATE)),age(CAST(current_date||'T00:00:00Z' AS INSTANT))").unwrap()[0].rows,
        vec![vec![Value::Interval("0 mons 0 days 00:00:00.000000".into()); 2]]);
    db.execute("CREATE TABLE birthdays(d DATE)").unwrap();
    assert!(
        db.execute("CREATE INDEX moving_age ON birthdays(age(d))")
            .is_err()
    );
    assert!(
        db.execute("CREATE TABLE moving(d DATE,v INTERVAL GENERATED ALWAYS AS(age(d)) STORED)")
            .is_err()
    );
}

#[test]
fn temporal_functions_propagate_null_and_reject_invalid_fields_types_and_overflows() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT extract(NULL,CAST('2024-01-01' AS DATE)),extract('year',NULL),make_interval(NULL),age(NULL),age(CAST('2024-01-01' AS DATE),NULL),date_trunc(NULL,CAST('2024-01-01' AS DATE)),date_trunc('day',NULL)").unwrap()[0].rows,
        vec![vec![Value::Null; 7]]);
    for sql in [
        "SELECT make_interval(0,0,0,0,0,0,0,0)",
        "SELECT make_interval(0.5)",
        "SELECT make_interval('1')",
        "SELECT make_interval(2147483647)",
        "SELECT make_interval(0,0,2147483647)",
        "SELECT make_interval(0,0,0,0,0,0,CAST('9223372036854.775808' AS DECIMAL))",
        "SELECT extract('unknown',CAST('2024-01-01' AS DATE))",
        "SELECT extract('hour',CAST('2024-01-01' AS DATE))",
        "SELECT extract('timezone',CAST('2024-01-01T00:00:00' AS DATETIME))",
        "SELECT extract('year','2024-01-01')",
        "SELECT age(CAST('2024-01-01T00:00:00Z' AS INSTANT),CAST('2024-01-01' AS DATE))",
        "SELECT date_trunc('week',CAST('1 month' AS INTERVAL))",
        "SELECT date_trunc('century',CAST('0000-01-01' AS DATE))",
        "SELECT date_trunc('day',1)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
}

#[test]
fn temporal_functions_survive_generated_columns_indexes_and_snapshots() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE events(earlier DATETIME,at DATETIME,span INTERVAL GENERATED ALWAYS AS(age(at,earlier)) STORED,bucket DATETIME GENERATED ALWAYS AS(date_trunc('month',at)) STORED,seconds DECIMAL GENERATED ALWAYS AS(extract('second',at)) STORED,v INTERVAL GENERATED ALWAYS AS(make_interval(0,0,0,1)) STORED); CREATE INDEX events_day ON events(date_trunc('day',at)); INSERT INTO events(earlier,at) VALUES('2024-02-01T00:00:00','2024-03-01T03:04:05.123456')").unwrap();
    let expected = vec![vec![
        Value::Interval("1 mons 0 days 03:04:05.123456".into()),
        Value::DateTime("2024-03-01T00:00:00.000000".into()),
        Value::Decimal("5.123456".into()),
        Value::Interval("0 mons 1 days 00:00:00.000000".into()),
    ]];
    for format in [Format::Binary, Format::Readable] {
        let mut restored = Database::from_snapshot(&db.export(format).unwrap()).unwrap();
        for plan in ["INDEXED BY events_day", "NOT INDEXED"] {
            let sql = format!(
                "SELECT span,bucket,seconds,v FROM events {plan} WHERE date_trunc('day',at)=CAST('2024-03-01T00:00:00' AS DATETIME)"
            );
            assert_eq!(restored.execute(&sql).unwrap()[0].rows, expected);
        }
        assert_eq!(
            serde_json::to_value(
                restored
                    .execute("SELECT span,bucket,seconds,v FROM events")
                    .unwrap()[0]
                    .rows
                    .clone()
            )
            .unwrap(),
            serde_json::json!([[{"Interval":"1 mons 0 days 03:04:05.123456"},{"DateTime":"2024-03-01T00:00:00.000000"},{"Decimal":"5.123456"},{"Interval":"0 mons 1 days 00:00:00.000000"}]])
        );
    }
}
