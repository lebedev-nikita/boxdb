use boxdb::{Database, Format, Value};

#[test]
fn binary_snapshots_use_boxdb_header() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO notes VALUES(1,'snapshot')").unwrap();
    let snapshot = db.export(Format::Binary).unwrap();
    assert!(snapshot.starts_with(b"BOXDB\0\x01"));
    let mut decoded = Database::from_snapshot(&snapshot).unwrap();
    assert_eq!(
        decoded.execute("SELECT id,body FROM notes").unwrap()[0].rows,
        vec![vec![Value::Integer(1), Value::Text("snapshot".into())]]
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("restored.boxdb");
    drop(Database::restore(&path, &snapshot).unwrap());
    assert!(std::fs::read(&path).unwrap().starts_with(b"BOXDB\0\x01"));
    let mut reopened = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        reopened.execute("SELECT body FROM notes").unwrap()[0].rows,
        vec![vec![Value::Text("snapshot".into())]]
    );
}

#[test]
fn string_snapshots_round_trip_rich_types_without_losing_precision_or_nulls() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE rich(s SMALLINT,i INTEGER,b BIGINT,lo BIGINT,f DOUBLE PRECISION,n NUMERIC(20,4),ok BOOLEAN,word VARCHAR(3),bytes BYTEA,u UUID,j JSON,d DATE,t TIME,ts TIMESTAMP,tz TIMESTAMPTZ,tags TEXT[],matrix BIGINT[][],nothing TEXT,literal TEXT,empty TEXT,json_null JSON,empty_bytes BYTEA,empty_array TEXT[],booleans BOOLEAN[])").unwrap();
    let values = vec![
        Value::Integer(i16::MIN.into()),
        Value::Integer(i32::MAX.into()),
        Value::Integer(i64::MAX),
        Value::Integer(i64::MIN),
        Value::Real(f64::MAX),
        Value::Numeric("1234567890123456.1234".into()),
        Value::Boolean(true),
        Value::Text("a🙂".into()),
        Value::Bytea(vec![0, 127, 255]),
        Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
        Value::Json(serde_json::json!({"integer": i64::MAX, "text": "\"x\n", "null": null})),
        Value::Date("2024-02-29".into()),
        Value::Time("12:34:56.123456789".into()),
        Value::Timestamp("2024-02-29 12:34:56.123456789".into()),
        Value::Timestamptz("2024-02-29T12:34:56.123456789Z".into()),
        Value::Array(vec![
            Value::Text("NULL".into()),
            Value::Text(String::new()),
            Value::Text("quotes\"\n雪".into()),
            Value::Null,
        ]),
        Value::Array(vec![
            Value::Array(vec![Value::Integer(i64::MAX), Value::Null]),
            Value::Array(vec![]),
            Value::Null,
        ]),
        Value::Null,
        Value::Text("NULL".into()),
        Value::Text(String::new()),
        Value::Json(serde_json::Value::Null),
        Value::Bytea(vec![]),
        Value::Array(vec![]),
        Value::Array(vec![
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Null,
        ]),
    ];
    let placeholders = vec!["?"; values.len()].join(",");
    db.execute_params(&format!("INSERT INTO rich VALUES({placeholders})"), &values)
        .unwrap();
    let expected = db.execute("SELECT * FROM rich").unwrap()[0].rows.clone();
    let readable = db.export(Format::Readable).unwrap();
    let row: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&readable)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    let cells = row["values"].as_array().unwrap();
    assert!(
        cells
            .iter()
            .all(|value| value.is_string() || value.is_null())
    );
    assert_eq!(cells[2], i64::MAX.to_string());
    assert_eq!(cells[3], i64::MIN.to_string());
    assert_eq!(cells[5], "1234567890123456.1234");
    assert_eq!(cells[6], "true");
    assert_eq!(cells[8], "007fff");
    assert_eq!(cells[17], serde_json::Value::Null);
    assert_eq!(cells[18], "NULL");
    assert_eq!(cells[19], "");
    assert_eq!(cells[20], "null");
    assert_eq!(cells[21], "");
    assert_eq!(cells[22], "[]");
    let tags: serde_json::Value = serde_json::from_str(cells[15].as_str().unwrap()).unwrap();
    assert_eq!(tags, serde_json::json!(["NULL", "", "quotes\"\n雪", null]));
    let matrix: Vec<Option<String>> = serde_json::from_str(cells[16].as_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(matrix[0].as_ref().unwrap()).unwrap(),
        serde_json::json!([i64::MAX.to_string(), null])
    );
    assert_eq!(matrix[1].as_deref(), Some("[]"));
    assert_eq!(matrix[2], None);
    for format in [Format::Readable, Format::Binary] {
        let snapshot = db.export(format).unwrap();
        let mut reopened = Database::from_snapshot(&snapshot).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM rich").unwrap()[0].rows,
            expected
        );
        assert_eq!(reopened.export(Format::Readable).unwrap(), readable);
    }
}

