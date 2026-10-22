#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, Type, Value};

#[test]
fn time_is_a_native_time_of_day_in_sql_and_rust() {
    assert_eq!(Type::parse("time").unwrap(), Type::Time);
    let expected = Value::Time("12:34:56.123457".into());
    assert_eq!(
        Type::Time
            .validate(&Value::Time("12:34:56.123456789".into()))
            .unwrap(),
        expected
    );
    assert_eq!(
        serde_json::to_value(&expected).unwrap(),
        serde_json::json!({"Time":"12:34:56.123457"})
    );
    let mut db = Database::memory();
    db.execute("CREATE TABLE schedule(at TIME); INSERT INTO schedule VALUES('12:34:56.123456789')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT at, typeof(at), CAST('00:00:00' AS TIME) FROM schedule")
            .unwrap()[0]
            .rows,
        vec![vec![
            expected,
            Value::Text("time".into()),
            Value::Time("00:00:00.000000".into())
        ]]
    );
}

#[test]
fn times_sort_and_match_indexes_without_becoming_numbers() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE schedule(id INT64 PRIMARY KEY, at TIME UNIQUE); INSERT INTO schedule VALUES(1,'23:59:59.999999'),(2,'00:00:00'),(3,'12:34:56.123456')").unwrap();
    assert_eq!(
        db.execute("SELECT id FROM schedule ORDER BY at").unwrap()[0].rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(1)]
        ]
    );
    db.execute("CREATE INDEX schedule_at ON schedule(at)")
        .unwrap();
    for plan in ["INDEXED BY schedule_at", "NOT INDEXED"] {
        assert_eq!(
            db.execute(&format!(
                "SELECT id FROM schedule {plan} WHERE at='12:34:56.123456499'"
            ))
            .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(3)]]
        );
    }
    assert!(
        db.execute("INSERT INTO schedule VALUES(4,'12:34:56.123456499')")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT COUNT(DISTINCT at),MIN(at),MAX(at) FROM schedule")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Integer(3),
            Value::Time("00:00:00.000000".into()),
            Value::Time("23:59:59.999999".into())
        ]]
    );
    for sql in [
        "SELECT SUM(at) FROM schedule",
        "SELECT AVG(at) FROM schedule",
        "SELECT abs(at) FROM schedule",
        "SELECT CAST(at AS INT64) FROM schedule",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert_eq!(db.execute("SELECT CAST('12:34:56.123456' AS TIME)=CAST('45296123456' AS UINT64), CAST('12:34:56.123456' AS TIME)=CAST('2024-01-01T12:34:56.123456' AS DATETIME)").unwrap()[0].rows,
        vec![vec![Value::Integer(0), Value::Integer(0)]]);
}

#[test]
fn time_validates_clock_fields_and_rounds_without_a_date_or_zone() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(at TIME)").unwrap();
    for (input, expected) in [
        ("00:00:00", "00:00:00.000000"),
        ("23:59:59.999999", "23:59:59.999999"),
        ("12:34:56.123456500", "12:34:56.123456"),
        ("12:34:56.123457500", "12:34:56.123458"),
        ("23:59:59.999999500", "00:00:00.000000"),
    ] {
        assert_eq!(
            Type::Time.cast(Value::Text(input.into())).unwrap(),
            Value::Time(expected.into())
        );
        db.execute_params("INSERT INTO t VALUES(?)", &[Value::Time(input.into())])
            .unwrap();
    }
    for input in [
        "24:00:00",
        "12:60:00",
        "12:00:60",
        "12:34",
        "1:02:03",
        "12:34:56Z",
        "12:34:56+04:00",
        "2024-02-29T12:34:56",
        "12:34:56.1234567891",
        "-01:00:00",
    ] {
        assert!(
            Type::Time.validate(&Value::Time(input.into())).is_err(),
            "{input}"
        );
        assert!(
            db.execute_params("INSERT INTO t VALUES(?)", &[Value::Text(input.into())])
                .is_err(),
            "{input}"
        );
        assert!(
            db.execute(&format!("UPDATE t SET at='{input}'")).is_err(),
            "{input}"
        );
    }
    assert_eq!(
        db.execute("SELECT COUNT(*) FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(5)]]
    );
    db.execute("INSERT INTO t VALUES(NULL)").unwrap();
}

