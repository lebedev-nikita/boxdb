use boxdb::{Database, Format, Type, Value};

#[test]
fn integer_types_require_an_explicit_supported_bit_width() {
    for name in [
        "INT", "INTEGER", "UINT", "SMALLINT", "BIGINT", "INT2", "INT4", "INT128", "UINT64",
        "UINT128", "INT24", "UINT24", "INT64(1)",
    ] {
        assert!(Type::parse(name).is_err(), "{name}");
    }
    for name in [
        "INT8", "INT16", "INT32", "INT64", "UINT8", "UINT16", "UINT32",
    ] {
        assert!(Type::parse(name).is_ok(), "{name}");
        assert!(Type::parse(&format!("{name}[]")).is_ok(), "{name}[]");
    }
}

#[test]
fn integer_validation_and_casts_enforce_signed_and_unsigned_ranges() {
    for (name, min, max, below, above) in [
        ("INT8", "-128", "127", "-129", "128"),
        ("INT16", "-32768", "32767", "-32769", "32768"),
        (
            "INT32",
            "-2147483648",
            "2147483647",
            "-2147483649",
            "2147483648",
        ),
        (
            "INT64",
            "-9223372036854775808",
            "9223372036854775807",
            "-9223372036854775809",
            "9223372036854775808",
        ),
        ("UINT8", "0", "255", "-1", "256"),
        ("UINT16", "0", "65535", "-1", "65536"),
        ("UINT32", "0", "4294967295", "-1", "4294967296"),
    ] {
        let ty = Type::parse(name).unwrap();
        for value in [min, max] {
            let cast = ty.cast(Value::Text(value.into())).unwrap();
            assert_eq!(cast.to_string(), value, "{name}");
            assert_eq!(ty.validate(&cast).unwrap(), cast);
        }
        for value in [below, above] {
            assert!(
                ty.cast(Value::Text(value.into())).is_err(),
                "{name}: {value}"
            );
        }
        assert_eq!(ty.validate(&Value::Null).unwrap(), Value::Null);
        assert!(ty.validate(&Value::Real(1.5)).is_err());
    }
}

#[test]
fn sql_declarations_reject_unsized_and_unsupported_integers() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(body TEXT)").unwrap();
    for name in [
        "INT", "INTEGER", "UINT", "SMALLINT", "BIGINT", "INT2", "INT4", "INT128", "UINT64",
        "UINT128", "INT24", "INT64(1)",
    ] {
        for sql in [
            format!("CREATE TABLE bad(n {name})"),
            format!("CREATE TEMP TABLE bad(n {name})"),
            format!("ALTER TABLE t ADD COLUMN n {name}"),
        ] {
            assert!(db.execute(&sql).is_err(), "{sql}");
        }
    }
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap()[0].columns,
        vec!["body"]
    );
}

#[test]
fn sql_integer_columns_enforce_ranges_and_return_unsigned_values() {
    for (name, min, max, invalid) in [
        ("INT8", Value::Integer(-128), Value::Integer(127), "128"),
        (
            "INT16",
            Value::Integer(-32768),
            Value::Integer(32767),
            "32768",
        ),
        (
            "INT32",
            Value::Integer(-2147483648),
            Value::Integer(2147483647),
            "2147483648",
        ),
        (
            "INT64",
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            "9223372036854775808",
        ),
        (
            "UINT8",
            Value::UnsignedInteger(0),
            Value::UnsignedInteger(255),
            "256",
        ),
        (
            "UINT16",
            Value::UnsignedInteger(0),
            Value::UnsignedInteger(65535),
            "65536",
        ),
        (
            "UINT32",
            Value::UnsignedInteger(0),
            Value::UnsignedInteger(4294967295),
            "4294967296",
        ),
    ] {
        let mut db = Database::memory();
        db.execute(&format!("CREATE TABLE t(n {name})")).unwrap();
        db.execute_params(
            "INSERT INTO t VALUES(?),(?),(NULL)",
            &[min.clone(), max.clone()],
        )
        .unwrap();
        assert_eq!(
            db.execute("SELECT n FROM t ORDER BY rowid").unwrap()[0].rows,
            vec![vec![min], vec![max], vec![Value::Null]],
            "{name}"
        );
        assert!(
            db.execute(&format!("UPDATE t SET n={invalid}")).is_err(),
            "{name}"
        );
        if name.starts_with("UINT") {
            assert!(db.execute("UPDATE t SET n=-1").is_err(), "{name}");
        }
    }
}

