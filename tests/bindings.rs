use boxdb::{Database, Type, Value};

#[test]
fn native_values_keep_validation_and_sqlite_storage_types() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(b BOOLEAN,j JSON,n DECIMAL(12,2)); INSERT INTO t VALUES(1,'{\"x\":1}','12.50')").unwrap();
    assert_eq!(
        db.execute("SELECT b,j,n FROM t").unwrap()[0].rows,
        vec![vec![
            Value::Boolean(true),
            Value::Json(serde_json::json!({"x":1})),
            Value::Decimal("12.50".into())
        ]]
    );
    assert_eq!(
        db.execute_native("SELECT b,j,n FROM t", &[]).unwrap().rows,
        vec![vec![
            Value::Integer(1),
            Value::Text("{\"x\":1}".into()),
            Value::Decimal("12.5".into())
        ]]
    );
    assert!(
        db.execute_native("UPDATE t SET b=?", &[Value::Integer(2)])
            .is_err()
    );
    assert_eq!(
        db.execute_native("SELECT b FROM t", &[]).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn native_reads_reject_invalid_values_written_outside_boxdb() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.db");
    let mut db = Database::open(&path, boxdb::Format::Binary).unwrap();
    db.execute("CREATE TABLE t(j JSON)").unwrap();
    let sqlite = rusqlite::Connection::open(&path).unwrap();
    sqlite
        .execute("INSERT INTO t VALUES('invalid json')", [])
        .unwrap();
    assert!(db.execute_native("SELECT j FROM t", &[]).is_err());
}

#[test]
fn preparation_reports_parameters_and_columns_without_executing_sql() {
    let mut db = Database::memory();
    let create = db.statement_info("CREATE TABLE t(n INTEGER)").unwrap();
    assert!(create.columns.is_empty());
    assert!(db.execute("SELECT * FROM t").is_err());
    db.execute_native("CREATE TABLE t(n INTEGER)", &[]).unwrap();
    let info = db
        .statement_info("SELECT n AS value FROM t WHERE n=:n AND n > ?")
        .unwrap();
    assert_eq!(info.columns, vec!["value"]);
    assert_eq!(info.parameters, vec![Some(":n".into()), None]);
    assert!(info.readonly);
    assert!(db.statement_info("SELECT 1; SELECT 2").is_err());
    assert!(db.statement_info("-- no SQL").is_err());
    assert!(
        db.execute_native("SELECT 1; -- trailing comment", &[])
            .is_ok()
    );
}

#[test]
fn drizzle_serial_primary_keys_use_sqlite_rowids() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE __drizzle_migrations(id SERIAL PRIMARY KEY,hash TEXT NOT NULL,created_at INTEGER); INSERT INTO __drizzle_migrations(hash,created_at) VALUES('hash',123)").unwrap();
    assert_eq!(db.last_insert_rowid(), 1);
    assert_eq!(
        db.execute_native("SELECT id,created_at FROM __drizzle_migrations", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1), Value::Integer(123)]]
    );
    assert_eq!(Type::parse("INTEGER").unwrap(), Type::Int64);
}
