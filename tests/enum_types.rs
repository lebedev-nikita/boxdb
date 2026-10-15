use boxdb::{Database, Format, SchemaFormat, Type, Value};

#[test]
fn enum_declarations_preserve_and_validate_exact_string_labels() {
    let ty = Type::parse("enum('todo', 'Done', '', 'it''s, (雪)[] ARRAY<')").unwrap();
    assert_eq!(
        ty,
        Type::Enum(vec![
            "todo".into(),
            "Done".into(),
            "".into(),
            "it's, (雪)[] ARRAY<".into()
        ])
    );
    for label in ["todo", "Done", "", "it's, (雪)[] ARRAY<"] {
        let value = Value::Text(label.into());
        assert_eq!(ty.validate(&value).unwrap(), value);
    }
    assert_eq!(ty.validate(&Value::Null).unwrap(), Value::Null);
    for value in [
        Value::Text("done".into()),
        Value::Text("todo ".into()),
        Value::Integer(1),
    ] {
        assert!(ty.validate(&value).is_err());
    }
    for declaration in [
        "ENUM",
        "ENUM()",
        "ENUM('x','x')",
        "ENUM(1)",
        "ENUM(NULL)",
        "ENUM(\"x\")",
        "ENUM('x',)",
        "ENUM('x') junk",
        "ENUM('unterminated)",
    ] {
        assert!(Type::parse(declaration).is_err(), "{declaration}");
    }
    for labels in [vec![], vec!["x".into(), "x".into()]] {
        assert!(Type::Enum(labels).validate(&Value::Null).is_err());
    }
}

#[test]
fn unsupported_enum_sql_casts_fail_during_preparation() {
    let mut db = Database::memory();
    for sql in [
        "SELECT CAST('todo' AS ENUM)",
        "SELECT CAST(NULL AS enum) WHERE 0",
        "SELECT CAST('1' AS ENUM(1))",
        "SELECT CAST('todo' AS ENUM /* comment */ (1))",
        "SELECT CAST('todo' AS ENUM -- comment\n (1))",
        "SELECT CASE WHEN 0 THEN CAST('todo' AS \"ENUM\") ELSE 'todo' END",
        "CREATE VIEW rejected AS SELECT CAST('todo' AS ENUM)",
    ] {
        assert!(db.statement_info(sql).is_err(), "prepare: {sql}");
        assert!(db.execute(sql).is_err(), "execute: {sql}");
    }
    assert_eq!(
        db.execute("SELECT 'ENUM', 'todo' AS \"ENUM\", CAST('todo' AS TEXT)")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("ENUM".into()),
            Value::Text("todo".into()),
            Value::Text("todo".into()),
        ]]
    );
}

