#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, Value};

#[test]
fn yaml_omits_sql_null_columns_and_restores_them_without_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("source.db");
    let readable = temp.path().join("data.yaml");
    let mut db = Database::open(&binary, Format::Binary).unwrap();
    db.execute("CREATE TABLE notes(body TEXT DEFAULT 'default',active BOOLEAN DEFAULT TRUE,doc JSON); INSERT INTO notes(rowid,body,active,doc) VALUES(7,NULL,NULL,NULL),(9,'null',FALSE,'{\"nested\":null}')").unwrap();
    let query = "SELECT rowid,* FROM notes ORDER BY rowid";
    let expected = db.execute(query).unwrap();
    Database::convert(&binary, &readable, Format::Readable).unwrap();
    let bytes = std::fs::read(&readable).unwrap();
    assert_eq!(
        table_rows(&bytes, "notes"),
        vec![
            serde_json::json!({"rowid": 7}),
            serde_json::json!({"rowid": 9, "body": "null", "active": false, "doc": "{\"nested\":null}"}),
        ]
    );
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("- {rowid: 7}\n")
    );
    let mut restored = Database::open(&readable, Format::Readable).unwrap();
    assert_eq!(restored.execute(query).unwrap(), expected);
    restored.execute("UPDATE notes SET body=NULL,active=NULL,doc=NULL WHERE rowid=9; UPDATE notes SET body='',active=FALSE WHERE rowid=7").unwrap();
    let expected = restored.execute(query).unwrap();
    let bytes = canonical_file(&readable);
    assert_eq!(
        table_rows(&bytes, "notes"),
        vec![
            serde_json::json!({"rowid": 7, "body": "", "active": false}),
            serde_json::json!({"rowid": 9})
        ]
    );
    assert_eq!(
        files::converted_bytes(&mut restored, Format::Readable).unwrap(),
        bytes
    );
    drop(restored);
    assert_eq!(
        Database::open(&readable, Format::Readable)
            .unwrap()
            .execute(query)
            .unwrap(),
        expected
    );
}

#[test]
fn boolean_columns_use_yaml_booleans_and_roundtrip_through_updates() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("source.db");
    let readable = temp.path().join("data.yaml");
    let mut db = Database::open(&binary, Format::Binary).unwrap();
    db.execute("CREATE TABLE flags(id INT64 PRIMARY KEY,active BOOLEAN,alias BOOL,n INTEGER,untyped ANY); INSERT INTO flags VALUES(1,TRUE,FALSE,1,0),(2,FALSE,TRUE,0,1),(3,NULL,NULL,NULL,NULL)").unwrap();
    let query = "SELECT * FROM flags ORDER BY id";
    let expected = db.execute(query).unwrap();
    Database::convert(&binary, &readable, Format::Readable).unwrap();
    let bytes = std::fs::read(&readable).unwrap();
    assert_eq!(
        table_rows(&bytes, "flags"),
        vec![
            serde_json::json!({"rowid": 1, "id": 1, "active": true, "alias": false, "n": 1, "untyped": 0}),
            serde_json::json!({"rowid": 2, "id": 2, "active": false, "alias": true, "n": 0, "untyped": 1}),
            serde_json::json!({"rowid": 3, "id": 3}),
        ]
    );
    let mut restored = Database::open(&readable, Format::Readable).unwrap();
    assert_eq!(restored.execute(query).unwrap(), expected);
    restored
        .execute("UPDATE flags SET active=NOT active,alias=NOT alias WHERE id<3")
        .unwrap();
    let yaml = std::fs::read_to_string(&readable).unwrap();
    assert!(yaml.contains("active: true,"));
    assert!(yaml.contains("active: false,"));
    assert!(!yaml.contains("active: 0") && !yaml.contains("active: 1"));
    let expected = restored.execute(query).unwrap();
    drop(restored);
    assert_eq!(
        Database::open(&readable, Format::Readable)
            .unwrap()
            .execute(query)
            .unwrap(),
        expected
    );
}

