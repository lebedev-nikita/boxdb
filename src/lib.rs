//! An extended SQLite client. Binary files are ordinary SQLite databases;
//! readable files independently store schemas and rows as a YAML stream.
//!
//! ```
//! use boxdb::{Database, Value};
//! let mut db = Database::memory();
//! db.execute("CREATE TABLE users(id INT64 PRIMARY KEY, name TEXT NOT NULL)")?;
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
mod error;
pub use error::Error;
pub type Result<T> = std::result::Result<T, Error>;
pub mod migrations;
pub mod types;
pub use rusqlite::InterruptHandle;
pub use types::{Type, Value};
mod decimal;
mod extensions;
mod readable;
mod schema;
mod schema_dump;
pub use schema_dump::SchemaFormat;
mod storage;
#[cfg(doctest)]
#[doc = include_str!("../tests/removed_snapshot_api.md")]
mod removed_snapshot_api {}
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
/// Metadata used by language bindings without executing the statement.
#[derive(Clone, Debug)]
pub struct StatementInfo {
    pub columns: Vec<String>,
    pub parameters: Vec<Option<String>>,
    pub readonly: bool,
}
/// Options used by asynchronous language bindings while opening a connection.
#[derive(Clone, Copy, Default)]
pub struct OpenOptions {
    pub read_only: bool,
    pub busy_handler: Option<fn(i32) -> bool>,
    pub progress_handler: Option<fn() -> bool>,
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
    progress_operations: Cell<i32>,
    progress_callback: Cell<Option<fn() -> bool>>,
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
        if let A::Function { function_name } = context.action
            && function_name.starts_with("__boxdb_instant_write_")
            && !context
                .accessor
                .is_some_and(|name| name.starts_with("__boxdb_validate_instant_"))
        {
            return Z::Deny;
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
fn configure(db: &Connection, binary: bool, options: OpenOptions) -> Result<()> {
    extensions::register(db)?;
    if let Some(callback) = options.busy_handler {
        db.busy_handler(Some(callback))?;
    } else {
        db.busy_timeout(Duration::from_secs(5))?;
    }
    db.progress_handler(1000, options.progress_handler)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.pragma_update(None, "fullfsync", true)?;
    if binary && !options.read_only {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "locking_mode", "NORMAL")?;
    }
    db.set_prepared_statement_cache_capacity(512);
    schema::configure_codec(db, options.read_only)?;
    Ok(())
}
impl Database {
    /// Export schema and BoxDB metadata, excluding application rows and sequences.
    pub fn dump_schema(&self, format: SchemaFormat) -> Result<Vec<u8>> {
        schema_dump::dump(&self.connection, format)
    }

    /// Export committed schema without opening a writer or changing the source.
    pub fn dump_schema_file(path: impl AsRef<Path>, format: SchemaFormat) -> Result<Vec<u8>> {
        schema_dump::dump(&storage::reader(path.as_ref())?, format)
    }

