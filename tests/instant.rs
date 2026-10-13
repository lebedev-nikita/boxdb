use boxdb::{Database, Format, Type, Value};
use rusqlite::Connection;

const UTC: &str = "2026-10-13T12:00:00.123456789Z";
const OFFSET: &str = "2026-10-13T16:00:00.123456789+04:00";

#[test]
fn instant_is_the_sql_type_and_serialized_value_name() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE events(at INSTANT)").unwrap();
    db.execute_params(
        "INSERT INTO events VALUES(?)",
        &[Value::Text(OFFSET.into())],
    )
    .unwrap();
    let result = db.execute("SELECT at FROM events").unwrap();
    assert_eq!(
        serde_json::to_value(&result[0].rows).unwrap(),
        serde_json::json!([[{"Instant": "2026-10-13T12:00:00.123456789Z"}]])
    );
    assert!(db.execute("CREATE TABLE old(at TIMESTAMP)").is_err());
}

fn raw(db: &mut Database, table: &str) -> Vec<Vec<Value>> {
    db.execute(&format!("SELECT CAST(ts AS TEXT) FROM {table} ORDER BY ts"))
        .unwrap()[0]
        .rows
        .clone()
}

#[test]
fn instant_is_the_only_temporal_type() {
    assert_eq!(Type::parse("instant").unwrap(), Type::Instant);
    let mut db = Database::memory();
    for ty in [
        "DATE",
        "TIME",
        "TIMESTAMPTZ",
        "TIMESTAMP",
        "timestamp",
        "TIMESTAMP[]",
        "ARRAY<TIMESTAMP>",
        "TIME WITHOUT TIME ZONE",
        "TIME WITH TIME ZONE",
        "TIMESTAMP WITHOUT TIME ZONE",
        "TIMESTAMP WITH TIME ZONE",
        "DATE[]",
        "TIME[]",
        "TIMESTAMPTZ[]",
    ] {
        assert!(Type::parse(ty).is_err(), "{ty}");
        assert!(
            db.execute(&format!("CREATE TABLE t(ts {ty})")).is_err(),
            "{ty}"
        );
    }
    for value in [
        r#"{"Date":"2026-10-13"}"#,
        r#"{"Time":"12:00:00"}"#,
        r#"{"Timestamp":"2026-10-13T12:00:00Z"}"#,
        r#"{"Timestamptz":"2026-10-13T12:00:00Z"}"#,
    ] {
        assert!(serde_json::from_str::<Value>(value).is_err());
    }
}

#[test]
fn offsets_are_shifted_to_utc_without_losing_nanoseconds() {
    for (input, expected) in [
        (OFFSET, UTC),
        ("2026-10-13 08:30:00.123456789-03:30", UTC),
        ("2026-10-13T17:45:00.123456789+05:45", UTC),
        (
            "2024-03-01T00:30:00+01:00",
            "2024-02-29T23:30:00.000000000Z",
        ),
        (
            "2024-12-31T23:30:00-02:00",
            "2025-01-01T01:30:00.000000000Z",
        ),
        (
            "1969-12-31T23:59:59.999999999Z",
            "1969-12-31T23:59:59.999999999Z",
        ),
        (
            "2026-10-13T12:00:00+00:00",
            "2026-10-13T12:00:00.000000000Z",
        ),
    ] {
        assert_eq!(
            Type::Instant
                .validate(&Value::Instant(input.into()))
                .unwrap(),
            Value::Instant(expected.into()),
            "{input}"
        );
    }
}

#[test]
fn every_boxdb_write_path_stores_utc_in_both_formats() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("instants");
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE events(id INT64 PRIMARY KEY,ts INSTANT)")
            .unwrap();
        for (id, value) in [Value::Instant(OFFSET.into()), Value::Text(OFFSET.into())]
            .into_iter()
            .enumerate()
        {
            db.execute_params(
                "INSERT INTO events VALUES(?,?)",
                &[Value::Integer(id as i64), value],
            )
            .unwrap();
        }
        db.execute(&format!(
            "INSERT INTO events VALUES(2,'{OFFSET}'); INSERT INTO events SELECT 3,'{OFFSET}'"
        ))
        .unwrap();
        db.execute("CREATE TABLE source(id INT64 PRIMARY KEY); CREATE TRIGGER copy AFTER INSERT ON source BEGIN INSERT INTO events VALUES(NEW.id,'2026-10-13T08:30:00.123456789-03:30'); END; INSERT INTO source VALUES(4)").unwrap();
        db.execute("UPDATE events SET ts='2026-10-13T16:00:00.123456789'||'+04:00' WHERE id=0")
            .unwrap();
        let expected = vec![vec![Value::Text(UTC.into())]; 5];
        assert_eq!(raw(&mut db, "events"), expected);
        for snapshot in [Format::Binary, Format::Readable] {
            let mut restored = Database::from_snapshot(&db.export(snapshot).unwrap()).unwrap();
            assert_eq!(raw(&mut restored, "events"), expected);
        }
        if format == Format::Binary {
            let stored: String = Connection::open(&path)
                .unwrap()
                .query_row("SELECT ts FROM events WHERE id=0", [], |r| r.get(0))
                .unwrap();
            assert_eq!(stored, UTC);
        } else {
            let yaml = std::fs::read_to_string(&path).unwrap();
            assert!(yaml.contains(UTC));
            assert!(!yaml.contains(OFFSET));
        }
        drop(db);
        assert_eq!(
            raw(&mut Database::open(&path, format).unwrap(), "events"),
            expected
        );
    }
}