#[test]
fn yaml_boolean_columns_follow_committed_schema_changes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE flags(id TEXT PRIMARY KEY,active BOOLEAN) WITHOUT ROWID; INSERT INTO flags VALUES('a',TRUE)").unwrap();
    db.execute("BEGIN; DROP TABLE flags; CREATE TABLE flags(id TEXT PRIMARY KEY,active INTEGER) WITHOUT ROWID; INSERT INTO flags VALUES('b',0); ROLLBACK").unwrap();
    let yaml = std::fs::read_to_string(&path).unwrap();
    assert!(yaml.contains("- {active: true, id: a}\n"), "{yaml}");
    assert_eq!(
        db.execute("SELECT * FROM flags").unwrap()[0].rows,
        vec![vec![Value::Text("a".into()), Value::Boolean(true)]]
    );
    db.execute("ALTER TABLE flags ADD COLUMN other BOOL DEFAULT FALSE; ALTER TABLE flags RENAME COLUMN active TO enabled; ALTER TABLE flags RENAME TO settings; UPDATE settings SET enabled=FALSE,other=TRUE").unwrap();
    let yaml = std::fs::read_to_string(&path).unwrap();
    assert!(
        yaml.contains("- {enabled: false, id: a, other: true}\n"),
        "{yaml}"
    );
    let expected = db.execute("SELECT * FROM settings").unwrap();
    drop(db);
    assert_eq!(
        Database::open(&path, Format::Readable)
            .unwrap()
            .execute("SELECT * FROM settings")
            .unwrap(),
        expected
    );
}

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
            "user.name@example.com",
            "docs/v1.2+beta",
            "e1",
            "a.",
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
    let bytes = files::converted_bytes(&mut db, Format::Readable).unwrap();
    let yaml = std::str::from_utf8(&bytes).unwrap();
    assert!(yaml.starts_with("---\nkind: database\nversion: 4\n"));
    assert!(yaml.contains("rows:\n- {rowid: 0, id: 0}\n"), "{yaml}");
    assert!(yaml.contains(", \"n\": user.name@example.com}\n"), "{yaml}");
    assert!(yaml.contains(", \"n\": docs/v1.2+beta}\n"), "{yaml}");
    assert!(
        yaml.contains("|"),
        "multiline text should use block scalars"
    );
    let first_row = &table_rows(&bytes, "cells")[0];
    assert_eq!(first_row["id"], 0);
    assert!(first_row.get("n").is_none());
    let mut restored = files::open_bytes(&bytes).unwrap();
    assert_eq!(restored.execute(query).unwrap(), before);
    assert_eq!(restored.execute("SELECT * FROM keys").unwrap(), keys);
    assert_eq!(
        files::converted_bytes(&mut restored, Format::Readable).unwrap(),
        bytes
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cells.yaml");
    let mut file = files::write_converted(&path, &bytes).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let update = "UPDATE cells SET n='edited\nblock\n' WHERE id=1";
    file.execute(update).unwrap();
    db.execute(update).unwrap();
    assert_eq!(
        canonical_file(&path),
        files::converted_bytes(&mut db, Format::Readable).unwrap()
    );
    assert_eq!(file.execute(query).unwrap(), db.execute(query).unwrap());
}

const MANUAL: &str = r#"# Documents and rows may be edited by hand.
---
kind: database
version: 4
codec_version: 1
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
kind: rows
table: notes
rows:
- body: |-
    First line
    雪
  weight: 1.0
  flag: true
  bytes:
    blob: AP8=
  id: 7
...
"#;

const MANUAL_V3: &str = r#"# Documents and row fields may be edited by hand.
---
kind: database
version: 3
codec_version: 1
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
    let bytes = files::converted_bytes(&mut db, Format::Readable).unwrap();
    let mut restored = files::open_bytes(&bytes).unwrap();
    assert_eq!(
        restored.execute("SELECT COUNT(*),SUM(n) FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1100), Value::Integer(605550)]]
    );
}

