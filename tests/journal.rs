//! Readable files are logs: one YAML line per committed transaction, replayed
//! in order when the file is opened. VACUUM rewrites the canonical log.
#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{CheckMode, Database, Format, OpenOptions, Value};
use std::io::Write;

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn append(path: &std::path::Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// Lines appended since `before`.
fn appended(path: &std::path::Path, before: &str) -> String {
    let text = read(path);
    assert!(text.starts_with(before), "{text}");
    text[before.len()..].to_owned()
}

/// The logical content of a readable file as a canonical log.
fn canonical(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let mut db = match files::open_bytes(&bytes) {
        Ok(db) => db,
        Err(error) => panic!("{error}\n{}", String::from_utf8_lossy(&bytes)),
    };
    files::converted_bytes(&mut db, Format::Readable).unwrap()
}

fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    db.execute(sql).unwrap().pop().unwrap().rows
}

#[test]
fn each_transaction_appends_one_line() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notes.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(read(&path), "- {boxdb: 5}\n");
    db.execute("CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT)")
        .unwrap();
    let schema = read(&path);
    assert_eq!(schema.lines().count(), 2, "{schema}");
    assert!(
        schema
            .contains("{schema: [\"CREATE TABLE notes (id INTEGER PRIMARY KEY,body TEXT) STRICT\""),
        "{schema}"
    );
    db.execute("INSERT INTO notes VALUES(1,'first'),(2,'second')")
        .unwrap();
    assert_eq!(
        appended(&path, &schema),
        "- {insert: notes, rows: [{id: 1, body: first}, {id: 2, body: second}]}\n"
    );
    let before = read(&path);
    db.execute("BEGIN; UPDATE notes SET body='Line one'||char(10)||'line two' WHERE id=1; DELETE FROM notes WHERE id=2; INSERT INTO notes VALUES(3,NULL); COMMIT").unwrap();
    assert_eq!(
        appended(&path, &before),
        "- [{insert: notes, rows: [{id: 3}]}, {update: notes, key: {id: 1}, set: {body: \"Line one\\nline two\"}, was: {body: first}}, {delete: notes, rows: [{id: 2, body: second}]}]\n"
    );
    let before = read(&path);
    db.execute("PRAGMA user_version=3").unwrap();
    assert_eq!(
        appended(&path, &before),
        "- {pragma: {user_version: 3}, was: {user_version: 0}}\n"
    );
    let before = read(&path);
    db.execute("SELECT * FROM notes").unwrap();
    db.execute("BEGIN; UPDATE notes SET body=body; COMMIT")
        .unwrap();
    assert_eq!(read(&path), before, "unchanged transactions append nothing");
    let expected = rows(&mut db, "SELECT * FROM notes ORDER BY id");
    drop(db);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        rows(&mut reopened, "SELECT * FROM notes ORDER BY id"),
        expected
    );
    assert_eq!(
        rows(&mut reopened, "PRAGMA user_version"),
        vec![vec![Value::Integer(3)]]
    );
    reopened.execute("DELETE FROM notes WHERE id=3").unwrap();
    assert_eq!(
        appended(&path, &before),
        "- {delete: notes, rows: [{id: 3, body: null}]}\n",
        "reopening keeps appending"
    );
}

#[test]
fn autoincrement_sequences_are_written_only_when_replay_would_differ() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("seq.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE seq(id INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT)")
        .unwrap();
    let before = read(&path);
    db.execute("INSERT INTO seq(body) VALUES('x')").unwrap();
    assert_eq!(
        appended(&path, &before),
        "- {insert: seq, rows: [{id: 1, body: x}]}\n"
    );
    let before = read(&path);
    db.execute("BEGIN; INSERT INTO seq(body) VALUES('y'); DELETE FROM seq WHERE body='y'; COMMIT")
        .unwrap();
    assert_eq!(appended(&path, &before), "- {sequences: {seq: 2}}\n");
    drop(db);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    reopened
        .execute("INSERT INTO seq(body) VALUES('z')")
        .unwrap();
    assert_eq!(
        rows(&mut reopened, "SELECT id FROM seq ORDER BY id"),
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
}

