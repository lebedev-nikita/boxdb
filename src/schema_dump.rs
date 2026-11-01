//! Schema-only exports as logical BoxDB SQL or YAML.
use crate::{Error, Result, schema, storage};
use rusqlite::Connection;
mod yaml;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SchemaFormat {
    Sql,
    Yaml,
}

pub(crate) fn dump(db: &Connection, format: SchemaFormat) -> Result<Vec<u8>> {
    if !db.is_autocommit() {
        return Err(Error::new(
            "schema dump requires no active transaction".into(),
        ));
    }
    let transaction = db.unchecked_transaction()?;
    schema::load(&transaction)?;
    match format {
        SchemaFormat::Sql => sql(&transaction),
        SchemaFormat::Yaml => yaml::encode(&transaction),
    }
}

fn sql(db: &Connection) -> Result<Vec<u8>> {
    let types = schema::load(db)?;
    let readonly = schema::readonly::columns(db)?;
    let mut output = String::from("-- BoxDB schema.\nBEGIN;\n");
    for pragma in ["application_id", "user_version"] {
        let value: i64 = db.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))?;
        output.push_str(&format!("PRAGMA {pragma}={value};\n"));
    }
    let mut statement = db.prepare("SELECT type,name,sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND lower(name) NOT GLOB 'sqlite_*' AND lower(name) NOT GLOB '__boxdb_*' AND lower(tbl_name) NOT GLOB '__boxdb_*' ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END,name")?;
    let objects = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, name, sql) in &objects {
        if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE") {
            return Err(Error::new(
                "schema dump of virtual tables is unsupported".into(),
            ));
        }
        if kind == "table"
            && types
                .keys()
                .chain(&readonly)
                .any(|(table, _)| table == name)
        {
            output.push_str(&yaml::table_sql(name, sql, &types, &readonly)?);
        } else {
            output.push_str(sql);
        }
        output.push_str(";\n");
    }
    output.push_str("COMMIT;\n");
    if output.len() > storage::MAX_SNAPSHOT {
        return Err(Error::new("schema dump exceeds 256 MiB limit".into()));
    }
    Ok(output.into_bytes())
}
