//! `changeset_rows(changeset)` lists one row per defined field of a changeset,
//! resolving column names from the current main schema where it still fits.
use crate::{
    Result,
    changeset::{self, Operation, RawChange},
    decimal::WireValue,
};
use rusqlite::{
    Connection, ffi,
    types::Value as SqlValue,
    vtab::{Context, Filters, IndexInfo, Module, VTab, VTabConfig, VTabConnection, VTabCursor},
};
use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{CStr, c_int},
    marker::PhantomData,
};

const MODULE_NAME: &CStr = c"changeset_rows";
const COLUMN_CHANGESET: c_int = 8;

pub(crate) fn register(db: &Connection) -> Result<()> {
    const MODULE: Module<RowsTab> = Module::eponymous_only_module();
    db.create_module(MODULE_NAME, &MODULE, None::<()>)?;
    Ok(())
}

#[repr(C)]
struct RowsTab {
    base: ffi::sqlite3_vtab,
    handle: usize,
}

unsafe impl<'vtab> VTab<'vtab> for RowsTab {
    type Aux = ();
    type Cursor = RowsCursor<'vtab>;

    fn connect(
        db: &mut VTabConnection,
        _aux: Option<&()>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        _args: &[&[u8]],
    ) -> rusqlite::Result<(Cow<'static, CStr>, Self)> {
        db.config(VTabConfig::Innocuous)?;
        // SAFETY: the handle is only used to look up schema metadata.
        let handle = unsafe { db.handle() } as usize;
        Ok((
            Cow::Borrowed(
                c"CREATE TABLE x(seq INTEGER,\"table\" TEXT,operation TEXT,position INTEGER,column_name TEXT,primary_key INTEGER,old,new,changeset hidden)",
            ),
            Self {
                base: ffi::sqlite3_vtab::default(),
                handle,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<bool> {
        let mut argument = None;
        for (i, constraint) in info.constraints().enumerate() {
            if constraint.column() == COLUMN_CHANGESET
                && constraint.is_usable()
                && constraint.operator()
                    == rusqlite::vtab::IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            {
                argument = Some(i);
            }
        }
        match argument {
            Some(i) => {
                let mut usage = info.constraint_usage(i);
                usage.set_argv_index(1);
                usage.set_omit(true);
                info.set_estimated_cost(1.0);
                info.set_idx_num(1);
            }
            None => {
                info.set_estimated_cost(f64::MAX);
                info.set_idx_num(0);
            }
        }
        Ok(true)
    }

    fn open(&'vtab mut self) -> rusqlite::Result<RowsCursor<'vtab>> {
        Ok(RowsCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            handle: self.handle,
            rows: vec![],
            position: 0,
            phantom: PhantomData,
        })
    }
}

struct Field {
    seq: i64,
    table: String,
    operation: Operation,
    position: i64,
    column_name: Option<String>,
    primary_key: bool,
    old: Option<SqlValue>,
    new: Option<SqlValue>,
}

#[repr(C)]
struct RowsCursor<'vtab> {
    base: ffi::sqlite3_vtab_cursor,
    handle: usize,
    rows: Vec<Field>,
    position: usize,
    phantom: PhantomData<&'vtab RowsTab>,
}

fn column_names(handle: usize, table: &str) -> Option<Vec<String>> {
    // SAFETY: the handle belongs to the live connection running this query;
    // the temporary wrapper does not close it.
    let db = unsafe { Connection::from_handle(handle as *mut ffi::sqlite3) }.ok()?;
    let mut statement = db
        .prepare("SELECT name FROM pragma_table_xinfo(?1,'main') WHERE hidden=0 ORDER BY cid")
        .ok()?;
    let names: Vec<String> = statement
        .query_map([table], |row| row.get(0))
        .ok()?
        .collect::<rusqlite::Result<_>>()
        .ok()?;
    (!names.is_empty()).then_some(names)
}

fn fields(handle: usize, changes: &[RawChange]) -> Vec<Field> {
    let mut names: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut fields = vec![];
    for (seq, change) in changes.iter().enumerate() {
        let names = names
            .entry(change.table.to_lowercase())
            .or_insert_with(|| column_names(handle, &change.table));
        let columns = change.primary_key.len();
        for position in 0..columns {
            let old = change
                .old
                .as_ref()
                .and_then(|record| record[position].clone());
            let new = change
                .new
                .as_ref()
                .and_then(|record| record[position].clone());
            if old.is_none() && new.is_none() {
                continue;
            }
            fields.push(Field {
                seq: seq as i64,
                table: change.table.clone(),
                operation: change.operation,
                position: position as i64,
                column_name: names
                    .as_ref()
                    .filter(|names| names.len() >= columns)
                    .map(|names| names[position].clone()),
                primary_key: change.primary_key[position],
                old,
                new,
            });
        }
    }
    fields
}

unsafe impl VTabCursor for RowsCursor<'_> {
    fn filter(
        &mut self,
        idx_num: c_int,
        _idx_str: Option<&str>,
        args: &Filters<'_>,
    ) -> rusqlite::Result<()> {
        self.rows.clear();
        self.position = 0;
        if idx_num == 0 || args.is_empty() {
            return Err(rusqlite::Error::ModuleError(
                "changeset_rows requires a changeset argument".into(),
            ));
        }
        let bytes: Option<Vec<u8>> = args.get(0)?;
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let decoded = changeset::decode_raw(&bytes)
            .map_err(|error| rusqlite::Error::ModuleError(error.to_string()))?;
        self.rows = fields(self.handle, &decoded.changes);
        Ok(())
    }
    fn next(&mut self) -> rusqlite::Result<()> {
        self.position += 1;
        Ok(())
    }
    fn eof(&self) -> bool {
        self.position >= self.rows.len()
    }
    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        let field = &self.rows[self.position];
        match i {
            0 => ctx.set_result(&field.seq),
            1 => ctx.set_result(&field.table),
            2 => ctx.set_result(&field.operation.name()),
            3 => ctx.set_result(&field.position),
            4 => ctx.set_result(&field.column_name),
            5 => ctx.set_result(&i64::from(field.primary_key)),
            6 => ctx.set_result(&WireValue::new(field.old.clone().unwrap_or(SqlValue::Null))),
            7 => ctx.set_result(&WireValue::new(field.new.clone().unwrap_or(SqlValue::Null))),
            _ => ctx.set_result(&SqlValue::Null),
        }
    }
    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.position as i64)
    }
}