#[test]
fn sequences_changed_without_row_changes_are_appended() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("seq.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(
        "CREATE TABLE seq(id INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT); \
         INSERT INTO seq(body) VALUES('x')",
    )
    .unwrap();
    let before = read(&path);
    db.execute("UPDATE sqlite_sequence SET seq=40 WHERE name='seq'")
        .unwrap();
    assert_eq!(appended(&path, &before), "- {sequences: {seq: 40}}\n");
    let before = read(&path);
    // SQLite records a sequence for a statement that inserted no rows.
    db.execute("CREATE TABLE other(id INTEGER PRIMARY KEY AUTOINCREMENT)")
        .unwrap();
    let created = read(&path);
    db.execute("INSERT INTO other SELECT id FROM seq WHERE id<0")
        .unwrap();
    assert_eq!(appended(&path, &created), "- {sequences: {other: 0}}\n");
    assert!(created.starts_with(&before));
    let live = files::converted_bytes(&mut db, Format::Readable).unwrap();
    drop(db);
    assert_eq!(canonical(&path), live);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    reopened
        .execute("INSERT INTO seq(body) VALUES('z')")
        .unwrap();
    assert_eq!(
        rows(&mut reopened, "SELECT id FROM seq ORDER BY id"),
        vec![vec![Value::Integer(1)], vec![Value::Integer(41)]]
    );
}

#[test]
fn rolled_back_changes_are_not_appended() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); CREATE TRIGGER reject AFTER INSERT ON t WHEN NEW.body='bad' BEGIN SELECT RAISE(ABORT,'bad row'); END").unwrap();
    db.execute("INSERT INTO t VALUES(1,'a')").unwrap();
    let before = read(&path);
    db.execute("BEGIN; UPDATE t SET body='discarded' WHERE id=1; INSERT INTO t VALUES(2,'discarded'); CREATE TABLE gone(x TEXT); ROLLBACK").unwrap();
    assert!(db.execute("INSERT INTO t VALUES(3,'bad')").is_err());
    db.execute("BEGIN; INSERT INTO t VALUES(5,'kept'); SAVEPOINT s; DELETE FROM t WHERE id=5; CREATE TABLE gone(x TEXT); INSERT INTO gone VALUES('x'); ROLLBACK TO s; RELEASE s; COMMIT").unwrap();
    db.execute("INSERT INTO t VALUES(4,'d')").unwrap();
    assert_eq!(
        appended(&path, &before),
        "- {insert: t, rows: [{id: 5, body: kept}]}\n- {insert: t, rows: [{id: 4, body: d}]}\n"
    );
    assert_eq!(
        canonical(&path),
        files::converted_bytes(&mut db, Format::Readable).unwrap()
    );
}

#[test]
fn replay_restores_schema_changes_swaps_cascades_and_identity_changes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    let mut reference = Database::memory();
    for sql in [
        "CREATE TABLE parent(id INTEGER PRIMARY KEY,label TEXT UNIQUE); CREATE TABLE child(id INTEGER PRIMARY KEY,p INTEGER REFERENCES parent(id) ON DELETE CASCADE ON UPDATE CASCADE); CREATE TABLE plain(body TEXT UNIQUE)",
        "INSERT INTO parent VALUES(1,'a'),(2,'b'),(3,'c'); INSERT INTO child VALUES(1,1),(2,2),(3,3); INSERT INTO plain VALUES('x'),('y')",
        "BEGIN; UPDATE parent SET label='tmp' WHERE id=1; UPDATE parent SET label='a' WHERE id=2; UPDATE parent SET label='b' WHERE id=1; COMMIT",
        "BEGIN; UPDATE child SET p=3 WHERE id=1; DELETE FROM parent WHERE id=1; COMMIT",
        "DELETE FROM parent WHERE id=2",
        "UPDATE parent SET id=30 WHERE id=3",
        "INSERT OR REPLACE INTO parent VALUES(40,'c')",
        "UPDATE plain SET rowid=rowid+10",
        "BEGIN; INSERT INTO plain VALUES('z'); SAVEPOINT s; DELETE FROM plain; ROLLBACK TO s; RELEASE s; COMMIT",
        "CREATE TABLE codes(code TEXT PRIMARY KEY,n INT64); INSERT INTO codes VALUES('a',1),('b',2); UPDATE codes SET code='c' WHERE code='a'",
        "CREATE TABLE pairs(a TEXT,b INT64,v TEXT,PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO pairs VALUES('x',1,'one'),('x',2,'two'); UPDATE pairs SET b=3 WHERE b=1; UPDATE pairs SET v='TWO' WHERE b=2",
        "CREATE TABLE copy AS SELECT id,label FROM parent",
        "ALTER TABLE parent ADD COLUMN prio INT32 DEFAULT 5",
        "ALTER TABLE parent RENAME COLUMN label TO name; ALTER TABLE copy RENAME TO copied",
        "ALTER TABLE copied DROP COLUMN label",
        "CREATE INDEX parent_prio ON parent(prio); CREATE VIEW names AS SELECT name FROM parent",
        "DROP TABLE parent",
        "CREATE TABLE typed(id UUID PRIMARY KEY,at INSTANT,amount DECIMAL(10,2),tags TEXT[],flag BOOLEAN,locked TEXT READONLY,g INTEGER AS (length(locked)) STORED)",
        "INSERT INTO typed(id,at,amount,tags,flag,locked) VALUES('00000000-0000-4000-8000-000000000001','2024-02-29T12:00:00Z','12.5','[\"a\",\"b\"]',TRUE,'fixed')",
        "UPDATE typed SET amount=amount+1,tags='[\"c\"]',flag=FALSE",
        "ALTER TABLE typed ADD COLUMN extra TEXT[] DEFAULT '[]'",
        "PRAGMA user_version=7; PRAGMA application_id=99",
    ] {
        db.execute(sql).unwrap();
        reference.execute(sql).unwrap();
        assert_eq!(
            canonical(&path),
            files::converted_bytes(&mut reference, Format::Readable).unwrap(),
            "{sql}\n{}",
            read(&path)
        );
    }
}

