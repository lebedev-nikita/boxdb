use super::{Config, Engine, Result, Rows, Value};
use rusqlite::{Connection, types::ValueRef};

// Shared by the patched-core client benchmark and the isolated stock executable.
pub(crate) struct Sqlite {
    connection: Box<Connection>,
    cached: bool,
}

impl Sqlite {
    pub(crate) fn open(engine: Engine, path: &std::path::Path, config: &Config) -> Result<Self> {
        if !matches!(engine, Engine::Sqlite) {
            return Err("the stock executable only supports --engine sqlite".into());
        }
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL")?;
        db.pragma_update(None, "fullfsync", config.sqlite_fullfsync)?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(
            None,
            "locking_mode",
            if config.sqlite_exclusive {
                "EXCLUSIVE"
            } else {
                "NORMAL"
            },
        )?;
        db.set_prepared_statement_cache_capacity(512);
        Ok(Self {
            connection: Box::new(db),
            cached: config.sqlite_cached,
        })
    }

    pub(crate) fn execute(&mut self, sql: &str) -> Result<()> {
        if self.cached {
            self.connection.prepare_cached(sql)?.execute([])?;
        } else {
            self.connection.execute_batch(sql)?;
        }
        Ok(())
    }

    pub(crate) fn query(&mut self, sql: &str) -> Result<Rows> {
        if self.cached {
            let mut statement = self.connection.prepare_cached(sql)?;
            sqlite_rows(&mut statement)
        } else {
            sqlite_rows(&mut self.connection.prepare(sql)?)
        }
    }
}

fn sqlite_rows(statement: &mut rusqlite::Statement<'_>) -> Result<Rows> {
    let columns = statement.column_count();
    let mut cursor = statement.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut values = Vec::with_capacity(columns);
        for column in 0..columns {
            values.push(match row.get_ref(column)? {
                ValueRef::Integer(n) => Value::Integer(n),
                ValueRef::Text(s) => Value::Text(std::str::from_utf8(s)?.into()),
                other => return Err(format!("unexpected SQLite value: {other:?}").into()),
            });
        }
        rows.push(values);
    }
    Ok(rows)
}