#[test]
fn unsigned_arrays_and_integers_survive_snapshots_and_reopening() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("integers");
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE t(id INT64 PRIMARY KEY AUTOINCREMENT,n UINT32,values_array UINT32[],small UINT8[])").unwrap();
        let values = vec![
            Value::UnsignedInteger(4294967295),
            Value::Array(vec![
                Value::UnsignedInteger(0),
                Value::UnsignedInteger(2147483648),
                Value::UnsignedInteger(4294967295),
                Value::Null,
            ]),
            Value::Array(vec![Value::UnsignedInteger(255)]),
        ];
        db.execute_params("INSERT INTO t(n,values_array,small) VALUES(?,?,?)", &values)
            .unwrap();
        let expected = vec![vec![
            Value::Integer(1),
            values[0].clone(),
            values[1].clone(),
            values[2].clone(),
        ]];
        assert_eq!(db.execute("SELECT * FROM t").unwrap()[0].rows, expected);
        assert_eq!(
            db.execute("SELECT n+0 FROM t").unwrap()[0].rows,
            vec![vec![Value::Integer(4294967295)]]
        );
        assert!(
            db.execute_params(
                "UPDATE t SET small=?",
                &[Value::Array(vec![Value::UnsignedInteger(256)])]
            )
            .is_err()
        );
        let snapshot = db.export(format).unwrap();
        let mut restored = Database::from_snapshot(&snapshot).unwrap();
        assert_eq!(
            restored.execute("SELECT * FROM t").unwrap()[0].rows,
            expected
        );
        drop(db);
        let mut reopened = Database::open(&path, format).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM t").unwrap()[0].rows,
            expected
        );
        assert!(reopened.execute("UPDATE t SET small='[256]'").is_err());
    }
}

#[test]
fn temp_alter_cannot_bypass_explicit_integer_widths() {
    let mut db = Database::memory();
    db.execute("CREATE TEMP TABLE t(body TEXT)").unwrap();
    assert!(
        db.execute("ALTER TABLE temp.t ADD COLUMN n INTEGER")
            .is_err()
    );
    db.execute("ALTER TABLE temp.t ADD COLUMN n INT64 DEFAULT 42")
        .unwrap();
    db.execute("INSERT INTO temp.t(body) VALUES('row')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT n FROM temp.t").unwrap()[0].rows,
        vec![vec![Value::Integer(42)]]
    );
}

#[test]
fn existing_integer_metadata_keeps_its_original_bit_width_on_open() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.db");
    let sqlite = rusqlite::Connection::open(&path).unwrap();
    let table_sql = "CREATE TABLE legacy(s INTEGER,n INTEGER,b INTEGER) STRICT";
    sqlite.execute_batch(table_sql).unwrap();
    sqlite.execute_batch("CREATE TABLE __boxdb_columns(table_name TEXT NOT NULL,column_name TEXT NOT NULL,logical_type TEXT NOT NULL,table_sql TEXT NOT NULL,version INTEGER NOT NULL,PRIMARY KEY(table_name,column_name)) STRICT; INSERT INTO legacy VALUES(32767,2147483647,9223372036854775807)").unwrap();
    for (column, ty) in [
        ("s", "\"SmallInt\""),
        ("n", "\"Integer\""),
        ("b", "\"BigInt\""),
    ] {
        sqlite
            .execute(
                "INSERT INTO __boxdb_columns VALUES('legacy',?1,?2,?3,1)",
                rusqlite::params![column, ty, table_sql],
            )
            .unwrap();
    }
    drop(sqlite);
    let mut db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT * FROM legacy").unwrap()[0].rows,
        vec![vec![
            Value::Integer(32767),
            Value::Integer(2147483647),
            Value::Integer(i64::MAX)
        ]]
    );
    assert!(db.execute("UPDATE legacy SET s=32768").is_err());
    assert!(db.execute("UPDATE legacy SET n=2147483648").is_err());
}