#[test]
fn vacuum_rewrites_the_canonical_log() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE keyless(body TEXT); INSERT INTO keyless VALUES('1'),('2'),('3'); DELETE FROM keyless WHERE body='2'").unwrap();
    for n in 0..20 {
        db.execute(&format!("UPDATE keyless SET body='{n}' WHERE rowid=1"))
            .unwrap();
    }
    let long = read(&path);
    db.execute("VACUUM").unwrap();
    let compacted = read(&path);
    assert!(compacted.len() < long.len(), "{compacted}");
    assert_eq!(
        compacted.as_bytes(),
        files::converted_bytes(&mut db, Format::Readable).unwrap()
    );
    assert!(
        compacted.contains("- {insert: keyless, rows: [{rowid: 2, body: \"3\"}]}\n"),
        "VACUUM renumbers rows without a key: {compacted}"
    );
    db.execute("UPDATE keyless SET body='last' WHERE rowid=2")
        .unwrap();
    let expected = rows(&mut db, "SELECT rowid,body FROM keyless ORDER BY rowid");
    drop(db);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        rows(
            &mut reopened,
            "SELECT rowid,body FROM keyless ORDER BY rowid"
        ),
        expected
    );
}

/// A union merge of two branches: the shared base followed by the lines each
/// branch appended.
fn merge(setup: &str, ours: &str, theirs: &str) -> boxdb::Result<files::DatabaseFile> {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("base.yaml");
    Database::open(&base, Format::Readable)
        .unwrap()
        .execute(setup)
        .unwrap();
    let base = read(&base);
    let mut merged = base.clone();
    for (name, sql) in [("ours.yaml", ours), ("theirs.yaml", theirs)] {
        let path = temp.path().join(name);
        std::fs::write(&path, &base).unwrap();
        Database::open(&path, Format::Readable)
            .unwrap()
            .execute(sql)
            .unwrap();
        merged.push_str(&appended(&path, &base));
    }
    files::open_bytes(merged.as_bytes())
}

const NOTES: &str = "CREATE TABLE notes(id INTEGER PRIMARY KEY,title TEXT,done BOOLEAN NOT NULL DEFAULT FALSE); INSERT INTO notes(id,title) VALUES(1,'buy milk'),(2,'call mom'),(3,'read')";

#[test]
fn merged_branches_replay_changes_to_different_columns_and_rows() {
    let mut db = merge(
        NOTES,
        "UPDATE notes SET title='buy oat milk' WHERE id=1; DELETE FROM notes WHERE id=3",
        "UPDATE notes SET done=TRUE WHERE id=1; UPDATE notes SET title='call dad' WHERE id=2",
    )
    .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT * FROM notes ORDER BY id"),
        vec![
            vec![
                Value::Integer(1),
                Value::Text("buy oat milk".into()),
                Value::Boolean(true)
            ],
            vec![
                Value::Integer(2),
                Value::Text("call dad".into()),
                Value::Boolean(false)
            ],
        ]
    );
    let mut db = merge(
        NOTES,
        "ALTER TABLE notes ADD COLUMN prio INTEGER NOT NULL DEFAULT 5",
        "INSERT INTO notes(id,title) VALUES(9,'new')",
    )
    .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT id,prio FROM notes WHERE id=9"),
        vec![vec![Value::Integer(9), Value::Integer(5)]],
        "rows written before a column existed receive its default"
    );
    let mut db = merge(
        "CREATE TABLE tags(id UUID PRIMARY KEY,name TEXT NOT NULL)",
        "INSERT INTO tags VALUES('00000000-0000-4000-8000-000000000001','ours')",
        "INSERT INTO tags VALUES('00000000-0000-4000-8000-000000000002','theirs')",
    )
    .unwrap();
    assert_eq!(
        rows(&mut db, "SELECT name FROM tags ORDER BY name"),
        vec![
            vec![Value::Text("ours".into())],
            vec![Value::Text("theirs".into())]
        ],
        "rows with a declared key do not collide on hidden rowids"
    );
}

