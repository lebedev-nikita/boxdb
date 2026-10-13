use super::{MAX_BYTES, declare_index, declare_table, error};
use crate::{Error, Result, Type, Value, engine::State};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Database {
        version: u32,
    },
    Table {
        name: String,
        next_rowid: u64,
        sql: String,
    },
    Index {
        name: String,
        sql: String,
    },
    Row {
        table: String,
        id: u64,
        values: Vec<Option<String>>,
    },
}
fn encode_value(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::Array(values) => {
            let values = values
                .iter()
                .map(encode_value)
                .collect::<Result<Vec<_>>>()?;
            serde_json::to_string(&values).map(Some).map_err(error)
        }
        _ => Ok(Some(value.to_string())),
    }
}
fn decode_value(ty: &Type, value: Option<String>) -> Result<Value> {
    let Some(text) = value else {
        return ty.validate(&Value::Null);
    };
    let value = match ty {
        Type::Bytea => Value::Bytea(hex::decode(text).map_err(error)?),
        Type::Array(inner) => {
            let values: Vec<Option<String>> = serde_json::from_str(&text).map_err(error)?;
            Value::Array(
                values
                    .into_iter()
                    .map(|value| decode_value(inner, value))
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        _ => return ty.cast(Value::Text(text)),
    };
    ty.validate(&value)
}
pub(super) fn encode(state: &State) -> Result<Vec<u8>> {
    let mut records = vec![Record::Database { version: 1 }];
    for (name, table) in &state.tables {
        records.push(Record::Table {
            name: name.clone(),
            next_rowid: table.next_rowid,
            sql: table.sql.clone(),
        });
        for (id, row) in table.rows.records() {
            records.push(Record::Row {
                table: name.clone(),
                id,
                values: row.iter().map(encode_value).collect::<Result<Vec<_>>>()?,
            });
        }
    }
    for (name, index) in &state.indexes {
        records.push(Record::Index {
            name: name.clone(),
            sql: index.sql.clone(),
        });
    }
    let mut text = vec![];
    for record in records {
        serde_json::to_writer(&mut text, &record).map_err(error)?;
        text.push(b'\n');
    }
    if text.len() as u64 > MAX_BYTES {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    Ok(text)
}
pub(super) fn decode(bytes: &[u8]) -> Result<State> {
    let text = std::str::from_utf8(bytes).map_err(error)?;
    let mut records = vec![];
    for (line_number, line) in text.lines().enumerate() {
        let record: Record = serde_json::from_str(line)
            .map_err(|e| Error(format!("line {}: {e}", line_number + 1)))?;
        records.push(record);
    }
    if !matches!(records.first(), Some(Record::Database { version: 1 }))
        || records
            .iter()
            .skip(1)
            .any(|r| matches!(r, Record::Database { .. }))
    {
        return Err(Error("invalid database header or format version".into()));
    }
    let mut state = State::default();
    // Load all declarations before rows: a child can sort before its parent.
    for record in &records {
        if let Record::Table {
            name,
            sql,
            next_rowid,
        } = record
        {
            declare_table(&mut state, name, sql, *next_rowid)?;
        }
    }
    for record in &records {
        if let Record::Index { name, sql } = record {
            declare_index(&mut state, name, sql)?;
        }
    }
    for record in records {
        if let Record::Row { table, id, values } = record {
            let table = state
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error("row references unknown table".into()))?;
            if id == 0 || id >= table.next_rowid || table.rows.last_id() >= id {
                return Err(Error("invalid or duplicate row id".into()));
            }
            if values.len() != table.columns.len() {
                return Err(Error("column count mismatch".into()));
            }
            let values = table
                .columns
                .iter()
                .zip(values)
                .map(|(column, value)| decode_value(&column.ty, value))
                .collect::<Result<Vec<_>>>()?;
            table.rows.push(id, values)?;
        }
    }
    Ok(state)
}
