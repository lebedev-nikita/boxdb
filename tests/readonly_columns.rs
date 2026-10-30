use boxdb::{Database, Format, SchemaFormat, Value};

fn invoices() -> Database {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE invoices(
            id INT64 PRIMARY KEY,
            tenant UUID NOT NULL READONLY,
            amount DECIMAL(12,2)
        );
        INSERT INTO invoices(id,tenant,amount)
            VALUES(1,'00000000-0000-4000-8000-000000000001','1.50')",
    )
    .unwrap();
    db
}
fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    db.execute(sql).unwrap().remove(0).rows
}

#[test]
fn updating_a_readonly_column_fails_while_other_columns_stay_writable() {
    let mut db = invoices();
    let error = db
        .execute("UPDATE invoices SET tenant='00000000-0000-4000-8000-000000000002'")
        .unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{error}");
    assert_eq!(error.extended_code(), Some(1811));
    assert!(error.to_string().contains("invoices.tenant"), "{error}");
    db.execute("UPDATE invoices SET amount='2.25'").unwrap();
    assert_eq!(
        rows(&mut db, "SELECT tenant,amount FROM invoices"),
        vec![vec![
            Value::Uuid("00000000-0000-4000-8000-000000000001".into()),
            Value::Decimal("2.25".into()),
        ]]
    );
}

#[test]
fn every_update_form_naming_a_readonly_column_fails_even_without_a_change() {
    let mut db = invoices();
    for sql in [
        "UPDATE invoices SET tenant=tenant",
        "UPDATE OR REPLACE invoices SET tenant='00000000-0000-4000-8000-000000000002'",
        "INSERT INTO invoices(id,tenant) VALUES(1,'00000000-0000-4000-8000-000000000002')
            ON CONFLICT(id) DO UPDATE SET tenant=excluded.tenant",
    ] {
        let error = db.execute(sql).unwrap_err();
        assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{sql}: {error}");
    }
    db.execute(
        "INSERT INTO invoices(id,tenant) VALUES(1,'00000000-0000-4000-8000-000000000003')
            ON CONFLICT(id) DO UPDATE SET amount='9.99'",
    )
    .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT tenant,amount FROM invoices"),
        vec![vec![
            Value::Uuid("00000000-0000-4000-8000-000000000001".into()),
            Value::Decimal("9.99".into()),
        ]]
    );
}

#[test]
fn replacing_a_row_deletes_it_and_inserts_a_new_readonly_value() {
    let mut db = invoices();
    db.execute("INSERT OR REPLACE INTO invoices(id,tenant) VALUES(1,'00000000-0000-4000-8000-000000000002')")
        .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT tenant,amount FROM invoices"),
        vec![vec![
            Value::Uuid("00000000-0000-4000-8000-000000000002".into()),
            Value::Null,
        ]]
    );
}

#[test]
fn a_readonly_rowid_alias_rejects_updates_through_every_rowid_name() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE notes(id INT64 PRIMARY KEY READONLY, body TEXT); INSERT INTO notes VALUES(1,'a')")
        .unwrap();
    for column in ["id", "rowid", "oid", "_rowid_"] {
        let error = db
            .execute(&format!("UPDATE notes SET {column}=2"))
            .unwrap_err();
        assert_eq!(
            error.code(),
            "BOXDB_CONSTRAINT_READONLY",
            "{column}: {error}"
        );
    }
    db.execute("UPDATE notes SET body='b'").unwrap();
    assert_eq!(
        rows(&mut db, "SELECT id,body FROM notes"),
        vec![vec![Value::Integer(1), Value::Text("b".into())]]
    );
}

#[test]
fn a_column_named_rowid_does_not_make_the_hidden_rowid_readonly() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE notes(id INT64 PRIMARY KEY READONLY, rowid TEXT); INSERT INTO notes VALUES(1,'a')",
    )
    .unwrap();
    db.execute("UPDATE notes SET rowid='b'").unwrap();
    let error = db.execute("UPDATE notes SET oid=2").unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{error}");
}