#[test]
fn string_snapshots_reject_invalid_values_and_column_constraints() {
    let cases = [
        ("SMALLINT", serde_json::json!("32768")),
        ("INTEGER", serde_json::json!("2147483648")),
        ("BIGINT", serde_json::json!("9223372036854775808")),
        ("INTEGER", serde_json::json!("not a number")),
        ("INTEGER", serde_json::json!(1)),
        ("INTEGER", serde_json::json!({"Integer": "1"})),
        ("DOUBLE PRECISION", serde_json::json!("NaN")),
        ("REAL", serde_json::json!("1e100")),
        ("NUMERIC(3,1)", serde_json::json!("123.4")),
        ("NUMERIC", serde_json::json!("bad")),
        ("BOOLEAN", serde_json::json!("1")),
        ("BOOLEAN", serde_json::json!(true)),
        ("VARCHAR(3)", serde_json::json!("long")),
        ("BYTEA", serde_json::json!("0")),
        ("BYTEA", serde_json::json!("zz")),
        ("UUID", serde_json::json!("bad")),
        ("JSON", serde_json::json!("{")),
        ("DATE", serde_json::json!("2024-02-30")),
        ("TIME", serde_json::json!("25:00:00")),
        ("TIMESTAMP", serde_json::json!("bad")),
        ("TIMESTAMPTZ", serde_json::json!("bad")),
        ("INTEGER[]", serde_json::json!("[1]")),
        ("INTEGER[]", serde_json::json!("[\"bad\"]")),
        ("INTEGER[]", serde_json::json!("{}")),
        ("INTEGER[][]", serde_json::json!("[\"[1]\"]")),
        ("TEXT NOT NULL", serde_json::Value::Null),
        ("INTEGER CHECK(n>0)", serde_json::json!("-1")),
    ];
    for (column, value) in cases {
        let mut db = Database::memory();
        db.execute(&format!("CREATE TABLE t(n {column})")).unwrap();
        let mut snapshot = String::from_utf8(db.export(Format::Readable).unwrap()).unwrap();
        snapshot = snapshot.replace("\"next_rowid\":1", "\"next_rowid\":2");
        let row = serde_json::json!({"kind": "row", "table": "t", "id": 1, "values": [value]});
        snapshot.push_str(&format!("{row}\n"));
        assert!(
            Database::from_snapshot(snapshot.as_bytes()).is_err(),
            "accepted invalid {column} value: {row}"
        );
    }
}

#[test]
fn both_formats_reopen_committed_typed_rows_and_discard_uncommitted_changes() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.boxdb");
        {
            let mut db = Database::open(&path, format).unwrap();
            db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n NUMERIC(20,4),tags TEXT[],created DATE); INSERT INTO t VALUES(1,1234567890123456.1234,ARRAY['a'],DATE '2024-02-29'); BEGIN; UPDATE t SET n=2; COMMIT; BEGIN; UPDATE t SET n=3").unwrap();
            assert!(Database::open(&path, format).is_err());
        }
        let mut db = Database::open(&path, Format::Binary).unwrap();
        assert_eq!(
            db.execute("SELECT n,tags,created FROM t").unwrap()[0].rows,
            vec![vec![
                Value::Numeric("2.0000".into()),
                Value::Array(vec![Value::Text("a".into())]),
                Value::Date("2024-02-29".into())
            ]]
        );
    }
}

