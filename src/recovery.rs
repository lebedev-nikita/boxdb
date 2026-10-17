//! Explicit salvage and index rebuilding. Sources are never opened as writers.
use crate::{
    CheckMode, CheckReport, CheckStatus, Database, Error, Format, Result, diagnostics, extensions,
    schema, storage,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RejectedDocument {
    pub document: usize,
    pub line: usize,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub output: CheckReport,
    pub recovered_rows: u64,
    /// Salvage cannot prove that all original content survived.
    pub loss_possible: bool,
    pub rejected: Vec<RejectedDocument>,
    pub rejected_path: Option<PathBuf>,
    pub warnings: Vec<String>,
}

pub(crate) fn temporary(destination: &Path) -> Result<tempfile::NamedTempFile> {
    if destination.try_exists()? {
        return Err(Error::new("destination must be new".into()));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = destination.as_os_str().to_os_string();
        name.push(suffix);
        if Path::new(&name).try_exists()? {
            return Err(Error::new(
                "destination has existing SQLite sidecars".into(),
            ));
        }
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}

pub(crate) fn publish(temp: tempfile::NamedTempFile, destination: &Path) -> Result<()> {
    temp.as_file().sync_all()?;
    temp.persist_noclobber(destination)
        .map_err(|error| Error::from(error.error))?;
    std::fs::File::open(
        destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

impl Database {
    /// Salvage readable data into a new file. Success does not prove completeness.
    pub fn recover_file(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        format: Format,
    ) -> Result<RecoveryReport> {
        Self::recover_inner(source.as_ref(), destination.as_ref(), format).map_err(|error| {
            error.state(
                crate::ErrorOperation::Recover,
                crate::OperationOutcome::Unknown,
                false,
                false,
            )
        })
    }
    fn recover_inner(source: &Path, destination: &Path, format: Format) -> Result<RecoveryReport> {
        let temp = temporary(destination)?;
        let mut header = [0; 16];
        use std::io::Read;
        let mut source_file = std::fs::File::open(source)?;
        let count = source_file.read(&mut header)?;
        if count == 0 {
            return Err(
                Error::new("existing database file is empty".into()).with_code("BOXDB_FORMAT")
            );
        }
        let mut rejected = vec![];
        let binary = count == 16
            && (header == storage::SQLITE_HEADER
                || header.contains(&0)
                || std::str::from_utf8(&header).is_err());
        if binary {
            let input =
                Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            extensions::register(&input)?;
            input.busy_timeout(std::time::Duration::from_secs(5))?;
            recover_binary(&input, temp.path())?;
        } else {
            let mut bytes = header[..count].to_vec();
            source_file
                .take((storage::MAX_SNAPSHOT + 1 - count) as u64)
                .read_to_end(&mut bytes)?;
            let (input, rejects) = crate::readable::salvage(&bytes)?;
            rejected = rejects;
            storage::copy(&input, &mut Connection::open(temp.path())?)?;
        }
        let output = Connection::open(temp.path())?;
        extensions::register(&output)?;
        let mut report = CheckReport::new(CheckMode::Full, Some(Format::Binary));
        diagnostics::check_connection(&output, &mut report);
        let recovered_rows = row_count(&output)?;
        drop(output);
        let temp = if format == Format::Readable {
            if report.status != CheckStatus::Valid || !report.complete {
                return Err(Error::new(
                    "invalid recovered database cannot be exported as readable storage".into(),
                ));
            }
            let yaml = temporary(destination)?;
            let directory = tempfile::tempdir()?;
            let converted = directory.path().join("recovered.yaml");
            Database::convert(temp.path(), &converted, Format::Readable)?;
            std::fs::copy(converted, yaml.path())?;
            report.format = Some(Format::Readable);
            yaml
        } else {
            temp
        };
        let rejected_path = if rejected.is_empty() {
            None
        } else {
            let mut name = destination.as_os_str().to_os_string();
            name.push(".rejected");
            let path = PathBuf::from(name);
            let mut fragments = temporary(&path)?;
            use std::io::Write;
            for (document, bytes) in &rejected {
                writeln!(
                    fragments,
                    "# Rejected document {}, source line {}: {}",
                    document.document, document.line, document.code
                )?;
                fragments.write_all(bytes)?;
                fragments.write_all(b"\n")?;
            }
            publish(fragments, &path)?;
            Some(path)
        };
        publish(temp, destination)?;
        let mut warnings = vec![
            "Recovery may omit or alter rows and schema objects; total loss cannot be determined."
                .into(),
        ];
        if binary {
            warnings.push(
                "Freelist pages were excluded; unassigned records may be in lost_and_found.".into(),
            );
        }
        if recovered_rows == 0 {
            warnings.push("No application rows were recovered.".into());
        }
        Ok(RecoveryReport {
            output: report,
            recovered_rows,
            loss_possible: true,
            rejected: rejected.into_iter().map(|(document, _)| document).collect(),
            rejected_path,
            warnings,
        })
    }
    /// Rebuild indexes from readable table data into a new binary file.
    pub fn repair_file(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<RecoveryReport> {
        Self::repair_inner(source.as_ref(), destination.as_ref()).map_err(|error| {
            error.state(
                crate::ErrorOperation::Repair,
                crate::OperationOutcome::Unknown,
                false,
                false,
            )
        })
    }
    fn repair_inner(source: &Path, destination: &Path) -> Result<RecoveryReport> {
        let temp = temporary(destination)?;
        let mut source = storage::reader(source)?;
        extensions::register(&source)?;
        let source = source.transaction()?;
        schema::load(&source)?;
        let mut output = Connection::open(temp.path())?;
        extensions::register(&output)?;
        output.pragma_update(None, "foreign_keys", false)?;
        let transaction = output.transaction()?;
        let objects: Vec<(String, String, String)> = source.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut recovered_rows = 0;
        for (kind, table, sql) in &objects {
            if kind != "table" {
                continue;
            }
            if sql
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE VIRTUAL")
            {
                return Err(
                    Error::new("index repair does not support virtual tables".into())
                        .with_code("BOXDB_UNSUPPORTED"),
                );
            }
            transaction.execute_batch(sql)?;
            let columns: Vec<String> = source
                .prepare("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden=0 ORDER BY cid")?
                .query_map([table], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let alias = crate::readable::rowid(&source, table)?;
            let names = alias
                .into_iter()
                .chain(columns.iter().map(String::as_str))
                .map(schema::quote)
                .collect::<Vec<_>>()
                .join(",");
            let mut select = source.prepare(&format!(
                "SELECT {names} FROM {} NOT INDEXED",
                schema::quote(table)
            ))?;
            let mut rows = select.query([])?;
            let sql = format!(
                "INSERT INTO {}({names}) VALUES({})",
                schema::quote(table),
                vec!["?"; columns.len() + usize::from(alias.is_some())].join(",")
            );
            let mut insert = transaction.prepare(&sql)?;
            while let Some(row) = rows.next()? {
                let values = (0..columns.len() + usize::from(alias.is_some()))
                    .map(|i| {
                        row.get::<_, SqlValue>(i)
                            .map(crate::decimal::WireValue::new)
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                insert.execute(rusqlite::params_from_iter(values))?;
                if !table.starts_with("__boxdb_") {
                    recovered_rows += 1;
                }
            }
        }
        if source.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='sqlite_sequence')",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            transaction.execute_batch("DELETE FROM sqlite_sequence")?;
            let mut statement = source.prepare("SELECT name,seq FROM sqlite_sequence")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                transaction.execute(
                    "INSERT INTO sqlite_sequence VALUES(?1,?2)",
                    rusqlite::params![row.get::<_, String>(0)?, row.get::<_, i64>(1)?],
                )?;
            }
        }
        for (kind, _, sql) in &objects {
            if kind != "table" {
                transaction.execute_batch(sql)?;
            }
        }
        for pragma in ["application_id", "user_version"] {
            let value: i64 = source.pragma_query_value(None, pragma, |row| row.get(0))?;
            transaction.pragma_update(None, pragma, value)?;
        }
        transaction.commit()?;
        let mut report = CheckReport::new(CheckMode::Full, Some(crate::Format::Binary));
        diagnostics::check_connection(&output, &mut report);
        if report.status != CheckStatus::Valid || !report.complete {
            return Err(
                Error::new("rebuilt database failed validation; source retained".into())
                    .with_code("BOXDB_REPAIR_FAILED"),
            );
        }
        drop(output);
        publish(temp, destination)?;
        Ok(RecoveryReport {
            output: report,
            recovered_rows,
            loss_possible: false,
            rejected: vec![],
            rejected_path: None,
            warnings: vec![],
        })
    }
}

fn row_count(db: &Connection) -> Result<u64> {
    let tables = db.prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%' AND name NOT GLOB '__boxdb_*'")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut count = 0;
    for table in tables {
        count += db.query_row(
            &format!("SELECT count(*) FROM {} NOT INDEXED", schema::quote(&table)),
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
    }
    Ok(count)
}

unsafe extern "C" {
    fn sqlite3_recover_init(
        db: *mut rusqlite::ffi::sqlite3,
        schema: *const std::ffi::c_char,
        output: *const std::ffi::c_char,
    ) -> *mut std::ffi::c_void;
    fn sqlite3_recover_config(
        handle: *mut std::ffi::c_void,
        option: i32,
        value: *mut std::ffi::c_void,
    ) -> i32;
    fn sqlite3_recover_run(handle: *mut std::ffi::c_void) -> i32;
    fn sqlite3_recover_errmsg(handle: *mut std::ffi::c_void) -> *const std::ffi::c_char;
    fn sqlite3_recover_finish(handle: *mut std::ffi::c_void) -> i32;
}

// SAFETY: SQLite recovery calls this with its live output handle before any
// transaction. The borrowed rusqlite wrapper does not close the C-owned handle.
#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_recovery_configure(handle: *mut rusqlite::ffi::sqlite3) -> i32 {
    std::panic::catch_unwind(|| {
        let result = (|| -> Result<()> {
            let db = unsafe { Connection::from_handle(handle) }?;
            extensions::register(&db)?;
            db.pragma_update(None, "foreign_keys", false)?;
            Ok(())
        })();
        result.map_or_else(
            |error| error.extended_code().unwrap_or(rusqlite::ffi::SQLITE_ERROR),
            |()| rusqlite::ffi::SQLITE_OK,
        )
    })
    .unwrap_or(rusqlite::ffi::SQLITE_ERROR)
}

fn recover_binary(db: &Connection, destination: &Path) -> Result<()> {
    use std::ffi::{CStr, CString};
    let destination = CString::new(
        destination
            .to_str()
            .ok_or_else(|| Error::new("recovery destination must be UTF-8".into()))?,
    )
    .map_err(Error::validation)?;
    // SAFETY: the live connection and NUL-terminated strings outlive the recovery handle.
    // finish is called exactly once, after copying its error message.
    unsafe {
        let handle = sqlite3_recover_init(db.handle(), c"main".as_ptr(), destination.as_ptr());
        if handle.is_null() {
            return Err(Error::sqlite(
                rusqlite::ffi::SQLITE_NOMEM,
                "cannot allocate recovery handle".into(),
            ));
        }
        let code = sqlite3_recover_config(handle, 1, c"lost_and_found".as_ptr().cast_mut().cast());
        let code = if code == rusqlite::ffi::SQLITE_OK {
            sqlite3_recover_run(handle)
        } else {
            code
        };
        let message = sqlite3_recover_errmsg(handle);
        let message = if message.is_null() {
            "database recovery failed".into()
        } else {
            CStr::from_ptr(message).to_string_lossy().into_owned()
        };
        let finish = sqlite3_recover_finish(handle);
        if code != rusqlite::ffi::SQLITE_OK {
            return Err(Error::sqlite(code, message).with_context("recover database"));
        }
        if finish != rusqlite::ffi::SQLITE_OK {
            return Err(Error::sqlite(finish, "finish database recovery".into()));
        }
    }
    Ok(())
}
