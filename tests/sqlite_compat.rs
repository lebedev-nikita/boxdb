use boxdb::{Database, Format, Value};
use rusqlite::Connection;
mod common;
use common::sqlite_execute;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Case {
    id: String,
    file: String,
    line: usize,
    setup: String,
    sql: String,
    expected: Vec<String>,
    affected: Option<usize>,
    error: Option<String>,
}
fn suite(group: &str) {
    for format in [None, Some(Format::Binary), Some(Format::Readable)] {
        suite_mode(group, format);
    }
}
fn suite_mode(group: &str, format: Option<Format>) {
    let suites: BTreeMap<String, Vec<Case>> =
        serde_json::from_str(include_str!("sqlite/cases.json")).unwrap();
    for case in &suites[group] {
        let label = format!("{} ({}:{}, {format:?})", case.id, case.file, case.line);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("database");
        let mut db = match format {
            None => Database::memory(),
            Some(format) => Database::open(&path, format).unwrap(),
        };
        db.execute(&common::integer_sql(&case.setup, true))
            .unwrap_or_else(|e| panic!("{label} setup: {e}"));
        let reference = Connection::open_in_memory().unwrap();
        reference.pragma_update(None, "foreign_keys", true).unwrap();
        sqlite_execute(&reference, &case.setup)
            .unwrap_or_else(|e| panic!("{label} SQLite setup: {e}"));
        for (client, results) in [
            ("BoxDB", db.execute(&common::integer_sql(&case.sql, true))),
            ("SQLite", sqlite_execute(&reference, &case.sql)),
        ] {
            let label = format!("{client} {label}");

            if let Some(expected) = &case.error {
                assert!(
                    results.unwrap_err().to_string().contains(expected),
                    "{label}: expected {expected}"
                );
                continue;
            }
            let results = results.unwrap_or_else(|e| panic!("{label}: {e}"));
            if let Some(expected) = case.affected {
                assert_eq!(
                    results.iter().map(|r| r.affected).sum::<usize>(),
                    expected,
                    "{label}"
                );
                continue;
            }
            let values = results
                .into_iter()
                .flat_map(|r| r.rows)
                .flatten()
                .collect::<Vec<_>>();
            assert_eq!(values.len(), case.expected.len(), "{label}: {values:?}");
            for (value, expected) in values.iter().zip(&case.expected) {
                let actual = match value {
                    Value::Null => String::new(),
                    Value::Boolean(b) => if *b { "1" } else { "0" }.into(),
                    _ => value.to_string(),
                };
                if &actual == expected {
                    continue;
                }
                if let Value::Real(n) = value
                    && let Ok(expected) = expected.parse::<f64>()
                {
                    assert!(
                        (n - expected).abs() <= 1e-12 * expected.abs().max(1.0),
                        "{label}: {actual} != {expected}"
                    );
                    continue;
                }
                if matches!(value, Value::Integer(_) | Value::Decimal(_))
                    && let (Ok(a), Ok(b)) = (
                        actual.parse::<bigdecimal::BigDecimal>(),
                        expected.parse::<bigdecimal::BigDecimal>(),
                    )
                {
                    assert_eq!(a, b, "{label}");
                    continue;
                }
                assert_eq!(&actual, expected, "{label}");
            }
        }
        if let Some(format) = format {
            // Closing a connection rolls back pending user transactions. Make
            // that state explicit in both clients before checking persistence.
            if db.in_transaction() {
                db.execute("ROLLBACK").unwrap();
            }
            if !reference.is_autocommit() {
                sqlite_execute(&reference, "ROLLBACK").unwrap();
            }
            drop(db);
            let mut reopened =
                Database::open(&path, format).unwrap_or_else(|e| panic!("{label} reopen: {e}"));
            let names = sqlite_execute(&reference, "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
            for row in &names[0].rows {
                let Value::Text(name) = &row[0] else {
                    panic!("table name")
                };
                let quoted = format!("\"{}\"", name.replace('"', "\"\""));
                let info =
                    sqlite_execute(&reference, &format!("PRAGMA table_info({quoted})")).unwrap();
                // Text projections normalize BoxDB decimal declarations to the
                // native numeric reference while retaining NULL and row shape.
                let columns = info[0]
                    .rows
                    .iter()
                    .map(|r| {
                        let Value::Text(name) = &r[1] else {
                            panic!("column name")
                        };
                        let name = name.replace('"', "\"\"");
                        format!("CAST(\"{name}\" AS TEXT) AS \"{name}\"")
                    })
                    .collect::<Vec<_>>();
                let order = (1..=columns.len())
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT {} FROM {quoted} ORDER BY {order}",
                    columns.join(",")
                );
                assert_eq!(
                    reopened.execute(&sql).unwrap(),
                    sqlite_execute(&reference, &sql).unwrap(),
                    "{label} persisted {name}"
                );
            }
        }
    }
}
#[test]
fn sqlite_null() {
    suite("null");
}
#[test]
fn sqlite_select() {
    suite("select");
}
#[test]
fn sqlite_expressions() {
    suite("expr");
}
#[test]
fn sqlite_update() {
    suite("update");
}
#[test]
fn sqlite_insert() {
    suite("insert");
}
#[test]
fn sqlite_transactions() {
    suite("transaction");
}
#[test]
fn sqlite_delete() {
    suite("delete");
}
#[test]
fn sqlite_index() {
    suite("index");
}
#[test]
fn sqlite_unique() {
    suite("unique");
}
#[test]
fn sqlite_check() {
    suite("check");
}
#[test]
fn sqlite_insert_select() {
    suite("insert_select");
}
#[test]
fn sqlite_foreign_keys() {
    suite("foreign_key");
}

#[test]
fn upstream_sources_and_cases_match_provenance() {
    use sha2::{Digest, Sha256};
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("sqlite/provenance.json")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlite/upstream");
    for (name, source) in manifest["files"].as_object().unwrap() {
        let bytes = std::fs::read(root.join(name)).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            source["sha256"].as_str().unwrap(),
            "upstream file changed: {name}"
        );
    }
    let groups: BTreeMap<String, Vec<Case>> =
        serde_json::from_str(include_str!("sqlite/cases.json")).unwrap();
    for (group, cases) in groups {
        assert_eq!(
            cases.len() as u64,
            manifest["adapted_cases"][&group].as_u64().unwrap()
        );
        for case in cases {
            let source = std::fs::read_to_string(root.join(&case.file)).unwrap();
            assert!(
                source
                    .lines()
                    .nth(case.line - 1)
                    .unwrap()
                    .contains(&case.id),
                "{} source location changed",
                case.id
            );
        }
    }
}