#[test]
fn instants_reject_ambiguous_or_lossy_inputs_atomically() {
    let mut db = Database::memory();
    db.execute(&format!(
        "CREATE TABLE t(ts INSTANT); INSERT INTO t VALUES('{UTC}')"
    ))
    .unwrap();
    for input in [
        "2026-10-13",
        "12:00:00",
        "2026-10-13T12:00:00",
        "2026-10-13 12:00:00",
        "2026-02-29T12:00:00Z",
        "2026-10-13T25:00:00Z",
        "2026-10-13T12:00:00+24:00",
        "2026-10-13T12:00:00[Asia/Tbilisi]",
        "2026-10-13T12:00:00.1234567891Z",
        "2016-12-31T23:59:60Z",
        "0000-01-01T00:00:00+01:00",
        "9999-12-31T23:59:59-01:00",
    ] {
        assert!(
            db.execute_params("INSERT INTO t VALUES(?)", &[Value::Instant(input.into())])
                .is_err(),
            "{input}"
        );
        assert!(
            db.execute(&format!("INSERT INTO t VALUES('{OFFSET}'),('{input}')"))
                .is_err(),
            "{input}"
        );
        assert!(
            db.execute(&format!("UPDATE t SET ts='{input}'")).is_err(),
            "{input}"
        );
        assert_eq!(raw(&mut db, "t"), vec![vec![Value::Text(UTC.into())]]);
    }
    db.execute("INSERT INTO t VALUES(NULL)").unwrap();
    assert_eq!(raw(&mut db, "t")[0], vec![Value::Null]);
}

