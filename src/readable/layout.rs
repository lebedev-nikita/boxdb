//! Columns, row identity and value classes of a table in a readable log.
use super::*;

/// How a column's cells are written and read.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Class {
    /// Natural YAML scalars; BLOBs and native values use explicit mappings.
    Plain,
    /// Plain cells with 0 and 1 written as YAML booleans.
    Boolean,
    /// Base64 text.
    Bytea,
    /// Canonical text of one native type.
    Native(Kind),
}
impl Class {
    pub(super) fn of(ty: Option<&Type>) -> Self {
        match ty {
            Some(Type::Boolean) => Self::Boolean,
            Some(Type::Bytea) => Self::Bytea,
            // Narrow unsigned columns store ordinary INTEGER cells.
            Some(ty) => match ty.native_kind() {
                None | Some(Kind::Uint8 | Kind::Uint16 | Kind::Uint32) => Self::Plain,
                Some(kind) => Self::Native(kind),
            },
            None => Self::Plain,
        }
    }
}

/// How log entries identify a row.
#[derive(Clone, PartialEq)]
pub(super) enum Identity {
    /// The INTEGER PRIMARY KEY column is the rowid.
    Alias(usize),
    /// A rowid table without a declared key, by its accessible rowid name.
    Rowid(&'static str),
    /// Declared primary key columns; the hidden rowid is not stored.
    Key(Vec<usize>),
    /// No key and no accessible rowid: rows are identified by all values.
    Row,
}

/// A table's columns, identity, logical types and the SQL that reads and
/// writes its rows.
pub(super) struct Layout {
    pub(super) table: String,
    pub(super) columns: Vec<String>,
    /// YAML spelling of each column name.
    pub(super) names: Vec<String>,
    /// Column positions in SQLite's row, which includes generated columns.
    pub(super) cids: Vec<usize>,
    /// Columns whose default is NULL; inserted rows omit their NULL cells.
    pub(super) null_default: Vec<bool>,
    pub(super) types: Vec<Option<Type>>,
    pub(super) classes: Vec<Class>,
    index: HashMap<String, usize>,
    pub(super) identity: Identity,
    /// The accessible rowid name of a rowid table.
    pub(super) rowid: Option<&'static str>,
    pub(super) autoincrement: bool,
    /// `SELECT [rowid,] columns FROM table`
    pub(super) select: String,
    /// `select` restricted to the identity columns, bound in order.
    pub(super) lookup: String,
    /// `DELETE` restricted to the identity columns.
    pub(super) delete: String,
}

impl Layout {
    pub(super) fn load(db: &Connection, table: &str, types: &schema::Types) -> Result<Self> {
        let without_rowid: Option<bool> = db
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema='main' AND type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .optional()?;
        let Some(without_rowid) = without_rowid else {
            return Err(Error::new(format!("unknown table: {table}")));
        };
        let mut columns = vec![];
        let mut cids = vec![];
        let mut null_default = vec![];
        for row in db
            .prepare("SELECT cid,name,dflt_value FROM pragma_table_xinfo(?1,'main') WHERE hidden=0 ORDER BY cid")?
            .query_map([table], |r| {
                Ok((
                    r.get::<_, i64>(0)? as usize,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
        {
            let (cid, name, default) = row?;
            cids.push(cid);
            columns.push(name);
            null_default.push(
                default.is_none_or(|default| default.trim().eq_ignore_ascii_case("NULL")),
            );
        }
        let primary: Vec<usize> = db
            .prepare("SELECT name FROM pragma_table_info(?1,'main') WHERE pk>0 ORDER BY pk")?
            .query_map([table], |r| r.get::<_, String>(0))?
            .map(|name| {
                let name = name?;
                Ok(columns
                    .iter()
                    .position(|c| *c == name)
                    .unwrap_or(usize::MAX))
            })
            .collect::<Result<_>>()?;
        if primary.contains(&usize::MAX) {
            return Err(Error::new(format!(
                "primary key of {table} includes a generated column"
            )));
        }
        let rowid = if without_rowid {
            None
        } else {
            rowid(db, table)?
        };
        // An INTEGER PRIMARY KEY aliases the rowid and has no separate index.
        let alias = !without_rowid
            && primary.len() == 1
            && !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1,'main') WHERE origin='pk')",
                [table],
                |r| r.get::<_, bool>(0),
            )?;
        let identity = if alias {
            Identity::Alias(primary[0])
        } else if !primary.is_empty() {
            Identity::Key(primary)
        } else if let Some(name) = rowid {
            Identity::Rowid(name)
        } else {
            Identity::Row
        };
        let sql: String = db.query_row(
            "SELECT sql FROM main.sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )?;
        let autoincrement = sql.to_ascii_uppercase().contains("AUTOINCREMENT");
        let types: Vec<Option<Type>> = columns
            .iter()
            .map(|column| types.get(&(table.to_owned(), column.clone())).cloned())
            .collect();
        let classes = types.iter().map(|ty| Class::of(ty.as_ref())).collect();
        let names = columns
            .iter()
            .map(|column| {
                // YAML limits implicit keys to 1024 characters.
                if column.chars().count() > 1000 {
                    return Err(Error::new(
                        "readable storage supports column names up to 1000 characters".into(),
                    ));
                }
                Ok(key(column))
            })
            .collect::<Result<_>>()?;
        let quoted = quote(table);
        let selected = rowid
            .filter(|_| !alias)
            .map(quote)
            .into_iter()
            .chain(columns.iter().map(|c| quote(c)))
            .collect::<Vec<_>>()
            .join(",");
        let select = format!("SELECT {selected} FROM main.{quoted}");
        let condition = |columns: &[String]| -> String {
            columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{} IS ?{}", quote(c), i + 1))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let identity_columns: Vec<String> = match &identity {
            Identity::Alias(i) => vec![columns[*i].clone()],
            Identity::Rowid(name) => vec![(*name).to_owned()],
            Identity::Key(key) => key.iter().map(|i| columns[*i].clone()).collect(),
            Identity::Row => columns.clone(),
        };
        let condition = condition(&identity_columns);
        let lookup = format!("{select} WHERE {condition}");
        let delete = format!("DELETE FROM main.{quoted} WHERE {condition}");
        let index = columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), i))
            .collect();
        Ok(Self {
            table: table.to_owned(),
            columns,
            names,
            cids,
            null_default,
            types,
            classes,
            index,
            identity,
            rowid,
            autoincrement,
            select,
            lookup,
            delete,
        })
    }
    /// Column index of a YAML key; `None` for the stored rowid name.
    pub(super) fn column(&self, name: &str) -> Result<Option<usize>> {
        if let Identity::Rowid(rowid) = self.identity
            && rowid == name
        {
            return Ok(None);
        }
        self.index
            .get(name)
            .copied()
            .map(Some)
            .ok_or_else(|| Error::new(format!("unknown column: {}.{name}", self.table)))
    }
    /// Whether `select` starts with the rowid before the columns.
    pub(super) fn selects_rowid(&self) -> bool {
        self.rowid.is_some() && !matches!(self.identity, Identity::Alias(_))
    }
    /// Identity values of a complete row, bound in `lookup` order.
    pub(super) fn identity_of(&self, rowid: Option<i64>, values: &[SqlValue]) -> Vec<SqlValue> {
        match &self.identity {
            Identity::Alias(i) => vec![values[*i].clone()],
            Identity::Rowid(_) => vec![rowid.map_or(SqlValue::Null, SqlValue::Integer)],
            Identity::Key(key) => key.iter().map(|i| values[*i].clone()).collect(),
            Identity::Row => values.to_vec(),
        }
    }
    pub(super) fn mismatch(&self, index: usize) -> Error {
        Error::new(format!(
            "readable export found a value of another type in typed column {}.{}",
            self.table, self.columns[index]
        ))
        .with_code("BOXDB_INVALID_VALUE")
    }
    /// Append the YAML text of a cell, on one line.
    pub(super) fn cell(&self, out: &mut String, index: usize, value: ValueRef<'_>) -> Result<()> {
        use std::fmt::Write;
        let class = self.classes[index];
        match (class, value) {
            (_, ValueRef::Null) => out.push_str("null"),
            (Class::Boolean, ValueRef::Integer(n @ (0 | 1))) => {
                out.push_str(if n == 1 { "true" } else { "false" })
            }
            (Class::Plain | Class::Boolean, ValueRef::Integer(n)) => {
                write!(out, "{n}").unwrap();
            }
            (Class::Plain | Class::Boolean, ValueRef::Real(n)) if n.is_finite() => {
                out.push_str(serde_saphyr::to_string(&n)?.trim_end());
            }
            (Class::Plain | Class::Boolean, ValueRef::Real(_)) => {
                return Err(Error::new(
                    "readable storage cannot represent non-finite SQLite values".into(),
                ));
            }
            (Class::Plain | Class::Boolean, ValueRef::Text(bytes)) => {
                let text = std::str::from_utf8(bytes).map_err(Error::validation)?;
                if plain_text(text) {
                    out.push_str(text);
                } else {
                    out.push_str(&quoted(text));
                }
            }
            (Class::Plain | Class::Boolean, ValueRef::Blob(bytes)) => match native_text(bytes) {
                Some((kind, text)) => {
                    write!(out, "{{{}: {}}}", native_name(kind), bare_or_quoted(&text)).unwrap();
                }
                None => write!(
                    out,
                    "{{blob: {}}}",
                    bare_or_quoted(&BASE64.encode(libsqlite3_sys::wire::blob_payload(bytes)))
                )
                .unwrap(),
            },
            (Class::Bytea, ValueRef::Blob(bytes)) if native_text(bytes).is_none() => out.push_str(
                &bare_or_quoted(&BASE64.encode(libsqlite3_sys::wire::blob_payload(bytes))),
            ),
            (Class::Native(expected), ValueRef::Blob(bytes)) => match native_text(bytes) {
                Some((kind, text)) if kind == expected => out.push_str(&bare_or_quoted(&text)),
                _ => return Err(self.mismatch(index)),
            },
            _ => return Err(self.mismatch(index)),
        }
        Ok(())
    }
    /// The SQLite value of a YAML cell.
    pub(super) fn decode(&self, index: usize, node: &Node<'_>) -> Result<SqlValue> {
        let invalid = |message: String| {
            Error::new(format!("{}.{}: {message}", self.table, self.columns[index]))
                .with_code("BOXDB_INVALID_VALUE")
        };
        match (self.classes[index], node) {
            (_, Node::Scalar(scalar)) if scalar.is_null() => Ok(SqlValue::Null),
            (Class::Plain | Class::Boolean, Node::Scalar(scalar)) => {
                Ok(match scalar.resolve().map_err(invalid)? {
                    Resolved::Null => SqlValue::Null,
                    Resolved::Bool(b) => SqlValue::Integer(i64::from(b)),
                    Resolved::Integer(n) => SqlValue::Integer(n),
                    Resolved::Real(n) => SqlValue::Real(n),
                    Resolved::Text(text) => SqlValue::Text(text.into()),
                })
            }
            (Class::Plain | Class::Boolean, Node::Map(entries)) => {
                let [(name, Node::Scalar(text))] = entries.as_slice() else {
                    return Err(invalid("expected {type: text}".into()));
                };
                let kind = match name.text.as_ref() {
                    "blob" => {
                        return BASE64
                            .decode(text.text.as_bytes())
                            .map(|bytes| SqlValue::Blob(crate::decimal::encode_blob(&bytes)))
                            .map_err(|e| invalid(e.to_string()));
                    }
                    "decimal" => Kind::Decimal,
                    "uuid" => Kind::Uuid,
                    "uint8" => Kind::Uint8,
                    "uint16" => Kind::Uint16,
                    "uint32" => Kind::Uint32,
                    "uint64" => Kind::Uint64,
                    "instant" => Kind::Instant,
                    "date" => Kind::Date,
                    "time" => Kind::Time,
                    "datetime" => Kind::DateTime,
                    "interval" => Kind::Interval,
                    other => return Err(invalid(format!("unknown value type: {other}"))),
                };
                libsqlite3_sys::native::encode(kind, &text.text)
                    .map(SqlValue::Blob)
                    .map_err(|e| invalid(format!("{}: {}", e.0, text.text)))
            }
            (Class::Native(kind), Node::Scalar(scalar)) => {
                libsqlite3_sys::native::encode(kind, &scalar.text)
                    .map(SqlValue::Blob)
                    .map_err(|e| invalid(format!("{}: {}", e.0, scalar.text)))
            }
            (Class::Bytea, Node::Scalar(scalar)) => BASE64
                .decode(scalar.text.as_bytes())
                .map(|bytes| SqlValue::Blob(crate::decimal::encode_blob(&bytes)))
                .map_err(|e| invalid(e.to_string())),
            _ => Err(invalid("expected a scalar".into())),
        }
    }
}

