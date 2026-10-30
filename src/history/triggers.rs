//! Commit triggers run SQL inside a transaction right before it commits, with
//! the recorded changeset available. SQLite has no such object, so the
//! declaration stays in BoxDB metadata and never reaches `sqlite_schema`.
use crate::{Error, Result, schema};
use rusqlite::{Connection, OptionalExtension};
use sqlparser::{
    dialect::SQLiteDialect,
    tokenizer::{Token, Tokenizer},
};

pub(crate) const META: &str = "__boxdb_commit_triggers";
const META_SQL: &str = "CREATE TABLE IF NOT EXISTS __boxdb_commit_triggers(name TEXT NOT NULL PRIMARY KEY,position INTEGER NOT NULL,tables TEXT,sql TEXT NOT NULL,version INTEGER NOT NULL CHECK(version=1)) STRICT";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommitTrigger {
    pub name: String,
    /// Recorded main tables, or every main table when absent.
    pub tables: Option<Vec<String>>,
    pub body: Vec<String>,
    pub sql: String,
}
impl CommitTrigger {
    pub fn covers(&self, table: &str) -> bool {
        self.tables
            .as_ref()
            .is_none_or(|tables| tables.iter().any(|name| name.eq_ignore_ascii_case(table)))
    }
}
pub(crate) struct Declaration {
    pub trigger: CommitTrigger,
    pub if_not_exists: bool,
}

fn word(token: &Token, text: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(text))
}
fn identifier(token: Option<&Token>) -> Result<String> {
    match token {
        Some(Token::Word(w)) => Ok(w.value.clone()),
        Some(Token::SingleQuotedString(s)) => Ok(s.clone()),
        _ => Err(Error::new("expected an identifier".into())),
    }
}
fn tokens(sql: &str) -> Result<Vec<Token>> {
    Ok(Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize()
        .map_err(Error::validation)?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect())
}
fn render(tokens: &[Token]) -> String {
    let mut sql = String::new();
    for (i, token) in tokens.iter().enumerate() {
        let text = match token {
            Token::Word(w) if w.quote_style == Some('"') => schema::quote(&w.value),
            Token::Word(w) if w.quote_style == Some('`') => {
                format!("`{}`", w.value.replace('`', "``"))
            }
            Token::Word(w) if w.quote_style == Some('[') => format!("[{}]", w.value),
            Token::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
            Token::HexStringLiteral(s) => format!("X'{}'", s.replace('\'', "''")),
            other => other.to_string(),
        };
        let tight = matches!(token, Token::RParen | Token::Comma | Token::Period)
            || i > 0 && matches!(tokens[i - 1], Token::LParen | Token::Period);
        if !sql.is_empty() && !tight {
            sql.push(' ');
        }
        sql.push_str(&text);
    }
    sql
}

