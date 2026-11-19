//! Named columns, rowid aliases and value classes of tables in a stream.
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

/// A table's named columns, rowid alias, logical types and column classes.
pub(super) struct Layout {
    pub(super) rowid: Option<&'static str>,
    pub(super) columns: Vec<String>,
    pub(super) types: Vec<Option<Type>>,
    pub(super) classes: Vec<Class>,
    pub(super) insert: String,
    pub(super) delete: Option<String>,
}
impl Layout {
    pub(super) fn load(db: &Connection, table: &str, types: &schema::Types) -> Result<Self> {
        if !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1)",
            [table],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::new(format!(
                "rows reference an unknown table: {table}"
            )));
        }
        let columns = columns(db, table)?;
        let rowid = rowid(db, table)?;
        let types: Vec<Option<Type>> = columns
            .iter()
            .map(|column| types.get(&(table.to_owned(), column.clone())).cloned())
            .collect();
        let classes = types.iter().map(|ty| Class::of(ty.as_ref())).collect();
        let names = rowid
            .into_iter()
            .chain(columns.iter().map(String::as_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = vec!["?"; columns.len() + usize::from(rowid.is_some())].join(",");
        Ok(Self {
            rowid,
            insert: format!(
                "INSERT INTO {}({names}) VALUES({placeholders})",
                quote(table)
            ),
            delete: rowid
                .map(|alias| format!("DELETE FROM {} WHERE {}=?1", quote(table), quote(alias))),
            columns,
            types,
            classes,
        })
    }
    pub(super) fn insert(&self, db: &Connection, row: Row) -> Result<()> {
        let rowid = self
            .rowid
            .map(|_| row.rowid.map_or(SqlValue::Null, SqlValue::Integer));
        db.prepare_cached(&self.insert)?
            .execute(rusqlite::params_from_iter(
                rowid
                    .into_iter()
                    .chain(row.values)
                    .map(crate::decimal::WireValue::new),
            ))?;
        Ok(())
    }
}

pub(super) fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT name FROM pragma_table_xinfo(?1,'main') WHERE hidden=0 ORDER BY cid")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
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

impl Layout {
    /// Salvage rejects a row whose typed value the column type does not accept.
    pub(super) fn check(&self, row: &Row) -> Result<()> {
        for (ty, value) in self.types.iter().zip(&row.values) {
            if let Some(ty) = ty {
                schema::decode(ty, value.clone())
                    .map_err(|error| error.with_code("BOXDB_INVALID_VALUE"))?;
            }
        }
        Ok(())
    }
}

/// Layouts by table name. BoxDB metadata tables have no logical types; other
/// types load on first use, once metadata rows exist.
pub(super) struct Layouts<'a> {
    db: &'a Connection,
    types: Option<schema::Types>,
    cache: HashMap<String, Arc<Layout>>,
}
impl<'a> Layouts<'a> {
    pub(super) fn new(db: &'a Connection, types: Option<schema::Types>) -> Self {
        Self {
            db,
            types,
            cache: HashMap::new(),
        }
    }
    pub(super) fn get(&mut self, table: &str) -> Result<Arc<Layout>> {
        if let Some(layout) = self.cache.get(table) {
            return Ok(Arc::clone(layout));
        }
        let layout = Arc::new(if table.starts_with(METADATA_PREFIX) {
            Layout::load(self.db, table, &schema::Types::new())?
        } else {
            Layout::load(self.db, table, self.types()?)?
        });
        self.cache.insert(table.into(), Arc::clone(&layout));
        Ok(layout)
    }
    pub(super) fn types(&mut self) -> Result<&schema::Types> {
        if self.types.is_none() {
            self.types = Some(schema::load(self.db)?);
        }
        Ok(self.types.as_ref().unwrap())
    }
    pub(super) fn into_types(mut self) -> Result<schema::Types> {
        self.types()?;
        Ok(self.types.take().unwrap())
    }
}