#[test]
fn datetime_can_explicitly_discard_its_date_but_time_cannot_invent_one() {
    let mut db = Database::memory();
    assert_eq!(
        Type::Time
            .cast(Value::DateTime("2024-02-29T12:34:56.123456".into()))
            .unwrap(),
        Value::Time("12:34:56.123456".into())
    );
    assert_eq!(db.execute("SELECT CAST(CAST('2024-02-29T12:34:56.123456' AS DATETIME) AS TIME),CAST(CAST('2024-02-29T16:34:56.123456+04:00' AS INSTANT) AS TIME)").unwrap()[0].rows,
        vec![vec![Value::Time("12:34:56.123456".into());2]]);
    db.execute("CREATE TABLE t(at TIME)").unwrap();
    assert!(
        db.execute("INSERT INTO t VALUES(CAST('2024-02-29T12:34:56' AS DATETIME))")
            .is_err()
    );
    for ty in ["DATE", "DATETIME", "INSTANT", "INTERVAL"] {
        assert!(
            db.execute(&format!("SELECT CAST(CAST('12:34:56' AS TIME) AS {ty})"))
                .is_err(),
            "{ty}"
        );
    }
}

#[test]
fn time_survives_storage_conversion_arrays_and_schema_changes() {
    for format in [Format::Binary, Format::Readable] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("times");
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE schedule(id INT64 PRIMARY KEY,at TIME DEFAULT '08:30:00',times TIME[][],rounded TIME AS(at) STORED); CREATE INDEX schedule_at ON schedule(at); INSERT INTO schedule(id) VALUES(1)").unwrap();
        db.execute_params(
            "INSERT INTO schedule(id,at,times) VALUES(2,?,?)",
            &[
                Value::Time("12:34:56.123456789".into()),
                Value::Array(vec![
                    Value::Array(vec![Value::Time("23:59:59.9999995".into()), Value::Null]),
                    Value::Array(vec![]),
                ]),
            ],
        )
        .unwrap();
        db.execute("ALTER TABLE schedule ADD COLUMN closing TIME DEFAULT '17:00:00'; CREATE TABLE copied AS SELECT at FROM schedule; CREATE TABLE source(id INT64); CREATE TRIGGER fill AFTER INSERT ON source BEGIN INSERT INTO schedule(id,at) VALUES(NEW.id,'06:00:00'); END; INSERT INTO source VALUES(3)").unwrap();
        let expected = vec![
            vec![
                Value::Time("08:30:00.000000".into()),
                Value::Null,
                Value::Time("08:30:00.000000".into()),
                Value::Time("17:00:00.000000".into()),
            ],
            vec![
                Value::Time("12:34:56.123457".into()),
                Value::Array(vec![
                    Value::Array(vec![Value::Time("00:00:00.000000".into()), Value::Null]),
                    Value::Array(vec![]),
                ]),
                Value::Time("12:34:56.123457".into()),
                Value::Time("17:00:00.000000".into()),
            ],
            vec![
                Value::Time("06:00:00.000000".into()),
                Value::Null,
                Value::Time("06:00:00.000000".into()),
                Value::Time("17:00:00.000000".into()),
            ],
        ];
        for target in [Format::Binary, Format::Readable] {
            let bytes = files::converted_bytes(&mut db, target).unwrap();
            if target == Format::Binary {
                assert!(bytes.starts_with(b"SQLite format 3\0"));
            } else {
                assert!(String::from_utf8(bytes.clone()).unwrap().contains("time:"));
            }
            let mut restored = files::open_bytes(&bytes).unwrap();
            assert_eq!(
                restored
                    .execute("SELECT at,times,rounded,closing FROM schedule ORDER BY id")
                    .unwrap()[0]
                    .rows,
                expected
            );
            assert_eq!(
                restored.execute("SELECT typeof(at) FROM copied").unwrap()[0].rows,
                vec![vec![Value::Text("time".into())]; 2]
            );
            assert_eq!(
                restored.execute("PRAGMA integrity_check").unwrap()[0].rows,
                vec![vec![Value::Text("ok".into())]]
            );
        }
        drop(db);
        let mut db = Database::open(&path, format).unwrap();
        assert_eq!(
            db.execute("SELECT at,times,rounded,closing FROM schedule ORDER BY id")
                .unwrap()[0]
                .rows,
            expected
        );
    }
}
