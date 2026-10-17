use boxdb::{CheckStatus, Database, Format, Value};
use std::io::{Seek, SeekFrom, Write};

fn damage_page(path: &std::path::Path, object: &str) {
    let db = rusqlite::Connection::open(path).unwrap();
    let root: i64 = db
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name=?1",
            [object],
            |r| r.get(0),
        )
        .unwrap();
    let size: i64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    drop(db);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(((root - 1) * size) as u64))
        .unwrap();
    file.write_all(&[0xff]).unwrap();
}

#[test]
fn yaml_salvage_keeps_valid_documents_and_preserves_rejected_fragments() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.yaml");
    let output = directory.path().join("recovered.db");
    let mut db = Database::open(&source, Format::Readable).unwrap();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, doc JSON); INSERT INTO items VALUES(1,'{\"saved\":true}')").unwrap();
    drop(db);
    let mut yaml = std::fs::read_to_string(&source).unwrap();
    yaml.push_str("---\nkind: row\ntable: items\nrowid: 2\nvalues: [broken\n---\nkind: row\ntable: items\nrowid: 3\nvalues:\n  id: 3\n  doc: invalid-json\n---\nkind: row\ntable: items\nrowid: 4\nvalues:\n  id: 4\n  doc: '{\"later\":true}'\n");
    std::fs::write(&source, &yaml).unwrap();
    let report = Database::recover_file(&source, &output, Format::Binary).unwrap();
    assert_eq!(report.output.status, CheckStatus::Valid);
    assert!(report.loss_possible);
    assert_eq!(report.recovered_rows, 2);
    assert_eq!(report.rejected.len(), 2);
    let rejects = std::fs::read_to_string(report.rejected_path.unwrap()).unwrap();
    assert!(rejects.contains("values: [broken"));
    assert!(rejects.contains("invalid-json"));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), yaml);
    let mut db = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT id FROM items ORDER BY id").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(4)]]
    );
}

#[test]
fn failed_readable_recovery_does_not_publish_partial_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.yaml");
    let output = directory.path().join("recovered.yaml");
    let mut db = Database::open(&source, Format::Readable).unwrap();
    db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(id INTEGER PRIMARY KEY,parent INTEGER REFERENCES parents(id))").unwrap();
    drop(db);
    let mut yaml = std::fs::read_to_string(&source).unwrap();
    yaml.push_str("---\nkind: row\ntable: children\nrowid: 1\nvalues:\n  id: 1\n  parent: 99\n---\nkind: row\nvalues: [broken\n");
    std::fs::write(&source, &yaml).unwrap();
    assert!(Database::recover_file(&source, &output, Format::Readable).is_err());
    assert!(!output.exists());
    assert!(!directory.path().join("recovered.yaml.rejected").exists());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), yaml);
}

#[test]
fn binary_salvage_recovers_undamaged_tables_with_the_boxdb_engine() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let output = directory.path().join("recovered.db");
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE saved(id INTEGER PRIMARY KEY, amount DECIMAL, bytes BYTEA); INSERT INTO saved VALUES(7,'99.25',X'0011'); CREATE INDEX amounts ON saved(amount); CREATE TABLE broken(id INTEGER PRIMARY KEY, note TEXT); INSERT INTO broken VALUES(1,'lost')").unwrap();
    drop(db);
    damage_page(&source, "broken");
    let before = std::fs::read(&source).unwrap();
    let report = Database::recover_file(&source, &output, Format::Binary).unwrap();
    assert!(report.loss_possible);
    assert_eq!(report.output.status, CheckStatus::Valid);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let mut db = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT amount,bytes FROM saved INDEXED BY amounts WHERE amount > 99")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Decimal("99.25".into()),
            Value::Bytea(vec![0, 17])
        ]]
    );
    assert!(Database::recover_file(&source, &output, Format::Binary).is_err());
}

#[test]
fn binary_recovery_supports_generated_columns_using_bundled_extensions() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let output = directory.path().join("recovered.db");
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(note TEXT, lowered TEXT GENERATED ALWAYS AS(text_lower(note)) STORED); INSERT INTO items(note) VALUES('SAVED')").unwrap();
    drop(db);
    let report = Database::recover_file(&source, &output, Format::Binary).unwrap();
    assert_eq!(report.output.status, CheckStatus::Valid);
    let mut db = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT lowered FROM items").unwrap()[0].rows,
        vec![vec![Value::Text("saved".into())]]
    );
}

#[test]
fn binary_recovery_can_salvage_a_damaged_file_signature() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let output = directory.path().join("recovered.db");
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(7)")
        .unwrap();
    drop(db);
    let mut bytes = std::fs::read(&source).unwrap();
    bytes[0] = b'X';
    std::fs::write(&source, &bytes).unwrap();
    let report = Database::recover_file(&source, &output, Format::Binary).unwrap();
    assert_eq!(report.recovered_rows, 1);
    let mut db = Database::open(output, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT id FROM items").unwrap()[0].rows,
        vec![vec![Value::Integer(7)]]
    );
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}

#[test]
fn repair_rebuilds_corrupt_indexes_in_a_new_file_preserving_native_values_and_rowids() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let output = directory.path().join("repaired.db");
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(amount DECIMAL, note TEXT); INSERT INTO items(rowid,amount,note) VALUES(42,'12.50','saved'); CREATE INDEX amounts ON items(amount)").unwrap();
    drop(db);
    damage_page(&source, "amounts");
    let before = std::fs::read(&source).unwrap();
    let report = Database::repair_file(&source, &output).unwrap();
    assert_eq!(report.output.status, CheckStatus::Valid);
    assert_eq!(report.recovered_rows, 1);
    assert!(!report.loss_possible);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let mut db = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT rowid,amount FROM items INDEXED BY amounts WHERE amount > 12")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(42), Value::Decimal("12.5".into())]]
    );
    assert!(Database::repair_file(&source, &output).is_err());
}

#[test]
fn recovery_refuses_missing_sources_and_existing_destination_sidecars() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let output = directory.path().join("output.db");
    let error = Database::repair_file(&source, &output).unwrap_err();
    assert_eq!(error.operation(), Some(boxdb::ErrorOperation::Repair));
    assert!(!source.exists());
    assert!(!output.exists());
    let mut db = Database::open(&source, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        .unwrap();
    drop(db);
    let sidecar = directory.path().join("output.db-wal");
    std::fs::write(&sidecar, b"must stay").unwrap();
    assert!(Database::repair_file(&source, &output).is_err());
    assert!(Database::recover_file(&source, &output, Format::Binary).is_err());
    assert!(!output.exists());
    assert_eq!(std::fs::read(sidecar).unwrap(), b"must stay");
}
