use boxdb::{QueryResult, Value};
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::{Batch, Connection, types::Value as Native};
pub fn sqlite_execute(db: &Connection, sql: &str) -> boxdb::Result<Vec<QueryResult>> {
    let sql = integer_sql(sql, false);
    let mut batch = Batch::new(db, &sql);
    let mut results = vec![];
    while let Some(mut statement) = batch.next()? {
        let before = db.total_changes();
        let mut rows = vec![];
        if statement.column_count() == 0 {
            statement.execute([])?;
        } else {
            let mut cursor = statement.query([])?;
            while let Some(row) = cursor.next()? {
                rows.push(
                    (0..row.as_ref().column_count())
                        .map(|i| {
                            row.get::<_, Native>(i).map(|v| match v {
                                Native::Null => Value::Null,
                                Native::Integer(n) => Value::Integer(n),
                                Native::Real(n) => Value::Real(n),
                                Native::Text(t) => Value::Text(t),
                                Native::Blob(b) => Value::Bytea(b),
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
        }
        results.push(QueryResult {
            columns: statement
                .column_names()
                .into_iter()
                .map(String::from)
                .collect(),
            rows,
            affected: if db.total_changes() != before {
                db.changes() as usize
            } else {
                0
            },
        });
    }
    Ok(results)
}

// Keep the SQLite corpus unchanged while adapting its integer declarations to
// BoxDB's explicit-width policy. The reference client receives native types.
pub fn integer_sql(sql: &str, explicit: bool) -> String {
    use sqlparser::{
        dialect::SQLiteDialect,
        tokenizer::{Token, Tokenizer},
    };
    Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize()
        .unwrap()
        .into_iter()
        .map(|token| match token {
            Token::Word(mut w) if w.quote_style.is_none() => {
                if explicit
                    && ["INT", "INTEGER", "BIGINT"]
                        .iter()
                        .any(|name| w.value.eq_ignore_ascii_case(name))
                {
                    w.value = "INT64".into();
                } else if !explicit && w.value.eq_ignore_ascii_case("INT64") {
                    w.value = "INTEGER".into();
                }
                w.to_string()
            }
            Token::Word(w) if w.quote_style == Some('"') => {
                format!("\"{}\"", w.value.replace('"', "\"\""))
            }
            Token::Word(w) if w.quote_style == Some('`') => {
                format!("`{}`", w.value.replace('`', "``"))
            }
            Token::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
            other => other.to_string(),
        })
        .collect()
}