#[test]
fn declarations_reject_readonly_columns_that_would_be_updated_implicitly() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE parents(id INT64 PRIMARY KEY)")
        .unwrap();
    for sql in [
        "CREATE TABLE t(a TEXT, b TEXT AS (lower(a)) READONLY)",
        "CREATE TABLE t(a TEXT, b TEXT GENERATED ALWAYS AS ('x') STORED READONLY)",
        "CREATE TABLE t(p INT64 READONLY REFERENCES parents(id) ON UPDATE CASCADE)",
        "CREATE TABLE t(p INT64 READONLY REFERENCES parents(id) ON DELETE CASCADE ON UPDATE SET NULL)",
        "CREATE TABLE t(p INT64 READONLY, FOREIGN KEY(p) REFERENCES parents(id) ON UPDATE SET DEFAULT)",
    ] {
        let error = db.execute(sql).unwrap_err();
        assert_eq!(error.code(), "BOXDB_VALIDATION", "{sql}: {error}");
        assert!(error.to_string().contains("READONLY"), "{sql}: {error}");
        assert!(
            rows(&mut db, "SELECT name FROM sqlite_schema WHERE tbl_name='t'").is_empty(),
            "{sql}"
        );
    }
    db.execute(
        "CREATE TABLE t(p INT64 READONLY REFERENCES parents(id) ON UPDATE RESTRICT ON DELETE CASCADE)",
    )
    .unwrap();
}

#[test]
fn readonly_names_remain_ordinary_identifiers() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE readonly(readonly INT64 PRIMARY KEY);
        CREATE TABLE refs(
            readonly INT64 REFERENCES readonly(readonly),
            label TEXT DEFAULT readonly,
            fixed TEXT CONSTRAINT readonly NOT NULL DEFAULT 'x' READONLY
        );
        INSERT INTO readonly VALUES(1);
        INSERT INTO refs(readonly) VALUES(1);
        UPDATE refs SET readonly=1, label='y'",
    )
    .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT readonly,label,fixed FROM refs"),
        vec![vec![
            Value::Integer(1),
            Value::Text("y".into()),
            Value::Text("x".into()),
        ]]
    );
    let error = db.execute("UPDATE refs SET fixed='z'").unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{error}");
}

#[test]
fn added_columns_can_be_readonly() {
    let mut db = invoices();
    db.execute("ALTER TABLE invoices ADD COLUMN source TEXT NOT NULL DEFAULT 'web' READONLY")
        .unwrap();
    let error = db.execute("UPDATE invoices SET source='api'").unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{error}");
    assert!(error.to_string().contains("invoices.source"), "{error}");
    assert_eq!(
        rows(&mut db, "SELECT source FROM invoices"),
        vec![vec![Value::Text("web".into())]]
    );
}

#[test]
fn renamed_tables_and_columns_stay_readonly_under_their_new_names() {
    let mut db = invoices();
    db.execute(
        "ALTER TABLE invoices RENAME COLUMN tenant TO owner;
        ALTER TABLE invoices RENAME TO bills",
    )
    .unwrap();
    let error = db
        .execute("UPDATE bills SET owner='00000000-0000-4000-8000-000000000002'")
        .unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_READONLY", "{error}");
    assert!(error.to_string().contains("bills.owner"), "{error}");
    db.execute("ALTER TABLE bills ADD COLUMN tenant TEXT; UPDATE bills SET tenant='x'")
        .unwrap();
}