/// A YAML mapping key.
pub(super) fn key(name: &str) -> String {
    if plain_text(name) {
        name.into()
    } else {
        quoted(name)
    }
}

pub(crate) fn rowid(db: &Connection, table: &str) -> Result<Option<&'static str>> {
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
        .prepare("SELECT name FROM pragma_table_xinfo(?1,'main')")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(["rowid", "_rowid_", "oid"]
        .into_iter()
        .find(|name| !columns.iter().any(|c| c.eq_ignore_ascii_case(name))))
}

/// Layouts by table name. BoxDB metadata tables have no logical types; other
/// layouts load the types once metadata rows exist.
pub(super) struct Layouts {
    types: Option<schema::Types>,
    cache: HashMap<String, Arc<Layout>>,
}
impl Layouts {
    pub(super) fn new() -> Self {
        Self {
            types: None,
            cache: HashMap::new(),
        }
    }
    pub(super) fn get(&mut self, db: &Connection, table: &str) -> Result<Arc<Layout>> {
        if let Some(layout) = self.cache.get(table) {
            return Ok(Arc::clone(layout));
        }
        let layout = Arc::new(if table.starts_with(METADATA_PREFIX) {
            Layout::load(db, table, &schema::Types::new())?
        } else {
            if self.types.is_none() {
                self.types = Some(schema::load(db)?);
            }
            Layout::load(db, table, self.types.as_ref().unwrap())?
        });
        self.cache.insert(table.into(), Arc::clone(&layout));
        Ok(layout)
    }
    /// Forget layouts after a schema or metadata change.
    pub(super) fn clear(&mut self) {
        self.types = None;
        self.cache.clear();
    }
}