#[test]
fn invalid_yaml_cannot_publish_a_database() {
    let invalid = [
        MANUAL.replace("version: 4", "version: 1"),
        MANUAL.replace("version: 4", "version: 4\nversion: 4"),
        MANUAL.replace("version: 4", "version: 4\nrows: []"),
        MANUAL.replace("  id: 7", "  id: 7\nversion: 4"),
        MANUAL.replace("  id: 7", "  id: 7\n  rowid: 7\n  rowid: 8"),
        MANUAL.replace("table: notes\nrows:", "rows:"),
        MANUAL.replace("kind: rows\ntable: notes", "kind: rows\ntable: missing"),
        MANUAL.replace("  id: 7", "  id: 7\n  id: 8"),
        MANUAL.replace("  id: 7", "  wrong: 7"),
        MANUAL
            .replace("body TEXT", "body TEXT NOT NULL")
            .replace("- body: |-\n    First line\n    雪\n  weight", "- weight"),
        MANUAL.replace("blob: AP8=", "blob: xyz"),
        MANUAL.replace("    blob: AP8=", "    blob: AP8=\n    extra: 1"),
        MANUAL.replace("weight: 1.0", "weight: .nan"),
        MANUAL.replace("weight: 1.0", "weight: .inf"),
        MANUAL.replace("weight: 1.0", "weight: 1e999"),
        MANUAL.replace("  id: 7", "  id: 9223372036854775808"),
        MANUAL.replace("weight: 1.0", "weight: !custom 1.0"),
        MANUAL.replace("weight: 1.0", "weight: [unterminated"),
        format!(
            "{MANUAL}---\nkind: database\nversion: 4\ncodec_version: 1\napplication_id: 0\nuser_version: 0\n"
        ),
    ];
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("invalid.yaml");
    for yaml in invalid {
        assert!(files::open_bytes(yaml.as_bytes()).is_err(), "{yaml}");
        assert!(
            files::write_converted(&path, yaml.as_bytes()).is_err(),
            "{yaml}"
        );
        assert!(!path.exists());
    }
}

#[test]
fn readable_file_matches_conversion_after_sqlite_mutations() {
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
        "CREATE TABLE typed(d INSTANT); INSERT INTO typed VALUES('2024-01-01T00:00:00.000000Z')",
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
            canonical_file(&path),
            files::converted_bytes(&mut reference, Format::Readable).unwrap(),
            "{sql}"
        );
        assert_eq!(
            files::converted_bytes(&mut db, Format::Readable).unwrap(),
            files::converted_bytes(&mut reference, Format::Readable).unwrap(),
            "{sql}"
        );
    }
    let invalid = "INSERT INTO dates VALUES('2024-02-29T00:00:00.000000Z'),('invalid')";
    assert!(db.execute(invalid).is_err());
    assert!(reference.execute(invalid).is_err());
    assert_eq!(
        files::converted_bytes(&mut db, Format::Readable).unwrap(),
        files::converted_bytes(&mut reference, Format::Readable).unwrap()
    );
    db.execute("UPDATE dates SET day='2024-03-01T00:00:00.000000Z'")
        .unwrap();
    reference
        .execute("UPDATE dates SET day='2024-03-01T00:00:00.000000Z'")
        .unwrap();
    assert_eq!(
        canonical_file(&path),
        files::converted_bytes(&mut reference, Format::Readable).unwrap()
    );
}

#[test]
fn readable_file_handles_complex_names_and_main_tables_shadowed_by_temp() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("names.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(\"line\nbreak\" TEXT,\" ORDER BY \" TEXT,\"<<\" TEXT); INSERT INTO t VALUES('first','second','third')").unwrap();
    db.execute("CREATE TEMP TABLE t(ignored TEXT); INSERT INTO temp.t VALUES('temporary'); UPDATE main.t SET \"<<\"='updated'").unwrap();
    let mut imported = files::open_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        imported.execute("SELECT * FROM t").unwrap()[0].rows,
        vec![vec![
            Value::Text("first".into()),
            Value::Text("second".into()),
            Value::Text("updated".into())
        ]]
    );
    assert_eq!(
        files::converted_bytes(&mut db, Format::Readable).unwrap(),
        files::converted_bytes(&mut imported, Format::Readable).unwrap()
    );
}

