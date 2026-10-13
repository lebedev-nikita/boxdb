//! A standalone, deterministic YAML stream of SQLite schemas and named rows.
use crate::{
    Error, Result,
    schema::{self, quote},
    storage::MAX_SNAPSHOT,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeMap,
};
use std::{collections::BTreeMap, fmt};

/// Native SQLite cells have natural YAML scalars. BLOBs use an explicit hex
/// mapping so text, byte arrays and numbers remain distinguishable on import.
struct Cell(SqlValue);
impl Serialize for Cell {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match &self.0 {
            SqlValue::Null => serializer.serialize_none(),
            SqlValue::Integer(n) => serializer.serialize_i64(*n),
            SqlValue::Real(n) if n.is_finite() => serializer.serialize_f64(*n),
            SqlValue::Real(_) => Err(serde::ser::Error::custom(
                "readable storage cannot represent non-finite SQLite values",
            )),
            SqlValue::Text(s) => serializer.serialize_str(s),
            SqlValue::Blob(bytes) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("blob", &hex::encode(bytes))?;
                map.end()
            }
        }
    }
}
impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CellVisitor;
        impl<'de> Visitor<'de> for CellVisitor {
            type Value = Cell;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a SQLite scalar or a {blob: hex} mapping")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Null))
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(n)))
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> std::result::Result<Cell, E> {
                i64::try_from(n)
                    .map(|n| Cell(SqlValue::Integer(n)))
                    .map_err(E::custom)
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> std::result::Result<Cell, E> {
                if n.is_finite() {
                    Ok(Cell(SqlValue::Real(n)))
                } else {
                    Err(E::custom("non-finite SQLite value"))
                }
            }
            fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(i64::from(b))))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s.into())))
            }
            fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s)))
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> std::result::Result<Cell, M::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Blob {
                    blob: String,
                }
                let Blob { blob } = Blob::deserialize(MapAccessDeserializer::new(map))?;
                hex::decode(blob)
                    .map(|bytes| Cell(SqlValue::Blob(bytes)))
                    .map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_any(CellVisitor)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Database {
        version: u32,
        application_id: i64,
        user_version: i64,
    },
    Schema {
        object_type: String,
        name: String,
        table: String,
        sql: String,
    },
    Row {
        table: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rowid: Option<i64>,
        values: BTreeMap<String, Cell>,
    },
    Sequence {
        table: String,
        value: i64,
    },
}
fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden=0 ORDER BY cid")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn rowid(db: &Connection, table: &str) -> Result<Option<&'static str>> {
    let without: bool = db.query_row(
        "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    if without {
        return Ok(None);
    }
    // Generated columns also shadow SQLite's implicit rowid aliases.
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_xinfo(?1)")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(["_rowid_", "rowid", "oid"]
        .into_iter()
        .find(|name| !columns.iter().any(|c| c.eq_ignore_ascii_case(name))))
}
pub(crate) fn encode(db: &Connection) -> Result<Vec<u8>> {
    let mut records = vec![Record::Database {
        version: 2,
        application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
        user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
    }];
    let objects:Vec<(String,String,String,String)>=db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END,name")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    for (object_type, name, table, sql) in &objects {
        if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE") {
            return Err(Error(
                "readable export of virtual tables is unsupported".into(),
            ));
        }
        records.push(Record::Schema {
            object_type: object_type.clone(),
            name: name.clone(),
            table: table.clone(),
            sql: sql.clone(),
        });
    }
    for (kind, table, _, _) in &objects {
        if kind != "table" {
            continue;
        }
        let columns = columns(db, table)?;
        let rowid = rowid(db, table)?;
        let selected = columns
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(",");
        let prefix = rowid.map(|r| format!("{},", quote(r))).unwrap_or_default();
        let ordering = rowid.map(quote).unwrap_or_else(|| selected.clone());
        let mut statement = db.prepare(&format!(
            "SELECT {prefix}{selected} FROM {} ORDER BY {ordering}",
            quote(table)
        ))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let offset = usize::from(rowid.is_some());
            let values = columns
                .iter()
                .enumerate()
                .map(|(i, name)| row.get(i + offset).map(|value| (name.clone(), Cell(value))))
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
            records.push(Record::Row {
                table: table.clone(),
                rowid: if rowid.is_some() {
                    Some(row.get(0)?)
                } else {
                    None
                },
                values,
            });
        }
    }
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='sqlite_sequence')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        for row in db
            .prepare("SELECT name,seq FROM sqlite_sequence ORDER BY name")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        {
            let (table, value) = row?;
            records.push(Record::Sequence { table, value });
        }
    }
    let mut bytes = vec![];
    for record in records {
        bytes.extend_from_slice(b"---\n");
        serde_saphyr::to_io_writer(&mut bytes, &record)?;
        if bytes.len() > MAX_SNAPSHOT {
            return Err(Error("snapshot exceeds 256 MiB limit".into()));
        }
    }
    Ok(bytes)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Connection> {
    let records: Vec<Record> = serde_saphyr::from_slice_multiple_with_options(
        bytes,
        serde_saphyr::options! {
            strict_booleans: true,
            reject_unsupported_tags: true,
            // A database stream has one document per row, unlike a typical
            // configuration file. Tie structural budgets to the bounded input.
            budget: serde_saphyr::budget! {
                max_documents: bytes.len().saturating_add(1),
                max_nodes: bytes.len().saturating_add(16),
                max_events: bytes.len().saturating_mul(2).saturating_add(16),
                max_total_scalar_bytes: MAX_SNAPSHOT,
                max_total_comment_bytes: MAX_SNAPSHOT,
            },
        },
    )?;
    let Some(Record::Database {
        version: 2,
        application_id,
        user_version,
    }) = records.first()
    else {
        return Err(Error("invalid readable format version".into()));
    };
    if records
        .iter()
        .skip(1)
        .any(|r| matches!(r, Record::Database { .. }))
    {
        return Err(Error("duplicate readable header".into()));
    }
    let db = Connection::open_in_memory()?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.execute_batch("BEGIN; PRAGMA defer_foreign_keys=ON")?;
    for record in &records {
        if let Record::Schema {
            object_type,
            name,
            sql,
            ..
        } = record
            && object_type == "table"
        {
            if !sql
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE TABLE")
            {
                return Err(Error("invalid table declaration".into()));
            }
            db.prepare(sql)?.execute([])?;
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [name],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error("mismatched schema record".into()));
            }
        }
    }
    for record in &records {
        if let Record::Row {
            table,
            rowid: id,
            values,
        } = record
        {
            let mut columns = columns(&db, table)?;
            if columns.len() != values.len()
                || columns.iter().any(|name| !values.contains_key(name))
            {
                return Err(Error(format!("row columns do not match table: {table}")));
            }
            let mut values = columns
                .iter()
                .map(|name| values[name].0.clone())
                .collect::<Vec<_>>();
            if let Some(id) = id {
                let alias = rowid(&db, table)?.ok_or_else(|| Error("unexpected rowid".into()))?;
                columns.insert(0, alias.into());
                values.insert(0, SqlValue::Integer(*id));
            }
            let names = columns
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = vec!["?"; values.len()].join(",");
            db.execute(
                &format!(
                    "INSERT INTO {}({names}) VALUES({placeholders})",
                    quote(table)
                ),
                rusqlite::params_from_iter(values),
            )?;
        }
    }
    for record in &records {
        match record {
            Record::Schema {
                object_type,
                sql,
                name,
                ..
            } if object_type != "table" => {
                if !["index", "view", "trigger"].contains(&object_type.as_str())
                    || !sql.trim_start().to_ascii_uppercase().starts_with("CREATE ")
                {
                    return Err(Error("invalid schema record".into()));
                }
                db.prepare(sql)?.execute([])?;
                if !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
                    rusqlite::params![object_type, name],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(Error("mismatched schema record".into()));
                }
            }
            Record::Sequence { table, value } => {
                let updated = db.execute(
                    "UPDATE sqlite_sequence SET seq=?1 WHERE name=?2",
                    rusqlite::params![value, table],
                )?;
                if updated == 0 {
                    db.execute(
                        "INSERT INTO sqlite_sequence(name,seq) VALUES(?1,?2)",
                        rusqlite::params![table, value],
                    )?;
                }
            }
            _ => (),
        }
    }
    db.pragma_update(None, "application_id", application_id)?;
    db.pragma_update(None, "user_version", user_version)?;
    db.execute_batch("COMMIT")?;
    let types = schema::load(&db)?;
    schema::validate_all(&db, &types)?;
    Ok(db)
}
