use boxdb::{Database, Format, Value};

#[test]
fn ordinary_tables_are_strict_without_an_explicit_keyword() {
    for format in [None, Some(Format::Binary), Some(Format::Readable)] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("strict");
        let mut db = match format {
            None => Database::memory(),
            Some(format) => Database::open(&path, format).unwrap(),
        };
        db.execute("CREATE TABLE t(id INT64 PRIMARY KEY,n INT64)")
            .unwrap();
        assert_eq!(
            db.execute("SELECT strict FROM pragma_table_list WHERE schema='main' AND name='t'")
                .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        db.execute("INSERT INTO t VALUES(1,10)").unwrap();
        assert!(
            db.execute("INSERT INTO t VALUES(2,20),(3,'wrong')")
                .is_err()
        );
        assert_eq!(
            db.execute("SELECT id,n FROM t ORDER BY id").unwrap()[0].rows,
            vec![vec![Value::Integer(1), Value::Integer(10)]]
        );
        if let Some(format) = format {
            drop(db);
            let mut db = Database::open(&path, format).unwrap();
            assert!(db.execute("UPDATE t SET n='wrong'").is_err());
            assert_eq!(
                db.execute("SELECT n FROM t").unwrap()[0].rows,
                vec![vec![Value::Integer(10)]]
            );
        }
    }
}

#[test]
fn explicit_strict_and_without_rowid_options_preserve_extended_types() {
    let mut db = Database::memory();
    for (name, suffix) in [
        ("implicit", "WITHOUT ROWID"),
        ("explicit", "STRICT, WITHOUT ROWID"),
        ("reversed", "WITHOUT ROWID, STRICT"),
    ] {
        db.execute(&format!(
            "CREATE TABLE {name}(id TEXT PRIMARY KEY,d INSTANT,n DECIMAL(8,2)) {suffix}"
        ))
        .unwrap();
        assert_eq!(
            db.execute(&format!(
                "SELECT strict,wr FROM pragma_table_list WHERE name='{name}'"
            ))
            .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(1), Value::Integer(1)]]
        );
        db.execute(&format!(
            "INSERT INTO {name} VALUES('a','2024-02-29T00:00:00.000000Z','12.34')"
        ))
        .unwrap();
        assert!(db.execute(&format!("UPDATE {name} SET d='wrong'")).is_err());
        assert_eq!(
            db.execute(&format!("SELECT d,n FROM {name}")).unwrap()[0].rows,
            vec![vec![
                Value::Instant("2024-02-29T00:00:00.000000Z".into()),
                Value::Decimal("12.34".into())
            ]]
        );
    }
}

