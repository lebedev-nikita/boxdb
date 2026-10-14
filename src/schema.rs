//! Logical BoxDB types and native decimal declarations. Most validation triggers
//! live in TEMP; native decimal precision checks are part of the stored schema.
use crate::{Error, Result, Type, Value, decimal};
use rusqlite::{
    Connection,
    functions::FunctionFlags,
    types::{Value as SqlValue, ValueRef},
};
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::collections::BTreeMap;

pub(crate) const META: &str = "__boxdb_columns";
const META_SQL: &str = "CREATE TABLE IF NOT EXISTS __boxdb_columns(table_name TEXT NOT NULL,column_name TEXT NOT NULL,logical_type TEXT NOT NULL,table_sql TEXT NOT NULL,version INTEGER NOT NULL CHECK(version=1),PRIMARY KEY(table_name,column_name)) STRICT";
pub(crate) type Types = BTreeMap<(String, String), Type>;
pub(crate) fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
pub(crate) fn keyword(mut sql: &str) -> &str {
    loop {
        sql = sql.trim_start();
        if let Some(comment) = sql.strip_prefix("--") {
            sql = comment.split_once('\n').map_or("", |(_, rest)| rest);
            continue;
        }
        if let Some(comment) = sql.strip_prefix("/*") {
            sql = comment.split_once("*/").map_or("", |(_, rest)| rest);
            continue;
        }
        break;
    }
    let end = sql
        .bytes()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(sql.len());
    &sql[..end]
}
fn main_name(name: &ObjectName) -> Option<&str> {
    if name.0.len() == 1 || name.0.len() == 2 && name.0[0].value.eq_ignore_ascii_case("main") {
        name.0.last().map(|i| i.value.as_str())
    } else {
        None
    }
}
fn extension(input: &str) -> Result<Option<Type>> {
    let name = input.trim().to_ascii_uppercase();
    if matches!(
        name.as_str(),
        "REAL" | "FLOAT" | "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" | "TEXT" | "BLOB" | "ANY" | ""
    ) {
        return Ok(None);
    }
    // SERIAL uses SQLite's rowid-backed INTEGER.
    if name == "SERIAL" {
        return Ok(Some(Type::Int64));
    }
    match Type::parse(input) {
        Ok(ty) => Ok(Some(ty)),
        Err(e)
            if name.starts_with("INT")
                || name.starts_with("UINT")
                || name == "SMALLINT"
                || name == "BIGINT"
                || name.starts_with("NUMERIC")
                // Decimal declarations must use supported parameters.
                || name.starts_with("DECIMAL")
                || name.starts_with("VARCHAR")
                || name.contains("[]") =>
        {
            Err(e)
        }
        Err(_) => Ok(None),
    }
}
mod arrays;
mod create;
pub(crate) use arrays::normalize as normalize_arrays;
pub(crate) struct Declaration {
    pub schema: String,
    pub select: bool,
    pub sql: String,
    pub table: String,
    pub columns: Vec<(String, Type)>,
}
fn translate_column(column: &mut ColumnDef) -> Result<Option<Type>> {
    if let Some(ty) = extension(&column.data_type.to_string())? {
        ty.validate(&Value::Null)?;
        if ty == Type::Int64 {
            column.data_type = DataType::Custom(ObjectName(vec![Ident::new("INTEGER")]), vec![]);
            return Ok(None);
        }
        let storage = match ty {
            Type::Int8
            | Type::Int16
            | Type::Int32
            | Type::Uint8
            | Type::Uint16
            | Type::Uint32
            | Type::Boolean => "INTEGER",
            Type::Decimal(_) => "DECIMAL",
            Type::Uuid => "UUID",
            Type::Instant => "INSTANT",
            Type::Date => "DATE",
            Type::DateTime => "DATETIME",
            Type::Interval => "INTERVAL",
            Type::Real => "REAL",
            Type::Bytea => "BLOB",
            _ => "TEXT",
        };
        column.data_type = DataType::Custom(ObjectName(vec![Ident::new(storage)]), vec![]);
        // Store constraints in the schema; decimal precision is checked by the engine.
        let check = match &ty {
            Type::Decimal(Some((precision, scale))) => Some(format!(
                "__boxdb_decimal_valid({},{precision},{scale})",
                quote(&column.name.value)
            )),
            Type::Int8 => Some(format!(
                "{} BETWEEN -128 AND 127",
                quote(&column.name.value)
            )),
            Type::Uint8 => Some(format!("{} BETWEEN 0 AND 255", quote(&column.name.value))),
            Type::Uint16 => Some(format!("{} BETWEEN 0 AND 65535", quote(&column.name.value))),
            Type::Uint32 => Some(format!(
                "{} BETWEEN 0 AND 4294967295",
                quote(&column.name.value)
            )),
            Type::Int16 => Some(format!(
                "{} BETWEEN -32768 AND 32767",
                quote(&column.name.value)
            )),
            Type::Int32 => Some(format!(
                "{} BETWEEN -2147483648 AND 2147483647",
                quote(&column.name.value)
            )),
            Type::Boolean => Some(format!("{} IN(0,1)", quote(&column.name.value))),
            Type::Varchar(Some(n)) => Some(format!("length({})<={n}", quote(&column.name.value))),
            Type::Bytea => Some(format!(
                "{} IS NULL OR typeof({})='blob'",
                quote(&column.name.value),
                quote(&column.name.value)
            )),
            _ => None,
        };
        if let Some(check) = check {
            let parsed = Parser::parse_sql(&GenericDialect {}, &format!("SELECT {check}"))?;
            if let Statement::Query(q) = &parsed[0]
                && let SetExpr::Select(s) = q.body.as_ref()
                && let SelectItem::UnnamedExpr(expr) = &s.projection[0]
            {
                column.options.push(ColumnOptionDef {
                    name: None,
                    option: ColumnOption::Check(expr.clone()),
                });
            }
        }
        return Ok(Some(ty));
    }
    let native = match column.data_type.to_string().to_ascii_uppercase().as_str() {
        "" => Some("ANY"),
        "FLOAT" | "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" => Some("REAL"),
        _ => None,
    };
    if let Some(native) = native {
        column.data_type = DataType::Custom(ObjectName(vec![Ident::new(native)]), vec![]);
    }
    Ok(None)
}
pub(crate) fn declaration(sql: &str) -> Result<Option<Declaration>> {
    if !keyword(sql).eq_ignore_ascii_case("CREATE") {
        return Ok(None);
    }
    create::declaration(sql)
}