/// The logical content of a readable file, with its journal applied.
fn canonical_file(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let mut db = match files::open_bytes(&bytes) {
        Ok(db) => db,
        Err(error) => panic!("{error}\n{}", String::from_utf8_lossy(&bytes)),
    };
    files::converted_bytes(&mut db, Format::Readable).unwrap()
}

/// Row maps of one table document, including the rowid alias key.
fn table_rows(bytes: &[u8], table: &str) -> Vec<serde_json::Value> {
    let documents: Vec<serde_json::Value> = serde_saphyr::from_slice_multiple(bytes).unwrap();
    documents
        .into_iter()
        .filter(|doc| doc["kind"] == "rows" && doc["table"] == table)
        .flat_map(|doc| doc["rows"].as_array().unwrap().clone())
        .collect()
}

#[test]
fn yaml_groups_rows_by_table_with_one_line_per_row() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT,amount DECIMAL(10,2),at INSTANT,ref UUID,data BYTEA,raw ANY,done BOOLEAN);
         INSERT INTO notes VALUES(1,'hello','12.5','2024-02-29T12:00:00Z','00000000-0000-4000-8000-000000000001',X'00ff',X'0102',TRUE);
         INSERT INTO notes(id,body,raw) VALUES(2,'First line'||char(10)||'Second line',CAST('1.25' AS DECIMAL));
         INSERT INTO notes(id,body) VALUES(3,'Hello, world');
         CREATE TABLE empty(id INTEGER PRIMARY KEY)",
    )
    .unwrap();
    let query = "SELECT rowid,* FROM notes ORDER BY id";
    let expected = db.execute(query).unwrap();
    let bytes = files::converted_bytes(&mut db, Format::Readable).unwrap();
    let yaml = std::str::from_utf8(&bytes).unwrap();
    assert!(
        yaml.starts_with("---\nkind: database\nversion: 4\n"),
        "{yaml}"
    );
    let notes = "---
kind: rows
table: notes
rows:
- {rowid: 1, amount: 12.5, at: 2024-02-29T12:00:00.000000Z, body: hello, data: AP8=, done: true, id: 1, raw: {blob: AQI=}, ref: 00000000-0000-4000-8000-000000000001}
- rowid: 2
  body: |-
    First line
    Second line
  id: 2
  raw: {decimal: 1.25}
- {rowid: 3, body: \"Hello, world\", id: 3}
";
    let start = yaml.find("---\nkind: rows\ntable: notes\n").expect(yaml);
    assert!(yaml[start..].starts_with(notes), "{yaml}");
    let metadata = yaml.find("table: __boxdb_columns\n").expect(yaml);
    assert!(metadata < start, "metadata rows precede table rows: {yaml}");
    assert!(!yaml.contains("table: empty\nrows"), "{yaml}");
    assert_eq!(yaml.matches("kind: rows\n").count(), 3, "{yaml}");
    let mut restored = files::open_bytes(&bytes).unwrap();
    assert_eq!(restored.execute(query).unwrap(), expected);
    assert_eq!(
        files::converted_bytes(&mut restored, Format::Readable).unwrap(),
        bytes
    );
}

