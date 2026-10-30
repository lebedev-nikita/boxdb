//! Change recording, transaction context and commit triggers. Recording uses
//! SQLite's preupdate hook from Rust: the hook keeps each touched row's
//! original storage cells, and the changeset compares them with the current
//! table contents, so savepoint rollbacks need no special handling.
use crate::{
    Error, Result,
    changeset::{self, Operation, RawChange, Record},
    decimal::WireValue,
    schema,
};
use rusqlite::{
    Connection, ffi,
    functions::{Context, FunctionFlags},
    hooks::PreUpdateCase,
    types::{Value as SqlValue, ValueRef},
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

pub(crate) mod apply;
pub(crate) mod rows;
pub(crate) mod triggers;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

struct TableInfo {
    name: String,
    /// Declared column count, including generated columns the hook enumerates.
    total: usize,
    /// Storage column names with their declaration index.
    columns: Vec<(String, usize)>,
    primary_key: Vec<bool>,
}
enum Origin {
    Inserted,
    Existing(Vec<SqlValue>),
}

#[derive(Default)]
pub(crate) struct Recorder {
    pub active: bool,
    pub paused: bool,
    pub explicit: bool,
    pub skip_triggers: bool,
    tables: HashMap<String, TableInfo>,
    rows: BTreeMap<String, BTreeMap<Vec<u8>, Origin>>,
    broken: BTreeMap<String, String>,
}
impl Recorder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn start(&mut self, db: &Connection) -> Result<()> {
        if !self.active {
            self.rows.clear();
            self.broken.clear();
            self.active = true;
        }
        self.load_tables(db)
    }
    pub fn load_tables(&mut self, db: &Connection) -> Result<()> {
        let names: Vec<String> = db
            .prepare_cached("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%' AND name NOT GLOB '__boxdb_*'")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut tables = HashMap::new();
        for name in names {
            let mut statement = db.prepare_cached(
                "SELECT name, pk, hidden FROM pragma_table_xinfo(?1,'main') ORDER BY cid",
            )?;
            let mut columns = vec![];
            let mut primary_key = vec![];
            let mut total = 0usize;
            for row in statement.query_map([&name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })? {
                let (column, pk, hidden) = row?;
                if hidden == 0 {
                    columns.push((column, total));
                    primary_key.push(pk > 0);
                }
                total += 1;
            }
            if !primary_key.iter().any(|pk| *pk) {
                continue;
            }
            tables.insert(
                name.to_lowercase(),
                TableInfo {
                    name,
                    total,
                    columns,
                    primary_key,
                },
            );
        }
        // Rows recorded under a previous layout cannot be compared any more.
        for (key, previous) in &self.tables {
            if self.rows.contains_key(key)
                && !tables.get(key).is_some_and(|table| {
                    table.columns.len() == previous.columns.len()
                        && table.primary_key == previous.primary_key
                })
            {
                self.broken
                    .insert(key.clone(), "table layout changed during recording".into());
            }
        }
        self.tables = tables;
        Ok(())
    }
    /// Record one row change from the preupdate hook.
    ///
    /// # Safety
    /// `handle` must be the connection invoking the hook.
    pub unsafe fn record(
        &mut self,
        handle: *mut ffi::sqlite3,
        database: &str,
        table: &str,
        case: &PreUpdateCase,
    ) {
        if !self.active || self.paused || database != "main" {
            return;
        }
        let key = table.to_lowercase();
        let Some(info) = self.tables.get(&key) else {
            return;
        };
        if let PreUpdateCase::Unknown = case {
            self.broken.insert(key, "unsupported change".into());
            return;
        }
        // SAFETY: the hook is running for this connection and row.
        if unsafe { ffi::sqlite3_preupdate_count(handle) } != info.total as i32 {
            self.broken
                .insert(key, "table layout changed during recording".into());
            return;
        }
        let read = |old: bool| -> Result<Vec<SqlValue>> {
            info.columns
                .iter()
                // SAFETY: indexes come from the current table layout.
                .map(|(_, index)| unsafe { preupdate_value(handle, old, *index as i32) })
                .collect()
        };
        let pk_key = |values: &[SqlValue]| -> Option<Vec<u8>> {
            let mut bytes = vec![];
            for (value, pk) in values.iter().zip(&info.primary_key) {
                if *pk {
                    if *value == SqlValue::Null {
                        return None;
                    }
                    changeset::write_key(&mut bytes, value);
                }
            }
            Some(bytes)
        };
        let result = (|| -> Result<()> {
            let rows = self.rows.entry(key.clone()).or_default();
            match case {
                PreUpdateCase::Insert(_) => {
                    if let Some(key) = pk_key(&read(false)?) {
                        rows.entry(key).or_insert(Origin::Inserted);
                    }
                }
                PreUpdateCase::Delete(_) => {
                    let old = read(true)?;
                    if let Some(key) = pk_key(&old) {
                        rows.entry(key).or_insert(Origin::Existing(old));
                    }
                }
                PreUpdateCase::Update { .. } => {
                    let old = read(true)?;
                    let new_key = pk_key(&read(false)?);
                    if let Some(key) = pk_key(&old) {
                        if new_key.as_ref() != Some(&key)
                            && let Some(new_key) = new_key
                        {
                            rows.entry(new_key).or_insert(Origin::Inserted);
                        }
                        rows.entry(key).or_insert(Origin::Existing(old));
                    }
                }
                PreUpdateCase::Unknown => unreachable!(),
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.broken.insert(key, error.to_string());
        }
    }
    /// Compare recorded originals with the current rows of the transaction.
    pub fn changeset(&self, db: &Connection, cached: bool) -> Result<Vec<RawChange>> {
        if let Some((table, reason)) = self.broken.iter().next() {
            return Err(Error::new(format!(
                "cannot record changes to {table}: {reason}"
            )));
        }
        let mut changes = vec![];
        for (key, rows) in &self.rows {
            let Some(info) = self.tables.get(key) else {
                continue;
            };
            let columns = info
                .columns
                .iter()
                .map(|(name, _)| schema::quote(name))
                .collect::<Vec<_>>()
                .join(",");
            let conditions = info
                .columns
                .iter()
                .zip(&info.primary_key)
                .filter(|(_, pk)| **pk)
                .enumerate()
                .map(|(i, ((name, _), _))| format!("{}=?{}", schema::quote(name), i + 1))
                .collect::<Vec<_>>()
                .join(" AND ");
            let sql = format!(
                "SELECT {columns} FROM main.{} WHERE {conditions}",
                schema::quote(&info.name)
            );
            let mut cached_statement;
            let mut fresh_statement;
            let statement: &mut rusqlite::Statement<'_> = if cached {
                cached_statement = db.prepare_cached(&sql)?;
                &mut cached_statement
            } else {
                fresh_statement = db.prepare(&sql)?;
                &mut fresh_statement
            };
            for (key_bytes, origin) in rows {
                let key_values = changeset::read_key(key_bytes)?;
                let parameters: Vec<WireValue> =
                    key_values.into_iter().map(WireValue::new).collect();
                let mut cursor = statement.query(rusqlite::params_from_iter(&parameters))?;
                let current: Option<Vec<SqlValue>> = match cursor.next()? {
                    Some(row) => Some(
                        (0..info.columns.len())
                            .map(|i| row.get::<_, SqlValue>(i))
                            .collect::<rusqlite::Result<_>>()?,
                    ),
                    None => None,
                };
                drop(cursor);
                let change = match (origin, current) {
                    (Origin::Inserted, Some(current)) => Some(RawChange {
                        table: info.name.clone(),
                        primary_key: info.primary_key.clone(),
                        operation: Operation::Insert,
                        indirect: false,
                        old: None,
                        new: Some(current.into_iter().map(Some).collect()),
                    }),
                    (Origin::Inserted, None) => None,
                    (Origin::Existing(old), None) => Some(RawChange {
                        table: info.name.clone(),
                        primary_key: info.primary_key.clone(),
                        operation: Operation::Delete,
                        indirect: false,
                        old: Some(old.iter().cloned().map(Some).collect()),
                        new: None,
                    }),
                    (Origin::Existing(old), Some(current)) => {
                        let mut masked_old: Record = vec![None; old.len()];
                        let mut masked_new: Record = vec![None; old.len()];
                        let mut changed = false;
                        for (i, pk) in info.primary_key.iter().enumerate() {
                            if *pk {
                                masked_old[i] = Some(old[i].clone());
                            } else if old[i] != current[i] {
                                masked_old[i] = Some(old[i].clone());
                                masked_new[i] = Some(current[i].clone());
                                changed = true;
                            }
                        }
                        changed.then(|| RawChange {
                            table: info.name.clone(),
                            primary_key: info.primary_key.clone(),
                            operation: Operation::Update,
                            indirect: false,
                            old: Some(masked_old),
                            new: Some(masked_new),
                        })
                    }
                };
                changes.extend(change);
            }
        }
        Ok(changes)
    }
}

/// # Safety
/// Only valid while the preupdate hook runs for `handle`.
unsafe fn preupdate_value(handle: *mut ffi::sqlite3, old: bool, index: i32) -> Result<SqlValue> {
    let mut value: *mut ffi::sqlite3_value = std::ptr::null_mut();
    // SAFETY: the caller guarantees an active preupdate hook on this handle.
    let code = unsafe {
        if old {
            ffi::sqlite3_preupdate_old(handle, index, &mut value)
        } else {
            ffi::sqlite3_preupdate_new(handle, index, &mut value)
        }
    };
    if code != ffi::SQLITE_OK || value.is_null() {
        return Err(Error::sqlite(
            code,
            "cannot read the row being changed".into(),
        ));
    }
    // SAFETY: SQLite owns the value for the duration of the hook.
    unsafe {
        Ok(match ffi::sqlite3_value_type(value) {
            ffi::SQLITE_NULL => SqlValue::Null,
            ffi::SQLITE_INTEGER => SqlValue::Integer(ffi::sqlite3_value_int64(value)),
            ffi::SQLITE_FLOAT => SqlValue::Real(ffi::sqlite3_value_double(value)),
            ffi::SQLITE_TEXT => {
                let text = ffi::sqlite3_value_text(value);
                let length = ffi::sqlite3_value_bytes(value);
                let bytes = if text.is_null() || length <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(text.cast::<u8>(), length as usize)
                };
                SqlValue::Text(
                    std::str::from_utf8(bytes)
                        .map_err(Error::validation)?
                        .to_owned(),
                )
            }
            _ => {
                let blob = ffi::sqlite3_boxdb_value_blob(value);
                let length = ffi::sqlite3_boxdb_value_bytes(value);
                let bytes = if blob.is_null() || length <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(blob.cast::<u8>(), length as usize)
                };
                SqlValue::Blob(bytes.to_vec())
            }
        })
    }
}

