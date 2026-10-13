use boxdb::{Database, Format, Value};
use rusqlite::Connection;

#[test]
fn binary_files_are_sqlite_and_share_live_wal_with_other_clients() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shared.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'boxdb')")
        .unwrap();
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(b"SQLite format 3\0")
    );
    assert!(temp.path().join("shared.db-wal").exists());
    let sqlite = Connection::open(&path).unwrap();
    assert_eq!(
        sqlite
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        sqlite
            .query_row("SELECT body FROM t", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "boxdb"
    );
    sqlite
        .execute("INSERT INTO t VALUES(2,'sqlite')", [])
        .unwrap();
    let mut other = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        other.execute("SELECT id FROM t ORDER BY id").unwrap()[0]
            .rows
            .len(),
        2
    );
    db.execute("BEGIN; INSERT INTO t VALUES(3,'pending')")
        .unwrap();
    assert!(db.export(Format::Binary).is_err());
    let snapshot = Database::read_snapshot(&path).unwrap();
    let mut snapshot = Database::from_snapshot(&snapshot).unwrap();
    assert_eq!(
        snapshot.execute("SELECT id FROM t").unwrap()[0].rows.len(),
        2
    );
    db.execute("ROLLBACK").unwrap();
}
#[test]
fn sqlite_and_jsonl_convert_schemas_rows_and_autoincrement_state() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT,n TEXT); INSERT INTO t VALUES(1,'a'),(10,'b'); DELETE FROM t WHERE id=10; CREATE INDEX t_n ON t(n); CREATE VIEW v AS SELECT * FROM t; CREATE TABLE audit(n TEXT); CREATE TRIGGER log AFTER INSERT ON t BEGIN INSERT INTO audit VALUES(NEW.n); END; CREATE TABLE emptied(id INTEGER PRIMARY KEY AUTOINCREMENT); INSERT INTO emptied VALUES(42); DELETE FROM emptied;").unwrap();
    for format in [Format::Binary, Format::Readable] {
        let bytes = db.export(format).unwrap();
        let mut restored = Database::from_snapshot(&bytes).unwrap();
        restored.execute("INSERT INTO t(n) VALUES('next')").unwrap();
        restored
            .execute("INSERT INTO emptied DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            restored.execute("SELECT id FROM emptied").unwrap()[0].rows,
            vec![vec![Value::Integer(43)]]
        );
        assert_eq!(
            restored.execute("SELECT id,n FROM v ORDER BY id").unwrap()[0].rows,
            vec![
                vec![Value::Integer(1), Value::Text("a".into())],
                vec![Value::Integer(11), Value::Text("next".into())]
            ]
        );
        assert_eq!(
            restored.execute("SELECT n FROM audit").unwrap()[0].rows,
            vec![vec![Value::Text("next".into())]]
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("restored.db");
        drop(Database::restore(&path, &bytes).unwrap());
        assert!(Database::restore(&path, &bytes).is_err());
        let mut reopened = Database::open(&path, Format::Binary).unwrap();
        assert_eq!(reopened.format(), Some(format));
        assert_eq!(
            reopened.execute("SELECT n FROM t").unwrap()[0].rows,
            vec![vec![Value::Text("a".into())]]
        );
    }
}
#[test]
fn readable_storage_handles_generated_and_without_rowid_tables() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id TEXT PRIMARY KEY,n INTEGER,doubled INTEGER GENERATED ALWAYS AS(n*2) STORED) WITHOUT ROWID; INSERT INTO t(id,n) VALUES('a',2),('b',3)").unwrap();
    let bytes = db.export(Format::Readable).unwrap();
    let mut restored = Database::from_snapshot(&bytes).unwrap();
    assert_eq!(
        restored.execute("SELECT * FROM t ORDER BY id").unwrap(),
        db.execute("SELECT * FROM t ORDER BY id").unwrap()
    );
    assert_eq!(restored.export(Format::Readable).unwrap(), bytes);

    db.execute("CREATE TABLE shadow(n INTEGER,_rowid_ INTEGER GENERATED ALWAYS AS(n*2)); INSERT INTO shadow(rowid,n) VALUES(99,3)").unwrap();
    let mut restored = Database::from_snapshot(&db.export(Format::Readable).unwrap()).unwrap();
    assert_eq!(
        restored.execute("SELECT rowid,* FROM shadow").unwrap(),
        db.execute("SELECT rowid,* FROM shadow").unwrap()
    );
}
#[test]
fn readable_transactions_publish_only_committed_data_and_keep_exclusive_lock() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("readable.jsonl");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
    db.execute("BEGIN; INSERT INTO t VALUES(2)").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    db.execute("ROLLBACK").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    db.execute("SAVEPOINT s; INSERT INTO t VALUES(3); RELEASE s")
        .unwrap();
    drop(db);
    let mut reopened = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        reopened.execute("SELECT n FROM t ORDER BY n").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
}
#[test]
fn old_binary_and_corrupt_snapshots_are_rejected_without_overwriting_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bad.db");
    let old = b"BOXDB\0\x03obsolete";
    std::fs::write(&path, old).unwrap();
    assert!(Database::open(&path, Format::Binary).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old);
    for bytes in [old.as_slice(), b"SQLite format 3\0invalid", b"{}"] {
        assert!(Database::from_snapshot(bytes).is_err());
        let target = temp.path().join("target.db");
        assert!(Database::restore(&target, bytes).is_err());
        assert!(!target.exists());
    }
}
#[test]
fn failed_readable_publication_rolls_back_and_requires_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db.jsonl");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let saved = temp.path().join("saved.jsonl");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(db.execute("INSERT INTO t VALUES(2)").is_err());
    assert!(db.execute("SELECT n FROM t").is_err());
    drop(db);
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(saved, &path).unwrap();
    assert_eq!(
        Database::open(&path, Format::Readable)
            .unwrap()
            .execute("SELECT n FROM t")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
}
