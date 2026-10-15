#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, Type, Value};
use rusqlite::Connection;

#[test]
fn removed_type_names_are_rejected_by_the_public_type_parser() {
    for name in [
        "DOUBLE PRECISION",
        "CHARACTER VARYING",
        "JSONB",
        "FLOAT4",
        "FLOAT",
        "FLOAT8",
    ] {
        for declaration in [
            name.to_owned(),
            name.to_lowercase(),
            format!("{name}[]"),
            format!("ARRAY<{name}>"),
        ] {
            assert!(Type::parse(&declaration).is_err(), "{declaration}");
        }
    }
    assert_eq!(Type::parse("REAL").unwrap(), Type::Double);
    assert_eq!(Type::parse("DOUBLE").unwrap(), Type::Double);
    assert!(Type::parse("VARCHAR(3)").is_ok());
    assert!(Type::parse("JSON").is_ok());
}

#[test]
fn removed_type_names_cannot_be_used_in_sql_declarations_or_casts() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE existing(n INT64); CREATE TEMP TABLE scratch(n INT64); ATTACH ':memory:' AS other; CREATE TABLE other.existing(n INT64)").unwrap();
    for name in [
        "DOUBLE PRECISION",
        "CHARACTER VARYING",
        "JSONB",
        "FLOAT4",
        "FLOAT",
        "FLOAT8",
        "FLOAT(24)",
        "CHARACTER VARYING(3)",
    ] {
        for sql in [
            format!("CREATE TABLE rejected(n {name})"),
            format!("CREATE TABLE rejected(n {name}[])"),
            format!("CREATE TEMP TABLE rejected(n {name})"),
            format!("CREATE TABLE other.rejected(n {name})"),
            format!("ALTER TABLE existing ADD COLUMN removed {name}"),
            format!("ALTER TABLE temp.scratch ADD COLUMN removed {name}"),
            format!("ALTER TABLE other.existing ADD COLUMN removed {name}"),
            format!("SELECT CAST(1 AS {name})"),
            format!("SELECT CAST(NULL AS {name}) WHERE 0"),
        ] {
            assert!(db.statement_info(&sql).is_err(), "prepare: {sql}");
            assert!(db.execute(&sql).is_err(), "execute: {sql}");
        }
    }
    assert_eq!(
        db.execute("SELECT 'FLOAT', 'JSONB', CAST(0.5 AS REAL), CAST(0.5 AS DOUBLE)")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("FLOAT".into()),
            Value::Text("JSONB".into()),
            Value::Real(0.5),
            Value::Real(0.5)
        ]]
    );
}

