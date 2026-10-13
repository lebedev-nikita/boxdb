//! An extended SQLite client. Binary files are ordinary SQLite databases;
//! readable files independently store schemas and rows as a YAML stream.
//!
//! ```
//! use boxdb::{Database, Value};
//! let mut db = Database::memory();
//! db.execute("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)")?;
//! db.execute_params("INSERT INTO users(name) VALUES(?)", &[Value::Text("Ada".into())])?;
//! assert_eq!(db.execute("SELECT name FROM users")?[0].rows,
//!            vec![vec![Value::Text("Ada".into())]]);
//! # Ok::<(), boxdb::Error>(())
//! ```
use rusqlite::{Connection, OpenFlags, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
macro_rules! convert_error {($($ty:ty),*)=>{$(impl From<$ty> for Error {fn from(e:$ty)->Self {Self(e.to_string())}})*};}
convert_error!(
    rusqlite::Error,
    std::io::Error,
    serde_json::Error,
    serde_saphyr::DeserializeError,
    serde_saphyr::SerializeError,
    sqlparser::parser::ParserError
);
pub mod types;
pub use types::{Type, Value};
mod readable;
mod schema;
mod storage;
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Binary,
    Readable,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected: usize,
}
pub struct Database {
    connection: Connection,
    format: Option<Format>,
    readable: Option<storage::ReadableFile>,
    types: RefCell<schema::Types>,
    result_types: RefCell<HashMap<String, Rc<[Option<Type>]>>>,
    pager_version: Cell<u32>,
    validation_dirty: Cell<bool>,
    data_version: i64,
    write_transaction_ready: bool,
    poisoned: bool,
    internal: Arc<AtomicBool>,
}
struct InternalGuard(Arc<AtomicBool>, bool);
impl InternalGuard {
    fn enter(flag: &Arc<AtomicBool>) -> Self {
        let old = flag.swap(true, Ordering::Relaxed);
        Self(flag.clone(), old)
    }
}
impl Drop for InternalGuard {
    fn drop(&mut self) {
        self.0.store(self.1, Ordering::Relaxed);
    }
}
fn authorizer(db: &Connection, internal: Arc<AtomicBool>) -> Result<()> {
    use rusqlite::hooks::{AuthAction as A, Authorization as Z};
    db.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
        if internal.load(Ordering::Relaxed) {
            return Z::Allow;
        }
        let object = match context.action {
            A::Insert { table_name }
            | A::Update { table_name, .. }
            | A::Delete { table_name }
            | A::CreateTable { table_name }
            | A::DropTable { table_name }
            | A::CreateTempTable { table_name }
            | A::DropTempTable { table_name } => Some(table_name),
            A::AlterTable { table_name, .. } => Some(table_name),
            A::CreateTrigger { trigger_name, .. }
            | A::CreateTempTrigger { trigger_name, .. }
            | A::DropTrigger { trigger_name, .. }
            | A::DropTempTrigger { trigger_name, .. } => Some(trigger_name),
            _ => None,
        };
        if object.is_some_and(|n| n.to_ascii_lowercase().starts_with("__boxdb_")) {
            Z::Deny
        } else {
            Z::Allow
        }
    }))?;
    Ok(())
}
fn configure(db: &Connection, binary: bool) -> Result<()> {
    db.busy_timeout(Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.pragma_update(None, "fullfsync", true)?;
    if binary {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "locking_mode", "NORMAL")?;
    }
    db.set_prepared_statement_cache_capacity(512);
    Ok(())
}
impl Database {
    fn new(connection: Connection, binary: bool) -> Result<Self> {
        configure(&connection, binary)?;
        let types = schema::load(&connection)?;
        schema::install(&connection, &types)?;
        let data_version = connection.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let pager_version = storage::data_version(&connection)?;
        let internal = Arc::new(AtomicBool::new(false));
        authorizer(&connection, internal.clone())?;
        Ok(Self {
            connection,
            format: None,
            readable: None,
            types: RefCell::new(types),
            result_types: RefCell::new(HashMap::new()),
            pager_version: Cell::new(pager_version),
            validation_dirty: Cell::new(false),
            data_version,
            write_transaction_ready: false,
            poisoned: false,
            internal,
        })
    }
    pub fn memory() -> Self {
        Self::new(
            Connection::open_in_memory().expect("open SQLite memory database"),
            false,
        )
        .expect("configure SQLite memory database")
    }
    /// Open an ordinary SQLite binary database or a standalone YAML stream.
    /// Existing contents determine the format; `format` applies to new files.
    pub fn open(path: impl AsRef<Path>, format: Format) -> Result<Self> {
        let path = path.as_ref();
        let detected = if path.exists() && path.metadata()?.len() > 0 {
            use std::io::Read;
            let mut header = [0u8; 16];
            let mut file = std::fs::File::open(path)?;
            let count = file.read(&mut header)?;
            if header.starts_with(b"BOXDB\0") {
                return Err(Error("obsolete BoxDB binary format is unsupported".into()));
            }
            if count == 16 && header == storage::SQLITE_HEADER {
                Format::Binary
            } else {
                Format::Readable
            }
        } else {
            format
        };
        let mut db = match detected {
            Format::Binary => {
                let connection = Connection::open(path)?;
                Self::new(connection, true)?
            }
            Format::Readable => {
                let file = storage::ReadableFile::open(path)?;
                let connection = if path.exists() && path.metadata()?.len() > 0 {
                    storage::import(&std::fs::read(path)?)?
                } else {
                    Connection::open_in_memory()?
                };
                let mut db = Self::new(connection, false)?;
                file.watch(&db.connection)?;
                if !path.exists() || path.metadata()?.len() == 0 {
                    file.publish(&db.connection)?;
                }
                db.readable = Some(file);
                db
            }
        };
        db.format = Some(detected);
        Ok(db)
    }
    pub fn format(&self) -> Option<Format> {
        self.format
    }
    pub fn in_transaction(&self) -> bool {
        !self.connection.is_autocommit()
    }
    /// Export a consistent snapshot. Finish the current transaction first.
    pub fn export(&self, format: Format) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(Error(
                "connection must be reopened after a publication failure".into(),
            ));
        }
        if self.in_transaction() {
            return Err(Error("export requires no active transaction".into()));
        }
        match format {
            Format::Binary => storage::snapshot(&self.connection),
            Format::Readable => {
                // Hold one SQLite read snapshot across the codec's schema and
                // table scans, even when another connection commits meanwhile.
                let transaction = self.connection.unchecked_transaction()?;
                match &self.readable {
                    Some(file) => file.export(&transaction),
                    None => readable::encode(&transaction),
                }
            }
        }
    }
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let connection = storage::import(bytes)?;
        let types = schema::load(&connection)?;
        schema::validate_all(&connection, &types)?;
        if connection
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
        {
            return Err(Error("FOREIGN KEY constraint failed in snapshot".into()));
        }
        Self::new(connection, false)
    }
    /// Restore into a new path. Binary restoration uses SQLite's backup API.
    pub fn restore(path: impl AsRef<Path>, bytes: &[u8]) -> Result<Self> {
        let path = path.as_ref();
        let source = Self::from_snapshot(bytes)?;
        // Reserve the destination before publication; never overwrite existing data.
        let reservation = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        drop(reservation);
        let result = (|| {
            if bytes.starts_with(storage::SQLITE_HEADER) {
                let mut target = Connection::open(path)?;
                storage::copy(&source.connection, &mut target)?;
                drop(target);
                Self::open(path, Format::Binary)
            } else {
                let file = storage::ReadableFile::open(path)?;
                file.publish(&source.connection)?;
                drop(file);
                Self::open(path, Format::Readable)
            }
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
    /// Read committed binary data through SQLite, including its active WAL.
    pub fn read_snapshot(path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let path = path.as_ref();
        let mut header = [0u8; 16];
        use std::io::Read;
        let count = std::fs::File::open(path)?.read(&mut header)?;
        if count == 16 && header == storage::SQLITE_HEADER {
            let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            connection.busy_timeout(Duration::from_secs(5))?;
            storage::snapshot(&connection)
        } else {
            let bytes = std::fs::read(path)?;
            Self::from_snapshot(&bytes)?;
            Ok(bytes)
        }
    }
    fn refresh(&mut self, force: bool) -> Result<()> {
        let version = self
            .connection
            .query_row("PRAGMA data_version", [], |r| r.get(0))?;
        if force || version != self.data_version || self.validation_dirty.get() {
            let _guard = InternalGuard::enter(&self.internal);
            let types = schema::load(&self.connection)?;
            schema::install(&self.connection, &types)?;
            self.types.replace(types);
            self.result_types.borrow_mut().clear();
            self.validation_dirty.set(false);
            self.pager_version
                .set(storage::data_version(&self.connection)?);
            self.data_version = version;
        }
        Ok(())
    }
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        if !sql.contains(';') {
            return if sql.trim().is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![self.execute_one(sql, &[])?])
            };
        }
        let mut results = vec![];
        for statement in split(sql)? {
            results.push(self.execute_one(statement, &[])?);
        }
        Ok(results)
    }
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<Vec<QueryResult>> {
        let statements = split(sql)?;
        let [sql] = statements.as_slice() else {
            return Err(Error(
                "parameterized execution requires one statement".into(),
            ));
        };
        let values = params
            .iter()
            .map(schema::encode)
            .collect::<Result<Vec<_>>>()?;
        Ok(vec![self.execute_one(sql, &values)?])
    }
    fn execute_one(&mut self, sql: &str, params: &[SqlValue]) -> Result<QueryResult> {
        if self.poisoned {
            return Err(Error(
                "connection must be reopened after a publication failure".into(),
            ));
        }
        let keyword = schema::keyword(sql);
        if keyword.eq_ignore_ascii_case("SELECT") || keyword.eq_ignore_ascii_case("VALUES") {
            return self.run(sql, params);
        }
        let keyword = keyword.to_ascii_uppercase();
        let declaration = schema::declaration(sql)?;
        if let Some(declaration) = declaration {
            if declaration
                .table
                .to_ascii_lowercase()
                .starts_with("__boxdb_")
            {
                return Err(Error("__boxdb_ names are reserved".into()));
            }
            let _guard = InternalGuard::enter(&self.internal);
            if !params.is_empty() {
                return Err(Error("DDL parameters are unsupported".into()));
            }
            if self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1 COLLATE NOCASE)",
                [&declaration.table],
                |r| r.get::<_, bool>(0),
            )? {
                // Preserve native IF NOT EXISTS behavior, without replacing metadata.
                self.connection.execute_batch(&declaration.sql)?;
                return Ok(empty());
            }
            self.connection
                .execute_batch("SAVEPOINT __boxdb_declaration")?;
            let result = (|| {
                self.connection.execute_batch(&declaration.sql)?;
                schema::save(&self.connection, &declaration)?;
                self.refresh(true)?;
                self.connection
                    .execute_batch("RELEASE __boxdb_declaration")?;
                Ok(empty())
            })();
            if result.is_err() {
                let _ = self
                    .connection
                    .execute_batch("ROLLBACK TO __boxdb_declaration; RELEASE __boxdb_declaration");
                let _ = self.refresh(true);
            }
            if result.is_ok() {
                self.publish_readable()?;
            }
            return result;
        }
        let control = matches!(
            keyword.as_str(),
            "BEGIN" | "SAVEPOINT" | "COMMIT" | "END" | "RELEASE" | "ROLLBACK"
        );
        let ddl = matches!(keyword.as_str(), "CREATE" | "ALTER" | "DROP");
        let alteration = if keyword == "ALTER" {
            schema::alteration(sql)?
        } else {
            None
        };
        let sql = alteration.as_ref().map_or(sql, |a| a.sql.as_str());
        // Refresh validators before preparing a write: another BoxDB client may
        // have renamed/dropped a typed column referenced by our TEMP triggers.
        if !control
            && !matches!(keyword.as_str(), "SELECT" | "VALUES" | "EXPLAIN")
            && (!self.write_transaction_ready || self.connection.is_autocommit())
        {
            self.refresh(false)?;
        }
        let readonly = if ddl {
            false
        } else {
            self.connection.prepare_cached(sql)?.readonly()
        };
        let guarded = ddl
            || (self.readable.is_some()
                && self.connection.is_autocommit()
                && !readonly
                && !control
                && !matches!(keyword.as_str(), "PRAGMA" | "VACUUM"));
        let outer_autocommit = self.connection.is_autocommit();
        if guarded {
            self.connection.execute_batch("SAVEPOINT __boxdb_write")?;
        }
        if ddl {
            let _guard = InternalGuard::enter(&self.internal);
            schema::uninstall(&self.connection)?;
        }
        // VACUUM rebuilds the complete database, including reserved metadata.
        let _vacuum_guard = (keyword == "VACUUM").then(|| InternalGuard::enter(&self.internal));
        let result = self.run(sql, params);
        if result.is_err() {
            if guarded && !self.connection.is_autocommit() {
                let _ = self
                    .connection
                    .execute_batch("ROLLBACK TO __boxdb_write; RELEASE __boxdb_write");
            }
            if ddl {
                let _ = self.refresh(true);
            }
            return result;
        }
        let completion = (|| -> Result<()> {
            if ddl {
                let _guard = InternalGuard::enter(&self.internal);
                self.connection.flush_prepared_statement_cache();
                if let Some(change) = &alteration {
                    schema::save_alteration(&self.connection, change)?;
                    if !change.columns.is_empty() {
                        let types = schema::load(&self.connection)?;
                        schema::validate_all(&self.connection, &types)?;
                    }
                }
                schema::cleanup(&self.connection)?;
                self.refresh(true)?;
            }
            if guarded {
                // An internal savepoint nested inside a user transaction must
                // not publish that transaction's pending readable data.
                if outer_autocommit
                    && let Some(file) = &self.readable
                    && let Err(e) = file.publish(&self.connection)
                {
                    self.poisoned = true;
                    return Err(e);
                }
                self.connection.execute_batch("RELEASE __boxdb_write")?;
            } else if !readonly || matches!(keyword.as_str(), "COMMIT" | "END" | "RELEASE") {
                self.publish_readable()?;
            }
            Ok(())
        })();
        if let Err(e) = completion {
            if guarded && !self.connection.is_autocommit() {
                let _ = self
                    .connection
                    .execute_batch("ROLLBACK TO __boxdb_write; RELEASE __boxdb_write");
                let _ = self.refresh(true);
            }
            return Err(e);
        }
        if control {
            self.write_transaction_ready = false;
            self.refresh(true)?;
        } else if !readonly {
            self.pager_version
                .set(storage::data_version(&self.connection)?);
            self.write_transaction_ready = true;
        }
        result
    }
    fn publish_readable(&mut self) -> Result<()> {
        if self.connection.is_autocommit()
            && let Some(file) = &self.readable
            && let Err(e) = file.publish(&self.connection)
        {
            self.poisoned = true;
            return Err(e);
        }
        Ok(())
    }
    fn refresh_read_metadata(&self) -> Result<()> {
        let version = storage::data_version(&self.connection)?;
        if version != self.pager_version.get() {
            // External DDL can change column origins without changing logical
            // types (for example, replacing a view). Invalidate both together.
            self.result_types.borrow_mut().clear();
            let types = schema::load(&self.connection)?;
            if types != *self.types.borrow() {
                self.types.replace(types);
                self.validation_dirty.set(true);
            }
            self.pager_version.set(version);
        }
        Ok(())
    }
    fn run(&self, sql: &str, params: &[SqlValue]) -> Result<QueryResult> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let before = self.connection.total_changes();
        let readonly = statement.readonly();
        let mut rows = vec![];
        if statement.column_count() == 0 {
            statement.execute(rusqlite::params_from_iter(params))?;
        } else {
            let mut hints: Option<Rc<[Option<Type>]>> = None;
            let mut cursor = statement.query(rusqlite::params_from_iter(params))?;
            while let Some(row) = cursor.next()? {
                // SQLite may reprepare a cached statement on its first step.
                // Read metadata after that step, including a changed SELECT *.
                if rows.is_empty() {
                    self.refresh_read_metadata()?;
                }
                let count = row.as_ref().column_count();
                if hints.is_none() && !self.types.borrow().is_empty() {
                    hints = self.result_types.borrow().get(sql).cloned();
                    if hints.is_none() {
                        let types = self.types.borrow();
                        let columns: Rc<[Option<Type>]> = row
                            .as_ref()
                            .columns_with_metadata()
                            .iter()
                            .map(|c| {
                                if c.database_name() != Some("main") {
                                    return None;
                                }
                                c.table_name()
                                    .zip(c.origin_name())
                                    .and_then(|(t, c)| types.get(&(t.into(), c.into())).cloned())
                            })
                            .collect();
                        let mut cache = self.result_types.borrow_mut();
                        if cache.len() >= 512 {
                            cache.clear();
                        }
                        cache.insert(sql.to_owned(), columns.clone());
                        hints = Some(columns);
                    }
                }
                let mut values = Vec::with_capacity(count);
                for i in 0..count {
                    let value = row.get::<_, SqlValue>(i)?;
                    values.push(
                        if let Some(Some(ty)) = hints.as_ref().and_then(|h| h.get(i)) {
                            schema::decode(ty, value)?
                        } else {
                            schema::native(value)
                        },
                    );
                }
                rows.push(values);
            }
        }
        if rows.is_empty() && readonly {
            self.refresh_read_metadata()?;
        }
        let columns = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        let affected = if !readonly && self.connection.total_changes() != before {
            self.connection.changes() as usize
        } else {
            0
        };
        Ok(QueryResult {
            columns,
            rows,
            affected,
        })
    }
}
fn empty() -> QueryResult {
    QueryResult {
        columns: vec![],
        rows: vec![],
        affected: 0,
    }
}
/// SQLite's completeness checker handles quoted semicolons and trigger bodies.
fn split(sql: &str) -> Result<Vec<&str>> {
    use std::ffi::CString;
    let mut statements = vec![];
    let mut start = 0;
    for (i, byte) in sql.bytes().enumerate() {
        if byte == b';' {
            let prefix = &sql[start..=i];
            let text = CString::new(prefix).map_err(|_| Error("SQL contains NUL".into()))?;
            // SAFETY: CString provides a valid NUL-terminated string for this call.
            if unsafe { rusqlite::ffi::sqlite3_complete(text.as_ptr()) } != 0 {
                statements.push(prefix);
                start = i + 1;
            }
        }
    }
    if !sql[start..].trim().is_empty() {
        statements.push(&sql[start..]);
    }
    Ok(statements)
}