/// Connection state shared by the preupdate hook and SQL functions.
#[derive(Default)]
pub(crate) struct Shared {
    pub recorder: Mutex<Recorder>,
    pub context: Mutex<BTreeMap<String, SqlValue>>,
    /// The changeset a running commit trigger body sees.
    pub stash: Mutex<Option<Vec<u8>>>,
    pub readable: Mutex<Option<Arc<Mutex<crate::readable::cache::Changes>>>>,
    /// Whether the connection has commit triggers, so SQL functions that
    /// write can start recording like ordinary write statements.
    pub triggered: std::sync::atomic::AtomicBool,
}
impl Shared {
    pub fn reset(&self) {
        lock(&self.recorder).reset();
        lock(&self.context).clear();
        *lock(&self.stash) = None;
    }
}

pub(crate) fn install_hook(db: &Connection, shared: Arc<Shared>) -> Result<()> {
    // SAFETY: the handle is only compared and passed back to SQLite inside the hook.
    let handle = unsafe { db.handle() } as usize;
    db.preupdate_hook(Some(
        move |_, database: &str, table: &str, case: &PreUpdateCase| {
            // SAFETY: the hook runs on the connection whose handle was captured.
            unsafe {
                lock(&shared.recorder).record(handle as *mut ffi::sqlite3, database, table, case);
            }
            if let Some(changes) = lock(&shared.readable).as_ref() {
                crate::readable::cache::record(changes, database, table, case);
            }
        },
    ))?;
    Ok(())
}

