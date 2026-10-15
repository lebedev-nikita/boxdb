//! Versioned SQL migrations executed through BoxDB's typed connection.
use crate::{Database, Error, InternalGuard, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) const TABLE: &str = "__boxdb_migrations";
const CREATE: &str = "CREATE TABLE IF NOT EXISTS __boxdb_migrations(version TEXT PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL) STRICT";

#[derive(Clone, Debug, Serialize)]
pub struct MigrationInfo {
    pub version: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    Applied,
    Pending,
}

#[derive(Clone, Debug, Serialize)]
pub struct MigrationStatus {
    #[serde(flatten)]
    pub migration: MigrationInfo,
    pub state: MigrationState,
}

struct Migration {
    info: MigrationInfo,
    checksum: String,
    up: String,
    down: String,
}

/// A named SQL document, with the same filename and contents used by the CLI.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MigrationFile {
    pub filename: String,
    pub sql: String,
}

/// An immutable, validated set of versioned SQL documents, independent of a database.
pub struct MigrationSet {
    migrations: Vec<Migration>,
}

impl MigrationSet {
    /// Create a new SQL template without opening a database.
    pub fn create(directory: impl AsRef<Path>, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::new(
                "migration name must use lowercase letters, digits and underscores".into(),
            ));
        }
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        let mut time = chrono::Utc::now().naive_utc();
        let mut latest = String::new();
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "sql")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Some((version, _)) = stem.split_once('_')
                && version.len() == 14
                && version.bytes().all(|b| b.is_ascii_digit())
                && version > latest.as_str()
            {
                latest = version.into();
            }
        }
        if latest.as_str() >= time.format("%Y%m%d%H%M%S").to_string().as_str() {
            time = chrono::NaiveDateTime::parse_from_str(&latest, "%Y%m%d%H%M%S")
                .map_err(Error::validation)?
                .checked_add_signed(chrono::Duration::seconds(1))
                .ok_or_else(|| Error::new("migration version overflow".into()))?;
        }
        loop {
            let path = directory.join(format!("{}_{}.sql", time.format("%Y%m%d%H%M%S"), name));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(b"-- migrate:up\n\n-- migrate:down\n")?;
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    time = time
                        .checked_add_signed(chrono::Duration::seconds(1))
                        .ok_or_else(|| Error::new("migration version overflow".into()))?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Inspect committed migration state without modifying the database file.
    pub fn status_file(&self, path: impl AsRef<Path>) -> Result<Vec<MigrationStatus>> {
        let mut connection = crate::storage::reader(path.as_ref())?;
        let transaction = connection.transaction()?;
        crate::schema::load(&transaction)?;
        self.status_connection(&transaction)
    }

    pub fn status(&self, db: &Database) -> Result<Vec<MigrationStatus>> {
        require_idle(db)?;
        check_interrupt(db)?;
        // BEGIN itself may be interrupted after SQLite enters a transaction.
        let result = db
            .connection
            .execute_batch("BEGIN")
            .map_err(Error::from)
            .and_then(|()| self.status_connection(&db.connection));
        // Cleanup must complete even when a binding's cancellation stays set.
        db.connection.progress_handler(0, None::<fn() -> bool>)?;
        let cleanup = if db.in_transaction() {
            db.connection.execute_batch("ROLLBACK").map_err(Error::from)
        } else {
            Ok(())
        };
        let restore = db
            .connection
            .progress_handler(db.progress_operations.get(), db.progress_callback.get())
            .map_err(Error::from);
        match result {
            Ok(statuses) => {
                cleanup?;
                restore?;
                Ok(statuses)
            }
            Err(error) => Err(cleanup_error(error, cleanup.and(restore))),
        }
    }

    fn status_connection(&self, connection: &rusqlite::Connection) -> Result<Vec<MigrationStatus>> {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name=?1 AND type='table')",
            [TABLE],
            |r| r.get(0),
        )?;
        let applied: std::collections::BTreeMap<String, (String, String)> = if exists {
            connection
                .prepare("SELECT version,name,checksum FROM __boxdb_migrations")?
                .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
                .collect::<rusqlite::Result<_>>()?
        } else {
            Default::default()
        };
        for (version, (name, checksum)) in &applied {
            let migration = self
                .migrations
                .binary_search_by(|m| m.info.version.cmp(version))
                .ok()
                .map(|index| &self.migrations[index])
                .ok_or_else(|| Error::new(format!("missing applied migration: {version}")))?;
            if &migration.checksum != checksum || &migration.info.name != name {
                return Err(Error::new(format!(
                    "applied migration has changed: {version}"
                )));
            }
        }
        if let Some(latest) = applied.keys().next_back()
            && let Some(migration) = self
                .migrations
                .iter()
                .find(|m| m.info.version < *latest && !applied.contains_key(&m.info.version))
        {
            return Err(Error::new(format!(
                "out-of-order migration: {} precedes applied version {latest}",
                migration.info.version
            )));
        }
        Ok(self
            .migrations
            .iter()
            .map(|m| MigrationStatus {
                migration: m.info.clone(),
                state: if applied.contains_key(&m.info.version) {
                    MigrationState::Applied
                } else {
                    MigrationState::Pending
                },
            })
            .collect())
    }

    pub fn read(directory: impl AsRef<Path>) -> Result<Self> {
        let mut files = vec![];
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "sql") {
                continue;
            }
            files.push(MigrationFile {
                filename: path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::new("invalid migration filename".into()))?
                    .into(),
                sql: fs::read_to_string(&path)?,
            });
        }
        Self::from_files(files)
    }

    /// Validate embedded SQL documents using the same rules and checksums as files.
    pub fn from_files(files: impl IntoIterator<Item = MigrationFile>) -> Result<Self> {
        let mut migrations = vec![];
        for file in files {
            let path = Path::new(&file.filename);
            if file.filename.contains(['/', '\\'])
                || path.extension().is_none_or(|extension| extension != "sql")
            {
                return Err(Error::new(format!(
                    "invalid migration filename: {}",
                    file.filename
                )));
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::new("invalid migration filename".into()))?;
            let (version, name) = stem
                .split_once('_')
                .ok_or_else(|| Error::new(format!("invalid migration filename: {stem}")))?;
            if version.len() != 14
                || !version.bytes().all(|b| b.is_ascii_digit())
                || name.is_empty()
            {
                return Err(Error::new(format!(
                    "expected VERSION_NAME.sql with a 14-digit version: {stem}"
                )));
            }
            let sql = file.sql.replace("\r\n", "\n");
            let (up, down) =
                sections(&sql).map_err(|e| e.with_context(format!("migration {version}")))?;
            if !has_sql(up)? {
                return Err(Error::new(format!(
                    "migration {version}: empty migrate:up section"
                )));
            }
            validate_sql(up).map_err(|e| e.with_context(format!("migration {version} up")))?;
            validate_sql(down).map_err(|e| e.with_context(format!("migration {version} down")))?;
            migrations.push(Migration {
                info: MigrationInfo {
                    version: version.into(),
                    name: name.into(),
                },
                checksum: hex::encode(Sha256::digest(sql.as_bytes())),
                up: up.into(),
                down: down.into(),
            });
        }
        migrations.sort_by(|a, b| a.info.version.cmp(&b.info.version));
        for pair in migrations.windows(2) {
            if pair[0].info.version == pair[1].info.version {
                return Err(Error::new(format!(
                    "duplicate migration version: {}",
                    pair[0].info.version
                )));
            }
        }
        Ok(Self { migrations })
    }

    /// Apply pending files, committing each version independently.
    pub fn up(&self, db: &mut Database) -> Result<Vec<MigrationInfo>> {
        if self
            .status(db)?
            .iter()
            .all(|s| s.state == MigrationState::Applied)
        {
            return Ok(vec![]);
        }
        let mut applied = vec![];
        loop {
            check_interrupt(db)?;
            db.execute("BEGIN IMMEDIATE")
                .map_err(|error| rollback_after_error(db, error))?;
            let result: Result<Option<MigrationInfo>> = (|| {
                // Recheck under the write lock: another runner may have applied
                // or rolled back a version since the previous commit.
                let statuses = self.status_connection(&db.connection)?;
                let Some(index) = statuses
                    .iter()
                    .position(|s| s.state == MigrationState::Pending)
                else {
                    db.execute("ROLLBACK")?;
                    return Ok(None);
                };
                let migration = &self.migrations[index];
                let apply = (|| -> Result<()> {
                    {
                        let _guard = InternalGuard::enter(&db.internal);
                        db.connection.execute_batch(CREATE)?;
                    }
                    execute_sql(db, &migration.up)?;
                    check_interrupt(db)?;
                    {
                        let _guard = InternalGuard::enter(&db.internal);
                        db.connection.execute(
                            "INSERT INTO __boxdb_migrations VALUES(?1,?2,?3)",
                            rusqlite::params![
                                migration.info.version,
                                migration.info.name,
                                migration.checksum
                            ],
                        )?;
                    }
                    check_interrupt(db)?;
                    db.execute("COMMIT")?;
                    Ok(())
                })();
                apply.map_err(|e| {
                    e.with_context(format!("migration {} up", migration.info.version))
                })?;
                Ok(Some(migration.info.clone()))
            })();
            match result {
                Ok(Some(migration)) => applied.push(migration),
                Ok(None) => return Ok(applied),
                Err(error) => {
                    return Err(rollback_after_error(db, error));
                }
            }
        }
    }

    /// Roll back exactly one applied version. An empty history is a no-op.
    pub fn down(&self, db: &mut Database) -> Result<Option<MigrationInfo>> {
        require_idle(db)?;
        check_interrupt(db)?;
        db.execute("BEGIN IMMEDIATE")
            .map_err(|error| rollback_after_error(db, error))?;
        let result = (|| {
            self.status_connection(&db.connection)?;
            let exists: bool = db.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name=?1 AND type='table')",
                [TABLE],
                |r| r.get(0),
            )?;
            let latest: Option<String> = if exists {
                db.connection
                    .query_row(
                        "SELECT version FROM __boxdb_migrations ORDER BY version DESC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?
            } else {
                None
            };
            let Some(version) = latest else {
                db.execute("ROLLBACK")?;
                return Ok(None);
            };
            let migration = self
                .migrations
                .iter()
                .find(|m| m.info.version == version)
                .ok_or_else(|| Error::new(format!("missing applied migration: {version}")))?;
            if !has_sql(&migration.down)? {
                return Err(Error::new(format!(
                    "migration {version}: empty migrate:down section; cannot roll back"
                )));
            }
            execute_sql(db, &migration.down)
                .map_err(|e| e.with_context(format!("migration {version} down")))?;
            check_interrupt(db).map_err(|e| e.with_context(format!("migration {version} down")))?;
            {
                let _guard = InternalGuard::enter(&db.internal);
                db.connection.execute(
                    "DELETE FROM __boxdb_migrations WHERE version=?1",
                    [&version],
                )?;
            }
            check_interrupt(db).map_err(|e| e.with_context(format!("migration {version} down")))?;
            db.execute("COMMIT")?;
            Ok(Some(migration.info.clone()))
        })();
        result.map_err(|error| rollback_after_error(db, error))
    }
}

