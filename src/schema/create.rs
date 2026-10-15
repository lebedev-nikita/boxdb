//! Normalize table declarations while leaving SQLite constraints and expressions
//! to SQLite. The SQL parser's AST does not cover every SQLite table option.
use super::{Alteration, Declaration, translate_column};
use crate::{Error, Result, Type};
use rusqlite::Connection;
use sqlparser::{
    ast::{ColumnDef, DataType, Ident, ObjectName},
    dialect::SQLiteDialect,
    tokenizer::{Token, Tokenizer},
};

fn word(token: &Token, text: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(text))
}
fn identifier(token: &Token) -> Result<String> {
    match token {
        Token::Word(w) => Ok(w.value.clone()),
        Token::SingleQuotedString(s) => Ok(s.clone()),
        _ => Err(Error::new("expected table or column name".into())),
    }
}
fn render(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|token| match token {
            Token::Word(w) if w.quote_style == Some('"') => super::quote(&w.value),
            Token::Word(w) if w.quote_style == Some('`') => {
                format!("`{}`", w.value.replace('`', "``"))
            }
            Token::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
            Token::HexStringLiteral(s) => format!("X'{}'", s.replace('\'', "''")),
            _ => token.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn constraint(token: &Token) -> bool {
    [
        "CONSTRAINT",
        "PRIMARY",
        "NOT",
        "NULL",
        "UNIQUE",
        "CHECK",
        "DEFAULT",
        "COLLATE",
        "REFERENCES",
        "GENERATED",
        "AS",
    ]
    .iter()
    .any(|s| word(token, s))
}
fn column(tokens: &[Token]) -> Result<(String, Option<(String, Type)>)> {
    let Some(first) = tokens.first() else {
        return Err(Error::new("empty column definition".into()));
    };
    if ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
        .iter()
        .any(|s| word(first, s))
    {
        return Ok((render(tokens), None));
    }
    let name = identifier(first)?;
    let mut end = 1;
    let mut depth = 0usize;
    while let Some(token) = tokens.get(end) {
        if depth == 0 && constraint(token) {
            break;
        }
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            _ => (),
        }
        end += 1;
    }
    let mut datatype = String::new();
    for token in &tokens[1..end] {
        match token {
            Token::Word(w) if w.quote_style == Some('[') && w.value.trim().is_empty() => {
                datatype.push_str("[]")
            }
            Token::Word(w) => {
                if !datatype.is_empty() {
                    datatype.push(' ');
                }
                datatype.push_str(&w.value);
            }
            Token::SingleQuotedString(s) => {
                if datatype.is_empty() {
                    // SQLite also accepts a quoted type name, e.g. 'INT64'.
                    datatype.push_str(s);
                } else {
                    datatype.push(' ');
                    datatype.push_str(&format!("'{}'", s.replace('\'', "''")));
                }
            }
            other => datatype.push_str(&other.to_string()),
        }
    }
    let mut definition = ColumnDef {
        name: Ident::new(&name),
        data_type: DataType::Custom(ObjectName(vec![Ident::new(datatype)]), vec![]),
        collation: None,
        options: vec![],
    };
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(end) {
        if depth == 0 && word(token, "COLLATE") {
            let name = tokens
                .get(i + 1)
                .ok_or_else(|| Error::new("missing collation".into()))?;
            definition.collation = Some(ObjectName(vec![Ident::new(identifier(name)?)]));
        }
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            _ => (),
        }
    }
    let logical = translate_column(&mut definition)?;
    if ![
        "INT", "INTEGER", "REAL", "TEXT", "BLOB", "ANY", "DECIMAL", "UUID", "INSTANT", "DATE",
        "DATETIME", "INTERVAL", "UINT8", "UINT16", "UINT32", "UINT64",
    ]
    .iter()
    .any(|native| {
        definition
            .data_type
            .to_string()
            .eq_ignore_ascii_case(native)
    }) {
        return Err(Error::new(format!(
            "unsupported column type for {name}: {}",
            definition.data_type
        )));
    }
    let mut output = format!("{} {}", render(&tokens[..1]), definition.data_type);
    if end < tokens.len() {
        output.push(' ');
        output.push_str(&render(&tokens[end..]));
    }
    for option in definition.options {
        output.push(' ');
        output.push_str(&option.to_string());
    }
    Ok((output, logical.map(|ty| (name, ty))))
}
fn tokens(sql: &str) -> Result<Vec<Token>> {
    Ok(Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize()
        .map_err(Error::validation)?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_) | Token::SemiColon))
        .collect())
}