#[test]
fn enum_literal_contents_do_not_change_sql_or_array_nesting() {
    let label = "[] ARRAY<".repeat(33);
    let ty = Type::Enum(vec![label.clone()]);
    assert_eq!(Type::parse(&format!("ENUM('{label}')")).unwrap(), ty);
    assert_eq!(
        Type::parse("ARRAY<ENUM('Done')>").unwrap(),
        Type::Array(Box::new(Type::Enum(vec!["Done".into()])))
    );
    let mut db = Database::memory();
    db.execute(r#"CREATE TABLE "odd' table"("odd' column" ENUM('x''); DROP TABLE tasks; --','a\b','é','é',' padded '))"#).unwrap();
    for label in ["x'); DROP TABLE tasks; --", r"a\b", "é", "é", " padded "] {
        db.execute_params(
            "INSERT INTO \"odd' table\" VALUES(?)",
            &[Value::Text(label.into())],
        )
        .unwrap();
    }
    assert_eq!(
        db.execute("SELECT COUNT(DISTINCT \"odd' column\") FROM \"odd' table\"")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert!(
        db.execute("INSERT INTO \"odd' table\" VALUES('padded')")
            .is_err()
    );
}

#[test]
fn sql_enums_enforce_membership_defaults_nulls_and_statement_atomicity() {
    let mut db = Database::memory();
    let sql = "CREATE TABLE tasks(id INT64 PRIMARY KEY, status ENUM('todo','Done','it''s, (雪)[] ARRAY<','') NOT NULL DEFAULT 'todo', optional ENUM('yes','no'), numeric ENUM('1'))";
    db.statement_info(sql).unwrap();
    db.execute(sql).unwrap();
    db.execute("INSERT INTO tasks(id) VALUES(1)").unwrap();
    assert_eq!(
        db.execute("SELECT status,optional FROM tasks").unwrap()[0].rows,
        vec![vec![Value::Text("todo".into()), Value::Null]]
    );
    for (id, label) in [(2, "Done"), (3, "it's, (雪)[] ARRAY<"), (4, "")] {
        db.execute_params(
            "INSERT INTO tasks(id,status) VALUES(?,?)",
            &[Value::Integer(id), Value::Text(label.into())],
        )
        .unwrap();
    }
    db.execute("UPDATE tasks SET numeric=1 WHERE id=1").unwrap();
    assert_eq!(
        db.execute("SELECT numeric FROM tasks WHERE id=1").unwrap()[0].rows,
        vec![vec![Value::Text("1".into())]]
    );
    for sql in [
        "UPDATE tasks SET status='done' WHERE id=1",
        "UPDATE tasks SET status=NULL WHERE id=1",
        "UPDATE tasks SET status='todo ' WHERE id=1",
        "INSERT INTO tasks(id,status) VALUES(5,'todo'),(6,'bad')",
        "INSERT INTO tasks(id,status) SELECT 5,'bad'",
        "UPDATE tasks SET status=X'746f646f' WHERE id=1",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
        assert_eq!(
            db.execute("SELECT COUNT(*) FROM tasks").unwrap()[0].rows,
            vec![vec![Value::Integer(4)]]
        );
        assert_eq!(
            db.execute("SELECT status FROM tasks WHERE id=1").unwrap()[0].rows,
            vec![vec![Value::Text("todo".into())]]
        );
    }
    db.execute("CREATE TABLE source(id INT64); CREATE TRIGGER rejected AFTER INSERT ON source BEGIN INSERT INTO tasks(id,status) VALUES(NEW.id,'bad'); END").unwrap();
    assert!(db.execute("INSERT INTO source VALUES(5)").is_err());
    assert!(
        db.execute("SELECT * FROM source").unwrap()[0]
            .rows
            .is_empty()
    );
}

#[test]
fn enum_columns_keep_binary_equality_and_ordering_with_indexes() {
    let mut db = Database::memory();
    for collation in ["NOCASE", "RTRIM"] {
        assert!(
            db.execute(&format!(
                "CREATE TABLE rejected(v ENUM('x','X') COLLATE {collation})"
            ))
            .is_err()
        );
    }
    db.execute("CREATE TABLE ordered(v ENUM('z','a','A') COLLATE BINARY UNIQUE); INSERT INTO ordered VALUES('z'),('a'),('A'); CREATE INDEX ordered_v ON ordered(v)").unwrap();
    for plan in ["INDEXED BY ordered_v", "NOT INDEXED"] {
        assert_eq!(
            db.execute(&format!("SELECT v FROM ordered {plan} ORDER BY v"))
                .unwrap()[0]
                .rows,
            vec![
                vec![Value::Text("A".into())],
                vec![Value::Text("a".into())],
                vec![Value::Text("z".into())]
            ]
        );
        assert_eq!(
            db.execute(&format!("SELECT v FROM ordered {plan} WHERE v='a'"))
                .unwrap()[0]
                .rows,
            vec![vec![Value::Text("a".into())]]
        );
    }
    assert!(db.execute("INSERT INTO ordered VALUES('a')").is_err());
    assert_eq!(
        db.execute("SELECT MIN(v),MAX(v),COUNT(DISTINCT v) FROM ordered")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("A".into()),
            Value::Text("z".into()),
            Value::Integer(3)
        ]]
    );
}

