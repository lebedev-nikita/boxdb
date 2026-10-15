use super::*;
use serde_saphyr::FlowSeq;

#[derive(Default, Serialize)]
pub(super) struct Constraint {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_key: Option<FlowSeq<Vec<Key>>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    autoincrement: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unique: Option<FlowSeq<Vec<Key>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    foreign_key: Option<FlowSeq<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    references: Option<Reference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_delete: Option<ReferentialAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_update: Option<ReferentialAction>,
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    match_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deferrable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initially: Option<Initially>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_conflict: Option<Conflict>,
}
#[derive(Serialize)]
struct Reference {
    table: String,
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "flow")]
    columns: Vec<String>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum Key {
    Column(String),
    Ordered(KeyOptions),
}
#[derive(Serialize)]
struct KeyOptions {
    column: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    collate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<Order>,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Order {
    Asc,
    Desc,
}
fn keys(sql: &mut Sql) -> Result<(Vec<Key>, bool)> {
    let body = sql.group()?;
    let mut autoincrement = false;
    let mut keys = vec![];
    for tokens in split(&body) {
        if autoincrement {
            return Err(invalid("AUTOINCREMENT must follow the last key"));
        }
        let mut sql = Sql {
            tokens: tokens.to_vec(),
            pos: 0,
        };
        let column = sql.name()?;
        let collate = if sql.take("COLLATE") {
            Some(sql.name()?)
        } else {
            None
        };
        let order = if sql.take("ASC") {
            Some(Order::Asc)
        } else if sql.take("DESC") {
            Some(Order::Desc)
        } else {
            None
        };
        autoincrement = sql.take("AUTOINCREMENT");
        if sql.pos != sql.tokens.len() {
            return Err(invalid("unexpected constraint key clause"));
        }
        keys.push(if collate.is_none() && order.is_none() {
            Key::Column(column)
        } else {
            Key::Ordered(KeyOptions {
                column,
                collate,
                order,
            })
        });
    }
    Ok((keys, autoincrement))
}
fn flow<S: Serializer>(columns: &[String], s: S) -> std::result::Result<S::Ok, S::Error> {
    FlowSeq(columns).serialize(s)
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ReferentialAction {
    Cascade,
    Restrict,
    SetNull,
    SetDefault,
    NoAction,
}
impl ReferentialAction {
    fn parse(sql: &mut Sql) -> Result<Self> {
        if sql.take("CASCADE") {
            Ok(Self::Cascade)
        } else if sql.take("RESTRICT") {
            Ok(Self::Restrict)
        } else if sql.take("SET") {
            if sql.take("NULL") {
                Ok(Self::SetNull)
            } else {
                sql.expect("DEFAULT")?;
                Ok(Self::SetDefault)
            }
        } else {
            sql.expect("NO")?;
            sql.expect("ACTION")?;
            Ok(Self::NoAction)
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Initially {
    Deferred,
    Immediate,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Conflict {
    Rollback,
    Abort,
    Fail,
    Ignore,
    Replace,
}
impl Conflict {
    fn parse(sql: &mut Sql) -> Result<Self> {
        for (word, value) in [
            ("ROLLBACK", Self::Rollback),
            ("ABORT", Self::Abort),
            ("FAIL", Self::Fail),
            ("IGNORE", Self::Ignore),
            ("REPLACE", Self::Replace),
        ] {
            if sql.take(word) {
                return Ok(value);
            }
        }
        Err(invalid("invalid ON CONFLICT action"))
    }
}
fn names(sql: &mut Sql) -> Result<Vec<String>> {
    let body = sql.group()?;
    split(&body)
        .iter()
        .map(|part| {
            if part.len() != 1 {
                return Err(invalid("expected a column name"));
            }
            identifier(&part[0])
        })
        .collect()
}
impl Constraint {
    pub(super) fn parse(tokens: &[Token]) -> Result<Self> {
        let mut sql = Sql {
            tokens: tokens.to_vec(),
            pos: 0,
        };
        let mut value = Self::default();
        if sql.take("CONSTRAINT") {
            value.name = Some(sql.name()?);
        }
        if sql.take("PRIMARY") {
            sql.expect("KEY")?;
            let (keys, autoincrement) = keys(&mut sql)?;
            value.primary_key = Some(FlowSeq(keys));
            value.autoincrement = autoincrement;
        } else if sql.take("UNIQUE") {
            let (keys, autoincrement) = keys(&mut sql)?;
            if autoincrement {
                return Err(invalid("AUTOINCREMENT requires a primary key"));
            }
            value.unique = Some(FlowSeq(keys));
        } else if sql.take("CHECK") {
            value.check = Some(render(&sql.group()?));
        } else {
            sql.expect("FOREIGN")?;
            sql.expect("KEY")?;
            value.foreign_key = Some(FlowSeq(names(&mut sql)?));
            sql.expect("REFERENCES")?;
            value.references = Some(Reference {
                table: sql.name()?,
                columns: if sql.tokens.get(sql.pos) == Some(&Token::LParen) {
                    names(&mut sql)?
                } else {
                    vec![]
                },
            });
        }
        while sql.pos < sql.tokens.len() {
            if sql.take("ON") {
                if sql.take("CONFLICT") {
                    value.on_conflict = Some(Conflict::parse(&mut sql)?);
                } else if sql.take("DELETE") {
                    value.on_delete = Some(ReferentialAction::parse(&mut sql)?);
                } else {
                    sql.expect("UPDATE")?;
                    value.on_update = Some(ReferentialAction::parse(&mut sql)?);
                }
            } else if sql.take("MATCH") {
                value.match_name = Some(sql.name()?);
            } else if sql.take("DEFERRABLE") {
                value.deferrable = Some(true);
            } else if sql.take("NOT") {
                sql.expect("DEFERRABLE")?;
                value.deferrable = Some(false);
            } else if sql.take("INITIALLY") {
                value.initially = Some(if sql.take("DEFERRED") {
                    Initially::Deferred
                } else {
                    sql.expect("IMMEDIATE")?;
                    Initially::Immediate
                });
            } else {
                return Err(invalid("unsupported constraint clause"));
            }
        }
        Ok(value)
    }
}