// Preserve literal tokens when adding a column: the generic AST accepts
// non-string ENUM members and renders them as strings, losing that distinction.
pub(super) fn addition(sql: &str) -> Result<Option<Alteration>> {
    let tokens = tokens(sql)?;
    if tokens.len() < 4 || !word(&tokens[0], "ALTER") || !word(&tokens[1], "TABLE") {
        return Ok(None);
    }
    let first = identifier(&tokens[2])?;
    let mut position = 3;
    let (schema, table) = if tokens.get(position) == Some(&Token::Period) {
        position += 2;
        let table = identifier(
            tokens
                .get(position - 1)
                .ok_or_else(|| Error::new("missing table name".into()))?,
        )?;
        (first, table)
    } else {
        ("main".into(), first)
    };
    if !tokens.get(position).is_some_and(|t| word(t, "ADD")) {
        return Ok(None);
    }
    position += 1;
    if tokens.get(position).is_some_and(|t| word(t, "COLUMN")) {
        position += 1;
    }
    let (definition, logical) = column(&tokens[position..])?;
    let main = schema.eq_ignore_ascii_case("main");
    if !main
        && logical
            .as_ref()
            .is_some_and(|(_, ty)| ty.native_kind().is_none())
    {
        return Err(Error::new(
            "extended types require main database tables".into(),
        ));
    }
    Ok(Some(Alteration {
        main,
        sql: format!("{} {definition}", render(&tokens[..position])),
        table,
        columns: if main {
            logical.into_iter().collect()
        } else {
            vec![]
        },
        changes: vec![],
    }))
}

pub(super) fn declaration(sql: &str) -> Result<Option<Declaration>> {
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
    if !take("CREATE") {
        return Ok(None);
    }
    let temporary = take("TEMP") || take("TEMPORARY");
    if !take("TABLE") {
        return Ok(None);
    }
    if take("IF") && !(take("NOT") && take("EXISTS")) {
        return Err(Error::new("expected IF NOT EXISTS".into()));
    }
    let first = identifier(
        tokens
            .get(position)
            .ok_or_else(|| Error::new("missing table name".into()))?,
    )?;
    position += 1;
    let (schema, table) = if tokens.get(position) == Some(&Token::Period) {
        position += 1;
        let table = identifier(
            tokens
                .get(position)
                .ok_or_else(|| Error::new("missing table name".into()))?,
        )?;
        position += 1;
        (first, table)
    } else {
        (
            if temporary {
                "temp".into()
            } else {
                "main".into()
            },
            first,
        )
    };
    if temporary && !schema.eq_ignore_ascii_case("temp") {
        return Err(Error::new(
            "temporary tables must use the temp schema".into(),
        ));
    }
    let header = render(&tokens[..position]);
    if tokens.get(position).is_some_and(|t| word(t, "AS")) {
        return Ok(Some(Declaration {
            sql: sql.into(),
            schema,
            table,
            columns: vec![],
            select: true,
        }));
    }
    if tokens.get(position) != Some(&Token::LParen) {
        return Err(Error::new("CREATE TABLE requires a column list".into()));
    }
    position += 1;
    let mut start = position;
    let mut depth = 0usize;
    let mut definitions = vec![];
    let mut columns = vec![];
    loop {
        let token = tokens
            .get(position)
            .ok_or_else(|| Error::new("unclosed table definition".into()))?;
        if depth == 0 && matches!(token, Token::Comma | Token::RParen) {
            let (definition, logical) = column(&tokens[start..position])?;
            definitions.push(definition);
            if let Some(logical) = logical {
                columns.push(logical);
            }
            if token == &Token::RParen {
                position += 1;
                break;
            }
            start = position + 1;
        } else {
            match token {
                Token::LParen => depth += 1,
                Token::RParen => depth = depth.saturating_sub(1),
                _ => (),
            }
        }
        position += 1;
    }
    if !schema.eq_ignore_ascii_case("main") {
        // Native decimals carry their identity and constraints in SQLite.
        columns.retain(|(_, ty)| ty.native_kind().is_none());
        if !columns.is_empty() {
            return Err(Error::new(
                "extended types require main database tables".into(),
            ));
        }
    }
    let mut without_rowid = false;
    while position < tokens.len() {
        if word(&tokens[position], "STRICT") {
            position += 1;
        } else if word(&tokens[position], "WITHOUT")
            && tokens.get(position + 1).is_some_and(|t| word(t, "ROWID"))
        {
            without_rowid = true;
            position += 2;
        } else {
            return Err(Error::new("unsupported SQLite table option".into()));
        }
        if position < tokens.len() {
            if tokens[position] != Token::Comma {
                return Err(Error::new("table options must be comma-separated".into()));
            }
            position += 1;
            if position == tokens.len() {
                return Err(Error::new("missing table option".into()));
            }
        }
    }
    Ok(Some(Declaration {
        sql: format!(
            "{header} ({}) STRICT{}",
            definitions.join(","),
            if without_rowid { ", WITHOUT ROWID" } else { "" }
        ),
        schema,
        table,
        columns,
        select: false,
    }))
}

