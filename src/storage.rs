//! SQLite owns binary files and their WAL. Only readable YAML uses snapshot
//! replacement and a separate single-writer lock.
use crate::{Error, Result, readable};
use fs2::FileExt;
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};
pub(crate) const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";
pub(crate) const MAX_SNAPSHOT: usize = 256 * 1024 * 1024;
pub(crate) struct ReadableFile {
    pub path: PathBuf,
    _lock: File,
    cache: readable::SnapshotCache,
}
impl ReadableFile {
    pub fn open(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = if path.exists() {
            fs::canonicalize(path)?
        } else {
            fs::canonicalize(parent)?.join(
                path.file_name()
                    .ok_or_else(|| Error("invalid path".into()))?,
            )
        };
        let mut lock = path.as_os_str().to_os_string();
        lock.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock))?;
        lock.try_lock_exclusive()
            .map_err(|_| Error("readable database is locked".into()))?;
        Ok(Self {
            path,
            _lock: lock,
            cache: readable::SnapshotCache::default(),
        })
    }
    pub fn watch(&self, db: &Connection) -> Result<()> {
        self.cache.watch(db)
    }
    pub fn export(&self, db: &Connection) -> Result<Vec<u8>> {
        self.cache.export(db)
    }
    pub fn publish(&self, db: &Connection) -> Result<()> {
        self.cache.publish(db, |snapshot| {
            let parent = self.path.parent().unwrap();
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            {
                let mut writer = BufWriter::with_capacity(64 * 1024, temp.as_file_mut());
                snapshot.write(&mut writer)?;
                writer.flush()?;
            }
            temp.as_file().sync_all()?;
            temp.persist(&self.path).map_err(|e| Error(e.to_string()))?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })
    }
}
pub(crate) fn copy(source: &Connection, destination: &mut Connection) -> Result<()> {
    let backup = Backup::new(source, destination)?;
    match backup.step(-1)? {
        StepResult::Done => Ok(()),
        _ => Err(Error(
            "SQLite snapshot is busy; retry after the transaction completes".into(),
        )),
    }
}
pub(crate) fn snapshot(db: &Connection) -> Result<Vec<u8>> {
    if !db.is_autocommit() {
        return Err(Error("export requires no active transaction".into()));
    }
    let temp = tempfile::NamedTempFile::new()?;
    let mut target = Connection::open(temp.path())?;
    copy(db, &mut target)?;
    target.pragma_update(None, "journal_mode", "DELETE")?;
    drop(target);
    if fs::metadata(temp.path())?.len() > MAX_SNAPSHOT as u64 {
        return Err(Error("snapshot exceeds 256 MiB limit".into()));
    }
    Ok(fs::read(temp.path())?)
}
pub(crate) fn import(bytes: &[u8]) -> Result<Connection> {
    if bytes.len() > MAX_SNAPSHOT {
        return Err(Error("snapshot exceeds 256 MiB limit".into()));
    }
    if bytes.starts_with(b"BOXDB\0") {
        return Err(Error("obsolete BoxDB binary format is unsupported".into()));
    }
    if !bytes.starts_with(SQLITE_HEADER) {
        return readable::decode(bytes);
    }
    let mut temp = tempfile::NamedTempFile::new()?;
    temp.write_all(bytes)?;
    let source = Connection::open_with_flags(temp.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = source.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(Error(format!("invalid SQLite snapshot: {check}")));
    }
    let mut destination = Connection::open_in_memory()?;
    copy(&source, &mut destination)?;
    Ok(destination)
}

/// The pager counter is inspected after SQLite steps a query, so external WAL
/// commits are visible without issuing a separate PRAGMA on every hot read.
pub(crate) fn data_version(db: &Connection) -> Result<u32> {
    let mut version = 0u32;
    // SAFETY: the connection stays alive, the schema is NUL-terminated, and the
    // opcode writes a u32 into the supplied, correctly aligned mutable pointer.
    let code = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            db.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_DATA_VERSION,
            (&mut version as *mut u32).cast(),
        )
    };
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(Error(format!("cannot inspect SQLite pager: {code}")));
    }
    Ok(version)
}

/// Reject a pending commit before publishing its readable snapshot. SQLite's
/// counter avoids scanning every foreign-key table on each autocommit write.
pub(crate) fn check_commit_constraints(db: &Connection) -> Result<()> {
    let mut unresolved = 0;
    let mut highwater = 0;
    // SAFETY: the live connection owns its SQLite handle and both output
    // pointers refer to initialized, correctly aligned integers.
    let code = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            db.handle(),
            rusqlite::ffi::SQLITE_DBSTATUS_DEFERRED_FKS,
            &mut unresolved,
            &mut highwater,
            0,
        )
    };
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(Error(format!(
            "cannot inspect pending SQLite constraints: {code}"
        )));
    }
    if unresolved != 0 {
        return Err(Error("FOREIGN KEY constraint failed".into()));
    }
    Ok(())
}
