//! Normalize native elements in nested typed arrays stored as opaque text.
use super::{Types, decode, encode, quote};
use crate::{Error, Result, Type};
use rusqlite::{Connection, functions::FunctionFlags, types::Value as SqlValue};
use std::collections::BTreeMap;

fn contains_native(ty: &Type) -> bool {
    match ty {
        Type::Array(inner) => contains_native(inner),
        _ => ty.native_kind().is_some(),
    }
}
fn needs_normalization(ty: &Type) -> bool {
    matches!(ty, Type::Array(_)) && contains_native(ty)
}

struct Column {
    name: String,
    function: String,
    generated: bool,
}

fn tables(db: &Connection, types: &Types) -> Result<BTreeMap<String, Vec<Column>>> {
    let mut tables = BTreeMap::<String, Vec<Column>>::new();
    for (i, ((table, column), ty)) in types.iter().enumerate() {
        if needs_normalization(ty) {
            let hidden: i64 = db.query_row(
                "SELECT hidden FROM pragma_table_xinfo(?1,'main') WHERE name=?2",
                [table, column],
                |r| r.get(0),
            )?;
            tables.entry(table.clone()).or_default().push(Column {
                name: column.clone(),
                function: format!("__boxdb_array_{i}"),
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

fn identity(db: &Connection, table: &str) -> Result<Vec<String>> {
    let columns = db
        .prepare("SELECT name,pk FROM pragma_table_xinfo(?1,'main') ORDER BY pk,cid")?
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
        return Ok(vec![alias.into()]);
    }
    // WITHOUT ROWID tables have a non-null primary key. If all rowid aliases
    // are shadowed on an ordinary table, match its complete row instead; equal
    // duplicate rows require the same normalization.
    Ok(columns
        .iter()
        .filter(|(_, pk)| !without_rowid || *pk != 0)
        .map(|(name, _)| name.clone())
        .collect())
}

pub(super) fn install(db: &Connection, types: &Types) -> Result<()> {
    for (i, (_, ty)) in types.iter().enumerate() {
        if needs_normalization(ty) {
            let ty = ty.clone();
            db.create_scalar_function(
                format!("__boxdb_array_{i}").as_str(),
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
        let keys = identity(db, &table)?;
        let identity = keys
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{} COLLATE BINARY IS ?{}", quote(name), i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
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
            body.push_str(&format!("SELECT RAISE(ABORT,'generated typed array must be canonical') WHERE {generated_check};"));
        }
        if !set.is_empty() {
            // SQLite forbids qualified UPDATE targets inside trigger SQL. A
            // callback can explicitly update main even when temp shadows it.
            let update = format!(
                "UPDATE main.{} SET {set} WHERE {identity} AND ({dirty})",
                quote(&table)
            );
            let check = format!(
                "SELECT EXISTS(SELECT 1 FROM main.{} WHERE {identity} AND ({dirty}))",
                quote(&table)
            );
            let writer = format!("__boxdb_array_write_{i}");
            db.create_scalar_function(
                writer.as_str(),
                keys.len() as i32,
                FunctionFlags::SQLITE_UTF8,
                move |ctx| {
                    let params = (0..ctx.len())
                        .map(|i| ctx.get::<SqlValue>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    // SAFETY: the borrowed handle stays on the callback thread
                    // and cannot outlive the SQLite function context.
                    let connection = unsafe { ctx.get_connection()? };
                    connection.execute(&update, rusqlite::params_from_iter(&params))?;
                    let dirty: bool =
                        connection
                            .query_row(&check, rusqlite::params_from_iter(&params), |r| r.get(0))?;
                    if dirty {
                        return Err(rusqlite::Error::UserFunctionError(Box::new(Error::new(
                            "typed array normalization failed".into(),
                        ))));
                    }
                    Ok(0i64)
                },
            )?;
            let args = keys
                .iter()
                .map(|name| format!("NEW.{}", quote(name)))
                .collect::<Vec<_>>()
                .join(",");
            body.push_str(&format!("SELECT {writer}({args});"));
        }
        for action in ["INSERT", "UPDATE"] {
            db.execute_batch(&format!(
                "CREATE TEMP TRIGGER {} AFTER {action} ON main.{} WHEN {} BEGIN {body} END",
                quote(&format!("__boxdb_validate_instant_{i}_{action}")),
                quote(&table),
                differs(&columns, "NEW.")
            ))?;
        }
    }
    Ok(())
}

/// Normalize imported rows or added defaults inside the caller's transaction.
pub(crate) fn normalize(db: &Connection, types: &Types) -> Result<()> {
    for (table, columns) in tables(db, types)? {
        let set = assignments(&columns, "");
        if !set.is_empty() {
            db.execute_batch(&format!(
                "UPDATE main.{} SET {set} WHERE {};",
                quote(&table),
                differs(&columns, "")
            ))?;
        }
        let dirty: bool = db.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM main.{} WHERE {})",
                quote(&table),
                differs(&columns, "")
            ),
            [],
            |r| r.get(0),
        )?;
        if dirty {
            return Err(Error::new("generated typed array must be canonical".into()));
        }
    }
    Ok(())
}
