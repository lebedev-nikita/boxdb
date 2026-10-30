//! Apply changesets to the current transaction with declarative conflict
//! policies. The work runs inside the `boxdb_apply` SQL function so every
//! client shares one implementation; its writes pass through BoxDB's
//! validators and are recorded like any other change.
use super::{Shared, blob_argument, connection, user_error};
use crate::{
    Error, Result,
    changeset::{
        self, ApplyOptions, ApplyReport, Conflict, ConflictKind, ConflictPolicy, Operation,
        RawChange,
    },
    decimal::WireValue,
    schema,
};
use rusqlite::{Connection, functions::FunctionFlags, types::Value as SqlValue};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
struct Target {
    name: String,
    columns: Vec<String>,
    primary_key: Vec<bool>,
}
fn target(db: &Connection, table: &str, cache: &mut HashMap<String, Target>) -> Result<()> {
    let key = table.to_lowercase();
    if cache.contains_key(&key) {
        return Ok(());
    }
    let name: Option<String> = db
        .query_row(
            "SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name=?1 COLLATE NOCASE",
            [table],
            |row| row.get(0),
        )
        .ok();
    let Some(name) = name else {
        return Err(Error::new(format!(
            "cannot apply changes: no such main table: {table}"
        )));
    };
    let mut statement = db.prepare(
        "SELECT name, pk FROM pragma_table_xinfo(?1,'main') WHERE hidden=0 ORDER BY cid",
    )?;
    let mut columns = vec![];
    let mut primary_key = vec![];
    for row in statement.query_map([&name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (column, pk) = row?;
        columns.push(column);
        primary_key.push(pk > 0);
    }
    cache.insert(
        key,
        Target {
            name,
            columns,
            primary_key,
        },
    );
    Ok(())
}

fn json_key(values: &[SqlValue]) -> Vec<serde_json::Value> {
    values
        .iter()
        .map(|value| match value {
            SqlValue::Null => serde_json::Value::Null,
            SqlValue::Integer(n) => serde_json::json!(n),
            SqlValue::Real(n) => serde_json::json!(n),
            SqlValue::Text(text) => serde_json::Value::String(text.clone()),
            SqlValue::Blob(_) => {
                serde_json::Value::String(schema::native(value.clone()).to_string())
            }
        })
        .collect()
}

struct Applier<'a> {
    db: &'a Connection,
    policy: ConflictPolicy,
    targets: HashMap<String, Target>,
    report: ApplyReport,
}
impl Applier<'_> {
    fn current(&self, target: &Target, key: &[(usize, SqlValue)]) -> Result<Option<Vec<SqlValue>>> {
        let columns = target
            .columns
            .iter()
            .map(|name| schema::quote(name))
            .collect::<Vec<_>>()
            .join(",");
        let conditions = key
            .iter()
            .enumerate()
            .map(|(i, (column, _))| {
                format!("{}=?{}", schema::quote(&target.columns[*column]), i + 1)
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut statement = self.db.prepare(&format!(
            "SELECT {columns} FROM main.{} WHERE {conditions}",
            schema::quote(&target.name)
        ))?;
        let parameters: Vec<WireValue> = key
            .iter()
            .map(|(_, value)| WireValue::new(value.clone()))
            .collect();
        let mut rows = statement.query(rusqlite::params_from_iter(&parameters))?;
        match rows.next()? {
            Some(row) => Ok(Some(
                (0..target.columns.len())
                    .map(|i| row.get::<_, SqlValue>(i))
                    .collect::<rusqlite::Result<_>>()?,
            )),
            None => Ok(None),
        }
    }
    fn conflict(
        &mut self,
        change: &RawChange,
        kind: ConflictKind,
        key: &[(usize, SqlValue)],
    ) -> Result<bool> {
        let key_values: Vec<SqlValue> = key.iter().map(|(_, value)| value.clone()).collect();
        if self.policy == ConflictPolicy::Abort {
            return Err(Error::new(format!(
                "changeset conflict ({}) applying {} to {} {}",
                kind.name(),
                change.operation.name(),
                change.table,
                serde_json::Value::Array(json_key(&key_values))
            )));
        }
        self.report.conflicts.push(Conflict {
            table: change.table.clone(),
            operation: change.operation,
            kind,
            key: json_key(&key_values),
        });
        // Replace resolves data conflicts by writing anyway; every other
        // conflict kind, and the skip policy, omit the change.
        let apply = self.policy == ConflictPolicy::Replace
            && matches!(kind, ConflictKind::Data | ConflictKind::Conflict);
        if !apply {
            self.report.skipped += 1;
        }
        Ok(apply)
    }
    fn write(&mut self, change: &RawChange, sql: &str, parameters: &[SqlValue]) -> Result<()> {
        let parameters: Vec<WireValue> = parameters.iter().cloned().map(WireValue::new).collect();
        let result = self
            .db
            .prepare(sql)
            .and_then(|mut statement| statement.execute(rusqlite::params_from_iter(&parameters)));
        match result {
            Ok(_) => {
                self.report.applied += 1;
                Ok(())
            }
            Err(error) => {
                let error = Error::from(error);
                let constraint = error.result_code() == Some(rusqlite::ffi::SQLITE_CONSTRAINT);
                if !constraint || self.policy == ConflictPolicy::Abort {
                    return Err(error.with_context(format!(
                        "applying {} to {}",
                        change.operation.name(),
                        change.table
                    )));
                }
                let key: Vec<(usize, SqlValue)> = change
                    .old
                    .as_ref()
                    .or(change.new.as_ref())
                    .unwrap()
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| change.primary_key[*i])
                    .map(|(i, value)| (i, value.clone().unwrap_or(SqlValue::Null)))
                    .collect();
                self.conflict(change, ConflictKind::Constraint, &key)?;
                Ok(())
            }
        }
    }
    fn apply(&mut self, change: &RawChange) -> Result<()> {
        target(self.db, &change.table, &mut self.targets)?;
        let target = &self.targets[&change.table.to_lowercase()].clone();
        let columns = change.primary_key.len();
        if target.columns.len() < columns || target.primary_key[..columns] != change.primary_key[..]
        {
            return Err(Error::new(format!(
                "cannot apply changes: the layout of {} no longer matches",
                target.name
            )));
        }
        let record = change.old.as_ref().or(change.new.as_ref()).unwrap();
        let key: Vec<(usize, SqlValue)> = (0..columns)
            .filter(|i| change.primary_key[*i])
            .map(|i| (i, record[i].clone().unwrap_or(SqlValue::Null)))
            .collect();
        let table = schema::quote(&target.name);
        let condition = key
            .iter()
            .enumerate()
            .map(|(i, (column, _))| {
                format!("{}=?{}", schema::quote(&target.columns[*column]), i + 1)
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let current = self.current(target, &key)?;
        let differs = |record: &changeset::Record, current: &[SqlValue]| {
            (0..columns).any(|i| {
                !change.primary_key[i] && record[i].as_ref().is_some_and(|old| *old != current[i])
            })
        };
        match change.operation {
            Operation::Insert => {
                let new = change.new.as_ref().unwrap();
                if let Some(current) = current {
                    let same = (0..columns).all(|i| new[i].as_ref() == Some(&current[i]));
                    if same || !self.conflict(change, ConflictKind::Conflict, &key)? {
                        if same {
                            self.report.skipped += 1;
                        }
                        return Ok(());
                    }
                    let (sets, mut values) = assignments(target, new, &change.primary_key);
                    if sets.is_empty() {
                        self.report.applied += 1;
                        return Ok(());
                    }
                    let offset = values.len();
                    values.extend(key.iter().map(|(_, value)| value.clone()));
                    let condition = key
                        .iter()
                        .enumerate()
                        .map(|(i, (column, _))| {
                            format!(
                                "{}=?{}",
                                schema::quote(&target.columns[*column]),
                                offset + i + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    let sql = format!(
                        "UPDATE main.{table} SET {} WHERE {condition}",
                        sets.join(",")
                    );
                    return self.write(change, &sql, &values);
                }
                let names = (0..columns)
                    .map(|i| schema::quote(&target.columns[i]))
                    .collect::<Vec<_>>()
                    .join(",");
                let placeholders = (1..=columns)
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let values: Vec<SqlValue> = new
                    .iter()
                    .map(|value| value.clone().unwrap_or(SqlValue::Null))
                    .collect();
                let sql = format!("INSERT INTO main.{table}({names}) VALUES({placeholders})");
                self.write(change, &sql, &values)
            }
            Operation::Delete => {
                let old = change.old.as_ref().unwrap();
                let Some(current) = current else {
                    self.conflict(change, ConflictKind::NotFound, &key)?;
                    return Ok(());
                };
                if differs(old, &current) && !self.conflict(change, ConflictKind::Data, &key)? {
                    return Ok(());
                }
                let values: Vec<SqlValue> = key.iter().map(|(_, value)| value.clone()).collect();
                let sql = format!("DELETE FROM main.{table} WHERE {condition}");
                self.write(change, &sql, &values)
            }
            Operation::Update => {
                let new = change.new.as_ref().unwrap();
                let Some(current) = current else {
                    self.conflict(change, ConflictKind::NotFound, &key)?;
                    return Ok(());
                };
                if let Some(old) = &change.old
                    && differs(old, &current)
                    && !self.conflict(change, ConflictKind::Data, &key)?
                {
                    return Ok(());
                }
                let (sets, mut values) = assignments(target, new, &change.primary_key);
                if sets.is_empty() {
                    self.report.applied += 1;
                    return Ok(());
                }
                let offset = values.len();
                values.extend(key.iter().map(|(_, value)| value.clone()));
                let condition = key
                    .iter()
                    .enumerate()
                    .map(|(i, (column, _))| {
                        format!(
                            "{}=?{}",
                            schema::quote(&target.columns[*column]),
                            offset + i + 1
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let sql = format!(
                    "UPDATE main.{table} SET {} WHERE {condition}",
                    sets.join(",")
                );
                self.write(change, &sql, &values)
            }
        }
    }
}
fn assignments(
    target: &Target,
    record: &changeset::Record,
    primary_key: &[bool],
) -> (Vec<String>, Vec<SqlValue>) {
    let mut sets = vec![];
    let mut values = vec![];
    for (i, value) in record.iter().enumerate() {
        if primary_key[i] {
            continue;
        }
        if let Some(value) = value {
            sets.push(format!(
                "{}=?{}",
                schema::quote(&target.columns[i]),
                values.len() + 1
            ));
            values.push(value.clone());
        }
    }
    (sets, values)
}

/// Apply a changeset inside the connection's active transaction.
pub(crate) fn apply(db: &Connection, bytes: &[u8], options: &ApplyOptions) -> Result<ApplyReport> {
    if db.is_autocommit() {
        return Err(Error::new(
            "boxdb_apply requires an active transaction".into(),
        ));
    }
    let decoded = changeset::decode_raw(bytes)?;
    let changes = if options.invert {
        if decoded.patchset {
            return Err(Error::new("patchsets cannot be inverted".into()));
        }
        decoded
            .changes
            .iter()
            .map(changeset::invert_raw)
            .collect::<Result<Vec<_>>>()?
    } else {
        decoded.changes
    };
    db.execute_batch("SAVEPOINT __boxdb_apply; PRAGMA defer_foreign_keys=ON")?;
    let mut applier = Applier {
        db,
        policy: options.on_conflict,
        targets: HashMap::new(),
        report: ApplyReport::default(),
    };
    let result = changes.iter().try_for_each(|change| applier.apply(change));
    match result {
        Ok(()) => {
            db.execute_batch("RELEASE __boxdb_apply")?;
            Ok(applier.report)
        }
        Err(error) => {
            if let Err(cleanup) =
                db.execute_batch("ROLLBACK TO __boxdb_apply; RELEASE __boxdb_apply")
            {
                return Err(error.cleanup(cleanup.into()));
            }
            Err(error)
        }
    }
}

pub(crate) fn register(db: &Connection, shared: Arc<Shared>) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
    db.create_scalar_function("boxdb_apply", 2, flags, move |ctx| {
        let bytes = blob_argument(ctx, 0)?;
        let options: ApplyOptions = match ctx.get_raw(1) {
            rusqlite::types::ValueRef::Null => ApplyOptions::default(),
            rusqlite::types::ValueRef::Text(text) => {
                serde_json::from_slice(text).map_err(|error| {
                    user_error(Error::new(format!("invalid apply options: {error}")))
                })?
            }
            _ => {
                return Err(user_error(Error::new(
                    "apply options must be a JSON object".into(),
                )));
            }
        };
        let db = connection(ctx)?;
        super::ensure_recording(&shared, &db).map_err(user_error)?;
        let report = apply(&db, &bytes, &options).map_err(user_error)?;
        serde_json::to_string(&report).map_err(|error| user_error(Error::from(error)))
    })?;
    Ok(())
}