/// Report a failure from a SQL function. Engine result codes survive the
/// round trip; other failures surface as SQLITE_CONSTRAINT_FUNCTION.
fn user_error(error: Error) -> rusqlite::Error {
    match error.extended_code() {
        Some(code) => {
            rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(error.to_string()))
        }
        None => rusqlite::Error::UserFunctionError(Box::new(error)),
    }
}
fn connection<'a>(
    ctx: &'a Context<'_>,
) -> rusqlite::Result<rusqlite::functions::ConnectionRef<'a>> {
    // SAFETY: the function runs on the connection that owns this context.
    unsafe { ctx.get_connection() }
}
pub(crate) fn blob_result(bytes: Vec<u8>) -> WireValue {
    WireValue::new(SqlValue::Blob(crate::decimal::encode_blob(&bytes)))
}
pub(crate) fn blob_argument(ctx: &Context<'_>, index: usize) -> rusqlite::Result<Vec<u8>> {
    match ctx.get_raw(index) {
        ValueRef::Blob(bytes) => Ok(bytes.to_vec()),
        ValueRef::Null => Ok(vec![]),
        _ => Err(user_error(Error::new(
            "expected a CHANGESET or BLOB argument".into(),
        ))),
    }
}

/// Register the SQL functions. Import and inspection connections receive
/// state that never records; `Database` installs its own shared state.
pub(crate) fn register(db: &Connection, shared: Arc<Shared>) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
    let state = shared.clone();
    db.create_scalar_function("boxdb_record", 1, flags, move |ctx| {
        let enabled: i64 = ctx.get(0)?;
        let db = connection(ctx)?;
        if db.is_autocommit() {
            return Err(user_error(Error::new(
                "boxdb_record requires an active transaction".into(),
            )));
        }
        let mut recorder = lock(&state.recorder);
        if enabled != 0 {
            recorder.explicit = true;
            recorder.skip_triggers = false;
            recorder.start(&db).map_err(user_error)?;
        } else {
            recorder.reset();
            recorder.skip_triggers = true;
        }
        Ok(enabled)
    })?;
    let state = shared.clone();
    db.create_scalar_function("boxdb_changeset", 0, flags, move |ctx| {
        if let Some(bytes) = lock(&state.stash).as_ref() {
            return Ok(Some(blob_result(bytes.clone())));
        }
        let db = connection(ctx)?;
        let recorder = lock(&state.recorder);
        if !recorder.active {
            return Err(user_error(Error::new(
                "no active change recording; call boxdb_record(1) or declare a commit trigger"
                    .into(),
            )));
        }
        let changes = recorder.changeset(&db, false).map_err(user_error)?;
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(blob_result(changeset::encode_raw(&changes, false))))
    })?;
    let state = shared.clone();
    db.create_scalar_function("boxdb_set_context", 2, flags, move |ctx| {
        let key: String = ctx.get(0)?;
        let value: SqlValue = ctx.get(1)?;
        let db = connection(ctx)?;
        if db.is_autocommit() {
            return Err(user_error(Error::new(
                "boxdb_set_context requires an active transaction".into(),
            )));
        }
        lock(&state.context).insert(key, value.clone());
        Ok(WireValue::new(value))
    })?;
    let state = shared.clone();
    db.create_scalar_function("boxdb_context", 1, flags, move |ctx| {
        let key: String = ctx.get(0)?;
        Ok(WireValue::new(
            lock(&state.context)
                .get(&key)
                .cloned()
                .unwrap_or(SqlValue::Null),
        ))
    })?;
    let pure = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    db.create_scalar_function("changeset_invert", 1, pure, |ctx| {
        if matches!(ctx.get_raw(0), ValueRef::Null) {
            return Ok(None);
        }
        let bytes = blob_argument(ctx, 0)?;
        Ok(Some(blob_result(
            changeset::invert(&bytes).map_err(user_error)?,
        )))
    })?;
    db.create_scalar_function("changeset_concat", 2, pure, |ctx| {
        let first = blob_argument(ctx, 0)?;
        let second = blob_argument(ctx, 1)?;
        let combined = changeset::concat(&first, &second).map_err(user_error)?;
        if combined.is_empty() {
            return Ok(None);
        }
        Ok(Some(blob_result(combined)))
    })?;
    apply::register(db, shared.clone())?;
    rows::register(db)?;
    Ok(())
}

/// Start recording for a transaction that commit triggers will inspect.
pub(crate) fn ensure_recording(shared: &Shared, db: &Connection) -> Result<()> {
    if !shared.triggered.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    let mut recorder = lock(&shared.recorder);
    if !recorder.active && !recorder.skip_triggers {
        recorder.start(db)?;
    }
    Ok(())
}
