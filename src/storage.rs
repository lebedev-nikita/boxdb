use crate::{Error, Format, Result, engine::State};
use fs2::FileExt;
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

mod binary;
mod readable;

const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) struct Storage {
    pub path: PathBuf,
    pub format: Format,
    _lock: File,
    snapshot_lock: File,
    journal: Cell<Journal>,
}
#[derive(Clone, Copy, Default)]
struct Journal {
    length: u64,
    base_length: u64,
    commits: u32,
}
struct SnapshotGuard<'a>(&'a File);
impl<'a> SnapshotGuard<'a> {
    fn exclusive(file: &'a File) -> Result<Self> {
        fs2::FileExt::lock_exclusive(file).map_err(error)?;
        Ok(Self(file))
    }
    fn shared(file: &'a File) -> Result<Self> {
        fs2::FileExt::lock_shared(file).map_err(error)?;
        Ok(Self(file))
    }
}
impl Drop for SnapshotGuard<'_> {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(self.0);
    }
}
fn snapshot_lock(path: &Path) -> Result<File> {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".snapshot.lock");
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(lock_path))
        .map_err(error)
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
            snapshot_lock: snapshot_lock(&path)?,
            path,
            format,
            _lock: lock,
            journal: Cell::default(),
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
            snapshot_lock: snapshot_lock(&path)?,
            path,
            format,
            _lock: lock,
            journal: Cell::default(),
        };
        let state = if storage.path.exists() {
            let _guard = SnapshotGuard::exclusive(&storage.snapshot_lock)?;
            if !storage.path.is_file() {
                return Err(Error("database path is not a file".into()));
            }
            let bytes = read_limited(&storage.path)?;
            let (state, detected, journal) = decode_with_length(&bytes, true)?;
            if detected == Format::Binary {
                let valid_length = journal.length as usize;
                if valid_length < bytes.len() {
                    let file = OpenOptions::new()
                        .write(true)
                        .open(&storage.path)
                        .map_err(error)?;
                    file.set_len(valid_length as u64).map_err(error)?;
                    file.sync_all().map_err(error)?;
                }
                storage.journal.set(journal);
            }
            storage.format = detected;
            state
        } else {
            let state = State::default();
            storage.publish_new(&state)?;
            state
        };
        Ok((storage, state))
    }
    fn publish_new(&self, state: &State) -> Result<()> {
        let _guard = SnapshotGuard::exclusive(&self.snapshot_lock)?;
        self.write(state, true)
    }
    pub fn publish(&self, state: &State, previous: &State) -> Result<()> {
        let _guard = SnapshotGuard::exclusive(&self.snapshot_lock)?;
        if self.format == Format::Readable {
            return self.write(state, false);
        }
        let Some(frame) = binary::commit(state, previous)? else {
            return Ok(());
        };
        // Schema changes and row compaction already require a complete image.
        // Publish it directly instead of retaining obsolete snapshots in the log.
        if frame[0] == 0 {
            return self.write(state, false);
        }
        let journal = self.journal.get();
        let length = journal.length + frame.len() as u64;
        if length > MAX_BYTES
            || length > (journal.base_length * 2).max(1024 * 1024)
            || journal.commits >= 1024
        {
            return self.write(state, false);
        }
        let file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(error)?;
        if !file.metadata().map_err(error)?.is_file()
            || file.metadata().map_err(error)?.len() != journal.length
        {
            return Err(Error(
                "database file changed outside this connection".into(),
            ));
        }
        let mut file = file;
        if let Err(failure) = file.write_all(&frame).and_then(|_| file.sync_all()) {
            file.set_len(journal.length).map_err(error)?;
            file.sync_all().map_err(error)?;
            return Err(error(failure));
        }
        self.journal.set(Journal {
            length,
            commits: journal.commits + 1,
            ..journal
        });
        Ok(())
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
        if self.format == Format::Binary {
            self.journal.set(Journal {
                length: bytes.len() as u64,
                base_length: bytes.len() as u64,
                commits: 0,
            });
        }
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
// State is private and every successful statement maintains its invariants.
// Imported bytes are validated once below, before entering a Database.
pub(crate) fn encode(state: &State, format: Format) -> Result<Vec<u8>> {
    match format {
        Format::Binary => binary::encode(state),
        Format::Readable => readable::encode(state),
    }
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(State, Format)> {
    decode_with_length(bytes, false).map(|(state, format, _)| (state, format))
}
fn decode_with_length(bytes: &[u8], recover: bool) -> Result<(State, Format, Journal)> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    let (mut state, format, journal) = if bytes.starts_with(binary::MAGIC) {
        let (state, journal) = binary::decode(bytes, recover)?;
        (state, Format::Binary, journal)
    } else if bytes.starts_with(b"BOXDB\0") {
        return Err(Error("unsupported binary format version".into()));
    } else {
        (
            readable::decode(bytes)?,
            Format::Readable,
            Journal {
                length: bytes.len() as u64,
                ..Journal::default()
            },
        )
    };
    state.validate_import()?;
    Ok((state, format, journal))
}

fn declare_table(state: &mut State, name: &str, sql: &str, next_rowid: u64) -> Result<()> {
    let statements = crate::parse(sql)?;
    let [sqlparser::ast::Statement::CreateTable(table)] = statements.as_slice() else {
        return Err(Error("table record must contain one CREATE TABLE".into()));
    };
    if crate::engine::name(&table.name) != name || state.tables.contains_key(name) {
        return Err(Error("mismatched or duplicate table record".into()));
    }
    state.declare(&statements[0])?;
    state.tables.get_mut(name).unwrap().next_rowid = next_rowid;
    Ok(())
}
fn declare_index(state: &mut State, name: &str, sql: &str) -> Result<()> {
    let statements = crate::parse(sql)?;
    let [sqlparser::ast::Statement::CreateIndex(index)] = statements.as_slice() else {
        return Err(Error("index record must contain one CREATE INDEX".into()));
    };
    if index.name.as_ref().map(crate::engine::name).as_deref() != Some(name)
        || state.indexes.contains_key(name)
    {
        return Err(Error("mismatched or duplicate index record".into()));
    }
    state.declare(&statements[0])?;
    Ok(())
}

pub(crate) fn read_snapshot(path: &Path) -> Result<Vec<u8>> {
    let path = fs::canonicalize(path).map_err(error)?;
    let lock = snapshot_lock(&path)?;
    let _guard = SnapshotGuard::shared(&lock)?;
    let mut bytes = read_limited(&path)?;
    let (_, _, journal) = decode_with_length(&bytes, true)?;
    bytes.truncate(journal.length as usize);
    Ok(bytes)
}
