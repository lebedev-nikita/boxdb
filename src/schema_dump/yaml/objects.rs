use super::*;
use serde_saphyr::{FlowSeq, LitStr};

fn literal<S: Serializer>(sql: &str, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    LitStr(sql).serialize(serializer)
}
fn flow<S: Serializer>(names: &[String], serializer: S) -> std::result::Result<S::Ok, S::Error> {
    FlowSeq(names).serialize(serializer)
}
fn name_list(sql: &mut Sql) -> Result<Vec<String>> {
    let body = sql.group()?;
    split(&body)
        .iter()
        .map(|part| {
            if part.len() != 1 {
                return Err(invalid("expected column name"));
            }
            identifier(&part[0])
        })
        .collect()
}
fn remainder(sql: &Sql) -> &[Token] {
    let mut tail = &sql.tokens[sql.pos..];
    if tail.last() == Some(&Token::SemiColon) {
        tail = &tail[..tail.len() - 1];
    }
    tail
}
// Only insert line breaks at SQL token boundaries. String literals (including
// semicolons, newlines and keywords inside them) are never split or rewritten.
fn block(tokens: &[Token]) -> String {
    let mut output = String::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, token) in tokens.iter().enumerate() {
        if depth == 0
            && i > start
            && [
                "FROM",
                "WHERE",
                "GROUP",
                "HAVING",
                "ORDER",
                "LIMIT",
                "UNION",
                "RETURNING",
            ]
            .iter()
            .any(|w| word(token, w))
        {
            output.push_str(&render(&tokens[start..i]));
            output.push('\n');
            start = i;
        }
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::SemiColon if depth == 0 => {
                output.push_str(&render(&tokens[start..=i]));
                output.push('\n');
                start = i + 1;
            }
            _ => (),
        }
    }
    if start < tokens.len() {
        output.push_str(&render(&tokens[start..]));
        output.push('\n');
    }
    output
}

#[derive(Default, Serialize)]
#[serde(rename_all = "snake_case")]
enum IndexType {
    #[default]
    Regular,
    Unique,
}
#[derive(Serialize)]
pub(super) struct Index {
    #[serde(rename = "type")]
    kind: IndexType,
    table: String,
    expressions: Vec<String>,
    #[serde(rename = "where", skip_serializing_if = "Option::is_none")]
    predicate: Option<String>,
}
impl Index {
    pub(super) fn parse(sql: &str) -> Result<Self> {
        let mut sql = Sql::new(sql)?;
        sql.expect("CREATE")?;
        let kind = if sql.take("UNIQUE") {
            IndexType::Unique
        } else {
            IndexType::Regular
        };
        sql.expect("INDEX")?;
        sql.if_not_exists()?;
        sql.name()?;
        sql.expect("ON")?;
        let table = sql.name()?;
        let body = sql.group()?;
        let expressions = split(&body).iter().map(|e| render(e)).collect();
        let predicate = if sql.take("WHERE") {
            Some(render(remainder(&sql)))
        } else if remainder(&sql).is_empty() {
            None
        } else {
            return Err(invalid("unexpected index clause"));
        };
        Ok(Self {
            kind,
            table,
            expressions,
            predicate,
        })
    }
}

#[derive(Serialize)]
pub(super) struct View {
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "flow")]
    columns: Vec<String>,
    #[serde(serialize_with = "literal")]
    query: String,
}
impl View {
    pub(super) fn parse(sql: &str) -> Result<Self> {
        let mut sql = Sql::new(sql)?;
        sql.expect("CREATE")?;
        sql.expect("VIEW")?;
        sql.if_not_exists()?;
        sql.name()?;
        let columns = if sql.tokens.get(sql.pos) == Some(&Token::LParen) {
            name_list(&mut sql)?
        } else {
            vec![]
        };
        sql.expect("AS")?;
        Ok(Self {
            columns,
            query: block(remainder(&sql)),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Timing {
    Before,
    After,
    InsteadOf,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    Insert,
    Update,
    Delete,
}
#[derive(Serialize)]
pub(super) struct Trigger {
    table: String,
    timing: Timing,
    event: Event,
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "flow")]
    update_of: Vec<String>,
    #[serde(rename = "when", skip_serializing_if = "Option::is_none")]
    condition: Option<String>,
    #[serde(serialize_with = "literal")]
    body: String,
}
impl Trigger {
    pub(super) fn parse(sql: &str) -> Result<Self> {
        let mut sql = Sql::new(sql)?;
        sql.expect("CREATE")?;
        sql.expect("TRIGGER")?;
        sql.if_not_exists()?;
        sql.name()?;
        let timing = if sql.take("AFTER") {
            Timing::After
        } else if sql.take("INSTEAD") {
            sql.expect("OF")?;
            Timing::InsteadOf
        } else {
            sql.take("BEFORE");
            Timing::Before
        };
        let event = if sql.take("INSERT") {
            Event::Insert
        } else if sql.take("DELETE") {
            Event::Delete
        } else {
            sql.expect("UPDATE")?;
            Event::Update
        };
        let mut update_of = vec![];
        if sql.take("OF") {
            loop {
                update_of.push(sql.name()?);
                if sql.tokens.get(sql.pos) != Some(&Token::Comma) {
                    break;
                }
                sql.pos += 1;
            }
        }
        sql.expect("ON")?;
        let table = sql.name()?;
        if sql.take("FOR") {
            sql.expect("EACH")?;
            sql.expect("ROW")?;
        }
        let condition = if sql.take("WHEN") {
            let start = sql.pos;
            let mut depth = 0;
            while let Some(token) = sql.tokens.get(sql.pos) {
                // BEGIN can also be a column name, including NEW.begin.
                // The body delimiter is outside parentheses and precedes a
                // trigger statement rather than an expression continuation.
                if depth == 0
                    && word(token, "BEGIN")
                    && sql.tokens.get(sql.pos + 1).is_some_and(|next| {
                        ["INSERT", "REPLACE", "UPDATE", "DELETE", "SELECT"]
                            .iter()
                            .any(|w| word(next, w))
                    })
                    && (sql.pos == 0 || sql.tokens[sql.pos - 1] != Token::Period)
                {
                    break;
                }
                match token {
                    Token::LParen => depth += 1,
                    Token::RParen => depth -= 1,
                    _ => (),
                }
                sql.pos += 1;
            }
            Some(render(&sql.tokens[start..sql.pos]))
        } else {
            None
        };
        sql.expect("BEGIN")?;
        let body = remainder(&sql);
        if !body.last().is_some_and(|t| word(t, "END")) {
            return Err(invalid("missing trigger END"));
        }
        let body = block(&body[..body.len() - 1]);
        Ok(Self {
            table,
            timing,
            event,
            update_of,
            condition,
            body,
        })
    }
}
