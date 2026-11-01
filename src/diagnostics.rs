//! Read-only checks independent of normal connection initialization.
use crate::{Database, Error, Format, Result, extensions, schema, storage};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMode {
    Quick,
    #[default]
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Valid,
    Invalid,
    Incomplete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckIssue {
    pub check: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        with = "rowid_string"
    )]
    pub rowid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

mod rowid_string {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(
        value: &Option<i64>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        value.map(|n| n.to_string()).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<i64>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| text.parse().map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub status: CheckStatus,
    pub complete: bool,
    pub format: Option<Format>,
    pub mode: CheckMode,
    pub checks: Vec<Check>,
    pub issues: Vec<CheckIssue>,
}

impl CheckReport {
    pub(crate) fn new(mode: CheckMode, format: Option<Format>) -> Self {
        Self {
            status: CheckStatus::Valid,
            complete: true,
            format,
            mode,
            checks: vec![],
            issues: vec![],
        }
    }

    pub(crate) fn issue(&mut self, check: &str, error: &Error) {
        self.status = CheckStatus::Invalid;
        self.issues.push(CheckIssue {
            check: check.into(),
            code: error.code().into(),
            message: error.to_string(),
            table: None,
            column: None,
            rowid: None,
            document: None,
            line: None,
        });
    }

    fn step(&mut self, name: &str, run: impl FnOnce(&mut Self) -> Result<()>) {
        let before = self.issues.len();
        let previous = self.status;
        let status = match run(self) {
            Ok(()) if self.issues.len() == before => CheckStatus::Valid,
            Ok(()) => CheckStatus::Invalid,
            Err(error) => {
                self.complete = false;
                self.issue(name, &error);
                if matches!(
                    error.result_code(),
                    Some(1 | 5 | 6 | 7 | 8 | 9 | 10 | 13 | 14)
                ) {
                    self.status = if previous == CheckStatus::Invalid {
                        previous
                    } else {
                        CheckStatus::Incomplete
                    };
                    CheckStatus::Incomplete
                } else {
                    CheckStatus::Invalid
                }
            }
        };
        self.checks.push(Check {
            name: name.into(),
            status,
        });
    }
}

impl Database {
    /// Inspect an existing file without creating it or configuring a writer.
    pub fn check_file(path: impl AsRef<Path>, mode: CheckMode) -> Result<CheckReport> {
        check_file(path.as_ref(), mode).map_err(|error| {
            error.state(
                crate::ErrorOperation::Check,
                crate::OperationOutcome::NotApplied,
                false,
                false,
            )
        })
    }
}

fn check_file(path: &Path, mode: CheckMode) -> Result<CheckReport> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0; 16];
    let count = file.read(&mut header)?;
    let binary = count == 16 && header == storage::SQLITE_HEADER;
    let mut report = CheckReport::new(mode, binary.then_some(Format::Binary));
    if count == 0 {
        report.issue(
            "format",
            &Error::new("existing database file is empty".into()).with_code("BOXDB_FORMAT"),
        );
        report.complete = false;
        return Ok(report);
    }
    let connection = if binary {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(Error::from)
    } else {
        let mut bytes = header[..count].to_vec();
        file.take((storage::MAX_SNAPSHOT + 1 - count) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > storage::MAX_SNAPSHOT {
            return Err(Error::new("readable database exceeds 256 MiB limit".into()));
        }
        crate::readable::check_documents(&bytes, &mut report);
        if !report.issues.is_empty() {
            report.complete = false;
            return Ok(report);
        }
        report.format = Some(Format::Readable);
        match storage::import(&bytes) {
            Ok(db) => Ok(db),
            Err(error) => {
                report.complete = false;
                if let Ok((db, rejected)) = crate::readable::salvage(&bytes) {
                    for (document, _) in rejected {
                        report.issue(
                            "yaml",
                            &Error::new(document.message).with_code("BOXDB_INVALID_VALUE"),
                        );
                        let issue = report.issues.last_mut().unwrap();
                        issue.code = document.code;
                        issue.document = Some(document.document);
                        issue.line = Some(document.line);
                        if report.issues.len() >= 1000 {
                            break;
                        }
                    }
                    if report.issues.is_empty() {
                        report.issue("yaml", &error);
                    }
                    Ok(db)
                } else {
                    Err(error)
                }
            }
        }
    };
    match connection {
        Ok(mut db) => {
            extensions::register(&db)?;
            db.busy_timeout(std::time::Duration::from_secs(5))?;
            let transaction = db.transaction()?;
            check_connection(&transaction, &mut report);
        }
        Err(error) => {
            report.issue("format", &error);
            report.complete = false;
        }
    }
    Ok(report)
}