#[test]
fn enum_alterations_and_arrays_preserve_labels_and_validate_writes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE tasks(id INT64 PRIMARY KEY); INSERT INTO tasks VALUES(1)")
        .unwrap();
    let alter =
        "ALTER TABLE tasks ADD COLUMN status ENUM('todo','it''s, Done') NOT NULL DEFAULT 'todo'";
    db.statement_info(alter).unwrap();
    db.execute(alter).unwrap();
    db.execute("ALTER TABLE tasks RENAME COLUMN status TO state; ALTER TABLE tasks RENAME TO jobs; UPDATE jobs SET state='it''s, Done'").unwrap();
    assert!(db.execute("UPDATE jobs SET state='bad'").is_err());
    assert_eq!(
        db.execute("SELECT state FROM jobs").unwrap()[0].rows,
        vec![vec![Value::Text("it's, Done".into())]]
    );
    assert!(
        db.execute("ALTER TABLE jobs ADD COLUMN bad ENUM('a','A') COLLATE NOCASE")
            .is_err()
    );
    db.execute("CREATE TABLE lists(v ENUM('todo','Done')[])")
        .unwrap();
    let values = Value::Array(vec![Value::Text("Done".into()), Value::Null]);
    db.execute_params("INSERT INTO lists VALUES(?)", std::slice::from_ref(&values))
        .unwrap();
    assert_eq!(
        db.execute("SELECT v FROM lists").unwrap()[0].rows,
        vec![vec![values]]
    );
    assert!(
        db.execute("INSERT INTO lists VALUES('[\"done\"]')")
            .is_err()
    );
    for declaration in ["ENUM()", "ENUM('x','x')", "ENUM(1)", "ENUM(NULL)"] {
        for sql in [
            format!("CREATE TABLE rejected(v {declaration})"),
            format!("ALTER TABLE jobs ADD COLUMN rejected {declaration}"),
        ] {
            assert!(db.statement_info(&sql).is_err(), "{sql}");
            assert!(db.execute(&sql).is_err(), "{sql}");
        }
    }
}

#[test]
fn enum_constraints_survive_reopening_conversion_and_schema_exports() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE tasks(status ENUM('todo','it''s, Done') NOT NULL DEFAULT 'todo'); INSERT INTO tasks VALUES('it''s, Done')").unwrap();
    let sqlite = rusqlite::Connection::open(&source).unwrap();
    assert!(
        sqlite
            .execute("INSERT INTO tasks VALUES('bad')", [])
            .is_err()
    );
    sqlite
        .execute("INSERT INTO tasks VALUES('todo')", [])
        .unwrap();
    drop(sqlite);
    for format in [SchemaFormat::Sql, SchemaFormat::Yaml] {
        let dump = db.dump_schema(format).unwrap();
        let restored = directory.path().join(format!("schema-{format:?}.db"));
        match format {
            SchemaFormat::Sql => rusqlite::Connection::open(&restored)
                .unwrap()
                .execute_batch(std::str::from_utf8(&dump).unwrap())
                .unwrap(),
            SchemaFormat::Yaml => {
                let yaml = directory.path().join("schema.yaml");
                std::fs::write(&yaml, dump).unwrap();
                Database::convert(yaml, &restored, Format::Binary).unwrap();
            }
        }
        let mut restored = Database::open(restored, Format::Binary).unwrap();
        assert!(
            restored.execute("SELECT status FROM tasks").unwrap()[0]
                .rows
                .is_empty()
        );
        restored
            .execute("INSERT INTO tasks DEFAULT VALUES")
            .unwrap();
        assert!(restored.execute("INSERT INTO tasks VALUES('bad')").is_err());
        assert_eq!(
            restored.execute("SELECT status FROM tasks").unwrap()[0].rows,
            vec![vec![Value::Text("todo".into())]]
        );
    }
    drop(db);
    let yaml = directory.path().join("tasks.yaml");
    let binary = directory.path().join("restored.db");
    Database::convert(&source, &yaml, Format::Readable).unwrap();
    Database::convert(&yaml, &binary, Format::Binary).unwrap();
    for (path, format) in [
        (&source, Format::Binary),
        (&yaml, Format::Readable),
        (&binary, Format::Binary),
    ] {
        let mut db = Database::open(path, format).unwrap();
        assert_eq!(
            db.execute("SELECT status FROM tasks ORDER BY rowid")
                .unwrap()[0]
                .rows,
            vec![
                vec![Value::Text("it's, Done".into())],
                vec![Value::Text("todo".into())]
            ]
        );
        assert!(db.execute("UPDATE tasks SET status='bad'").is_err());
    }
}