#[test]
fn rich_types_round_trip_through_sqlite_and_readable_storage() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE rich(id INT64 PRIMARY KEY,s INT16,i INT32,b BOOLEAN,word VARCHAR(3),n DECIMAL(20,4),u UUID,j JSON,ts INSTANT,bytes BYTEA,tags TEXT[],matrix INT64[][],f DOUBLE,decimals DECIMAL(20,4)[],blobs BYTEA[])").unwrap();
    let values = vec![
        Value::Integer(1),
        Value::Integer(i16::MIN.into()),
        Value::Integer(i32::MAX.into()),
        Value::Boolean(true),
        Value::Text("猫🙂".into()),
        Value::Decimal("1234567890123456.1234".into()),
        Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
        Value::Json(serde_json::json!({"x":[true,null,1]})),
        Value::Instant("2024-02-29T12:34:56.123457Z".into()),
        Value::Bytea(vec![0, 255]),
        Value::Array(vec![
            Value::Text("NULL".into()),
            Value::Text("雪".into()),
            Value::Null,
        ]),
        Value::Array(vec![
            Value::Array(vec![Value::Integer(i64::MAX), Value::Null]),
            Value::Array(vec![]),
        ]),
        Value::Real(1.23456789012345),
        Value::Array(vec![
            Value::Decimal("1234567890123456.1234".into()),
            Value::Null,
        ]),
        Value::Array(vec![Value::Bytea(vec![0, 255]), Value::Bytea(vec![])]),
    ];
    db.execute_params(
        &format!(
            "INSERT INTO rich VALUES({})",
            vec!["?"; values.len()].join(",")
        ),
        &values,
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM rich").unwrap()[0].rows,
        vec![values.clone()]
    );
    for format in [Format::Binary, Format::Readable] {
        let bytes = files::converted_bytes(&mut db, format).unwrap();
        let mut restored = files::open_bytes(&bytes).unwrap();
        assert_eq!(
            restored.execute("SELECT * FROM rich").unwrap()[0].rows,
            vec![values.clone()]
        );
        assert_eq!(
            files::converted_bytes(&mut restored, format).unwrap(),
            bytes
        );
    }
}
#[test]
fn type_checks_cover_expressions_insert_select_triggers_and_atomic_batches() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE dates(id INT64 PRIMARY KEY,d INSTANT); INSERT INTO dates VALUES(1,'2024-02-29T00:00:00.000000Z')").unwrap();
    for sql in [
        "INSERT INTO dates VALUES(2,'2023-02-29T00:00:00.000000Z')",
        "UPDATE dates SET d='2024-'||'02-30' WHERE id=1",
        "INSERT INTO dates SELECT 2,'wrong'",
        "INSERT INTO dates VALUES(2,'2024-01-01T00:00:00.000000Z'),(3,'wrong')",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
        assert_eq!(
            db.execute("SELECT id,d FROM dates").unwrap()[0].rows.len(),
            1
        );
    }
    db.execute("CREATE TABLE source(n INT64); CREATE TRIGGER bad AFTER INSERT ON source BEGIN INSERT INTO dates VALUES(NEW.n,'wrong'); END;").unwrap();
    assert!(db.execute("INSERT INTO source VALUES(2)").is_err());
    assert!(
        db.execute("SELECT * FROM source").unwrap()[0]
            .rows
            .is_empty()
    );
}
#[test]
fn invalid_rich_values_fail_without_committing_other_rows() {
    for (ty, value) in [
        ("INT16", Value::Integer(32768)),
        ("INT32", Value::Integer(i64::MAX)),
        ("DOUBLE", Value::Real(f64::INFINITY)),
        ("BOOLEAN", Value::Integer(2)),
        ("VARCHAR(2)", Value::Text("abc".into())),
        ("DECIMAL(4,2)", Value::Decimal("1.234".into())),
        ("UUID", Value::Text("wrong".into())),
        ("JSON", Value::Text("{".into())),
        ("INSTANT", Value::Text("2024-02-30T00:00:00.000000Z".into())),
        ("INSTANT", Value::Text("2024-02-29T25:00:00Z".into())),
        ("TEXT[]", Value::Array(vec![Value::Integer(1)])),
        ("DOUBLE[]", Value::Array(vec![Value::Real(f64::NAN)])),
    ] {
        let mut db = Database::memory();
        db.execute(&format!("CREATE TABLE t(n {ty})")).unwrap();
        assert!(
            db.execute_params("INSERT INTO t VALUES(?)", &[value])
                .is_err(),
            "{ty}"
        );
        assert!(db.execute("SELECT * FROM t").unwrap()[0].rows.is_empty());
    }
}

#[test]
fn reopened_readable_files_preserve_rich_types_and_reject_invalid_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("typed.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(
        "CREATE TABLE rich(id INT64 PRIMARY KEY,u UUID,n DECIMAL(8,2),d INSTANT,tags TEXT[])",
    )
    .unwrap();
    let row = vec![
        Value::Integer(1),
        Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
        Value::Decimal("123.45".into()),
        Value::Instant("2024-02-29T00:00:00.000000Z".into()),
        Value::Array(vec![Value::Text("雪".into()), Value::Null]),
    ];
    db.execute_params("INSERT INTO rich VALUES(?,?,?,?,?)", &row)
        .unwrap();
    drop(db);
    for sql in [
        "UPDATE rich SET u='wrong'",
        "UPDATE rich SET n='123.456'",
        "UPDATE rich SET d='2023-02-29T00:00:00.000000Z'",
        "UPDATE rich SET tags='[1]'",
        "INSERT INTO rich VALUES(2,'550e8400-e29b-41d4-a716-446655440000','1.23','2024-01-01T00:00:00.000000Z','[]'),(3,'wrong','1.23','2024-01-01T00:00:00.000000Z','[]')",
    ] {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        assert!(db.execute(sql).is_err(), "{sql}");
        assert_eq!(
            db.execute("SELECT * FROM rich ORDER BY id").unwrap()[0].rows,
            vec![row.clone()]
        );
        drop(db);
        assert_eq!(
            Database::open(&path, Format::Readable)
                .unwrap()
                .execute("SELECT * FROM rich ORDER BY id")
                .unwrap()[0]
                .rows,
            vec![row.clone()]
        );
    }
}