pub(crate) struct Alteration {
    main: bool,
    pub sql: String,
    pub table: String,
    pub columns: Vec<(String, Type)>,
    changes: Vec<AlterTableOperation>,
}
pub(crate) fn alteration(sql: &str) -> Result<Option<Alteration>> {
    let Ok(mut statements) = Parser::parse_sql(&GenericDialect {}, sql) else {
        return Ok(None);
    };
    let [statement] = statements.as_mut_slice() else {
        return Ok(None);
    };
    let Statement::AlterTable {
        name, operations, ..
    } = statement
    else {
        return Ok(None);
    };
    let main = main_name(name).is_some();
    let table = name
        .0
        .last()
        .ok_or_else(|| Error::new("missing table name".into()))?
        .value
        .clone();
    let mut columns = vec![];
    let mut rewritten = false;
    for op in operations.iter_mut() {
        if let AlterTableOperation::AddColumn { column_def, .. } = op {
            let original = column_def.data_type.clone();
            if let Some(ty) = translate_column(column_def)? {
                if !main && ty.native_kind().is_none() {
                    return Err(Error::new(
                        "extended types require main database tables".into(),
                    ));
                }
                if main {
                    columns.push((column_def.name.value.clone(), ty));
                }
            }
            rewritten |= original != column_def.data_type;
        }
    }
    let changes = operations.clone();
    Ok(Some(Alteration {
        main,
        sql: if !rewritten {
            sql.into()
        } else {
            statement.to_string()
        },
        table,
        columns,
        changes,
    }))
}
pub(crate) fn save_alteration(db: &Connection, change: &Alteration) -> Result<()> {
    if !change.main {
        return Ok(());
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
        [META],
        |r| r.get(0),
    )?;
    if !exists && change.columns.is_empty() {
        return Ok(());
    }
    db.execute_batch(META_SQL)?;
    let mut table = change.table.clone();
    for op in &change.changes {
        match op {
            AlterTableOperation::RenameTable { table_name } => {
                let new = table_name.0.last().unwrap().value.clone();
                db.execute(
                    "UPDATE __boxdb_columns SET table_name=?1 WHERE table_name=?2 COLLATE NOCASE",
                    rusqlite::params![new, table],
                )?;
                table = new;
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => {
                db.execute("UPDATE __boxdb_columns SET column_name=?1 WHERE table_name=?2 COLLATE NOCASE AND column_name=?3 COLLATE NOCASE",rusqlite::params![new_column_name.value,table,old_column_name.value])?;
            }
            _ => (),
        }
    }
    let sql: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='table'",
        [&table],
        |r| r.get(0),
    )?;
    db.execute("DELETE FROM __boxdb_columns WHERE table_name=?1 COLLATE NOCASE AND column_name NOT IN(SELECT name FROM pragma_table_xinfo(?1))",[&table])?;
    db.execute(
        "UPDATE __boxdb_columns SET table_sql=?1 WHERE table_name=?2 COLLATE NOCASE",
        rusqlite::params![sql, table],
    )?;
    for (column, ty) in &change.columns {
        db.execute(
            "INSERT INTO __boxdb_columns VALUES(?1,?2,?3,?4,1)",
            rusqlite::params![table, column, serde_json::to_string(ty)?, sql],
        )?;
    }
    Ok(())
}
pub(crate) fn save(db: &Connection, declaration: &Declaration) -> Result<()> {
    if declaration.columns.is_empty() {
        return Ok(());
    }
    db.execute_batch(META_SQL)?;
    if db.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))? == 0 {
        db.pragma_update(None, "application_id", 0x424f5844i64)?;
    }
    let sql: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
        [&declaration.table],
        |r| r.get(0),
    )?;
    for (column, ty) in &declaration.columns {
        db.execute(
            "INSERT INTO __boxdb_columns VALUES(?1,?2,?3,?4,1)",
            rusqlite::params![declaration.table, column, serde_json::to_string(ty)?, sql],
        )?;
    }
    Ok(())
}
pub(crate) fn load(db: &Connection) -> Result<Types> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1 AND type='table')",
        [META],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Types::new());
    }
    let mut types = Types::new();
    let mut statement=db.prepare("SELECT m.table_name,m.column_name,m.logical_type,m.version,m.table_sql,s.sql FROM __boxdb_columns m LEFT JOIN sqlite_schema s ON s.name=m.table_name AND s.type='table' ORDER BY m.table_name,m.column_name")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let column: String = row.get(1)?;
        let text: String = row.get(2)?;
        let expected: String = row.get(4)?;
        let actual: Option<String> = row.get(5)?;
        if row.get::<_, i64>(3)? != 1 || actual.as_deref() != Some(&expected) {
            return Err(Error::new(format!(
                "BoxDB metadata does not match schema: {table}; change typed schemas through BoxDB"
            )));
        }
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE name=?2)",
            rusqlite::params![table, column],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::new(
                "BoxDB metadata references a missing column".into(),
            ));
        }
        let ty: Type = serde_json::from_str(&text)?;
        if matches!(ty, Type::Decimal(_)) {
            let physical: String = db.query_row(
                "SELECT type FROM pragma_table_xinfo(?1) WHERE name=?2",
                rusqlite::params![table, column],
                |row| row.get(0),
            )?;
            if !physical.eq_ignore_ascii_case("DECIMAL") {
                return Err(Error::new(format!(
                    "native DECIMAL requires DECIMAL storage: {table}.{column}"
                )));
            }
        }
        ty.validate(&Value::Null)?;
        types.insert((table, column), ty);
    }
    Ok(types)
}
pub(crate) fn cleanup(db: &Connection) -> Result<()> {
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
        [META],
        |r| r.get::<_, bool>(0),
    )? {
        db.execute("DELETE FROM __boxdb_columns WHERE table_name NOT IN(SELECT name FROM sqlite_schema WHERE type='table')",[])?;
    }
    Ok(())
}
pub(crate) fn uninstall(db: &Connection) -> Result<()> {
    let names:Vec<String>=db.prepare("SELECT name FROM sqlite_temp_schema WHERE type='trigger' AND name GLOB '__boxdb_validate_*'")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for name in names {
        db.execute_batch(&format!("DROP TRIGGER temp.{}", quote(&name)))?;
    }
    Ok(())
}
pub(crate) fn install(db: &Connection, types: &Types) -> Result<()> {
    uninstall(db)?;
    for (i, ((table, column), ty)) in types.iter().enumerate() {
        if ty.native_kind().is_some() {
            continue;
        }
        let function = format!("__boxdb_validate_{i}");
        let ty = ty.clone();
        db.create_scalar_function(
            function.as_str(),
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                // Primitive checks borrow SQLite's cells instead of copying
                // strings or large blobs just to validate a write.
                match (&ty, ctx.get_raw(0)) {
                    (_, ValueRef::Null) | (Type::Bytea, ValueRef::Blob(_)) => return Ok(1i64),
                    (Type::Decimal(_), ValueRef::Blob(bytes)) => {
                        let text = std::str::from_utf8(bytes)
                            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
                        return ty
                            .cast(Value::Text(text.into()))
                            .map(|_| 1)
                            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)));
                    }
                    (Type::Boolean, ValueRef::Integer(0 | 1)) => return Ok(1),
                    (Type::Int16, ValueRef::Integer(n)) if i16::try_from(n).is_ok() => {
                        return Ok(1);
                    }
                    (Type::Int32, ValueRef::Integer(n)) if i32::try_from(n).is_ok() => {
                        return Ok(1);
                    }
                    (Type::Varchar(limit), ValueRef::Text(bytes)) => {
                        let text = std::str::from_utf8(bytes)
                            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
                        if limit.is_none_or(|n| text.chars().count() <= n) {
                            return Ok(1);
                        }
                    }
                    _ => (),
                }
                decode(&ty, ctx.get::<SqlValue>(0)?)
                    .map(|_| 1i64)
                    .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
            },
        )?;
        for action in ["INSERT", "UPDATE"] {
            db.execute_batch(&format!("CREATE TEMP TRIGGER {} AFTER {action} ON main.{} BEGIN SELECT {function}(NEW.{}); END",quote(&format!("__boxdb_validate_{i}_{action}")),quote(table),quote(column)))?;
        }
    }
    arrays::install(db, types)?;
    Ok(())
}
fn native_value(kind: libsqlite3_sys::native::Kind, text: String) -> Value {
    match kind {
        libsqlite3_sys::native::Kind::Uuid => Value::Uuid(text),
        libsqlite3_sys::native::Kind::Decimal => Value::Decimal(text),
        libsqlite3_sys::native::Kind::Instant => Value::Instant(text),
        libsqlite3_sys::native::Kind::Date => Value::Date(text),
        libsqlite3_sys::native::Kind::DateTime => Value::DateTime(text),
        libsqlite3_sys::native::Kind::Interval => Value::Interval(text),
    }
}
pub(crate) fn native(value: SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(n) => Value::Integer(n),
        SqlValue::Real(n) => Value::Real(n),
        SqlValue::Text(s) => Value::Text(s),
        SqlValue::Blob(b) => match decimal::decode(&b) {
            Some(text) => Value::Decimal(text.into()),
            None => match libsqlite3_sys::native::decode(&b) {
                Some((kind, text)) => native_value(kind, text),
                None => Value::Bytea(decimal::unescape_blob(b)),
            },
        },
    }
}
pub(crate) fn encode(value: &Value) -> Result<SqlValue> {
    match value {
        Value::Decimal(text) => return Ok(SqlValue::Blob(decimal::encode(text)?)),
        Value::Uuid(text) => {
            return libsqlite3_sys::native::encode(libsqlite3_sys::native::Kind::Uuid, text)
                .map(SqlValue::Blob)
                .map_err(|e| Error::new(e.0));
        }
        Value::Instant(text)
        | Value::Date(text)
        | Value::DateTime(text)
        | Value::Interval(text) => {
            let kind = match value {
                Value::Instant(_) => libsqlite3_sys::native::Kind::Instant,
                Value::Date(_) => libsqlite3_sys::native::Kind::Date,
                Value::DateTime(_) => libsqlite3_sys::native::Kind::DateTime,
                _ => libsqlite3_sys::native::Kind::Interval,
            };
            return libsqlite3_sys::native::encode(kind, text)
                .map(SqlValue::Blob)
                .map_err(|e| Error::new(e.0));
        }
        _ => (),
    }
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Integer(n) => SqlValue::Integer(*n),
        Value::UnsignedInteger(n) => SqlValue::Integer(i64::try_from(*n).map_err(|error| {
            Error::new("unsigned value exceeds SQLite INTEGER range".into()).with_source(error)
        })?),
        Value::Real(n) if n.is_finite() => SqlValue::Real(*n),
        Value::Real(_) => return Err(Error::new("non-finite floating point value".into())),
        Value::Boolean(b) => SqlValue::Integer(i64::from(*b)),
        Value::Bytea(b) => SqlValue::Blob(decimal::escape_blob(b)),
        Value::Json(j) => SqlValue::Text(j.to_string()),
        Value::Array(a) => SqlValue::Text(array_json(a, 0)?.to_string()),
        _ => SqlValue::Text(value.to_string()),
    })
}
fn array_json(values: &[Value], depth: usize) -> Result<serde_json::Value> {
    use serde_json::Value as J;
    if depth > 32 {
        return Err(Error::new("array nesting exceeds limit".into()));
    }
    Ok(J::Array(
        values
            .iter()
            .map(|v| {
                Ok(match v {
                    Value::Null => J::Null,
                    Value::Integer(n) => J::from(*n),
                    Value::UnsignedInteger(n) => J::from(*n),
                    Value::Real(n) if n.is_finite() => J::from(*n),
                    Value::Real(_) => return Err(Error::new("non-finite array element".into())),
                    Value::Boolean(b) => J::from(*b),
                    Value::Array(a) => array_json(a, depth + 1)?,
                    Value::Json(j) => j.clone(),
                    Value::Bytea(bytes) => J::String(hex::encode(bytes)),
                    Value::Decimal(text) => {
                        J::String(decimal::parse(text)?.normalized().to_plain_string())
                    }
                    Value::Uuid(_)
                    | Value::Instant(_)
                    | Value::Date(_)
                    | Value::DateTime(_)
                    | Value::Interval(_) => {
                        let ty = match v {
                            Value::Uuid(_) => Type::Uuid,
                            Value::Instant(_) => Type::Instant,
                            Value::Date(_) => Type::Date,
                            Value::DateTime(_) => Type::DateTime,
                            _ => Type::Interval,
                        };
                        J::String(ty.validate(v)?.to_string())
                    }
                    _ => match encode(v)? {
                        SqlValue::Text(s) => J::String(s),
                        SqlValue::Blob(b) => J::String(hex::encode(b)),
                        _ => unreachable!(),
                    },
                })
            })
            .collect::<Result<Vec<_>>>()?,
    ))
}
fn array_value(ty: &Type, json: serde_json::Value) -> Result<Value> {
    use serde_json::Value as J;
    if json == J::Null {
        return Ok(Value::Null);
    }
    if let Type::Array(inner) = ty {
        let J::Array(values) = json else {
            return Err(Error::new("type mismatch: expected array".into()));
        };
        return Ok(Value::Array(
            values
                .into_iter()
                .map(|v| array_value(inner, v))
                .collect::<Result<_>>()?,
        ));
    }
    if matches!(ty, Type::Json) {
        return Ok(Value::Json(json));
    }
    let value = match json {
        J::Bool(b) => Value::Boolean(b),
        J::Number(n) => n
            .as_i64()
            .map(Value::Integer)
            .or_else(|| n.as_f64().map(Value::Real))
            .ok_or_else(|| Error::new("invalid array number".into()))?,
        J::String(s) if matches!(ty, Type::Bytea) => {
            return Ok(Value::Bytea(hex::decode(s).map_err(Error::validation)?));
        }
        J::String(s) if ty.native_kind().is_some() => return ty.cast(Value::Text(s)),
        J::String(s) => return decode(ty, SqlValue::Text(s)),
        _ => return Err(Error::new("invalid array cell".into())),
    };
    ty.validate(&value)
}
pub(crate) fn decode(ty: &Type, value: SqlValue) -> Result<Value> {
    if value == SqlValue::Null {
        return Ok(Value::Null);
    }
    match (ty, value) {
        (Type::Boolean, SqlValue::Integer(n)) if n == 0 || n == 1 => Ok(Value::Boolean(n == 1)),
        (Type::Array(inner), SqlValue::Text(s)) => {
            let json: serde_json::Value = serde_json::from_str(&s)?;
            array_value(&Type::Array(inner.clone()), json)
        }
        (Type::Decimal(_), SqlValue::Blob(bytes)) => {
            let text = decimal::decode(&bytes)
                .ok_or_else(|| Error::new("invalid DECIMAL encoding".into()))?;
            ty.validate(&Value::Decimal(text.into()))
        }
        (Type::Bytea, SqlValue::Blob(bytes)) => Ok(Value::Bytea(decimal::unescape_blob(bytes))),
        (Type::Varchar(limit), SqlValue::Text(text)) => {
            if limit.is_some_and(|n| text.chars().count() > n) {
                return Err(Error::new("VARCHAR length exceeded".into()));
            }
            Ok(Value::Text(text))
        }
        (Type::Json, SqlValue::Text(s)) => ty.cast(Value::Text(s)),
        (_, value) => ty.validate(&native(value)),
    }
}
pub(crate) fn validate_all(db: &Connection, types: &Types) -> Result<()> {
    for ((table, column), ty) in types {
        let mut statement =
            db.prepare(&format!("SELECT {} FROM {}", quote(column), quote(table)))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            decode(ty, row.get(0)?)?;
        }
    }
    Ok(())
}