#[test]
fn merged_branches_reject_conflicting_changes() {
    for (ours, theirs) in [
        (
            "UPDATE notes SET title='ours' WHERE id=2",
            "UPDATE notes SET title='theirs' WHERE id=2",
        ),
        (
            "DELETE FROM notes WHERE id=2",
            "UPDATE notes SET done=TRUE WHERE id=2",
        ),
        (
            "INSERT INTO notes(id,title) VALUES(4,'ours')",
            "INSERT INTO notes(id,title) VALUES(4,'theirs')",
        ),
        (
            "ALTER TABLE notes DROP COLUMN done",
            "UPDATE notes SET done=TRUE WHERE id=1",
        ),
    ] {
        let error = merge(NOTES, ours, theirs).err().expect(theirs);
        assert_eq!(error.code(), "BOXDB_CONFLICT", "{ours}; {theirs}: {error}");
        assert!(error.to_string().contains("line "), "{error}");
    }
}

#[test]
fn previous_formats_explain_the_binary_migration_path() {
    let v4 = "---\nkind: database\nversion: 4\ncodec_version: 1\napplication_id: 0\nuser_version: 0\n...\n";
    let error = files::open_bytes(v4.as_bytes()).err().unwrap();
    assert_eq!(error.code(), "BOXDB_FORMAT");
    assert!(error.to_string().contains("BoxDB 0.8"), "{error}");
    assert!(error.to_string().contains("binary"), "{error}");
}

const TORN: &str = "- {insert: t, rows: [{id: 9, bo";

#[test]
fn interrupted_appends_are_ignored_and_removed_by_the_next_writer() {
    for tail in [TORN.as_bytes(), b"- [{ins", &[0u8; 64], b"\0\0\0\0\n"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.yaml");
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
            .unwrap();
        db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
        drop(db);
        let committed = read(&path);
        append(&path, tail);
        let report = Database::check_file(&path, CheckMode::Full).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "BOXDB_INCOMPLETE_COMMIT"),
            "{report:?}"
        );
        let mut reader = Database::open_with_options(
            &path,
            Format::Readable,
            OpenOptions {
                read_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            rows(&mut reader, "SELECT id FROM t ORDER BY id"),
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        drop(reader);
        let mut writer = Database::open(&path, Format::Readable).unwrap();
        writer.execute("INSERT INTO t VALUES(3,'c')").unwrap();
        assert_eq!(
            appended(&path, &committed),
            "- {insert: t, rows: [{id: 3, body: c}]}\n"
        );
        let report = Database::check_file(&path, CheckMode::Full).unwrap();
        assert!(report.issues.is_empty(), "{report:?}");
    }
}

#[test]
fn damaged_lines_before_the_end_are_errors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    db.execute("INSERT INTO t VALUES(3,'c')").unwrap();
    drop(db);
    let text = read(&path);
    for damaged in [
        text.replacen("{id: 2, body: b}", "{id: 2, body: [b}", 1),
        text.replacen(
            "{insert: t, rows: [{id: 2",
            "{insert: missing, rows: [{id: 2",
            1,
        ),
        text.replacen("{id: 2, body: b}", "{id: 2, wrong: b}", 1),
        text.replacen("{insert: t, rows: [{id: 2", "{upsert: t, rows: [{id: 2", 1),
        text.replacen("- {boxdb: 5}\n", "", 1),
        format!("{text}- {{boxdb: 5}}\n"),
    ] {
        assert_ne!(damaged, text);
        let error = files::open_bytes(damaged.as_bytes()).err().expect(&damaged);
        assert!(error.to_string().contains("line "), "{error}");
    }
}

#[test]
fn recovery_applies_intact_lines_and_rejects_damaged_ones() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let output = temp.path().join("recovered.db");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    db.execute("INSERT INTO t VALUES(3,'c')").unwrap();
    drop(db);
    let text = read(&path).replacen("{id: 2, body: b}", "{id: 2, body: [b", 1);
    std::fs::write(&path, &text).unwrap();
    let report = Database::recover_file(&path, &output, Format::Binary).unwrap();
    assert_eq!(report.rejected.len(), 1, "{report:?}");
    assert!(
        std::fs::read_to_string(report.rejected_path.unwrap())
            .unwrap()
            .contains("body: [b")
    );
    let mut recovered = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        rows(&mut recovered, "SELECT id FROM t ORDER BY id"),
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
}