#[test]
fn a_select_with_an_invalid_typed_cell_returns_an_error_for_the_whole_result() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("invalid.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY,d JSON); INSERT INTO t VALUES(1,'{\"x\":1}'),(2,'{\"x\":2}')").unwrap();
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE t SET d='{broken' WHERE id=2", [])
        .unwrap();
    assert!(db.execute("SELECT id,d FROM t ORDER BY id").is_err());
    assert_eq!(
        db.execute("SELECT id,d FROM t WHERE id=1").unwrap()[0].rows,
        vec![vec![
            Value::Integer(1),
            Value::Json(serde_json::json!({"x":1}))
        ]]
    );
    assert_eq!(
        db.execute("SELECT id FROM t ORDER BY id").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}
#[test]
fn external_clients_need_no_boxdb_functions_and_invalid_values_are_detected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("types.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY,b BOOLEAN,d INSTANT,j JSON DEFAULT '{}'); INSERT INTO t(id,b,d) VALUES(1,1,'2024-02-29T00:00:00.000000Z'); CREATE TABLE blobs(bytes BYTEA)").unwrap();
    let sqlite = Connection::open(&path).unwrap();
    sqlite
        .execute_batch("INSERT INTO t(id,b,d) VALUES(2,0,'2024-01-01T00:00:00.000000Z')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT b FROM t ORDER BY id").unwrap()[0].rows,
        vec![vec![Value::Boolean(true)], vec![Value::Boolean(false)]]
    );
    assert!(
        sqlite
            .execute_batch("INSERT INTO t(id,b,d) VALUES(3,2,'2024-01-01T00:00:00.000000Z')")
            .is_err()
    );
    assert!(
        sqlite
            .execute("INSERT INTO blobs VALUES('text')", [])
            .is_err()
    );
    sqlite
        .execute("INSERT INTO blobs VALUES(x'00ff')", [])
        .unwrap();
    assert_eq!(
        db.execute("SELECT bytes FROM blobs").unwrap()[0].rows,
        vec![vec![Value::Bytea(vec![0, 255])]]
    );
    sqlite
        .execute_batch("UPDATE t SET j='invalid' WHERE id=2")
        .unwrap();
    assert!(db.execute("SELECT j FROM t WHERE id=2").is_err());
    assert!(db.execute("UPDATE t SET b=1 WHERE id=2").is_err());
    db.execute("UPDATE t SET j='{}' WHERE id=2").unwrap();
    assert!(db.execute("SELECT d FROM t").is_ok());
}
#[test]
fn typed_metadata_and_triggers_follow_rollback_and_savepoints() {
    let mut db = Database::memory();
    db.execute("BEGIN; CREATE TABLE t(d INSTANT); INSERT INTO t VALUES('2024-01-01T00:00:00.000000Z'); SAVEPOINT s; DROP TABLE t; CREATE TABLE t(n INT64); INSERT INTO t VALUES(1); ROLLBACK TO s").unwrap();
    assert!(db.execute("INSERT INTO t VALUES('wrong')").is_err());
    db.execute("ROLLBACK").unwrap();
    db.execute("CREATE TABLE t(n INT64); INSERT INTO t VALUES(1)")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn typed_alter_table_keeps_metadata_atomic_and_tracks_renames() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE t(id INT64 PRIMARY KEY,d INSTANT); INSERT INTO t VALUES(1,'2024-01-01T00:00:00.000000Z')",
    )
    .unwrap();
    db.execute("ALTER TABLE t ADD COLUMN tags TEXT[]; ALTER TABLE t RENAME COLUMN d TO day; ALTER TABLE t RENAME TO events").unwrap();
    db.execute_params(
        "UPDATE events SET tags=?",
        &[Value::Array(vec![Value::Text("new".into())])],
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT day,tags FROM events").unwrap()[0].rows,
        vec![vec![
            Value::Instant("2024-01-01T00:00:00.000000Z".into()),
            Value::Array(vec![Value::Text("new".into())])
        ]]
    );
    db.execute("BEGIN; ALTER TABLE events DROP COLUMN day; ROLLBACK")
        .unwrap();
    assert!(db.execute("UPDATE events SET day='wrong'").is_err());
    assert!(
        db.execute("ALTER TABLE events ADD COLUMN bad DECIMAL(0,2)")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT * FROM events").unwrap()[0].columns.len(),
        3
    );
    let mut restored =
        files::open_bytes(&files::converted_bytes(&mut db, Format::Binary).unwrap()).unwrap();
    assert!(restored.execute("UPDATE events SET day='wrong'").is_err());
}
#[test]
fn a_long_lived_client_discovers_types_created_by_another_boxdb_client() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("schema.db");
    let mut reader = Database::open(&path, Format::Binary).unwrap();
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    writer
        .execute("CREATE TABLE t(d INSTANT); INSERT INTO t VALUES('2024-01-01T00:00:00.000000Z')")
        .unwrap();
    assert_eq!(
        reader.execute("SELECT d FROM t").unwrap()[0].rows,
        vec![vec![Value::Instant("2024-01-01T00:00:00.000000Z".into())]]
    );
    assert!(reader.execute("INSERT INTO t VALUES('wrong')").is_err());
}

