use boxdb::{Database, Format, Value};
use std::error::Error as _;

#[test]
fn physical_corruption_invalidates_the_connection_and_keeps_operation_state() {
    use std::io::{Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)")
        .unwrap();
    drop(db);
    let raw = rusqlite::Connection::open(&path).unwrap();
    let root: i64 = raw
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='items'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let page_size: i64 = raw.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    drop(raw);
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(((root - 1) * page_size) as u64))
        .unwrap();
    file.write_all(&[0xff]).unwrap();
    drop(file);
    let mut db = Database::open(&path, Format::Binary).unwrap();
    let error = db.execute("SELECT * FROM items").unwrap_err();
    assert_eq!(error.code(), "BOXDB_CORRUPT");
    assert_eq!(error.operation(), Some(boxdb::ErrorOperation::Read));
    assert_eq!(error.connection_usable(), Some(false));
    assert_eq!(error.in_transaction(), Some(false));
    assert_eq!(error.outcome(), Some(boxdb::OperationOutcome::NotApplied));
    assert_eq!(
        db.execute("SELECT 1").unwrap_err().code(),
        "BOXDB_UNAVAILABLE"
    );
    assert_eq!(
        db.statement_info("SELECT 1").unwrap_err().code(),
        "BOXDB_UNAVAILABLE"
    );
}

#[test]
fn unique_errors_preserve_boxdb_codes_and_source() {
    let mut db = Database::try_memory().unwrap();
    db.execute("CREATE TABLE users(name TEXT UNIQUE); INSERT INTO users VALUES('Ada')")
        .unwrap();
    let error = db
        .execute_params("INSERT INTO users VALUES(?)", &[Value::Text("Ada".into())])
        .unwrap_err();

    assert_eq!(error.code(), "BOXDB_CONSTRAINT_UNIQUE");
    assert_eq!(error.result_code(), Some(19));
    assert_eq!(error.extended_code(), Some(2067));
    assert_eq!(error.to_string(), "UNIQUE constraint failed: users.name");
    assert!(error.source().unwrap().is::<rusqlite::Error>());
}

#[test]
fn failed_scripts_do_not_claim_that_earlier_statements_were_not_applied() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        .unwrap();
    let error = db
        .execute("INSERT INTO items VALUES(1); SELECT * FROM missing")
        .unwrap_err();
    assert_eq!(error.outcome(), Some(boxdb::OperationOutcome::Unknown));
    assert_eq!(
        db.execute("SELECT id FROM items").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn sql_errors_preserve_primary_and_other_constraint_codes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parents(id), name TEXT NOT NULL, n INTEGER CHECK(n > 0)); INSERT INTO children VALUES(1,NULL,'Ada',1)").unwrap();
    for (sql, name, primary, extended) in [
        (
            "INSERT INTO children VALUES(1,NULL,'Grace',1)",
            "BOXDB_CONSTRAINT_PRIMARYKEY",
            19,
            1555,
        ),
        (
            "INSERT INTO children VALUES(2,NULL,NULL,1)",
            "BOXDB_CONSTRAINT_NOTNULL",
            19,
            1299,
        ),
        (
            "INSERT INTO children VALUES(2,NULL,'Grace',0)",
            "BOXDB_CONSTRAINT_CHECK",
            19,
            275,
        ),
        (
            "INSERT INTO children VALUES(2,99,'Grace',1)",
            "BOXDB_CONSTRAINT_FOREIGNKEY",
            19,
            787,
        ),
        ("SELECT * FROM missing", "BOXDB_ERROR", 1, 1),
        ("SELECT FROM", "BOXDB_ERROR", 1, 1),
    ] {
        let error = db.execute(sql).unwrap_err();
        assert_eq!(error.code(), name, "{sql}");
        assert_eq!(error.result_code(), Some(primary), "{sql}");
        assert_eq!(error.extended_code(), Some(extended), "{sql}");
    }
}

#[test]
fn deferred_foreign_key_errors_have_the_same_codes_in_both_formats() {
    let temp = tempfile::tempdir().unwrap();
    for (name, format) in [("binary", Format::Binary), ("readable", Format::Readable)] {
        let mut db = Database::open(temp.path().join(name), format).unwrap();
        db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
        let error = db.execute("INSERT INTO children VALUES(99)").unwrap_err();
        assert_eq!(error.code(), "BOXDB_CONSTRAINT_FOREIGNKEY", "{name}");
        assert_eq!(error.result_code(), Some(19), "{name}");
        assert_eq!(error.extended_code(), Some(787), "{name}");
        assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    }
}

