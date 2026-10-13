use crate::{Error, Format, Result, Value, engine::State};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8] = b"BOXDB\0\x01";
// Accept snapshots created before the product rename; new writes use MAGIC.
const LEGACY_MAGIC: &[u8] = b"LIGHTDB\0\x01";
const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) struct Storage {
    pub path: PathBuf,
    pub format: Format,
    _lock: File,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Database {
        version: u32,
    },
    Table {
        name: String,
        next_rowid: u64,
        sql: String,
    },
    Index {
        name: String,
        sql: String,
    },
    Row {
        table: String,
        id: u64,
        values: Vec<Value>,
    },
}
fn error(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}
impl Storage {
    pub fn create(path: &Path, format: Format, state: &State) -> Result<Self> {
        if path.exists() {
            return Err(Error("destination already exists".into()));
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = fs::canonicalize(parent).map_err(error)?.join(
            path.file_name()
                .ok_or_else(|| Error("invalid destination path".into()))?,
        );
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock_path))
            .map_err(error)?;
        lock.try_lock_exclusive()
            .map_err(|_| Error("destination is locked".into()))?;
        let storage = Self {
            path,
            format,
            _lock: lock,
        };
        storage.publish_new(state)?;
        Ok(storage)
    }
    pub fn open(path: &Path, format: Format) -> Result<(Self, State)> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = if path.exists() {
            fs::canonicalize(path).map_err(error)?
        } else {
            fs::canonicalize(parent).map_err(error)?.join(
                path.file_name()
                    .ok_or_else(|| Error("invalid database path".into()))?,
            )
        };
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock_path))
            .map_err(error)?;
        lock.try_lock_exclusive()
            .map_err(|_| Error("database is locked by another connection".into()))?;
        let mut storage = Self {
            path,
            format,
            _lock: lock,
        };
        let state = if storage.path.exists() {
            if !storage.path.is_file() {
                return Err(Error("database path is not a file".into()));
            }
            let bytes = read_limited(&storage.path)?;
            let (state, detected) = decode(&bytes)?;
            storage.format = detected;
            state
        } else {
            let state = State::default();
            storage.publish_new(&state)?;
            state
        };
        state.validate()?;
        Ok((storage, state))
    }
    fn publish_new(&self, state: &State) -> Result<()> {
        self.write(state, true)
    }
    pub fn publish(&self, state: &State) -> Result<()> {
        self.write(state, false)
    }
    fn write(&self, state: &State, new: bool) -> Result<()> {
        let bytes = encode(state, self.format)?;
        let parent = self.path.parent().unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        temp.write_all(&bytes).map_err(error)?;
        temp.as_file().sync_all().map_err(error)?;
        if new {
            temp.persist_noclobber(&self.path).map_err(error)?;
        } else {
            temp.persist(&self.path).map_err(error)?;
        }
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(error)?;
        Ok(())
    }
}
pub(crate) fn read_limited(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    File::open(path)
        .map_err(error)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    Ok(bytes)
}
pub(crate) fn encode(state: &State, format: Format) -> Result<Vec<u8>> {
    state.validate()?;
    let mut records = vec![Record::Database { version: 1 }];
    for (name, table) in &state.tables {
        records.push(Record::Table {
            name: name.clone(),
            next_rowid: table.next_rowid,
            sql: table.sql.clone(),
        });
        for (id, row) in table.row_ids.iter().zip(&table.rows) {
            records.push(Record::Row {
                table: name.clone(),
                id: *id,
                values: row.clone(),
            });
        }
    }
    for (name, index) in &state.indexes {
        records.push(Record::Index {
            name: name.clone(),
            sql: index.sql.clone(),
        });
    }
    let mut text = vec![];
    for record in records {
        serde_json::to_writer(&mut text, &record).map_err(error)?;
        text.push(b'\n');
    }
    if text.len() as u64 > MAX_BYTES {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    match format {
        Format::Readable => Ok(text),
        Format::Binary => {
            let mut bytes = MAGIC.to_vec();
            bytes.extend(Sha256::digest(&text));
            bytes.extend(zstd::stream::encode_all(text.as_slice(), 3).map_err(error)?);
            Ok(bytes)
        }
    }
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(State, Format)> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    let header = [MAGIC, LEGACY_MAGIC]
        .into_iter()
        .find(|magic| bytes.starts_with(magic));
    let (text, format) = if let Some(magic) = header {
        if bytes.len() < magic.len() + 32 {
            return Err(Error("truncated binary header".into()));
        }
        let mut text = vec![];
        zstd::stream::read::Decoder::new(&bytes[magic.len() + 32..])
            .map_err(error)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut text)
            .map_err(error)?;
        if text.len() as u64 > MAX_BYTES {
            return Err(Error("decompressed database exceeds limit".into()));
        }
        if Sha256::digest(&text).as_slice() != &bytes[magic.len()..magic.len() + 32] {
            return Err(Error("database checksum mismatch".into()));
        }
        (text, Format::Binary)
    } else {
        (bytes.to_vec(), Format::Readable)
    };
    let text = std::str::from_utf8(&text).map_err(error)?;
    let mut records = vec![];
    for (line_number, line) in text.lines().enumerate() {
        let record: Record = serde_json::from_str(line)
            .map_err(|e| Error(format!("line {}: {e}", line_number + 1)))?;
        records.push(record);
    }
    if !matches!(records.first(), Some(Record::Database { version: 1 }))
        || records
            .iter()
            .skip(1)
            .any(|r| matches!(r, Record::Database { .. }))
    {
        return Err(Error("invalid database header or format version".into()));
    }
    let mut state = State::default();
    // Load all declarations before rows: a child can sort before its parent.
    for record in &records {
        if let Record::Table {
            name,
            sql,
            next_rowid,
        } = record
        {
            let statements = crate::parse(sql)?;
            let [sqlparser::ast::Statement::CreateTable(table)] = statements.as_slice() else {
                return Err(Error("table record must contain one CREATE TABLE".into()));
            };
            if crate::engine::name(&table.name) != *name || state.tables.contains_key(name) {
                return Err(Error("mismatched or duplicate table record".into()));
            }
            state.declare(&statements[0])?;
            state.tables.get_mut(name).unwrap().next_rowid = *next_rowid;
        }
    }
    for record in &records {
        if let Record::Index { name, sql } = record {
            let statements = crate::parse(sql)?;
            let [sqlparser::ast::Statement::CreateIndex(index)] = statements.as_slice() else {
                return Err(Error("index record must contain one CREATE INDEX".into()));
            };
            if index.name.as_ref().map(crate::engine::name).as_ref() != Some(name)
                || state.indexes.contains_key(name)
            {
                return Err(Error("mismatched or duplicate index record".into()));
            }
            state.declare(&statements[0])?;
        }
    }
    for record in records {
        if let Record::Row { table, id, values } = record {
            let table = state
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error("row references unknown table".into()))?;
            if id == 0
                || id >= table.next_rowid
                || table.row_ids.last().is_some_and(|previous| *previous >= id)
            {
                return Err(Error("invalid or duplicate row id".into()));
            }
            table.row_ids.push(id);
            if values.len() != table.columns.len() {
                return Err(Error("column count mismatch".into()));
            }
            let values = table
                .columns
                .iter()
                .zip(values)
                .map(|(column, value)| column.ty.validate(&value))
                .collect::<Result<Vec<_>>>()?;
            table.rows.push(values);
        }
    }
    state.validate()?;
    state.rebuild_indexes()?;
    Ok((state, format))
}
