//! Schema-only exports retain physical SQLite SQL and logical BoxDB metadata.
use crate::{Error, Result, migrations, readable, schema, storage};
use rusqlite::{Connection, types::Value};

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
        SchemaFormat::Yaml => readable::encode_schema(&transaction),
    }
}

fn sql(db: &Connection) -> Result<Vec<u8>> {
    let mut output =
        String::from("-- BoxDB schema: SQLite DDL and logical-type/migration metadata.\nBEGIN;\n");
    for pragma in ["application_id", "user_version"] {
        let value: i64 = db.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))?;
        output.push_str(&format!("PRAGMA {pragma}={value};\n"));
    }
    let mut statement = db.prepare("SELECT type,name,sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*' ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END,name")?;
    let objects = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (_, _, sql) in &objects {
        if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE") {
            return Err(Error::new(
                "schema dump of virtual tables is unsupported".into(),
            ));
        }
        output.push_str(sql);
        output.push_str(";\n");
    }
    for table in [schema::META, migrations::TABLE] {
        if !objects
            .iter()
            .any(|(kind, name, _)| kind == "table" && name == table)
        {
            continue;
        }
        let order = if table == schema::META {
            "table_name,column_name"
        } else {
            "version"
        };
        let mut statement = db.prepare(&format!(
            "SELECT * FROM {} ORDER BY {order}",
            schema::quote(table)
        ))?;
        let columns = statement
            .column_names()
            .into_iter()
            .map(schema::quote)
            .collect::<Vec<_>>()
            .join(",");
        let count = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let values = (0..count)
                .map(|i| literal(row.get(i)?))
                .collect::<Result<Vec<_>>>()?
                .join(",");
            output.push_str(&format!(
                "INSERT INTO {}({columns}) VALUES({values});\n",
                schema::quote(table)
            ));
        }
    }
    output.push_str("COMMIT;\n");
    if output.len() > storage::MAX_SNAPSHOT {
        return Err(Error::new("schema dump exceeds 256 MiB limit".into()));
    }
    Ok(output.into_bytes())
}

fn literal(value: Value) -> Result<String> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Integer(n) => n.to_string(),
        Value::Real(n) if n.is_finite() => n.to_string(),
        Value::Real(_) => return Err(Error::new("non-finite schema metadata value".into())),
        Value::Text(text) if text.contains('\0') => {
            format!("CAST(X'{}' AS TEXT)", hex::encode(text))
        }
        Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        Value::Blob(bytes) => format!("X'{}'", hex::encode(bytes)),
    })
}
