use boxdb::{Database, Format, Value};

#[test]
fn yaml_stream_preserves_sqlite_storage_classes_and_ambiguous_text() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE cells(id INTEGER PRIMARY KEY,n ANY) STRICT")
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
    db.execute("CREATE TABLE t(n INTEGER); WITH RECURSIVE rows(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<1100) INSERT INTO t SELECT n FROM rows").unwrap();
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