    fn new(connection: Connection, binary: bool) -> Result<Self> {
        Self::new_with_options(connection, binary, OpenOptions::default())
    }
    fn new_with_options(
        connection: Connection,
        binary: bool,
        options: OpenOptions,
    ) -> Result<Self> {
        configure(&connection, binary, options)?;
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
            progress_operations: Cell::new(1000),
            progress_callback: Cell::new(options.progress_handler),
            internal,
        })
    }
    pub fn memory() -> Self {
        Self::try_memory().expect("configure BoxDB memory database")
    }
    /// Open an isolated memory database, returning initialization failures.
    pub fn try_memory() -> Result<Self> {
        Self::new(Connection::open_in_memory()?, false)
    }
    /// Open an ordinary SQLite binary database or a standalone YAML stream.
    /// Existing contents determine the format; `format` applies to new files.
    pub fn open(path: impl AsRef<Path>, format: Format) -> Result<Self> {
        Self::open_with_options(path, format, OpenOptions::default())
    }
    pub fn memory_with_options(options: OpenOptions) -> Result<Self> {
        let db = Self::new_with_options(Connection::open_in_memory()?, false, options)?;
        if options.read_only {
            db.connection.pragma_update(None, "query_only", true)?;
        }
        Ok(db)
    }
    pub fn open_with_options(
        path: impl AsRef<Path>,
        format: Format,
        options: OpenOptions,
    ) -> Result<Self> {
        let path = path.as_ref();
        let detected = if path.exists() && path.metadata()?.len() > 0 {
            use std::io::Read;
            let mut header = [0u8; 16];
            let mut file = std::fs::File::open(path)?;
            let count = file.read(&mut header)?;
            if header.starts_with(b"BOXDB\0") {
                return Err(Error::new(
                    "obsolete BoxDB binary format is unsupported".into(),
                ));
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
                let connection = if options.read_only {
                    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
                } else {
                    Connection::open(path)?
                };
                Self::new_with_options(connection, true, options)?
            }
            Format::Readable => {
                if options.read_only {
                    let connection = storage::import(&std::fs::read(path)?)?;
                    let mut db = Self::new_with_options(connection, false, options)?;
                    db.connection.pragma_update(None, "query_only", true)?;
                    db.format = Some(detected);
                    return Ok(db);
                }
                let file = storage::ReadableFile::open(path)?;
                let connection = if path.exists() && path.metadata()?.len() > 0 {
                    storage::import(&std::fs::read(path)?)?
                } else {
                    Connection::open_in_memory()?
                };
                let mut db = Self::new_with_options(connection, false, options)?;
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
    /// Reject transaction control in driver SQL; handles manage this separately.
    pub fn validate_query_sql(sql: &str) -> Result<()> {
        for statement in split(sql)? {
            if matches!(
                schema::keyword(statement).to_ascii_uppercase().as_str(),
                "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "SAVEPOINT" | "RELEASE"
            ) {
                return Err(Error::new(
                    "use transaction handles for transaction control".into(),
                ));
            }
        }
        Ok(())
    }
    /// Set SQLite's query-only guard, returning its previous value.
    pub fn query_only(&self, enabled: bool) -> Result<bool> {
        if self.poisoned {
            return Err(Error::new(
                "connection must be reopened after a publication failure".into(),
            )
            .with_code("BOXDB_UNAVAILABLE"));
        }
        let previous = self
            .connection
            .pragma_query_value(None, "query_only", |row| row.get(0))?;
        self.connection.pragma_update(None, "query_only", enabled)?;
        Ok(previous)
    }
    /// Obtain a thread-safe handle for interrupting an active SQLite operation.
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.connection.get_interrupt_handle()
    }
    /// Replace the default five-second busy timeout with a callback.
    /// Returning false stops waiting for a SQLite lock. A later busy_timeout PRAGMA
    /// replaces this callback again.
    pub fn busy_handler(&self, callback: Option<fn(i32) -> bool>) -> Result<()> {
        self.connection.busy_handler(callback).map_err(Into::into)
    }
    /// Install a callback that interrupts SQL when it returns true.
    pub fn progress_handler(&self, operations: i32, callback: Option<fn() -> bool>) -> Result<()> {
        self.connection
            .progress_handler(operations, callback)
            .map_err(Error::from)?;
        self.progress_operations.set(operations);
        self.progress_callback
            .set(if operations > 0 { callback } else { None });
        Ok(())
    }
    /// Convert committed data into a new file without modifying the source.
    /// Existing destinations are never overwritten. Binary output uses SQLite's
    /// backup API; readable output uses the standalone YAML storage format.
    pub fn convert(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        format: Format,
    ) -> Result<()> {
        let source = storage::reader(source.as_ref())?;
        extensions::register(&source)?;
        let transaction = source.unchecked_transaction()?;
        let check: String = transaction.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(Error::new(format!("invalid source database: {check}")));
        }
        let types = schema::load(&transaction)?;
        schema::validate_all(&transaction, &types)?;
        if transaction
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
        {
            return Err(Error::new(
                "FOREIGN KEY constraint failed in source database".into(),
            ));
        }
        let path = destination.as_ref();
        // Reserve the destination before publication; never overwrite existing data.
        let reservation = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        drop(reservation);
        let result = (|| {
            match format {
                Format::Binary => {
                    let mut target = Connection::open(path)?;
                    storage::copy(&transaction, &mut target)?;
                }
                Format::Readable => {
                    let file = storage::ReadableFile::open(path)?;
                    file.publish(&transaction)?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
    fn refresh(&mut self, force: bool) -> Result<()> {
        let version = self
            .connection
            .query_row("PRAGMA data_version", [], |r| r.get(0))?;
        if force || version != self.data_version || self.validation_dirty.get() {
            let _guard = InternalGuard::enter(&self.internal);
            let types = schema::load(&self.connection)?;
            let query_only: bool =
                self.connection
                    .pragma_query_value(None, "query_only", |row| row.get(0))?;
            if !query_only {
                schema::install(&self.connection, &types)?;
            }
            self.types.replace(types);
            self.result_types.borrow_mut().clear();
            self.validation_dirty.set(query_only);
            self.pager_version
                .set(storage::data_version(&self.connection)?);
            self.data_version = version;
        }
        Ok(())
    }
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        self.execute_with_hook(sql, |_| Ok(()))
    }
    /// Execute a script, calling a hook before each statement. Bindings use this
    /// to restore cancellation handlers after a PRAGMA replaces the busy handler.
    pub fn execute_with_hook(
        &mut self,
        sql: &str,
        mut before_statement: impl FnMut(&Self) -> Result<()>,
    ) -> Result<Vec<QueryResult>> {
        if !sql.contains(';') {
            return if sql.trim().is_empty() {
                Ok(vec![])
            } else {
                before_statement(self)?;
                Ok(vec![self.execute_one(sql, &[], true)?])
            };
        }
        let mut results = vec![];
        for statement in split(sql)? {
            before_statement(self)?;
            results.push(self.execute_one(statement, &[], true)?);
        }
        Ok(results)
    }
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<Vec<QueryResult>> {
        self.execute_bound(sql, params, true)
            .map(|result| vec![result])
    }
    /// Execute one statement with SQLite storage values, retaining BoxDB validation.
    /// Language drivers use this when an ORM handles logical type conversions.
    pub fn execute_native(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.execute_bound(sql, params, false)
    }
    /// The rowid of the last successful INSERT on this connection.
    pub fn last_insert_rowid(&self) -> i64 {
        self.connection.last_insert_rowid()
    }
    /// Prepare a statement for validation and metadata, without running it.
    /// Execution uses the same connection's SQLite prepared statement cache.
    pub fn statement_info(&mut self, sql: &str) -> Result<StatementInfo> {
        if self.poisoned {
            return Err(Error::new(
                "connection must be reopened after a publication failure".into(),
            )
            .with_code("BOXDB_UNAVAILABLE"));
        }
        let statements = split(sql)?;
        let [sql] = statements.as_slice() else {
            return Err(Error::new("preparation requires one statement".into()));
        };
        self.refresh(false)?;
        let declaration = schema::declaration(sql)?;
        let alteration = if schema::keyword(sql).eq_ignore_ascii_case("ALTER") {
            schema::alteration(sql)?
        } else {
            None
        };
        let translated = declaration
            .as_ref()
            .map(|d| d.sql.as_str())
            .or_else(|| alteration.as_ref().map(|a| a.sql.as_str()))
            .unwrap_or(sql);
        let statement = self.connection.prepare_cached(translated)?;
        Ok(StatementInfo {
            columns: statement
                .column_names()
                .into_iter()
                .map(String::from)
                .collect(),
            parameters: (1..=statement.parameter_count())
                .map(|i| statement.parameter_name(i).map(String::from))
                .collect(),
            readonly: statement.readonly(),
        })
    }
    fn execute_bound(&mut self, sql: &str, params: &[Value], typed: bool) -> Result<QueryResult> {
        let statements = split(sql)?;
        let [sql] = statements.as_slice() else {
            return Err(Error::new(
                "parameterized execution requires one statement".into(),
            ));
        };
        let values = params
            .iter()
            .map(schema::encode)
            .collect::<Result<Vec<_>>>()?;
        self.execute_one(sql, &values, typed)
    }
    fn execute_one(&mut self, sql: &str, params: &[SqlValue], typed: bool) -> Result<QueryResult> {
        if self.poisoned {
            return Err(Error::new(
                "connection must be reopened after a publication failure".into(),
            )
            .with_code("BOXDB_UNAVAILABLE"));
        }
        let keyword = schema::keyword(sql);
        if keyword.eq_ignore_ascii_case("SELECT") || keyword.eq_ignore_ascii_case("VALUES") {
            return self.run(sql, params, typed);
        }
        let keyword = keyword.to_ascii_uppercase();
        let declaration = schema::declaration(sql)?;
        if let Some(declaration) = declaration {
            if declaration
                .table
                .to_ascii_lowercase()
                .starts_with("__boxdb_")
            {
                return Err(Error::new("__boxdb_ names are reserved".into()));
            }
            let _guard = InternalGuard::enter(&self.internal);
            if !params.is_empty() {
                return Err(Error::new("DDL parameters are unsupported".into()));
            }
            if self.connection.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {}.sqlite_schema WHERE name=?1 COLLATE NOCASE)",
                    schema::quote(&declaration.schema)
                ),
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
                declaration.execute(&self.connection)?;
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
        let result = self
            .run(sql, params, typed)
            .map(|result| if ddl { empty() } else { result });
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
                if self.readable.is_some()
                    && self.connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND type='virtual')",
                        [],
                        |row| row.get::<_, bool>(0),
                    )?
                {
                    return Err(Error::new("virtual tables require binary storage; readable storage does not support them".into()));
                }
                if let Some(change) = &alteration {
                    schema::save_alteration(&self.connection, change)?;
                    if !change.columns.is_empty() {
                        let types = schema::load(&self.connection)?;
                        schema::validate_all(&self.connection, &types)?;
                    }
                }
                schema::cleanup(&self.connection)?;
                schema::configure_codec(&self.connection, false)?;
                self.refresh(true)?;
                if alteration.as_ref().is_some_and(|a| !a.columns.is_empty()) {
                    schema::normalize_arrays(&self.connection, &self.types.borrow())?;
                }
            }
            if guarded {
                // An internal savepoint nested inside a user transaction must
                // not publish that transaction's pending readable data.
                if outer_autocommit && let Some(file) = &self.readable {
                    storage::check_commit_constraints(&self.connection)?;
                    if let Err(e) = file.publish(&self.connection) {
                        self.poisoned = true;
                        return Err(e);
                    }
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
    fn run(&self, sql: &str, params: &[SqlValue], typed: bool) -> Result<QueryResult> {
        let params: Vec<_> = params
            .iter()
            .cloned()
            .map(decimal::WireValue::new)
            .collect();
        let mut statement = self.connection.prepare_cached(sql)?;
        let before = self.connection.total_changes();
        let readonly = statement.readonly();
        let mut rows = vec![];
        if statement.column_count() == 0 {
            statement.execute(rusqlite::params_from_iter(&params))?;
        } else {
            let mut hints: Option<Rc<[Option<Type>]>> = None;
            let mut cursor = statement.query(rusqlite::params_from_iter(&params))?;
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
                            if typed {
                                schema::decode(ty, value)?
                            } else {
                                schema::decode(ty, value.clone())?;
                                schema::native(value)
                            }
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
            let text = CString::new(prefix)
                .map_err(|error| Error::new("SQL contains NUL".into()).with_source(error))?;
            // SAFETY: CString provides a valid NUL-terminated string for this call.
            if unsafe { rusqlite::ffi::sqlite3_complete(text.as_ptr()) } != 0 {
                if !schema::keyword(prefix).is_empty() {
                    statements.push(prefix);
                }
                start = i + 1;
            }
        }
    }
    if !schema::keyword(&sql[start..]).is_empty() {
        statements.push(&sql[start..]);
    }
    Ok(statements)
}