#[test]
fn typed_plain_scalars_are_validated_on_import() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,amount DECIMAL,at INSTANT,ref UUID,data BYTEA,big UINT64); INSERT INTO t VALUES(1,'1.50','2024-02-29T12:00:00Z','00000000-0000-4000-8000-000000000001',X'00ff',18446744073709551615)").unwrap();
    let valid =
        String::from_utf8(files::converted_bytes(&mut db, Format::Readable).unwrap()).unwrap();
    let row = "- {rowid: 1, amount: 1.5, at: 2024-02-29T12:00:00.000000Z, big: 18446744073709551615, data: AP8=, id: 1, ref: 00000000-0000-4000-8000-000000000001}\n";
    assert!(valid.contains(row), "{valid}");
    let restored = files::open_bytes(valid.as_bytes())
        .unwrap()
        .execute("SELECT amount,big FROM t")
        .unwrap();
    assert_eq!(
        restored[0].rows,
        vec![vec![
            Value::Decimal("1.5".into()),
            Value::UnsignedInteger(u64::MAX)
        ]]
    );
    let accepted = [
        row.replace("amount: 1.5", "amount: '1.50'"),
        row.replace("data: AP8=", "data: \"AP8=\""),
        row.replace("amount: 1.5, ", "amount: ~, "),
    ];
    for replacement in accepted {
        let yaml = valid.replace(row, &replacement);
        assert!(files::open_bytes(yaml.as_bytes()).is_ok(), "{yaml}");
    }
    let rejected = [
        "amount: true",
        "amount: {decimal: 1.5}",
        "amount: [1.5]",
        "amount: 1.5.0",
        "at: 2024-02-30T12:00:00Z",
        "ref: not-a-uuid",
        "data: '!!'",
        "data: AP8",
        "data: {blob: AP8=}",
        "big: 18446744073709551616",
        "big: -1",
    ];
    for value in rejected {
        let key = value.split(':').next().unwrap();
        let original = row
            .split(", ")
            .find(|field| field.starts_with(&format!("{key}:")))
            .unwrap();
        let yaml = valid.replace(row, &row.replace(original, value));
        assert_ne!(yaml, valid, "{value}");
        assert!(files::open_bytes(yaml.as_bytes()).is_err(), "{value}");
    }
}

#[test]
fn yaml_rejects_documents_out_of_stream_order() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,at INSTANT); INSERT INTO t VALUES(1,'2024-02-29T12:00:00Z')").unwrap();
    let valid =
        String::from_utf8(files::converted_bytes(&mut db, Format::Readable).unwrap()).unwrap();
    assert!(files::open_bytes(valid.as_bytes()).is_ok());
    let mut documents: Vec<&str> = valid.split("---\n").filter(|doc| !doc.is_empty()).collect();
    let count = documents.len();
    assert!(documents[count - 1].contains("table: t\n"), "{valid}");
    assert!(
        documents[count - 2].contains("table: __boxdb_columns\n"),
        "{valid}"
    );
    documents.swap(count - 1, count - 2);
    let reordered: String = documents.iter().map(|doc| format!("---\n{doc}")).collect();
    let error = files::open_bytes(reordered.as_bytes()).err().unwrap();
    assert!(error.to_string().contains("order"), "{error}");
}

#[test]
fn version_three_streams_explain_the_binary_migration_path() {
    let error = files::open_bytes(MANUAL_V3.as_bytes()).err().unwrap();
    assert_eq!(error.code(), "BOXDB_FORMAT");
    assert!(error.to_string().contains("version 3"), "{error}");
    assert!(error.to_string().contains("binary"), "{error}");
}

#[test]
fn yaml_text_quoting_roundtrips_generated_strings() {
    let alphabet: Vec<char> =
        "a Z0_-.:#,{}[]\"'\\\n\r\t\u{85}\u{2028}\u{feff}\u{7f}\u{1}é雪@/+?!&*%|>~="
            .chars()
            .collect();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    let mut db = Database::memory();
    db.execute("CREATE TABLE texts(id INTEGER PRIMARY KEY,body TEXT,cell ANY)")
        .unwrap();
    for id in 0..3000 {
        let length = next(10);
        let text: String = (0..length)
            .map(|_| alphabet[next(alphabet.len())])
            .collect();
        db.execute_params(
            "INSERT INTO texts VALUES(?,?,?)",
            &[
                Value::Integer(id),
                Value::Text(text.clone()),
                Value::Text(text),
            ],
        )
        .unwrap();
    }
    let query = "SELECT * FROM texts ORDER BY id";
    let expected = db.execute(query).unwrap();
    let bytes = files::converted_bytes(&mut db, Format::Readable).unwrap();
    let mut restored = files::open_bytes(&bytes).unwrap();
    assert_eq!(restored.execute(query).unwrap(), expected);
    assert_eq!(
        files::converted_bytes(&mut restored, Format::Readable).unwrap(),
        bytes
    );
}