/// Recognize `CREATE TRIGGER name BEFORE COMMIT [ON tables] BEGIN ... END`.
/// Other triggers return `None` and stay with SQLite.
pub(crate) fn parse(sql: &str) -> Result<Option<Declaration>> {
    if !schema::keyword(sql).eq_ignore_ascii_case("CREATE") {
        return Ok(None);
    }
    let tokens = tokens(sql)?;
    let mut position = 0;
    let mut take = |text: &str| {
        if tokens.get(position).is_some_and(|t| word(t, text)) {
            position += 1;
            true
        } else {
            false
        }
    };
    take("CREATE");
    let temporary = take("TEMP") || take("TEMPORARY");
    if !take("TRIGGER") {
        return Ok(None);
    }
    let if_not_exists = take("IF") && take("NOT") && take("EXISTS");
    let first = identifier(tokens.get(position))?;
    position += 1;
    let (schema_name, name) = if tokens.get(position) == Some(&Token::Period) {
        let name = identifier(tokens.get(position + 1))?;
        position += 2;
        (Some(first), name)
    } else {
        (None, first)
    };
    let timing = tokens
        .get(position)
        .filter(|t| word(t, "BEFORE") || word(t, "AFTER"))
        .cloned();
    let commit = tokens.get(position + 1).is_some_and(|t| word(t, "COMMIT"));
    if timing.is_none() || !commit {
        return Ok(None);
    }
    if !word(timing.as_ref().unwrap(), "BEFORE") {
        return Err(Error::new(
            "only BEFORE COMMIT triggers are supported".into(),
        ));
    }
    if temporary || schema_name.is_some_and(|s| !s.eq_ignore_ascii_case("main")) {
        return Err(Error::new(
            "commit triggers belong to the main database".into(),
        ));
    }
    position += 2;
    let mut tables = None;
    if tokens.get(position).is_some_and(|t| word(t, "ON")) {
        position += 1;
        let mut names = vec![];
        loop {
            let first = identifier(tokens.get(position))?;
            position += 1;
            let table = if tokens.get(position) == Some(&Token::Period) {
                let table = identifier(tokens.get(position + 1))?;
                position += 2;
                if !first.eq_ignore_ascii_case("main") {
                    return Err(Error::new(
                        "commit triggers record main database tables".into(),
                    ));
                }
                table
            } else {
                first
            };
            if table.to_ascii_lowercase().starts_with("__boxdb_") {
                return Err(Error::new("__boxdb_ names are reserved".into()));
            }
            names.push(table);
            if tokens.get(position) != Some(&Token::Comma) {
                break;
            }
            position += 1;
        }
        tables = Some(names);
    }
    if !tokens.get(position).is_some_and(|t| word(t, "BEGIN")) {
        return Err(Error::new(
            "expected BEGIN after the commit trigger header".into(),
        ));
    }
    position += 1;
    let mut end = tokens.len();
    while end > position && tokens[end - 1] == Token::SemiColon {
        end -= 1;
    }
    if end == position || !word(&tokens[end - 1], "END") {
        return Err(Error::new("missing commit trigger END".into()));
    }
    let body: Vec<String> = tokens[position..end - 1]
        .split(|t| *t == Token::SemiColon)
        .filter(|statement| !statement.is_empty())
        .map(render)
        .collect();
    if body.is_empty() {
        return Err(Error::new("commit trigger body is empty".into()));
    }
    let sql = sql.trim().trim_end_matches(';').trim().to_owned();
    Ok(Some(Declaration {
        trigger: CommitTrigger {
            name,
            tables,
            body,
            sql,
        },
        if_not_exists,
    }))
}

/// Parse `DROP TRIGGER [IF EXISTS] [main.]name`.
pub(crate) fn drop_statement(sql: &str) -> Result<Option<(String, bool)>> {
    let tokens = tokens(sql)?;
    let mut position = 0;
    let mut take = |text: &str| {
        if tokens.get(position).is_some_and(|t| word(t, text)) {
            position += 1;
            true
        } else {
            false
        }
    };
    if !(take("DROP") && take("TRIGGER")) {
        return Ok(None);
    }
    let if_exists = take("IF") && take("EXISTS");
    let first = identifier(tokens.get(position))?;
    position += 1;
    let name = if tokens.get(position) == Some(&Token::Period) {
        if !first.eq_ignore_ascii_case("main") {
            return Ok(None);
        }
        identifier(tokens.get(position + 1))?
    } else {
        first
    };
    Ok(Some((name, if_exists)))
}

