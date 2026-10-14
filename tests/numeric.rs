use boxdb::{Database, Format, Type, Value};

#[test]
fn decimal_declarations_are_rejected_without_changing_the_schema() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE prices(amount NUMERIC(12,2)); INSERT INTO prices VALUES('12.50')")
        .unwrap();
    for sql in [
        "CREATE TABLE removed(amount DECIMAL)",
        "CREATE TABLE removed(amount DECIMAL(12,2))",
        "CREATE TABLE removed(amount DECIMAL(12,2)[])",
        "CREATE TABLE removed(amount DECIMAL) WITHOUT STRICT",
        "CREATE TEMP TABLE removed(amount DECIMAL(12,2))",
        "ALTER TABLE prices ADD COLUMN removed DECIMAL(12,2)",
        "ALTER TABLE prices ADD COLUMN removed DECIMAL(12,2) AS (amount)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert_eq!(
        db.execute("SELECT * FROM prices").unwrap()[0].rows,
        vec![vec![Value::Numeric("12.50".into())]]
    );
    assert_eq!(
        db.execute(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT GLOB '__boxdb_*'"
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Text("prices".into())]]
    );
}

#[test]
fn numeric_is_the_only_exact_numeric_type_and_serialized_tag() {
    let ty = Type::parse("NUMERIC(12,2)").unwrap();
    assert_eq!(serde_json::to_string(&ty).unwrap(), r#"{"Numeric":[12,2]}"#);
    assert_eq!(
        serde_json::from_str::<Type>(r#"{"Numeric":[12,2]}"#).unwrap(),
        ty
    );
    let value: Value = serde_json::from_str(r#"{"Numeric":"12.50"}"#).unwrap();
    assert_eq!(ty.validate(&value).unwrap(), value);
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"Numeric":"12.50"}"#
    );
    assert!(serde_json::from_str::<Type>(r#"{"Decimal":[12,2]}"#).is_err());
    assert!(serde_json::from_str::<Value>(r#"{"Decimal":"12.50"}"#).is_err());
    for declaration in ["DECIMAL", "DECIMAL(12,2)", "DECIMAL[]", "ARRAY<DECIMAL>"] {
        assert!(Type::parse(declaration).is_err(), "{declaration}");
    }
}

#[test]
fn numeric_values_and_arrays_survive_reopening_and_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    for format in [Format::Binary, Format::Readable] {
        let path = directory.path().join(format!("{format:?}"));
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE prices(amount NUMERIC(20,4), history NUMERIC(20,4)[])")
            .unwrap();
        let values = vec![
            Value::Numeric("1234567890123456.1234".into()),
            Value::Array(vec![Value::Numeric("0.1000".into()), Value::Null]),
        ];
        db.execute_params("INSERT INTO prices VALUES(?,?)", &values)
            .unwrap();
        let snapshot = db.export(format).unwrap();
        drop(db);
        let mut reopened = Database::open(&path, format).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM prices").unwrap()[0].rows,
            vec![values.clone()]
        );
        let mut restored = Database::from_snapshot(&snapshot).unwrap();
        assert_eq!(
            restored.execute("SELECT * FROM prices").unwrap()[0].rows,
            vec![values]
        );
    }
    assert_eq!(
        serde_json::to_string(&Value::Numeric("12.50".into())).unwrap(),
        r#"{"Numeric":"12.50"}"#
    );
}

#[test]
fn numeric_declarations_preserve_storage_and_validation() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE prices(n NUMERIC(12,2), d NUMERIC(12,2), history NUMERIC(12,2)[]); ALTER TABLE prices ADD COLUMN added NUMERIC(12,2)").unwrap();
    db.execute("INSERT INTO prices VALUES('12.5','12.5','[\"12.5\"]','12.5')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM prices").unwrap()[0].rows,
        vec![vec![
            Value::Numeric("12.50".into()),
            Value::Numeric("12.50".into()),
            Value::Array(vec![Value::Numeric("12.50".into())]),
            Value::Numeric("12.50".into()),
        ]]
    );
    for column in ["n", "d", "added"] {
        assert!(
            db.execute(&format!("UPDATE prices SET {column}='1.234'"))
                .is_err()
        );
    }
}