#[test]
fn dropped_readonly_columns_and_tables_leave_no_protection_behind() {
    let mut db = invoices();
    db.execute(
        "ALTER TABLE invoices DROP COLUMN tenant;
        ALTER TABLE invoices ADD COLUMN tenant TEXT;
        UPDATE invoices SET tenant='x'",
    )
    .unwrap();
    db.execute(
        "CREATE TABLE notes(id INT64 PRIMARY KEY, body TEXT READONLY);
        DROP TABLE notes;
        CREATE TABLE notes(id INT64 PRIMARY KEY, body TEXT);
        INSERT INTO notes VALUES(1,'a');
        UPDATE notes SET body='b'",
    )
    .unwrap();
    assert!(
        rows(
            &mut db,
            "SELECT name FROM sqlite_schema WHERE type='trigger'"
        )
        .is_empty()
    );
}

#[test]
fn readonly_columns_survive_reopening_and_conversion_in_both_formats() {
    let temp = tempfile::tempdir().unwrap();
    for format in [Format::Binary, Format::Readable] {
        let path = temp.path().join(format!("{format:?}"));
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE notes(id INT64 PRIMARY KEY, body TEXT READONLY); INSERT INTO notes VALUES(1,'a')")
            .unwrap();
        drop(db);
        let converted = temp.path().join(format!("{format:?}-converted"));
        let other = match format {
            Format::Binary => Format::Readable,
            Format::Readable => Format::Binary,
        };
        Database::convert(&path, &converted, other).unwrap();
        for path in [&path, &converted] {
            let mut db = Database::open(path, format).unwrap();
            let error = db.execute("UPDATE notes SET body='b'").unwrap_err();
            assert_eq!(
                error.code(),
                "BOXDB_CONSTRAINT_READONLY",
                "{path:?}: {error}"
            );
            db.execute("INSERT INTO notes VALUES(2,'c')").unwrap();
        }
    }
}

#[test]
fn schema_dumps_declare_readonly_columns_and_restore_their_protection() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE notes(id INT64 PRIMARY KEY READONLY, body TEXT NOT NULL READONLY, at INSTANT READONLY, tag TEXT)",
    )
    .unwrap();
    let sql = String::from_utf8(db.dump_schema(SchemaFormat::Sql).unwrap()).unwrap();
    assert!(!sql.contains("__boxdb_"), "{sql}");
    let yaml = String::from_utf8(db.dump_schema(SchemaFormat::Yaml).unwrap()).unwrap();
    assert!(!yaml.contains("__boxdb_"), "{yaml}");
    let document: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
    assert_eq!(
        document["tables"]["notes"]["columns"],
        serde_json::json!({
            "id": "INTEGER PRIMARY KEY READONLY",
            "body": "TEXT READONLY",
            "at?": "INSTANT READONLY",
            "tag?": "TEXT",
        })
    );
    let mut restored = Database::memory();
    restored.execute(&sql).unwrap();
    restored
        .execute(
            "INSERT INTO notes VALUES(1,'a','2026-01-01T00:00:00Z','x'); UPDATE notes SET tag='y'",
        )
        .unwrap();
    for column in ["id", "rowid", "body", "at"] {
        let error = restored
            .execute(&format!("UPDATE notes SET {column}={column}"))
            .unwrap_err();
        assert_eq!(
            error.code(),
            "BOXDB_CONSTRAINT_READONLY",
            "{column}: {error}"
        );
    }
    assert_eq!(
        restored.dump_schema(SchemaFormat::Sql).unwrap(),
        sql.into_bytes()
    );
}

#[test]
fn opening_rejects_readonly_triggers_changed_outside_boxdb() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE notes(id INT64 PRIMARY KEY, body TEXT READONLY)")
        .unwrap();
    drop(db);
    let raw = rusqlite::Connection::open(&path).unwrap();
    let name: String = raw
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type='trigger'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    raw.execute_batch(&format!(
        "DROP TRIGGER \"{name}\"; CREATE TRIGGER \"{name}\" BEFORE UPDATE OF body ON notes BEGIN SELECT 1; END"
    ))
    .unwrap();
    drop(raw);
    let error = Database::open(&path, Format::Binary).err().unwrap();
    assert_eq!(error.code(), "BOXDB_METADATA", "{error}");
}
