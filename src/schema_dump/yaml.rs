//! The schema document describes logical DDL, independently of readable rows.
use crate::{Error, Result, Type, schema, storage::MAX_SNAPSHOT};
use rusqlite::Connection;
use serde::{Serialize, Serializer, ser::SerializeMap};
use sqlparser::{
    ast::{ColumnDef, DataType, Ident, ObjectName},
    dialect::SQLiteDialect,
    tokenizer::{Token, Tokenizer},
};
use std::collections::BTreeMap;
mod constraints;
use constraints::Constraint;
mod objects;
use objects::{Index, Trigger, View};

// Preserve SQL column order rather than sorting column names.
#[derive(Default)]
struct Columns(Vec<(String, String)>);
impl Serialize for Columns {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, definition) in &self.0 {
            map.serialize_entry(name, definition)?;
        }
        map.end()
    }
}
#[derive(Serialize)]
struct Document {
    format: String,
    version: u32,
    #[serde(skip_serializing_if = "Pragmas::is_empty")]
    pragmas: Pragmas,
    tables: BTreeMap<String, Table>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    indexes: BTreeMap<String, Index>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    views: BTreeMap<String, View>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    triggers: BTreeMap<String, Trigger>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    migrations: Vec<Migration>,
}
#[derive(Serialize)]
struct Migration {
    version: String,
    name: String,
    checksum: String,
}
#[derive(Default, Serialize)]
struct Pragmas {
    #[serde(skip_serializing_if = "is_zero")]
    application_id: i32,
    #[serde(skip_serializing_if = "is_zero")]
    user_version: i32,
}
fn is_zero(n: &i32) -> bool {
    *n == 0
}
impl Pragmas {
    fn is_empty(&self) -> bool {
        self.application_id == 0 && self.user_version == 0
    }
}
#[derive(Default, Serialize)]
struct Table {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    without_rowid: bool,
    columns: Columns,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    constraints: Vec<Constraint>,
}

