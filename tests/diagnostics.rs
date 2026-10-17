use boxdb::{CheckMode, CheckStatus, Database, Format};

#[test]
fn checking_a_database_reads_committed_wal_without_changing_its_contents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    writer.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, amount DECIMAL); INSERT INTO items VALUES(1, '12.50')").unwrap();
    let before = std::fs::read(&path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap();
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Valid);
    assert!(report.complete);
    assert!(report.issues.is_empty());
    assert!(report.checks.iter().any(|check| check.name == "values"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(path.with_extension("db-wal")).unwrap(), wal);
}

#[test]
fn a_locked_database_is_incomplete_instead_of_reported_corrupt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("locked.db");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TABLE items(id INTEGER); BEGIN EXCLUSIVE")
        .unwrap();
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Incomplete);
    assert!(!report.complete);
    assert!(report.issues.iter().all(|issue| issue.code == "BOXDB_BUSY"));
}

#[test]
fn diagnostics_inspects_metadata_that_normal_open_rejects_without_rewriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE items(doc JSON)").unwrap();
    drop(db);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute("UPDATE __boxdb_columns SET table_sql='broken'", [])
        .unwrap();
    drop(raw);
    assert!(Database::open(&path, Format::Binary).is_err());
    let before = std::fs::read(&path).unwrap();
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Invalid);
    assert!(!report.complete);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.check == "metadata" && issue.code == "BOXDB_METADATA")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn diagnostics_report_yaml_document_locations_and_reject_missing_or_empty_sources() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.yaml");
    assert!(Database::check_file(&path, CheckMode::Full).is_err());
    assert!(!path.exists());
    std::fs::write(&path, b"").unwrap();
    assert_eq!(
        Database::check_file(&path, CheckMode::Full).unwrap().status,
        CheckStatus::Invalid
    );
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        .unwrap();
    drop(db);
    let mut yaml = std::fs::read_to_string(&path).unwrap();
    let document = yaml.lines().filter(|line| *line == "---").count() + 1;
    let line = yaml.lines().count() + 1;
    yaml.push_str("---\nkind: row\ntable: items\nvalues: [broken\n");
    std::fs::write(&path, &yaml).unwrap();
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Invalid);
    assert_eq!(report.issues[0].document, Some(document));
    assert!(report.issues[0].line.unwrap() >= line);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), yaml);
}

#[test]
fn readable_value_errors_identify_the_rejected_document() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE items(doc JSON); INSERT INTO items VALUES('{\"ok\":true}')")
        .unwrap();
    drop(db);
    let yaml = std::fs::read_to_string(&path)
        .unwrap()
        .replace("true", "broken-json");
    assert!(yaml.contains("broken-json"));
    std::fs::write(&path, &yaml).unwrap();
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Invalid);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "BOXDB_INVALID_VALUE"
                && issue.document.is_some()
                && issue.line.is_some())
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), yaml);
}

#[test]
fn full_check_locates_invalid_values_and_foreign_keys_without_echoing_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE items(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parents(id), doc JSON)").unwrap();
    drop(db);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO items VALUES(1,99,'secret broken json'); INSERT INTO items VALUES(2,NULL,'also broken')").unwrap();
    drop(raw);
    let quick = Database::check_file(&path, CheckMode::Quick).unwrap();
    assert_eq!(quick.status, CheckStatus::Valid);
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert_eq!(report.status, CheckStatus::Invalid);
    assert!(report.complete);
    let invalid: Vec<_> = report
        .issues
        .iter()
        .filter(|issue| issue.check == "values")
        .collect();
    assert_eq!(invalid.len(), 2);
    assert_eq!(invalid[0].table.as_deref(), Some("items"));
    assert_eq!(invalid[0].column.as_deref(), Some("doc"));
    assert_eq!(invalid[0].rowid, Some(1));
    assert_eq!(serde_json::to_value(invalid[0]).unwrap()["rowid"], "1");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.check == "foreign_keys")
    );
    assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
}