pub(crate) fn check_connection(db: &Connection, report: &mut CheckReport) {
    let pragma = if report.mode == CheckMode::Full {
        "integrity_check"
    } else {
        "quick_check"
    };
    report.step("structure", |report| {
        let mut statement = db.prepare(&format!("PRAGMA {pragma}"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let message: String = row.get(0)?;
            if message != "ok" {
                report.issue("structure", &Error::new(message).with_code("BOXDB_CORRUPT"));
            }
        }
        Ok(())
    });
    if report.status == CheckStatus::Incomplete {
        return;
    }
    let mut types = None;
    report.step("metadata", |_| {
        types = Some(schema::load(db)?);
        Ok(())
    });
    if report.mode == CheckMode::Full {
        report.step("foreign_keys", |report| {
            let mut statement = db.prepare("PRAGMA foreign_key_check")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                report.issue(
                    "foreign_keys",
                    &Error::new("foreign key references a missing row".into())
                        .with_code("BOXDB_CONSTRAINT_FOREIGNKEY"),
                );
                let issue = report.issues.last_mut().unwrap();
                issue.table = Some(row.get(0)?);
                issue.rowid = row.get(1)?;
                if report.issues.len() >= 1000 {
                    report.complete = false;
                    break;
                }
            }
            Ok(())
        });
        if let Some(types) = types
            && report.issues.len() < 1000
        {
            report.step("values", |report| validate_values(db, &types, report));
        } else {
            report.complete = false;
            report.checks.push(Check {
                name: "values".into(),
                status: CheckStatus::Incomplete,
            });
        }
    }
}

fn validate_values(db: &Connection, types: &schema::Types, report: &mut CheckReport) -> Result<()> {
    use rusqlite::types::Value as SqlValue;
    let tables = db.prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%' AND name NOT GLOB '__boxdb_*' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for table in tables {
        let alias = crate::readable::rowid(db, &table)?;
        let prefix = alias.map_or(String::new(), |name| format!("{},", schema::quote(name)));
        let mut statement = db.prepare(&format!(
            "SELECT {prefix}* FROM {} NOT INDEXED",
            schema::quote(&table)
        ))?;
        let columns: Vec<_> = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for (index, column) in columns
                .iter()
                .enumerate()
                .skip(usize::from(alias.is_some()))
            {
                let value: SqlValue = row.get(index)?;
                let valid_blob = match &value {
                    SqlValue::Blob(bytes) => {
                        bytes.first() == Some(&libsqlite3_sys::wire::BYTEA_TAG)
                            || libsqlite3_sys::native::decode(bytes).is_some()
                            || crate::decimal::decode(bytes)
                                .is_some_and(|text| crate::decimal::parse(text).is_ok())
                    }
                    _ => true,
                };
                let valid = valid_blob
                    && types
                        .get(&(table.clone(), column.clone()))
                        .is_none_or(|ty| schema::decode(ty, value).is_ok());
                if !valid {
                    report.issue(
                        "values",
                        &Error::new(
                            "stored value does not match its BoxDB type or BLOB codec".into(),
                        )
                        .with_code("BOXDB_INVALID_VALUE"),
                    );
                    let issue = report.issues.last_mut().unwrap();
                    issue.table = Some(table.clone());
                    issue.column = Some(column.clone());
                    if alias.is_some() {
                        issue.rowid = row.get(0)?;
                    }
                    if report.issues.len() >= 1000 {
                        report.complete = false;
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}