fn cleanup_error(error: Error, cleanup: Result<()>) -> Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => error.with_context(format!("migration cleanup failed ({cleanup})")),
    }
}

fn check_interrupt(db: &Database) -> Result<()> {
    if db
        .progress_callback
        .get()
        .is_some_and(|callback| callback())
    {
        return Err(Error::new("migration interrupted".into()).with_code("BOXDB_INTERRUPT"));
    }
    Ok(())
}

fn rollback_after_error(db: &mut Database, error: Error) -> Error {
    if !db.in_transaction() {
        return error;
    }
    let cleanup = (|| -> Result<()> {
        db.connection.progress_handler(0, None::<fn() -> bool>)?;
        let rollback = db.execute("ROLLBACK").map(|_| ());
        let restore = db
            .connection
            .progress_handler(db.progress_operations.get(), db.progress_callback.get())
            .map_err(Error::from);
        rollback.and(restore)
    })();
    if cleanup.is_err() {
        db.poisoned = true;
    }
    cleanup_error(error, cleanup)
}

fn require_idle(db: &Database) -> Result<()> {
    if db.poisoned {
        return Err(
            Error::new("connection must be reopened after a publication failure".into())
                .with_code("BOXDB_UNAVAILABLE"),
        );
    }
    if db.in_transaction() {
        return Err(Error::new(
            "migration execution requires an idle connection".into(),
        ));
    }
    Ok(())
}

