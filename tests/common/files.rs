//! File fixtures exercise ordinary SQL and the public conversion command.
use boxdb::{Database, Format, Result, Value};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
};

pub struct DatabaseFile {
    database: Database,
    _directory: Option<tempfile::TempDir>,
}

impl Deref for DatabaseFile {
    type Target = Database;
    fn deref(&self) -> &Self::Target {
        &self.database
    }
}

impl DerefMut for DatabaseFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.database
    }
}

pub fn open(path: &Path, format: Format) -> Result<DatabaseFile> {
    Ok(DatabaseFile {
        database: Database::open(path, format)?,
        _directory: None,
    })
}

pub fn convert(source: &Path, destination: &Path, format: Format) -> Result<()> {
    Database::convert(source, destination, format)
}

pub fn converted_bytes(db: &mut Database, format: Format) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source.db");
    let destination = directory.path().join("converted");
    db.execute_params(
        "VACUUM main INTO ?",
        &[Value::Text(source.to_str().unwrap().into())],
    )?;
    convert(&source, &destination, format)?;
    Ok(std::fs::read(destination)?)
}

pub fn open_bytes(bytes: &[u8]) -> Result<DatabaseFile> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("converted");
    let database = write_converted(&destination, bytes)?;
    Ok(DatabaseFile {
        database,
        _directory: Some(directory),
    })
}

pub fn write_converted(destination: &Path, bytes: &[u8]) -> Result<Database> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source");
    std::fs::write(&source, bytes)?;
    let format = if bytes.starts_with(b"SQLite format 3\0") {
        Format::Binary
    } else {
        Format::Readable
    };
    convert(&source, destination, format)?;
    Database::open(destination, format)
}

pub fn converted_file(source: &Path, format: Format) -> Result<DatabaseFile> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("converted");
    convert(source, &destination, format)?;
    Ok(DatabaseFile {
        database: Database::open(&destination, format)?,
        _directory: Some(directory),
    })
}
