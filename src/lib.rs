//! A strict, typed embedded database with atomic snapshots and S3 replication.
//!
//! ```
//! use boxdb::{Database, Value};
//! let mut db = Database::memory();
//! db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")?;
//! db.execute_params("INSERT INTO users(name) VALUES(?)", &[Value::Text("Ada".into())])?;
//! assert_eq!(db.execute("SELECT name FROM users")?[0].rows,
//!            vec![vec![Value::Text("Ada".into())]]);
//! # Ok::<(), boxdb::Error>(())
//! ```
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

pub mod types;
pub use types::{Type, Value};
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

mod cache;
mod engine;
mod expression;
pub mod replication;
mod rows;
mod storage;
use engine::State;
pub struct Database {
    state: State,
    transaction: Option<State>,
    savepoints: Vec<(String, State)>,
    storage: Option<storage::Storage>,
    savepoint_started: bool,
    cache: cache::ParseCache,
    plans: std::collections::HashMap<String, engine::Plan>,
    plan_bytes: usize,
}
impl Database {
    /// Create an independent, nonpersistent database.
    pub fn memory() -> Self {
        Self {
            state: State::default(),
            transaction: None,
            savepoint_started: false,
            cache: cache::ParseCache::default(),
            plans: std::collections::HashMap::new(),
            plan_bytes: 0,
            savepoints: vec![],
            storage: None,
        }
    }
    /// Open or create a file and hold its exclusive connection lock until dropped.
    /// Existing files determine their own format; `format` applies to new files.
    pub fn open(path: impl AsRef<std::path::Path>, format: Format) -> Result<Self> {
        let (storage, state) = storage::Storage::open(path.as_ref(), format)?;
        Ok(Self {
            state,
            storage: Some(storage),
            transaction: None,
            savepoint_started: false,
            cache: cache::ParseCache::default(),
            plans: std::collections::HashMap::new(),
            plan_bytes: 0,
            savepoints: vec![],
        })
    }
    /// Export committed data; an active transaction's pending changes are excluded.
    pub fn export(&self, format: Format) -> Result<Vec<u8>> {
        storage::encode(self.transaction.as_ref().unwrap_or(&self.state), format)
    }
    /// Validate and atomically restore a snapshot. Never overwrite an existing file.
    pub fn restore(path: impl AsRef<std::path::Path>, bytes: &[u8]) -> Result<Self> {
        let (state, format) = storage::decode(bytes)?;
        let storage = storage::Storage::create(path.as_ref(), format, &state)?;
        Ok(Self {
            state,
            storage: Some(storage),
            transaction: None,
            savepoint_started: false,
            cache: cache::ParseCache::default(),
            plans: std::collections::HashMap::new(),
            plan_bytes: 0,
            savepoints: vec![],
        })
    }
    /// Load validated snapshot bytes into an independent in-memory database.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let (state, _) = storage::decode(bytes)?;
        Ok(Self {
            state,
            storage: None,
            transaction: None,
            savepoint_started: false,
            cache: cache::ParseCache::default(),
            plans: std::collections::HashMap::new(),
            plan_bytes: 0,
            savepoints: vec![],
        })
    }
    /// Read a validated committed snapshot while a writer may be active.
    /// An incomplete trailing binary commit is excluded from the returned bytes.
    pub fn read_snapshot(path: impl AsRef<std::path::Path>) -> Result<Vec<u8>> {
        storage::read_snapshot(path.as_ref())
    }
    pub fn format(&self) -> Option<Format> {
        self.storage.as_ref().map(|s| s.format)
    }
    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }
    /// Execute one statement with typed positional `?`, `?1`, or `$1` parameters.
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<Vec<QueryResult>> {
        use sqlparser::ast::{Expr, Value as SqlValue, visit_expressions_mut};
        let mut statements = self.cache.get(sql)?;
        if statements.len() != 1 {
            return Err(Error(
                "parameterized execution requires one statement".into(),
            ));
        }
        let mut highest = 0;
        if let std::ops::ControlFlow::Break(error) =
            visit_expressions_mut(std::sync::Arc::make_mut(&mut statements), |expr| {
                if let Expr::Value(SqlValue::Placeholder(placeholder)) = expr {
                    let index = if placeholder == "?" {
                        highest + 1
                    } else {
                        match placeholder
                            .strip_prefix('?')
                            .or_else(|| placeholder.strip_prefix('$'))
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|n| *n > 0)
                        {
                            Some(index) => index,
                            None => {
                                return std::ops::ControlFlow::Break(Error(
                                    "invalid positional parameter".into(),
                                ));
                            }
                        }
                    };
                    let Some(value) = params.get(index - 1) else {
                        return std::ops::ControlFlow::Break(Error("missing parameter".into()));
                    };
                    highest = highest.max(index);
                    *expr = engine::value_expression(value);
                }
                std::ops::ControlFlow::Continue(())
            })
        {
            return Err(error);
        }
        if highest != params.len() {
            return Err(Error("parameter count mismatch".into()));
        }
        if let Some(plan) = self.state.prepare(&statements[0])? {
            return Ok(vec![self.state.run(&plan)?]);
        }
        self.execute_statements(&statements)
    }
    /// Execute a SQL batch. Each statement is atomic; successful earlier statements
    /// remain applied if a later statement fails. Use BEGIN/COMMIT for a transaction.
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        if let Some(plan) = self.plans.get(sql) {
            return Ok(vec![self.state.run(plan)?]);
        }
        if let Some(insert) = engine::insert::parse(sql) {
            let mut next = self.state.clone();
            let result = next.insert_literals(insert)?;
            next.validate_changes(&self.state, true)?;
            if self.transaction.is_none()
                && let Some(storage) = &self.storage
            {
                storage.publish(&next, &self.state)?;
            }
            self.state = next;
            return Ok(vec![result]);
        }
        let statements = self.cache.get(sql)?;
        if statements.len() == 1
            && let Some(plan) = self.state.prepare(&statements[0])?
        {
            let result = self.state.run(&plan)?;
            if sql.len() <= 64 * 1024 {
                if self.plans.len() >= 512 || self.plan_bytes + sql.len() > 1024 * 1024 {
                    self.clear_plans();
                }
                self.plan_bytes += sql.len();
                self.plans.insert(sql.into(), plan);
            }
            return Ok(vec![result]);
        }
        self.execute_statements(&statements)
    }
    fn clear_plans(&mut self) {
        self.plans.clear();
        self.plan_bytes = 0;
    }
    fn execute_statements(
        &mut self,
        statements: &[sqlparser::ast::Statement],
    ) -> Result<Vec<QueryResult>> {
        let mut results = Vec::new();
        for statement in statements {
            use sqlparser::ast::Statement;
            let empty = || QueryResult {
                columns: vec![],
                rows: vec![],
                affected: 0,
            };
            match &statement {
                Statement::StartTransaction { modes, .. } => {
                    if !modes.is_empty() {
                        return Err(Error("transaction modes are unsupported".into()));
                    }
                    if self.transaction.is_some() {
                        return Err(Error("transaction already active".into()));
                    }
                    self.transaction = Some(self.state.clone());
                    results.push(empty());
                    continue;
                }
                Statement::Commit { chain } => {
                    if *chain {
                        return Err(Error("transaction chaining unsupported".into()));
                    }
                    if self.transaction.is_none() {
                        return Err(Error("no active transaction".into()));
                    }
                    if let Some(storage) = &self.storage {
                        storage.publish(&self.state, self.transaction.as_ref().unwrap())?;
                    }
                    self.transaction = None;
                    self.savepoint_started = false;
                    self.savepoints.clear();
                    results.push(empty());
                    continue;
                }
                Statement::Rollback {
                    savepoint: Some(n), ..
                } => {
                    let index = self
                        .savepoints
                        .iter()
                        .rposition(|(name, _)| name.eq_ignore_ascii_case(&n.value))
                        .ok_or_else(|| Error("no such savepoint".into()))?;
                    self.clear_plans();
                    self.state = self.savepoints[index].1.clone();
                    self.savepoints.truncate(index + 1);
                    results.push(empty());
                    continue;
                }
                Statement::Rollback {
                    chain,
                    savepoint: None,
                } => {
                    if *chain {
                        return Err(Error("transaction chaining unsupported".into()));
                    }
                    self.clear_plans();
                    self.state = self
                        .transaction
                        .take()
                        .ok_or_else(|| Error("no active transaction".into()))?;
                    self.savepoints.clear();
                    self.savepoint_started = false;
                    results.push(empty());
                    continue;
                }
                Statement::Savepoint { name } => {
                    if self.transaction.is_none() {
                        self.transaction = Some(self.state.clone());
                        self.savepoint_started = true;
                    }
                    self.savepoints
                        .push((name.value.clone(), self.state.clone()));
                    results.push(empty());
                    continue;
                }
                Statement::ReleaseSavepoint { name } => {
                    let index = self
                        .savepoints
                        .iter()
                        .rposition(|(n, _)| n.eq_ignore_ascii_case(&name.value))
                        .ok_or_else(|| Error("no such savepoint".into()))?;
                    if index == 0 && self.savepoint_started {
                        if let Some(storage) = &self.storage {
                            storage.publish(&self.state, self.transaction.as_ref().unwrap())?;
                        }
                        self.transaction = None;
                        self.savepoint_started = false;
                    }
                    self.savepoints.truncate(index);
                    results.push(empty());
                    continue;
                }
                Statement::Query(_) => {
                    results.push(self.state.execute(statement)?);
                    continue;
                }
                _ => (),
            }
            let mut next = self.state.clone();
            let result = next.execute(statement)?;
            if !matches!(statement, Statement::Query(_)) {
                next.validate_changes(
                    &self.state,
                    matches!(
                        statement,
                        Statement::Insert(_) | Statement::Update { .. } | Statement::Delete(_)
                    ),
                )?;
            }
            if self.transaction.is_none()
                && !matches!(statement, Statement::Query(_))
                && let Some(storage) = &self.storage
            {
                storage.publish(&next, &self.state)?;
            }
            if !matches!(
                statement,
                Statement::Insert(_) | Statement::Update { .. } | Statement::Delete(_)
            ) {
                self.clear_plans();
            }
            self.state = next;
            results.push(result);
        }
        Ok(results)
    }
}
fn parse(sql: &str) -> Result<Vec<sqlparser::ast::Statement>> {
    use sqlparser::{
        dialect::{GenericDialect, PostgreSqlDialect, SQLiteDialect},
        parser::Parser,
    };
    Parser::parse_sql(&GenericDialect {}, sql)
        .or_else(|_| Parser::parse_sql(&PostgreSqlDialect {}, sql))
        .or_else(|_| Parser::parse_sql(&SQLiteDialect {}, sql))
        .map_err(|e| Error(e.to_string()))
}