fn invalid(message: &str) -> Error {
    Error::new(format!("invalid YAML schema: {message}"))
}
fn tokens(sql: &str) -> Result<Vec<Token>> {
    Ok(Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize()
        .map_err(Error::validation)?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect())
}
fn word(t: &Token, value: &str) -> bool {
    matches!(t, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(value))
}
fn identifier(t: &Token) -> Result<String> {
    match t {
        Token::Word(w) => Ok(w.value.clone()),
        Token::SingleQuotedString(s) => Ok(s.clone()),
        _ => Err(invalid("expected identifier")),
    }
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
        let grammar_paren = i > 0
            && [
                "CHECK", "AS", "IN", "VALUES", "UNIQUE", "KEY", "BY", "AND", "OR", "NOT", "WHEN",
                "THEN", "ELSE", "DEFAULT", "FILTER", "OVER",
            ]
            .iter()
            .any(|w| word(&tokens[i - 1], w));
        let signed_number = matches!(token, Token::Number(..))
            && i > 0
            && matches!(tokens[i - 1], Token::Minus | Token::Plus)
            && (i == 1
                || matches!(tokens[i - 2], Token::LParen | Token::Comma | Token::Eq)
                || [
                    "DEFAULT", "THEN", "ELSE", "WHEN", "SELECT", "BETWEEN", "AND", "OR",
                ]
                .iter()
                .any(|w| word(&tokens[i - 2], w)));
        let tight = matches!(
            token,
            Token::RParen | Token::Comma | Token::Period | Token::SemiColon
        ) || i > 0 && matches!(tokens[i - 1], Token::LParen | Token::Period)
            || matches!(token, Token::LParen)
                && i > 0
                && matches!(tokens[i - 1], Token::Word(_))
                && !grammar_paren
            || signed_number;
        if !sql.is_empty() && !tight {
            sql.push(' ');
        }
        sql.push_str(&text);
    }
    sql
}
struct Sql {
    tokens: Vec<Token>,
    pos: usize,
}
impl Sql {
    fn new(sql: &str) -> Result<Self> {
        Ok(Self {
            tokens: tokens(sql)?,
            pos: 0,
        })
    }
    fn take(&mut self, value: &str) -> bool {
        if self.tokens.get(self.pos).is_some_and(|t| word(t, value)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, value: &str) -> Result<()> {
        if self.take(value) {
            Ok(())
        } else {
            Err(invalid(&format!("expected {value}")))
        }
    }
    fn name(&mut self) -> Result<String> {
        let name = identifier(
            self.tokens
                .get(self.pos)
                .ok_or_else(|| invalid("expected identifier"))?,
        )?;
        self.pos += 1;
        Ok(name)
    }
    fn if_not_exists(&mut self) -> Result<()> {
        if self.take("IF") {
            self.expect("NOT")?;
            self.expect("EXISTS")?;
        }
        Ok(())
    }
    fn group(&mut self) -> Result<Vec<Token>> {
        if self.tokens.get(self.pos) != Some(&Token::LParen) {
            return Err(invalid("expected parentheses"));
        }
        self.pos += 1;
        let start = self.pos;
        let mut depth = 0;
        while let Some(token) = self.tokens.get(self.pos) {
            match token {
                Token::LParen => depth += 1,
                Token::RParen if depth == 0 => {
                    let body = self.tokens[start..self.pos].to_vec();
                    self.pos += 1;
                    return Ok(body);
                }
                Token::RParen => depth -= 1,
                _ => (),
            }
            self.pos += 1;
        }
        Err(invalid("unclosed parentheses"))
    }
}
fn split(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = vec![];
    let mut depth = 0;
    let mut start = 0;
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => {
                parts.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => (),
        }
    }
    parts.push(&tokens[start..]);
    parts
}
fn type_sql(ty: &Type) -> String {
    match ty {
        Type::Int8 => "INT8".into(),
        Type::Int16 => "INT16".into(),
        Type::Int32 => "INT32".into(),
        Type::Int64 => "INTEGER".into(),
        Type::Uint8 => "UINT8".into(),
        Type::Uint16 => "UINT16".into(),
        Type::Uint32 => "UINT32".into(),
        Type::Uint64 => "UINT64".into(),
        Type::Double => "REAL".into(),
        Type::Decimal(None) => "DECIMAL".into(),
        Type::Decimal(Some((p, s))) => format!("DECIMAL({p}, {s})"),
        Type::Boolean => "BOOLEAN".into(),
        Type::Text => "TEXT".into(),
        Type::Varchar(None) => "VARCHAR".into(),
        Type::Varchar(Some(n)) => format!("VARCHAR({n})"),
        Type::Enum(labels) => format!(
            "ENUM({})",
            labels
                .iter()
                .map(|label| format!("'{}'", label.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Bytea => "BYTEA".into(),
        Type::Uuid => "UUID".into(),
        Type::Json => "JSON".into(),
        Type::Instant => "INSTANT".into(),
        Type::Date => "DATE".into(),
        Type::DateTime => "DATETIME".into(),
        Type::Interval => "INTERVAL".into(),
        Type::Array(inner) => format!("{}[]", type_sql(inner)),
    }
}
fn column(name: &str, definition: &[Token], ty: Option<&Type>) -> Result<String> {
    let Some(ty) = ty else {
        return Ok(render(definition));
    };
    let logical = type_sql(ty);
    let mut translated = ColumnDef {
        name: Ident::new(name),
        data_type: DataType::Custom(ObjectName(vec![Ident::new(&logical)]), vec![]),
        collation: None,
        options: vec![],
    };
    schema::translate_column(&mut translated)?;
    let mut end = definition.len();
    // Translation appends its generated checks after the user's column options.
    // Remove only that suffix, so user CHECKs and named constraints survive.
    for check in translated.options.iter().rev() {
        let check = tokens(&check.to_string())?;
        if definition[..end].ends_with(&check) {
            end -= check.len();
        } else {
            return Err(invalid(&format!(
                "missing generated check for column {name}"
            )));
        }
    }
    // The stored physical type can have parameters; the logical declaration
    // replaces it but leaves defaults, generated expressions and constraints.
    let mut type_end = 1;
    if definition.get(type_end) == Some(&Token::LParen) {
        let mut cursor = Sql {
            tokens: definition.to_vec(),
            pos: type_end,
        };
        cursor.group()?;
        type_end = cursor.pos;
    }
    let tail = render(&definition[type_end..end]);
    Ok(if tail.is_empty() {
        logical
    } else {
        format!("{logical} {tail}")
    })
}

pub(crate) fn encode(db: &Connection) -> Result<Vec<u8>> {
    let types = schema::load(db)?;
    let mut document = Document {
        format: "boxdb-schema".into(),
        version: 1,
        pragmas: Pragmas {
            application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
            user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
        },
        tables: BTreeMap::new(),
        indexes: BTreeMap::new(),
        views: BTreeMap::new(),
        triggers: BTreeMap::new(),
        migrations: vec![],
    };
    let mut statement = db.prepare("SELECT type,name,sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*' ORDER BY type,name")?;
    for object in statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (kind, name, sql) = object?;
        if name == crate::migrations::TABLE {
            document.migrations = db
                .prepare("SELECT version,name,checksum FROM __boxdb_migrations ORDER BY version")?
                .query_map([], |r| {
                    Ok(Migration {
                        version: r.get(0)?,
                        name: r.get(1)?,
                        checksum: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            continue;
        }
        if name.starts_with("__boxdb_") {
            continue;
        }
        match kind.as_str() {
            "index" => {
                document.indexes.insert(name, Index::parse(&sql)?);
                continue;
            }
            "view" => {
                document.views.insert(name, View::parse(&sql)?);
                continue;
            }
            "trigger" => {
                document.triggers.insert(name, Trigger::parse(&sql)?);
                continue;
            }
            "table" => (),
            _ => return Err(invalid("unsupported schema object")),
        }
        let mut cursor = Sql::new(&sql)?;
        cursor.expect("CREATE")?;
        if cursor.take("VIRTUAL") {
            return Err(invalid("schema dump of virtual tables is unsupported"));
        }
        cursor.expect("TABLE")?;
        cursor.if_not_exists()?;
        cursor.name()?;
        let strict: bool = db.query_row(
            "SELECT strict FROM pragma_table_list WHERE schema='main' AND name=?1",
            [&name],
            |r| r.get(0),
        )?;
        if !strict {
            return Err(invalid(&format!("non-strict table is unsupported: {name}")));
        }
        let definitions = cursor.group()?;
        let mut table = Table::default();
        for definition in split(&definitions) {
            if definition.first().is_some_and(|t| {
                ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
                    .iter()
                    .any(|w| word(t, w))
            }) {
                table.constraints.push(Constraint::parse(definition)?);
                continue;
            }
            let column_name =
                identifier(definition.first().ok_or_else(|| invalid("empty column"))?)?;
            let definition = column(
                &column_name,
                &definition[1..],
                types.get(&(name.clone(), column_name.clone())),
            )?;
            table.columns.0.push((column_name, definition));
        }
        table.without_rowid = cursor.tokens[cursor.pos..]
            .iter()
            .any(|t| word(t, "WITHOUT"));
        document.tables.insert(name, table);
    }
    let output = serde_saphyr::to_string_with_options(
        &document,
        serde_saphyr::ser_options! { prefer_block_scalars: false, compact_list_indent: false, min_fold_chars: 0 },
    )?;
    if output.len() > MAX_SNAPSHOT {
        return Err(invalid("schema dump exceeds 256 MiB limit"));
    }
    Ok(output.into_bytes())
}
