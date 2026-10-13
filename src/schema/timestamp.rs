//! Normalize timestamp storage on SQL writes, including nested typed arrays.
use super::{Types, decode, encode, quote};
use crate::{Result, Type};
use rusqlite::{Connection, functions::FunctionFlags, types::Value as SqlValue};
use std::collections::BTreeMap;

fn contains_timestamp(ty: &Type) -> bool {
    match ty {
        Type::Timestamp => true,
        Type::Array(inner) => contains_timestamp(inner),
        _ => false,
    }
}

struct Column {
    name: String,
    function: String,
    generated: bool,
}

fn tables(db: &Connection, types: &Types) -> Result<BTreeMap<String, Vec<Column>>> {
    let mut tables = BTreeMap::<String, Vec<Column>>::new();
    for (i, ((table, column), ty)) in types.iter().enumerate() {
        if contains_timestamp(ty) {
            let hidden: i64 = db.query_row(
                "SELECT hidden FROM pragma_table_xinfo(?1) WHERE name=?2",
                [table, column],
                |r| r.get(0),
            )?;
            tables.entry(table.clone()).or_default().push(Column {
                name: column.clone(),
                function: format!("__boxdb_timestamp_{i}"),
                generated: hidden != 0,
            });
        }
    }
    Ok(tables)
}

fn differs(columns: &[Column], prefix: &str) -> String {
    columns
        .iter()
        .map(|c| {
            let name = format!("{prefix}{}", quote(&c.name));
            format!("{name} IS NOT {}({name})", c.function)
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn assignments(columns: &[Column], prefix: &str) -> String {
    columns
        .iter()
        .filter(|c| !c.generated)
        .map(|c| {
            format!(
                "{}={}({prefix}{})",
                quote(&c.name),
                c.function,
                quote(&c.name)
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn identity(db: &Connection, table: &str) -> Result<String> {
    let columns = db
        .prepare("SELECT name,pk FROM pragma_table_xinfo(?1) ORDER BY pk,cid")?
        .query_map([table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let without_rowid: bool = db.query_row(
        "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    if !without_rowid
        && let Some(alias) = ["_rowid_", "rowid", "oid"].into_iter().find(|alias| {
            !columns
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(alias))
        })
    {
        return Ok(format!("{} IS NEW.{}", quote(alias), quote(alias)));
    }
    // WITHOUT ROWID tables have a non-null primary key. If all rowid aliases
    // are shadowed on an ordinary table, match its complete row instead; equal
    // duplicate rows require the same normalization.
    Ok(columns
        .iter()
        .filter(|(_, pk)| !without_rowid || *pk != 0)
        .map(|(name, _)| format!("{} COLLATE BINARY IS NEW.{}", quote(name), quote(name)))
        .collect::<Vec<_>>()
        .join(" AND "))
}

pub(super) fn install(db: &Connection, types: &Types) -> Result<()> {
    for (i, (_, ty)) in types.iter().enumerate() {
        if contains_timestamp(ty) {
            let ty = ty.clone();
            db.create_scalar_function(
                format!("__boxdb_timestamp_{i}").as_str(),
                1,
                FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
                move |ctx| {
                    decode(&ty, ctx.get::<SqlValue>(0)?)
                        .and_then(|value| encode(&value))
                        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
                },
            )?;
        }
    }
    for (i, (table, columns)) in tables(db, types)?.into_iter().enumerate() {
        let identity = identity(db, &table)?;
        let dirty = differs(&columns, "");
        let set = assignments(&columns, "");
        let generated: Vec<_> = columns.iter().filter(|c| c.generated).collect();
        let generated_check = generated
            .iter()
            .map(|c| {
                let name = format!("NEW.{}", quote(&c.name));
                format!("{name} IS NOT {}({name})", c.function)
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut body = String::new();
        if !generated_check.is_empty() {
            body.push_str(&format!("SELECT RAISE(ABORT,'generated TIMESTAMP must be canonical UTC') WHERE {generated_check};"));
        }
        if !set.is_empty() {
            body.push_str(&format!(
                "UPDATE {} SET {set} WHERE {identity} AND ({dirty});",
                quote(&table)
            ));
            // An outer OR IGNORE can suppress a UNIQUE failure in the UPDATE.
            // Do not let that leave the original, noncanonical value stored.
            body.push_str(&format!("SELECT RAISE(ABORT,'TIMESTAMP normalization failed') WHERE EXISTS(SELECT 1 FROM {} WHERE {identity} AND ({dirty}));", quote(&table)));
        }
        for action in ["INSERT", "UPDATE"] {
            db.execute_batch(&format!(
                "CREATE TEMP TRIGGER {} AFTER {action} ON main.{} WHEN {} BEGIN {body} END",
                quote(&format!("__boxdb_validate_timestamp_{i}_{action}")),
                quote(&table),
                differs(&columns, "NEW.")
            ))?;
        }
    }
    Ok(())
}

/// Backfill a newly added timestamp default while DDL still has its savepoint.
pub(crate) fn normalize(db: &Connection, types: &Types) -> Result<()> {
    for (table, columns) in tables(db, types)? {
        let set = assignments(&columns, "");
        if !set.is_empty() {
            db.execute_batch(&format!(
                "UPDATE {} SET {set} WHERE {};",
                quote(&table),
                differs(&columns, "")
            ))?;
        }
    }
    Ok(())
}