#[test]
fn metadata_is_reserved_and_invalid_typed_alter_defaults_rollback() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(d INSTANT); INSERT INTO t VALUES('2024-01-01T00:00:00.000000Z')")
        .unwrap();
    for sql in [
        "DELETE FROM __boxdb_columns",
        "DROP TABLE __boxdb_columns",
        "DROP TRIGGER temp.__boxdb_validate_0_INSERT",
        "CREATE TABLE __boxdb_fake(n INT64)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert!(
        db.execute("ALTER TABLE t ADD COLUMN wrong INSTANT DEFAULT 'invalid'")
            .is_err()
    );
    assert_eq!(db.execute("SELECT * FROM t").unwrap()[0].columns, vec!["d"]);
    assert!(db.execute("INSERT INTO t VALUES('wrong')").is_err());
}

#[test]
fn comments_and_explicit_main_schema_keep_type_validation() {
    let mut db = Database::memory();
    db.execute("-- schema\n /* BoxDB */ CREATE TABLE main.t(d INSTANT)")
        .unwrap();
    db.execute("CREATE TABLE IF NOT EXISTS main.T(d UUID)")
        .unwrap();
    assert!(db.execute("INSERT INTO t VALUES('invalid')").is_err());
    db.execute("/* migration */ ALTER TABLE main.t ADD COLUMN tags TEXT[]")
        .unwrap();
    assert!(
        db.execute("INSERT INTO t VALUES('2024-01-01T00:00:00.000000Z','[1]')")
            .is_err()
    );
}

#[test]
fn another_boxdb_clients_typed_schema_changes_refresh_write_validators() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("alter.db");
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    writer
        .execute("CREATE TABLE t(id INT64 PRIMARY KEY,d INSTANT)")
        .unwrap();
    let mut reader = Database::open(&path, Format::Binary).unwrap();
    writer
        .execute("ALTER TABLE t RENAME COLUMN d TO day")
        .unwrap();
    assert!(reader.execute("INSERT INTO t VALUES(1,'invalid')").is_err());
    reader
        .execute("INSERT INTO t VALUES(1,'2024-01-01T00:00:00.000000Z')")
        .unwrap();
    assert_eq!(
        reader.execute("SELECT day FROM t").unwrap()[0].rows,
        vec![vec![Value::Instant("2024-01-01T00:00:00.000000Z".into())]]
    );
    // Both clients and native SQLite may invalidate prepared column origins.
    writer
        .execute("CREATE VIEW v AS SELECT day AS value FROM t")
        .unwrap();
    let sql = "SELECT value FROM v";
    assert_eq!(
        reader.execute(sql).unwrap()[0].rows,
        vec![vec![Value::Instant("2024-01-01T00:00:00.000000Z".into())]]
    );
    let sqlite = Connection::open(&path).unwrap();
    sqlite
        .execute_batch("DROP VIEW v; CREATE VIEW v AS SELECT id AS value FROM t")
        .unwrap();
    assert_eq!(
        reader.execute(sql).unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
    writer.execute("ALTER TABLE t DROP COLUMN day; ALTER TABLE t ADD COLUMN day TEXT; UPDATE t SET day='native'").unwrap();
    assert_eq!(
        reader.execute("SELECT day FROM t").unwrap()[0].rows,
        vec![vec![Value::Text("native".into())]]
    );
}

#[test]
fn sqlite_vacuum_preserves_typed_metadata() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE t(d INSTANT); INSERT INTO t VALUES('2024-01-01T00:00:00.000000Z'); VACUUM",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT d FROM t").unwrap()[0].rows,
        vec![vec![Value::Instant("2024-01-01T00:00:00.000000Z".into())]]
    );
    assert!(db.execute("INSERT INTO t VALUES('invalid')").is_err());
}
