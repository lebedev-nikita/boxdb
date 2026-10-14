use boxdb::{Database, Format, Type, Value};

#[test]
fn decimal_is_the_only_public_exact_decimal_type_and_serialized_tag() {
    assert!(Type::parse("NUMERIC").is_err());
    assert!(serde_json::from_str::<Type>(r#"{"Numeric":[12,2]}"#).is_err());
    assert!(serde_json::from_str::<Value>(r#"{"Numeric":"12.50"}"#).is_err());
    let ty = Type::Decimal(Some((12, 2)));
    let value = Value::Decimal("12.50".into());
    assert_eq!(serde_json::to_string(&ty).unwrap(), r#"{"Decimal":[12,2]}"#);
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"Decimal":"12.50"}"#
    );
    assert_eq!(
        serde_json::from_str::<Type>(r#"{"Decimal":[12,2]}"#).unwrap(),
        ty
    );
    assert_eq!(
        serde_json::from_str::<Value>(r#"{"Decimal":"12.50"}"#).unwrap(),
        value
    );
}

#[test]
fn numeric_declarations_are_rejected_while_decimal_remains_native() {
    let mut db = Database::memory();
    for sql in [
        "CREATE TABLE rejected(v NUMERIC)",
        "CREATE TABLE rejected(v NUMERIC(12,2))",
        "CREATE TABLE rejected(v NUMERIC(12,2)[])",
        "CREATE TABLE rejected(v NUMERIC PRECISION)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    db.execute("CREATE TABLE prices(amount DECIMAL(12,2)); INSERT INTO prices VALUES('12.50')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT amount,typeof(amount),amount+CAST('0.25' AS DECIMAL) FROM prices")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Decimal("12.50".into()),
            Value::Text("decimal".into()),
            Value::Decimal("12.75".into())
        ]]
    );
    assert!(
        db.execute("ALTER TABLE prices ADD COLUMN obsolete NUMERIC")
            .is_err()
    );
}

#[test]
fn decimal_values_and_arrays_survive_reopening_and_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    for format in [Format::Binary, Format::Readable] {
        let path = directory.path().join(format!("{format:?}"));
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE prices(amount DECIMAL(20,4), history DECIMAL(20,4)[])")
            .unwrap();
        let values = vec![
            Value::Decimal("1234567890123456.1234".into()),
            Value::Array(vec![Value::Decimal("0.1000".into()), Value::Null]),
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
        serde_json::to_string(&Value::Decimal("12.50".into())).unwrap(),
        r#"{"Decimal":"12.50"}"#
    );
}

#[test]
fn decimal_declarations_preserve_storage_and_validation() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE prices(n DECIMAL(12,2), d DECIMAL(12,2), history DECIMAL(12,2)[]); ALTER TABLE prices ADD COLUMN added DECIMAL(12,2)").unwrap();
    db.execute("INSERT INTO prices VALUES('12.5','12.5','[\"12.5\"]','12.5')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM prices").unwrap()[0].rows,
        vec![vec![
            Value::Decimal("12.50".into()),
            Value::Decimal("12.50".into()),
            Value::Array(vec![Value::Decimal("12.50".into())]),
            Value::Decimal("12.50".into()),
        ]]
    );
    for column in ["n", "d", "added"] {
        assert!(
            db.execute(&format!("UPDATE prices SET {column}='1.234'"))
                .is_err()
        );
    }
}