#[test]
fn instant_arrays_normalize_typed_parameters_and_sql_json() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(ts INSTANT[][])").unwrap();
    db.execute_params(
        "INSERT INTO t VALUES(?)",
        &[Value::Array(vec![
            Value::Array(vec![Value::Instant(OFFSET.into()), Value::Null]),
            Value::Array(vec![]),
        ])],
    )
    .unwrap();
    db.execute(&format!(
        r#"INSERT INTO t VALUES('[ ["{OFFSET}", null], [] ]')"#
    ))
    .unwrap();
    assert_eq!(
        raw(&mut db, "t"),
        vec![vec![Value::Text(format!(r#"[["{UTC}",null],[]]"#))]; 2]
    );
    let snapshot = db.export(Format::Readable).unwrap();
    let mut restored = Database::from_snapshot(&snapshot).unwrap();
    assert_eq!(
        db.execute("SELECT ts FROM t").unwrap()[0].rows,
        restored.execute("SELECT ts FROM t").unwrap()[0].rows
    );
    assert!(
        db.execute(r#"UPDATE t SET ts='[["2026-10-13T12:00:00"]]'"#)
            .is_err()
    );
}

#[test]
fn normalization_handles_keys_shadowed_rowids_and_recursive_triggers() {
    let mut db = Database::memory();
    db.execute("PRAGMA recursive_triggers=ON").unwrap();
    for (table, declaration, insert) in [
        (
            "ordinary",
            "id INT64 PRIMARY KEY,ts INSTANT,other INSTANT",
            format!("1,'{OFFSET}','{OFFSET}'"),
        ),
        (
            "keyed",
            "ts INSTANT PRIMARY KEY,other INSTANT) WITHOUT ROWID --",
            format!("'{OFFSET}','{OFFSET}'"),
        ),
        (
            "shadowed",
            "rowid TEXT,_rowid_ TEXT,oid TEXT,ts INSTANT",
            format!("'a','b','c','{OFFSET}'"),
        ),
        (
            "quoted",
            "\"odd column\" TEXT PRIMARY KEY,ts INSTANT) WITHOUT ROWID --",
            format!("'a','{OFFSET}'"),
        ),
    ] {
        db.execute(&format!("CREATE TABLE {table}({declaration})"))
            .unwrap();
        db.execute(&format!("INSERT INTO {table} VALUES({insert})"))
            .unwrap();
        db.execute(&format!("UPDATE {table} SET ts='{OFFSET}'"))
            .unwrap();
        assert_eq!(
            raw(&mut db, table),
            vec![vec![Value::Text(UTC.into())]],
            "{table}"
        );
    }
    db.execute("CREATE TABLE composite(a TEXT,b INT64,ts INSTANT,PRIMARY KEY(a,b)) WITHOUT ROWID")
        .unwrap();
    db.execute(&format!(
        "INSERT INTO composite VALUES('x',1,'{OFFSET}'),('x',2,'2026-10-13T12:01:00Z')"
    ))
    .unwrap();
    db.execute(&format!("UPDATE composite SET b=3,ts='{OFFSET}' WHERE b=2"))
        .unwrap();
    assert_eq!(
        raw(&mut db, "composite"),
        vec![vec![Value::Text(UTC.into())]; 2]
    );
}

#[test]
fn instant_defaults_are_stored_in_utc_and_invalid_ddl_rolls_back() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY); INSERT INTO t VALUES(1)")
        .unwrap();
    db.execute(&format!(
        "ALTER TABLE t ADD COLUMN ts INSTANT DEFAULT '{OFFSET}'"
    ))
    .unwrap();
    db.execute("INSERT INTO t(id) VALUES(2)").unwrap();
    assert_eq!(raw(&mut db, "t"), vec![vec![Value::Text(UTC.into())]; 2]);
    assert!(
        db.execute("ALTER TABLE t ADD COLUMN bad INSTANT DEFAULT '2026-10-13T12:00:00'")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap()[0].columns,
        vec!["id", "ts"]
    );
    db.execute(&format!(
        "CREATE TABLE defaults(ts INSTANT DEFAULT '{OFFSET}'); INSERT INTO defaults DEFAULT VALUES"
    ))
    .unwrap();
    assert_eq!(
        raw(&mut db, "defaults"),
        vec![vec![Value::Text(UTC.into())]]
    );
}

#[test]
fn canonical_instants_sort_and_enforce_unique_instants() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY,ts INSTANT UNIQUE); INSERT INTO t VALUES(1,'2026-10-13T12:00:00.1Z'),(2,'2026-10-13T16:00:00+04:00'),(3,'2026-10-13T12:00:00.000000001Z')").unwrap();
    assert_eq!(
        db.execute("SELECT id FROM t ORDER BY ts").unwrap()[0].rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(1)]
        ]
    );
    for sql in [
        "INSERT INTO t VALUES(4,'2026-10-13T12:00:00Z')",
        "INSERT OR IGNORE INTO t VALUES(4,'2026-10-13T07:00:00-05:00')",
        "UPDATE OR IGNORE t SET ts='2026-10-13T12:00:00Z' WHERE id=1",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
        assert_eq!(raw(&mut db, "t").len(), 3);
    }
    assert_eq!(
        db.execute_params(
            "SELECT id FROM t WHERE ts=?",
            &[Value::Instant("2026-10-13T16:00:00+04:00".into())]
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn generated_instants_must_already_use_canonical_utc() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(input TEXT,ts INSTANT GENERATED ALWAYS AS(input) STORED)")
        .unwrap();
    db.execute_params("INSERT INTO t(input) VALUES(?)", &[Value::Text(UTC.into())])
        .unwrap();
    assert!(
        db.execute_params(
            "INSERT INTO t(input) VALUES(?)",
            &[Value::Text(OFFSET.into())]
        )
        .is_err()
    );
    assert_eq!(raw(&mut db, "t"), vec![vec![Value::Text(UTC.into())]]);
}

#[test]
fn a_temp_table_cannot_redirect_main_instant_normalization() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE events(id INT64 PRIMARY KEY,ts INSTANT); CREATE TEMP TABLE events(id INT64 PRIMARY KEY,ts TEXT); INSERT INTO temp.events VALUES(1,'temporary')").unwrap();
    db.execute(&format!("INSERT INTO main.events VALUES(1,'{OFFSET}')"))
        .unwrap();
    assert_eq!(
        raw(&mut db, "main.events"),
        vec![vec![Value::Text(UTC.into())]]
    );
    assert_eq!(
        raw(&mut db, "temp.events"),
        vec![vec![Value::Text("temporary".into())]]
    );
}

#[test]
fn readable_import_normalizes_offsets_without_replaying_user_triggers() {
    let mut db = Database::memory();
    db.execute(&format!("CREATE TABLE events(ts INSTANT); INSERT INTO events VALUES('{UTC}'); CREATE TABLE audit(message TEXT); CREATE TRIGGER log AFTER UPDATE ON events BEGIN INSERT INTO audit VALUES('updated'); END")).unwrap();
    let yaml = String::from_utf8(db.export(Format::Readable).unwrap())
        .unwrap()
        .replace(UTC, OFFSET);
    let mut imported = Database::from_snapshot(yaml.as_bytes()).unwrap();
    assert_eq!(
        raw(&mut imported, "events"),
        vec![vec![Value::Text(UTC.into())]]
    );
    assert_eq!(
        imported.execute("SELECT COUNT(*) FROM audit").unwrap()[0].rows,
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn readable_import_rejects_noncanonical_generated_instants() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE events(input TEXT,ts INSTANT GENERATED ALWAYS AS(input) STORED)")
        .unwrap();
    db.execute_params(
        "INSERT INTO events(input) VALUES(?)",
        &[Value::Text(UTC.into())],
    )
    .unwrap();
    let yaml = String::from_utf8(db.export(Format::Readable).unwrap())
        .unwrap()
        .replace(UTC, OFFSET);
    assert!(Database::from_snapshot(yaml.as_bytes()).is_err());
}