impl Declaration {
    pub fn execute(&self, db: &Connection) -> Result<()> {
        db.execute_batch(&self.sql)?;
        if !self.select {
            return Ok(());
        }
        // Let SQLite infer CTAS columns and evaluate the query once. Replace the
        // newly created table inside the caller's savepoint before publishing.
        let columns = db
            .prepare("SELECT name,type FROM pragma_table_xinfo(?1,?2) ORDER BY cid")?
            .query_map([&self.table, &self.schema], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut index = 0usize;
        let scratch = loop {
            let name = format!("__boxdb_ctas_{index}");
            if !db.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {}.sqlite_schema WHERE name=?1 COLLATE NOCASE)",
                    super::quote(&self.schema)
                ),
                [&name],
                |r| r.get::<_, bool>(0),
            )? {
                break name;
            }
            index += 1;
        };
        let target = format!(
            "{}.{}",
            super::quote(&self.schema),
            super::quote(&self.table)
        );
        let source = format!("{}.{}", super::quote(&self.schema), super::quote(&scratch));
        let definitions = columns
            .iter()
            .map(|(name, datatype)| {
                let datatype = match datatype.to_ascii_uppercase().as_str() {
                    "INT" | "INTEGER" => "INT",
                    "REAL" => "REAL",
                    "TEXT" => "TEXT",
                    "BLOB" => "BLOB",
                    "DECIMAL" => "DECIMAL",
                    "UUID" => "UUID",
                    "INSTANT" => "INSTANT",
                    "DATE" => "DATE",
                    "DATETIME" => "DATETIME",
                    "INTERVAL" => "INTERVAL",
                    _ => "ANY",
                };
                format!("{} {datatype}", super::quote(name))
            })
            .collect::<Vec<_>>()
            .join(",");
        let order = ["_rowid_", "rowid", "oid"]
            .into_iter()
            .find(|alias| {
                !columns
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(alias))
            })
            .map(|alias| format!(" ORDER BY {}", super::quote(alias)))
            .unwrap_or_default();
        // CTAS has no indexes. If all aliases are shadowed, its plain table scan
        // retains insertion order without sorting a user-defined rowid column.
        db.execute_batch(&format!("ALTER TABLE {target} RENAME TO {}; CREATE TABLE {target}({definitions}) STRICT; INSERT INTO {target} SELECT * FROM {source}{order}; DROP TABLE {source}", super::quote(&scratch)))?;
        Ok(())
    }
}