fn validate_sql(sql: &str) -> Result<()> {
    for statement in crate::split(sql)? {
        let keyword = crate::schema::keyword(statement).to_ascii_uppercase();
        if matches!(
            keyword.as_str(),
            "BEGIN"
                | "COMMIT"
                | "END"
                | "ROLLBACK"
                | "SAVEPOINT"
                | "RELEASE"
                | "VACUUM"
                | "ATTACH"
                | "DETACH"
                | "PRAGMA"
        ) {
            return Err(Error::new(format!(
                "{keyword} is not allowed inside a migration; BoxDB manages the connection and transaction"
            )));
        }
    }
    Ok(())
}
fn has_sql(sql: &str) -> Result<bool> {
    Ok(crate::split(sql)?
        .iter()
        .any(|sql| !crate::schema::keyword(sql).is_empty()))
}
fn execute_sql(db: &mut Database, sql: &str) -> Result<()> {
    for statement in crate::split(sql)? {
        if !crate::schema::keyword(statement).is_empty() {
            check_interrupt(db)?;
            db.execute(statement)?;
        }
    }
    Ok(())
}

fn sections(sql: &str) -> Result<(&str, &str)> {
    use sqlparser::{
        dialect::SQLiteDialect,
        tokenizer::{Token, Tokenizer, Whitespace},
    };
    let tokens = Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize_with_location()
        .map_err(Error::validation)?;
    let mut offsets = vec![0];
    for (i, byte) in sql.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(i + 1);
        }
    }
    let mut up_start = None;
    let mut down_range = None;
    for token in tokens {
        let Token::Whitespace(Whitespace::SingleLineComment { comment, prefix }) = token.token
        else {
            continue;
        };
        if prefix != "--" || !comment.trim().starts_with("migrate:") {
            continue;
        }
        let line = token.span.start.line as usize - 1;
        let start = offsets[line];
        let end = offsets.get(line + 1).copied().unwrap_or(sql.len());
        if !sql[start..end]
            .chars()
            .take(token.span.start.column as usize - 1)
            .all(char::is_whitespace)
        {
            continue;
        }
        let section_start = down_range.map(|(_, end)| end).or(up_start).unwrap_or(0);
        let previous = &sql[section_start..start];
        // A comment inside an unfinished trigger body is SQL, not a directive.
        if !crate::schema::keyword(previous).is_empty() {
            let text = std::ffi::CString::new(format!("{previous};"))
                .map_err(|error| Error::new("SQL contains NUL".into()).with_source(error))?;
            // SAFETY: CString remains alive and is NUL-terminated.
            if unsafe { rusqlite::ffi::sqlite3_complete(text.as_ptr()) } == 0 {
                continue;
            }
        }
        match comment.trim() {
            "migrate:up" if up_start.is_none() && down_range.is_none() => {
                if !crate::schema::keyword(&sql[..start]).is_empty() { return Err(Error::new("SQL before migrate:up".into())); }
                up_start = Some(end);
            },
            "migrate:down" if up_start.is_some() && down_range.is_none() => down_range = Some((start, end)),
            _ => return Err(Error::new("expected exactly one migrate:up followed by migrate:down; directive options are unsupported".into())),
        }
    }
    let start = up_start.ok_or_else(|| Error::new("missing migrate:up".into()))?;
    let (end, down) = down_range.ok_or_else(|| Error::new("missing migrate:down".into()))?;
    Ok((&sql[start..end], &sql[down..]))
}