#[test]
fn releasing_outer_implicit_savepoint_commits_to_disk() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("savepoints.boxdb");
    {
        let mut db = Database::open(&path, Format::Binary).unwrap();
        db.execute("CREATE TABLE t(n INTEGER); SAVEPOINT outermost; INSERT INTO t VALUES(1); SAVEPOINT inner; INSERT INTO t VALUES(2); RELEASE inner; RELEASE outermost").unwrap();
        assert!(!db.in_transaction());
    }
    let mut db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn readable_sql_schema_determines_row_types() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("schema.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
            .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("CREATE TABLE t (n INTEGER)", "CREATE TABLE t (n TEXT)");
    std::fs::write(&path, text).unwrap();
    let mut db = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Text("1".into())]]
    );
}

#[test]
fn foreign_keys_reopen_independently_of_table_name_order() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relations.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE zparent(n INTEGER PRIMARY KEY); CREATE TABLE achild(n INTEGER REFERENCES zparent(n) ON DELETE CASCADE); INSERT INTO zparent VALUES(1); INSERT INTO achild VALUES(1)").unwrap();
    }
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("DELETE FROM zparent").unwrap();
    assert!(
        db.execute("SELECT n FROM achild").unwrap()[0]
            .rows
            .is_empty()
    );
}

#[test]
fn foreign_keys_can_reopen_when_the_parent_key_is_a_separate_unique_index() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("indexes.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE parent(n INTEGER); CREATE UNIQUE INDEX parent_n ON parent(n); CREATE TABLE child(n INTEGER REFERENCES parent(n)); INSERT INTO parent VALUES(1); INSERT INTO child VALUES(1)").unwrap();
    }
    let mut db = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        db.execute("SELECT n FROM child").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn readable_value_normalization_cannot_bypass_unique_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("uuids.jsonl");
    let first = "550e8400-e29b-41d4-a716-446655440000";
    let second = "550e8400-e29b-41d4-a716-446655440001";
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(u UUID UNIQUE)").unwrap();
        db.execute_params(
            "INSERT INTO t VALUES(?),(?)",
            &[Value::Uuid(first.into()), Value::Uuid(second.into())],
        )
        .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace(second, &first.to_uppercase());
    std::fs::write(&path, text).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
}

#[test]
fn malformed_readable_constraints_return_errors_without_panicking() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(n INTEGER UNIQUE); INSERT INTO t VALUES(1)")
            .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("n INTEGER UNIQUE", "n INTEGER CHECK(missing>0)");
    std::fs::write(&path, text).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
}

#[test]
fn format_conversion_and_snapshot_restore_preserve_only_committed_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("restored.boxdb");
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER UNIQUE); INSERT INTO t VALUES(1); CREATE INDEX t_n ON t(n); BEGIN; INSERT INTO t VALUES(2)").unwrap();
    let bytes = db.export(Format::Readable).unwrap();
    let mut restored = Database::restore(&path, &bytes).unwrap();
    assert_eq!(restored.format(), Some(Format::Readable));
    assert_eq!(
        restored.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
    assert!(Database::restore(&path, &bytes).is_err());
    let binary = restored.export(Format::Binary).unwrap();
    assert_ne!(binary, bytes);
    let second = temp.path().join("second.boxdb");
    let mut reopened = Database::restore(&second, &binary).unwrap();
    assert!(reopened.execute("INSERT INTO t VALUES(1)").is_err());
    assert_eq!(
        reopened
            .execute("SELECT name FROM sqlite_schema WHERE type='index'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("t_n".into())]]
    );
    let invalid = temp.path().join("invalid.boxdb");
    assert!(Database::restore(&invalid, b"invalid").is_err());
    assert!(!invalid.exists());
}

#[test]
fn readable_updates_change_one_row_and_deletes_keep_other_row_ids_stable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.jsonl");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1),(2),(3)")
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    db.execute("UPDATE t SET n=20 WHERE n=2").unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count(),
        1
    );
    let last = after.lines().last().unwrap().to_string();
    db.execute("DELETE FROM t WHERE n=1").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
        last
    );
}

#[test]
fn corrupted_files_are_rejected_without_overwriting_them() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.boxdb");
        {
            let mut db = Database::open(&path, format).unwrap();
            db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
                .unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        if format == Format::Binary {
            bytes[10] ^= 1;
        } else {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace("\"values\":[\"1\"]", "\"values\":[\"bad\"]")
                .into_bytes();
        }
        std::fs::write(&path, &bytes).unwrap();
        assert!(Database::open(&path, format).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn failed_file_publication_does_not_change_live_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    std::fs::rename(&path, temp.path().join("saved.boxdb")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(db.execute("UPDATE t SET n=2").is_err());
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}