#[test]
fn strict_tables_support_native_aliases_any_temp_and_sqlite_constraints() {
    let mut db = Database::memory();
    db.execute("/* strict policy */ CREATE TABLE [quoted table]([id] INT64 PRIMARY KEY AUTOINCREMENT,n INT64,r DOUBLE,v ANY,untyped,word TEXT UNIQUE ON CONFLICT IGNORE)").unwrap();
    db.execute("INSERT INTO [quoted table](n,r,v,untyped,word) VALUES(9223372036854775807,0.5,'001',x'00ff','a')").unwrap();
    assert_eq!(
        db.execute("SELECT id,n,r,v,typeof(v),untyped FROM [quoted table]")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(i64::MAX),
            Value::Real(0.5),
            Value::Text("001".into()),
            Value::Text("text".into()),
            Value::Bytea(vec![0, 255])
        ]]
    );
    db.execute("CREATE TEMP TABLE t(n INT64); CREATE TABLE temp.explicit(n TEXT) STRICT; INSERT INTO t VALUES('42')").unwrap();
    assert_eq!(db.execute("SELECT strict FROM pragma_table_list WHERE schema='temp' AND name IN('t','explicit') ORDER BY name").unwrap()[0].rows, vec![vec![Value::Integer(1)],vec![Value::Integer(1)]]);
    assert_eq!(
        db.execute("SELECT n,typeof(n) FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(42), Value::Text("integer".into())]]
    );
    assert!(db.execute("INSERT INTO t VALUES('wrong')").is_err());
    assert!(
        db.execute("CREATE TABLE unsupported(n UNKNOWN_TYPE)")
            .is_err()
    );
    db.execute("CREATE TABLE IF NOT EXISTS [quoted table](ignored TEXT)")
        .unwrap();
    assert_eq!(
        db.execute("SELECT COUNT(*) FROM [quoted table]").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn create_table_as_select_is_strict_and_preserves_inferred_cells() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ctas");
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE source(id INT64,n REAL,body TEXT); INSERT INTO source VALUES(1,0.5,'001'); CREATE TABLE copied AS SELECT id,n,body,id+1 AS computed FROM source").unwrap();
        assert_eq!(
            db.execute(
                "SELECT strict FROM pragma_table_list WHERE schema='main' AND name='copied'"
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        assert!(db.execute("UPDATE copied SET id='wrong'").is_err());
        db.execute("CREATE TEMP TABLE temporary AS SELECT * FROM copied")
            .unwrap();
        assert_eq!(
            db.execute(
                "SELECT strict FROM pragma_table_list WHERE schema='temp' AND name='temporary'"
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        db.execute("CREATE TABLE IF NOT EXISTS copied AS SELECT 99 AS other")
            .unwrap();
        assert!(
            db.execute("CREATE TABLE failed AS SELECT * FROM missing")
                .is_err()
        );
        assert!(db.execute("SELECT * FROM failed").is_err());
        drop(db);
        let mut reopened = Database::open(&path, format).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM copied").unwrap()[0].rows,
            vec![vec![
                Value::Integer(1),
                Value::Real(0.5),
                Value::Text("001".into()),
                Value::Integer(2)
            ]]
        );
        assert_eq!(
            reopened
                .execute("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT GLOB '__boxdb_*' ORDER BY name")
                .unwrap()[0]
                .rows,
            vec![
                vec![Value::Text("copied".into())],
                vec![Value::Text("source".into())]
            ]
        );
    }
}

#[test]
fn create_table_as_select_preserves_row_order_with_shadowed_rowid_names() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE copied AS SELECT 'z' AS rowid,10 AS _rowid_ UNION ALL SELECT 'a',20")
        .unwrap();
    assert_eq!(
        db.execute("SELECT oid,rowid,_rowid_ FROM copied ORDER BY oid")
            .unwrap()[0]
            .rows,
        vec![
            vec![
                Value::Integer(1),
                Value::Text("z".into()),
                Value::Integer(10)
            ],
            vec![
                Value::Integer(2),
                Value::Text("a".into()),
                Value::Integer(20)
            ]
        ]
    );
}

#[test]
fn added_columns_keep_strict_typing_and_ddl_returns_no_internal_results() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY); INSERT INTO t VALUES(1)")
        .unwrap();
    let result = db
        .execute("ALTER TABLE t ADD COLUMN score INT64 DEFAULT 2")
        .unwrap();
    assert!(result[0].columns.is_empty());
    assert!(result[0].rows.is_empty());
    assert_eq!(
        db.execute("SELECT score FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
    assert!(db.execute("UPDATE t SET score='wrong'").is_err());
    db.execute("ALTER TABLE t ADD COLUMN ratio DOUBLE DEFAULT 0.5; ALTER TABLE t ADD COLUMN d INSTANT DEFAULT '2024-02-29T00:00:00.000000Z'").unwrap();
    assert_eq!(
        db.execute("SELECT ratio,d FROM t").unwrap()[0].rows,
        vec![vec![
            Value::Real(0.5),
            Value::Instant("2024-02-29T00:00:00.000000Z".into())
        ]]
    );
}

#[test]
fn strict_creation_preserves_quoted_names_and_default_values() {
    let mut db = Database::memory();
    db.execute(r#"CREATE TABLE "odd""table"("odd""column" TEXT DEFAULT 'it''s \path',tags TEXT[] DEFAULT '["a''b"]',bytes BLOB DEFAULT x'00ff',n 'INT64' DEFAULT 7)"#).unwrap();
    db.execute(r#"INSERT INTO "odd""table" DEFAULT VALUES"#)
        .unwrap();
    assert_eq!(
        db.execute(r#"SELECT * FROM "odd""table""#).unwrap()[0].rows,
        vec![vec![
            Value::Text("it's \\path".into()),
            Value::Array(vec![Value::Text("a'b".into())]),
            Value::Bytea(vec![0, 255]),
            Value::Integer(7)
        ]]
    );
    assert!(
        db.execute(r#"CREATE TABLE unsupported(n "TEXT); DROP TABLE t; --")"#)
            .is_err()
    );
    assert_eq!(
        db.execute(r#"SELECT COUNT(*) FROM "odd""table""#).unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}
