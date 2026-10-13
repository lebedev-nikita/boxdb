use boxdb::{Database, Format, Value};

#[test]
fn yaml_stream_preserves_sqlite_storage_classes_and_ambiguous_text() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE cells(id INT64 PRIMARY KEY,n ANY) STRICT")
        .unwrap();
    let mut values = vec![
        Value::Null,
        Value::Integer(i64::MIN),
        Value::Integer(i64::MAX),
        Value::Integer(1),
        Value::Real(1.0),
        Value::Real(-0.0),
        Value::Real(f64::MAX),
        Value::Real(f64::MIN_POSITIVE),
        Value::Real(f64::from_bits(1)),
        Value::Bytea(vec![]),
        Value::Bytea(vec![0, 0]),
        Value::Bytea(vec![0x12, 0x34]),
        Value::Bytea((0..=255).collect()),
        Value::Bytea((0..9000).map(|n| (n % 256) as u8).collect()),
    ];
    values.extend(
        [
            "",
            "001",
            "null",
            "true",
            "false",
            "yes",
            "off",
            "Y",
            "N",
            "TRUE",
            "Null",
            "simple text",
            "simple text ",
            "2024-02-29",
            "1e3",
            "---",
            "...",
            "# comment",
            "prefix: value",
            "猫🙂",
            "first line\n---\n...\nlast line\n\n",
            "  leading spaces\n trailing space ",
            "a\0b\t\\\"\r\n",
            "a\u{85}b\u{2028}c\u{2029}d\u{feff}e",
        ]
        .into_iter()
        .map(|s| Value::Text(s.into())),
    );
    for (i, value) in values.into_iter().enumerate() {
        db.execute_params(
            "INSERT INTO cells VALUES(?,?)",
            &[Value::Integer(i as i64), value],
        )
        .unwrap();
    }
    db.execute("CREATE TABLE keys(\"null\" TEXT,\"001\" TEXT,\"yes\" TEXT,\"a:b\" TEXT); INSERT INTO keys VALUES('one','two','three','four')").unwrap();
    let keys = db.execute("SELECT * FROM keys").unwrap();
    let query = "SELECT id,n,typeof(n) FROM cells ORDER BY id";
    let before = db.execute(query).unwrap();
    let bytes = db.export(Format::Readable).unwrap();
    let yaml = std::str::from_utf8(&bytes).unwrap();
    assert!(yaml.starts_with("---\nkind: database\nversion: 2\n"));
    assert!(yaml.contains("values:\n  id:"));
    assert!(
        yaml.contains("|"),
        "multiline text should use block scalars"
    );
    let documents: Vec<serde_json::Value> = serde_saphyr::from_slice_multiple(&bytes).unwrap();
    let first_row = documents
        .iter()
        .find(|doc| doc["kind"] == "row" && doc["table"] == "cells")
        .unwrap();
    assert_eq!(first_row["values"]["id"], 0);
    assert!(first_row["values"]["n"].is_null());
    let mut restored = Database::from_snapshot(&bytes).unwrap();
    assert_eq!(restored.execute(query).unwrap(), before);
    assert_eq!(restored.execute("SELECT * FROM keys").unwrap(), keys);
    assert_eq!(restored.export(Format::Readable).unwrap(), bytes);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cells.yaml");
    let mut file = Database::restore(&path, &bytes).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let update = "UPDATE cells SET n='edited\nblock\n' WHERE id=1";
    file.execute(update).unwrap();
    db.execute(update).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        db.export(Format::Readable).unwrap()
    );
    assert_eq!(file.execute(query).unwrap(), db.execute(query).unwrap());
}

const MANUAL: &str = r#"# Documents and row fields may be edited by hand.
---
kind: database
version: 2
application_id: 0
user_version: 0
---
kind: schema
object_type: table
name: notes
table: notes
sql: |
  CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT,flag INTEGER,weight REAL,bytes BLOB)
---
kind: row
table: notes
values:
  body: |-
    First line
    雪
  weight: 1.0
  flag: true
  bytes:
    blob: '00ff'
  id: 7
...
"#;

#[test]
fn handwritten_yaml_supports_comments_blocks_and_named_columns() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notes.yaml");
    std::fs::write(&path, MANUAL).unwrap();
    let mut db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(db.format(), Some(Format::Readable));
    assert_eq!(
        db.execute("SELECT rowid,* FROM notes").unwrap()[0].rows,
        vec![vec![
            Value::Integer(7),
            Value::Integer(7),
            Value::Text("First line\n雪".into()),
            Value::Integer(1),
            Value::Real(1.0),
            Value::Bytea(vec![0, 255]),
        ]]
    );
    db.execute("UPDATE notes SET body='Edited'").unwrap();
    drop(db);
    let yaml = std::fs::read_to_string(&path).unwrap();
    assert!(yaml.contains("body: Edited"));
    assert_eq!(
        Database::open(&path, Format::Readable)
            .unwrap()
            .execute("SELECT body FROM notes")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("Edited".into())]]
    );
}

#[test]
fn yaml_stream_handles_more_than_a_thousand_documents() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INT64); WITH RECURSIVE rows(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<1100) INSERT INTO t SELECT n FROM rows").unwrap();
    let bytes = db.export(Format::Readable).unwrap();
    let mut restored = Database::from_snapshot(&bytes).unwrap();
    assert_eq!(
        restored.execute("SELECT COUNT(*),SUM(n) FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1100), Value::Integer(605550)]]
    );
}