fn exists(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name=?1 AND type='table')",
        [META],
        |row| row.get(0),
    )?)
}
pub(crate) fn load(db: &Connection) -> Result<Vec<CommitTrigger>> {
    if !exists(db)? {
        return Ok(vec![]);
    }
    let mut statement = db.prepare_cached(
        "SELECT name,sql,version FROM main.__boxdb_commit_triggers ORDER BY position",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut triggers = vec![];
    for (name, sql, version) in rows {
        let declaration = parse(&sql)
            .ok()
            .flatten()
            .filter(|d| version == 1 && d.trigger.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| {
                Error::new(format!(
                    "BoxDB metadata holds an invalid commit trigger: {name}"
                ))
                .with_code("BOXDB_METADATA")
            })?;
        triggers.push(declaration.trigger);
    }
    Ok(triggers)
}
pub(crate) fn save(db: &Connection, trigger: &CommitTrigger) -> Result<()> {
    schema::configure_codec(db, false)?;
    db.execute_batch(META_SQL)?;
    let tables = trigger
        .tables
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    db.execute(
        "INSERT INTO main.__boxdb_commit_triggers(name,position,tables,sql,version) VALUES(?1,(SELECT COALESCE(MAX(position),0)+1 FROM main.__boxdb_commit_triggers),?2,?3,1)",
        rusqlite::params![trigger.name, tables, trigger.sql],
    )?;
    Ok(())
}
pub(crate) fn remove(db: &Connection, name: &str) -> Result<bool> {
    if !exists(db)? {
        return Ok(false);
    }
    Ok(db.execute(
        "DELETE FROM main.__boxdb_commit_triggers WHERE name=?1 COLLATE NOCASE",
        [name],
    )? > 0)
}
pub(crate) fn find(db: &Connection, name: &str) -> Result<Option<String>> {
    if !exists(db)? {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "SELECT name FROM main.__boxdb_commit_triggers WHERE name=?1 COLLATE NOCASE",
            [name],
            |row| row.get(0),
        )
        .optional()?)
}
/// Follow a table rename in every trigger that lists the table.
pub(crate) fn rename_table(db: &Connection, old: &str, new: &str) -> Result<()> {
    for trigger in load(db)? {
        let Some(tables) = &trigger.tables else {
            continue;
        };
        if !tables.iter().any(|name| name.eq_ignore_ascii_case(old)) {
            continue;
        }
        let renamed: Vec<String> = tables
            .iter()
            .map(|name| {
                if name.eq_ignore_ascii_case(old) {
                    new.to_owned()
                } else {
                    name.clone()
                }
            })
            .collect();
        let sql = rewrite_tables(&trigger, &renamed);
        db.execute(
            "UPDATE main.__boxdb_commit_triggers SET tables=?1, sql=?2 WHERE name=?3",
            rusqlite::params![serde_json::to_string(&renamed)?, sql, trigger.name],
        )?;
    }
    Ok(())
}
fn rewrite_tables(trigger: &CommitTrigger, tables: &[String]) -> String {
    let list = tables
        .iter()
        .map(|name| schema::quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE TRIGGER {} BEFORE COMMIT ON {list} BEGIN {}; END",
        schema::quote(&trigger.name),
        trigger.body.join("; ")
    )
}

pub(crate) enum Control {
    Savepoint(String),
    Release(String),
    RollbackTo(String),
}
/// Classify savepoint statements so BoxDB knows when a RELEASE commits.
pub(crate) fn control(sql: &str) -> Result<Option<Control>> {
    let keyword = schema::keyword(sql).to_ascii_uppercase();
    if !matches!(keyword.as_str(), "SAVEPOINT" | "RELEASE" | "ROLLBACK") {
        return Ok(None);
    }
    let tokens = tokens(sql)?;
    let mut position = 0;
    let mut take = |text: &str| {
        if tokens.get(position).is_some_and(|t| word(t, text)) {
            position += 1;
            true
        } else {
            false
        }
    };
    if take("SAVEPOINT") {
        return Ok(Some(Control::Savepoint(identifier(tokens.get(position))?)));
    }
    if take("RELEASE") {
        take("SAVEPOINT");
        return Ok(Some(Control::Release(identifier(tokens.get(position))?)));
    }
    take("ROLLBACK");
    take("TRANSACTION");
    if !take("TO") {
        return Ok(None);
    }
    take("SAVEPOINT");
    Ok(Some(Control::RollbackTo(identifier(tokens.get(position))?)))
}