#[test]
fn preparation_opening_and_boxdb_validation_keep_distinct_codes() {
    let mut db = Database::memory();
    let error = db.statement_info("SELECT FROM").unwrap_err();
    assert_eq!(error.code(), "BOXDB_ERROR");
    assert_eq!(error.result_code(), Some(1));
    assert_eq!(error.extended_code(), Some(1));

    let error = db.statement_info("SELECT 1; SELECT 2").unwrap_err();
    assert_eq!(error.code(), "BOXDB_VALIDATION");
    assert_eq!(error.result_code(), None);
    assert_eq!(error.extended_code(), None);

    let temp = tempfile::tempdir().unwrap();
    let error = match Database::open(temp.path().join("missing/data.db"), Format::Binary) {
        Ok(_) => panic!("opening in a missing directory must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "BOXDB_CANTOPEN");
    assert_eq!(error.result_code(), Some(14));
    assert_eq!(error.extended_code(), Some(14));
}

#[test]
fn adding_context_preserves_result_codes_and_source() {
    let mut db = Database::memory();
    let error = db
        .statement_info("SELECT * FROM missing")
        .unwrap_err()
        .with_context("migration 42 up");
    assert_eq!(error.to_string(), "migration 42 up: no such table: missing");
    assert_eq!(error.code(), "BOXDB_ERROR");
    assert_eq!(error.result_code(), Some(1));
    assert_eq!(error.extended_code(), Some(1));
    assert!(error.source().unwrap().is::<rusqlite::Error>());
}

#[test]
fn validation_and_io_failures_keep_categories_and_original_sources() {
    let mut db = Database::memory();
    let validation = db.statement_info("SELECT 1; SELECT 2").unwrap_err();
    assert_eq!(validation.code(), "BOXDB_VALIDATION");
    assert_eq!(validation.result_code(), None);
    assert_eq!(validation.extended_code(), None);

    let temp = tempfile::tempdir().unwrap();
    let io = match Database::open(temp.path().join("missing/data.yaml"), Format::Readable) {
        Ok(_) => panic!("opening in a missing directory must fail"),
        Err(error) => error.with_context("open readable database"),
    };
    assert_eq!(io.code(), "BOXDB_IO");
    assert_eq!(io.result_code(), None);
    assert_eq!(
        io.source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn invalid_json_casts_keep_the_parser_cause() {
    let error = boxdb::Type::Json.cast(Value::Text("{".into())).unwrap_err();
    assert_eq!(error.code(), "BOXDB_VALIDATION");
    assert!(error.source().unwrap().is::<serde_json::Error>());
}

#[test]
fn parameter_count_errors_are_validation_failures_with_the_original_source() {
    let mut db = Database::memory();
    let error = db.execute_native("SELECT ?", &[]).unwrap_err();
    assert_eq!(error.code(), "BOXDB_VALIDATION");
    assert_eq!(error.result_code(), None);
    assert!(error.source().unwrap().is::<rusqlite::Error>());
}

#[test]
fn readable_lock_errors_preserve_the_os_cause_without_engine_codes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let _writer = Database::open(&path, Format::Readable).unwrap();
    let error = match Database::open(&path, Format::Readable) {
        Ok(_) => panic!("a second readable writer must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "BOXDB_LOCKED");
    assert_eq!(error.result_code(), None);
    assert!(error.source().unwrap().is::<std::io::Error>());
}

#[test]
fn publication_failures_keep_io_sources_and_make_the_connection_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(n INT64)").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let error = db.execute("INSERT INTO t VALUES(1)").unwrap_err();
    assert_eq!(error.code(), "BOXDB_IO");
    assert!(error.source().unwrap().is::<std::io::Error>());
    assert_eq!(error.operation(), Some(boxdb::ErrorOperation::Publish));
    assert_eq!(error.outcome(), Some(boxdb::OperationOutcome::NotApplied));
    assert_eq!(error.connection_usable(), Some(false));
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap_err().code(),
        "BOXDB_UNAVAILABLE"
    );
}