#[test]
fn invalid_yaml_cannot_publish_a_database() {
    let invalid = [
        MANUAL.replace("version: 2", "version: 1"),
        MANUAL.replace("version: 2", "version: 2\nversion: 2"),
        MANUAL.replace("version: 2", "version: 2\nvalues: null"),
        MANUAL.replace("  id: 7", "  id: 7\nversion: 2"),
        MANUAL.replace("  id: 7", "  id: 7\n  id: 8"),
        MANUAL.replace("  id: 7", "  wrong: 7"),
        MANUAL.replace("  id: 7\n", ""),
        MANUAL.replace("blob: '00ff'", "blob: xyz"),
        MANUAL.replace("    blob: '00ff'", "    blob: '00ff'\n    extra: 1"),
        MANUAL.replace("weight: 1.0", "weight: .nan"),
        MANUAL.replace("weight: 1.0", "weight: .inf"),
        MANUAL.replace("weight: 1.0", "weight: 1e999"),
        MANUAL.replace("  id: 7", "  id: 9223372036854775808"),
        MANUAL.replace("weight: 1.0", "weight: !custom 1.0"),
        MANUAL.replace("weight: 1.0", "weight: [unterminated"),
        format!("{MANUAL}---\nkind: database\nversion: 2\napplication_id: 0\nuser_version: 0\n"),
    ];
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("invalid.yaml");
    for yaml in invalid {
        assert!(Database::from_snapshot(yaml.as_bytes()).is_err(), "{yaml}");
        assert!(Database::restore(&path, yaml.as_bytes()).is_err(), "{yaml}");
        assert!(!path.exists());
    }
}

#[test]
fn cached_yaml_matches_full_snapshots_after_sqlite_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cached.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    let mut reference = Database::memory();
    for sql in [
        "CREATE TABLE parent(id INT64 PRIMARY KEY,label TEXT UNIQUE); CREATE TABLE child(id INT64 PRIMARY KEY,p INT64 REFERENCES parent(id) ON DELETE CASCADE); CREATE TABLE auxiliary(pk TEXT PRIMARY KEY,body TEXT) WITHOUT ROWID; CREATE TABLE shadow(_rowid_ TEXT,rowid TEXT,oid TEXT)",
        "BEGIN; INSERT INTO parent VALUES(1,'a'),(2,'b'); INSERT INTO child VALUES(1,1); INSERT INTO auxiliary VALUES('x','start'); INSERT INTO shadow VALUES('a','b','c'); COMMIT",
        "INSERT OR REPLACE INTO parent VALUES(3,'a')",
        "UPDATE parent SET id=20 WHERE id=2",
        "CREATE TABLE audit(label TEXT); CREATE TRIGGER log AFTER UPDATE ON parent BEGIN INSERT INTO audit VALUES(NEW.label); UPDATE auxiliary SET body=NEW.label; END",
        "UPDATE parent SET label='changed' WHERE id=20",
        "UPDATE auxiliary SET pk='z'; UPDATE shadow SET oid='new'",
        "BEGIN; UPDATE parent SET id=40 WHERE id=20; SAVEPOINT s; DELETE FROM parent; ROLLBACK TO s; ROLLBACK",
        "SAVEPOINT s; INSERT INTO parent VALUES(50,'pending'); ROLLBACK TO s; RELEASE s",
        "CREATE TABLE typed(d TIMESTAMP); INSERT INTO typed VALUES('2024-01-01T00:00:00.000000000Z')",
        "BEGIN; ALTER TABLE typed ADD COLUMN tags TEXT[]; ROLLBACK",
        "ALTER TABLE typed RENAME COLUMN d TO day; ALTER TABLE typed RENAME TO dates",
        "CREATE TABLE seq(id INT64 PRIMARY KEY AUTOINCREMENT); INSERT INTO seq VALUES(40); DELETE FROM seq",
        "INSERT INTO seq DEFAULT VALUES",
        "PRAGMA user_version=17; PRAGMA application_id=123",
        "VACUUM",
        "DELETE FROM parent; DELETE FROM shadow; DELETE FROM auxiliary",
        "BEGIN; COMMIT",
    ] {
        db.execute(sql).unwrap();
        reference.execute(sql).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            reference.export(Format::Readable).unwrap(),
            "{sql}"
        );
        assert_eq!(
            db.export(Format::Readable).unwrap(),
            reference.export(Format::Readable).unwrap(),
            "{sql}"
        );
    }
    let invalid = "INSERT INTO dates VALUES('2024-02-29T00:00:00.000000000Z'),('invalid')";
    assert!(db.execute(invalid).is_err());
    assert!(reference.execute(invalid).is_err());
    assert_eq!(
        db.export(Format::Readable).unwrap(),
        reference.export(Format::Readable).unwrap()
    );
    db.execute("UPDATE dates SET day='2024-03-01T00:00:00.000000000Z'")
        .unwrap();
    reference
        .execute("UPDATE dates SET day='2024-03-01T00:00:00.000000000Z'")
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        reference.export(Format::Readable).unwrap()
    );
}

#[test]
fn cached_yaml_handles_complex_names_and_main_tables_shadowed_by_temp() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("names.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(\"line\nbreak\" TEXT,\" ORDER BY \" TEXT,\"<<\" TEXT); INSERT INTO t VALUES('first','second','third')").unwrap();
    db.execute("CREATE TEMP TABLE t(ignored TEXT); INSERT INTO temp.t VALUES('temporary'); UPDATE main.t SET \"<<\"='updated'").unwrap();
    let mut imported = Database::from_snapshot(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        imported.execute("SELECT * FROM t").unwrap()[0].rows,
        vec![vec![
            Value::Text("first".into()),
            Value::Text("second".into()),
            Value::Text("updated".into())
        ]]
    );
    assert_eq!(
        db.export(Format::Readable).unwrap(),
        imported.export(Format::Readable).unwrap()
    );
}
