//! READONLY columns are enforced by persistent schema triggers, so every
//! SQLite connection rejects updates. BoxDB owns their canonical SQL.
use super::quote;
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use sqlparser::{
    ast::{ObjectType, Statement},
    dialect::{GenericDialect, SQLiteDialect},
    parser::Parser,
    tokenizer::{Token, Tokenizer},
};
use std::collections::BTreeSet;

const PREFIX: &str = "__boxdb_readonly_";
pub(crate) const MESSAGE: &str = "cannot UPDATE read-only column ";

// An INTEGER PRIMARY KEY column is the rowid, which UPDATE can also assign
// through each rowid name that no real column shadows.
fn rowid_names(db: &Connection, table: &str, column: &str) -> Result<Vec<String>> {
    let alias: bool = db.query_row(
        "SELECT t.wr=0 AND (SELECT count(*) FROM pragma_table_xinfo(?1,'main') WHERE pk>0)=1
            AND EXISTS(SELECT 1 FROM pragma_table_xinfo(?1,'main') WHERE name=?2 COLLATE NOCASE AND pk=1 AND upper(type)='INTEGER')
            AND NOT EXISTS(SELECT 1 FROM pragma_index_list(?1,'main') WHERE origin='pk')
         FROM pragma_table_list t WHERE t.schema='main' AND t.name=?1 COLLATE NOCASE",
        [table, column],
        |row| row.get(0),
    )?;
    if !alias {
        return Ok(vec![]);
    }
    let mut names = vec![];
    for name in ["rowid", "oid", "_rowid_"] {
        let shadowed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1,'main') WHERE name=?2 COLLATE NOCASE)",
            [table, name],
            |row| row.get(0),
        )?;
        if !shadowed {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}
// The stored SQL omits the schema; creation names main so a TEMP table
// with the same name cannot capture the trigger.
fn trigger(db: &Connection, table: &str, column: &str) -> Result<String> {
    let name = format!("{PREFIX}{}_{table}_{column}", table.len());
    let message = format!("{MESSAGE}{table}.{column}").replace('\'', "''");
    let columns = std::iter::once(column.to_owned())
        .chain(rowid_names(db, table, column)?)
        .map(|name| quote(&name))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "{} BEFORE UPDATE OF {columns} ON {} BEGIN SELECT RAISE(ABORT,'{message}'); END",
        quote(&name),
        quote(table),
    ))
}
pub(crate) fn declare(db: &Connection, table: &str, column: &str) -> Result<()> {
    check(db, table, column)?;
    db.execute_batch(&format!(
        "CREATE TRIGGER main.{}",
        trigger(db, table, column)?
    ))?;
    Ok(())
}
// Generated values and foreign-key update actions would rewrite the column.
fn check(db: &Connection, table: &str, column: &str) -> Result<()> {
    let generated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1,'main') WHERE name=?2 COLLATE NOCASE AND hidden IN(2,3))",
        [table, column],
        |row| row.get(0),
    )?;
    if generated {
        return Err(Error::new(format!(
            "generated column {table}.{column} cannot be READONLY"
        )));
    }
    let action: Option<String> = db
        .query_row(
            "SELECT on_update FROM pragma_foreign_key_list(?1,'main')
             WHERE \"from\"=?2 COLLATE NOCASE AND on_update IN('CASCADE','SET NULL','SET DEFAULT')",
            [table, column],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(action) = action {
        return Err(Error::new(format!(
            "READONLY column {table}.{column} cannot use ON UPDATE {action}"
        )));
    }
    Ok(())
}
struct Declared {
    name: String,
    table: String,
    column: Option<String>,
    sql: String,
}
impl Declared {
    fn canonical(&self, db: &Connection) -> Result<bool> {
        Ok(match &self.column {
            Some(column) => {
                self.sql == format!("CREATE TRIGGER {}", trigger(db, &self.table, column)?)
            }
            None => false,
        })
    }
}
// SQLite renames identifiers inside trigger SQL and leaves dropped columns in
// UPDATE OF lists. The first UPDATE OF entry names the protected column.
fn declared(db: &Connection) -> Result<Vec<Declared>> {
    let triggers = db
        .prepare("SELECT name,tbl_name,sql FROM main.sqlite_schema WHERE type='trigger' AND name GLOB ?1")?
        .query_map([format!("{PREFIX}*")], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    triggers
        .into_iter()
        .map(|(name, table, sql)| {
            let tokens = Tokenizer::new(&SQLiteDialect {}, &sql)
                .tokenize()
                .map_err(Error::validation)?
                .into_iter()
                .filter(|t| !matches!(t, Token::Whitespace(_)))
                .collect::<Vec<_>>();
            let named = tokens
                .iter()
                .position(|t| matches!(t, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("OF")))
                .and_then(|i| match tokens.get(i + 1) {
                    Some(Token::Word(w)) => Some(w.value.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    Error::new(format!("invalid READONLY trigger: {name}")).with_code("BOXDB_METADATA")
                })?;
            let column = db
                .query_row(
                    "SELECT name FROM pragma_table_xinfo(?1,'main') WHERE name=?2 COLLATE NOCASE",
                    [&table, &named],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(Declared {
                name,
                table,
                column,
                sql,
            })
        })
        .collect()
}
/// Protected (table, column) pairs, using the table's column spelling.
pub(crate) fn columns(db: &Connection) -> Result<BTreeSet<(String, String)>> {
    Ok(declared(db)?
        .into_iter()
        .filter_map(|declared| Some((declared.table, declared.column?)))
        .collect())
}
/// DROP TABLE drops its triggers, which the authorizer reserves for BoxDB.
/// Drop them first inside the caller's savepoint.
pub(crate) fn before_drop(db: &Connection, sql: &str) -> Result<()> {
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, sql) else {
        return Ok(());
    };
    let [
        Statement::Drop {
            object_type: ObjectType::Table,
            names,
            ..
        },
    ] = statements.as_slice()
    else {
        return Ok(());
    };
    for name in names {
        let Some(table) = super::main_name(name) else {
            continue;
        };
        let shadowed = name.0.len() == 1
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE type='table' AND name=?1 COLLATE NOCASE)",
                [table],
                |row| row.get::<_, bool>(0),
            )?;
        if shadowed {
            continue;
        }
        for declared in declared(db)? {
            if declared.table.eq_ignore_ascii_case(table) {
                db.execute_batch(&format!("DROP TRIGGER main.{}", quote(&declared.name)))?;
            }
        }
    }
    Ok(())
}
/// Reject READONLY triggers that DDL outside BoxDB changed or left dangling.
pub(crate) fn verify(db: &Connection) -> Result<()> {
    for declared in declared(db)? {
        if !declared.canonical(db)? {
            return Err(Error::new(format!(
                "BoxDB READONLY trigger does not match schema: {}; change READONLY columns through BoxDB",
                declared.table
            ))
            .with_code("BOXDB_METADATA"));
        }
    }
    Ok(())
}
/// Restore canonical triggers after DDL renamed or dropped their columns.
pub(crate) fn normalize(db: &Connection) -> Result<()> {
    for declared in declared(db)? {
        if declared.canonical(db)? {
            continue;
        }
        db.execute_batch(&format!("DROP TRIGGER main.{}", quote(&declared.name)))?;
        if let Some(column) = &declared.column {
            declare(db, &declared.table, column)?;
        }
    }
    Ok(())
}
