use boxdb::{Database, Format, Value};
use std::error::Error as _;

#[test]
fn unique_errors_preserve_sqlite_codes_and_source() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE users(name TEXT UNIQUE); INSERT INTO users VALUES('Ada')")
        .unwrap();
    let error = db
        .execute_params("INSERT INTO users VALUES(?)", &[Value::Text("Ada".into())])
        .unwrap_err();

    assert_eq!(error.code(), "SQLITE_CONSTRAINT_UNIQUE");
    assert_eq!(error.sqlite_code(), Some(19));
    assert_eq!(error.sqlite_extended_code(), Some(2067));
    assert_eq!(error.to_string(), "UNIQUE constraint failed: users.name");
    assert!(error.source().unwrap().is::<rusqlite::Error>());
}

#[test]
fn sql_errors_preserve_primary_and_other_constraint_codes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parents(id), name TEXT NOT NULL, n INTEGER CHECK(n > 0)); INSERT INTO children VALUES(1,NULL,'Ada',1)").unwrap();
    for (sql, name, primary, extended) in [
        (
            "INSERT INTO children VALUES(1,NULL,'Grace',1)",
            "SQLITE_CONSTRAINT_PRIMARYKEY",
            19,
            1555,
        ),
        (
            "INSERT INTO children VALUES(2,NULL,NULL,1)",
            "SQLITE_CONSTRAINT_NOTNULL",
            19,
            1299,
        ),
        (
            "INSERT INTO children VALUES(2,NULL,'Grace',0)",
            "SQLITE_CONSTRAINT_CHECK",
            19,
            275,
        ),
        (
            "INSERT INTO children VALUES(2,99,'Grace',1)",
            "SQLITE_CONSTRAINT_FOREIGNKEY",
            19,
            787,
        ),
        ("SELECT * FROM missing", "SQLITE_ERROR", 1, 1),
        ("SELECT FROM", "SQLITE_ERROR", 1, 1),
    ] {
        let error = db.execute(sql).unwrap_err();
        assert_eq!(error.code(), name, "{sql}");
        assert_eq!(error.sqlite_code(), Some(primary), "{sql}");
        assert_eq!(error.sqlite_extended_code(), Some(extended), "{sql}");
    }
}

#[test]
fn deferred_foreign_key_errors_have_the_same_codes_in_both_formats() {
    let temp = tempfile::tempdir().unwrap();
    for (name, format) in [("binary", Format::Binary), ("readable", Format::Readable)] {
        let mut db = Database::open(temp.path().join(name), format).unwrap();
        db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
        let error = db.execute("INSERT INTO children VALUES(99)").unwrap_err();
        assert_eq!(error.code(), "SQLITE_CONSTRAINT_FOREIGNKEY", "{name}");
        assert_eq!(error.sqlite_code(), Some(19), "{name}");
        assert_eq!(error.sqlite_extended_code(), Some(787), "{name}");
        assert_eq!(error.to_string(), "FOREIGN KEY constraint failed");
    }
}

#[test]
fn preparation_opening_and_boxdb_validation_keep_distinct_codes() {
    let mut db = Database::memory();
    let error = db.statement_info("SELECT FROM").unwrap_err();
    assert_eq!(error.code(), "SQLITE_ERROR");
    assert_eq!(error.sqlite_code(), Some(1));
    assert_eq!(error.sqlite_extended_code(), Some(1));

    let error = db.statement_info("SELECT 1; SELECT 2").unwrap_err();
    assert_eq!(error.code(), "BOXDB_ERROR");
    assert_eq!(error.sqlite_code(), None);
    assert_eq!(error.sqlite_extended_code(), None);

    let temp = tempfile::tempdir().unwrap();
    let error = match Database::open(temp.path().join("missing/data.db"), Format::Binary) {
        Ok(_) => panic!("opening in a missing directory must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "SQLITE_CANTOPEN");
    assert_eq!(error.sqlite_code(), Some(14));
    assert_eq!(error.sqlite_extended_code(), Some(14));
}

#[test]
fn adding_context_preserves_sqlite_codes_and_source() {
    let mut db = Database::memory();
    let error = db
        .statement_info("SELECT * FROM missing")
        .unwrap_err()
        .with_context("migration 42 up");
    assert_eq!(error.to_string(), "migration 42 up: no such table: missing");
    assert_eq!(error.code(), "SQLITE_ERROR");
    assert_eq!(error.sqlite_code(), Some(1));
    assert_eq!(error.sqlite_extended_code(), Some(1));
    assert!(error.source().unwrap().is::<rusqlite::Error>());
}
