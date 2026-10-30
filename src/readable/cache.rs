//! Retain canonical row documents and rebuild only rows touched by SQLite.
//! Hooks record identities only; querying happens after the SQL statement ends.
use super::{LimitedWriter, Record, TablePlan, schema_records, sequence_records, write_document};
use crate::{Error, Result, schema};
use rusqlite::{Connection, hooks::PreUpdateCase};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(crate) struct ChangedRows {
    ids: BTreeSet<i64>,
    whole_table: bool,
}
pub(crate) type Changes = BTreeMap<String, ChangedRows>;
enum Rows {
    Indexed(BTreeMap<i64, Vec<u8>>),
    Ordered(Vec<u8>),
}
struct CachedTable {
    plan: TablePlan,
    rows: Rows,
    bytes: usize,
}
impl CachedTable {
    fn load(db: &Connection, name: &str, types: &schema::Types) -> Result<Self> {
        let plan = TablePlan::new(db, name, types)?;
        let mut statement = db.prepare(&plan.select)?;
        let mut cursor = statement.query([])?;
        let mut size = 0usize;
        let rows = if plan.rowid.is_some() {
            let mut rows = BTreeMap::new();
            while let Some(row) = cursor.next()? {
                let bytes = Self::row_document(&plan, row)?;
                size = size.saturating_add(bytes.len());
                if size > super::MAX_SNAPSHOT {
                    return Err(Error::new("snapshot exceeds 256 MiB limit".into()));
                }
                rows.insert(row.get(0)?, bytes);
            }
            Rows::Indexed(rows)
        } else {
            let mut bytes = vec![];
            let mut writer = LimitedWriter {
                inner: &mut bytes,
                bytes: 0,
            };
            while let Some(row) = cursor.next()? {
                plan.write_row(row, &mut writer)?;
            }
            size = bytes.len();
            Rows::Ordered(bytes)
        };
        Ok(Self {
            plan,
            rows,
            bytes: size,
        })
    }
    fn row_document(plan: &TablePlan, row: &rusqlite::Row<'_>) -> Result<Vec<u8>> {
        let mut bytes = vec![];
        plan.write_row(
            row,
            &mut LimitedWriter {
                inner: &mut bytes,
                bytes: 0,
            },
        )?;
        Ok(bytes)
    }
    fn update(
        &mut self,
        db: &Connection,
        name: &str,
        changes: ChangedRows,
        types: &schema::Types,
    ) -> Result<bool> {
        let Rows::Indexed(rows) = &mut self.rows else {
            // WITHOUT ROWID and fully shadowed rowid aliases are reloaded as a
            // whole table; their callback rowid arguments are undefined.
            *self = Self::load(db, name, types)?;
            return Ok(true);
        };
        if changes.whole_table {
            *self = Self::load(db, name, types)?;
            return Ok(true);
        }
        let mut statement = db.prepare_cached(self.plan.lookup.as_ref().unwrap())?;
        let mut changed = false;
        for id in changes.ids {
            let mut cursor = statement.query([id])?;
            if let Some(row) = cursor.next()? {
                let bytes = Self::row_document(&self.plan, row)?;
                if rows.get(&id) != Some(&bytes) {
                    self.bytes = self.bytes.saturating_add(bytes.len());
                    if let Some(previous) = rows.insert(id, bytes) {
                        self.bytes -= previous.len();
                    }
                    changed = true;
                }
            } else if let Some(previous) = rows.remove(&id) {
                self.bytes -= previous.len();
                changed = true;
            }
        }
        Ok(changed)
    }
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        match &self.rows {
            Rows::Indexed(rows) => {
                for bytes in rows.values() {
                    writer.write_all(bytes)?;
                }
            }
            Rows::Ordered(bytes) => writer.write_all(bytes)?,
        }
        Ok(())
    }
}
pub(crate) struct Snapshot {
    schema_version: i64,
    header: Vec<u8>,
    schema: Vec<u8>,
    tables: BTreeMap<String, CachedTable>,
    sequences: Vec<u8>,
    unpublished: bool,
}
impl Snapshot {
    pub fn write(&self, writer: &mut impl Write) -> Result<()> {
        let mut writer = LimitedWriter {
            inner: writer,
            bytes: 0,
        };
        writer.write_all(&self.header)?;
        writer.write_all(&self.schema)?;
        for table in self.tables.values() {
            table.write(&mut writer)?;
        }
        writer.write_all(&self.sequences)?;
        Ok(())
    }
    fn load(db: &Connection, version: i64, types: &schema::Types) -> Result<Self> {
        let records = schema_records(db)?;
        let mut header = vec![];
        write_document(&mut header, &records[0])?;
        let mut schema = vec![];
        let mut tables = BTreeMap::new();
        for record in &records[1..] {
            write_document(&mut schema, record)?;
            if let Record::Schema {
                object_type, name, ..
            } = record
                && object_type == "table"
            {
                tables.insert(name.clone(), CachedTable::load(db, name, types)?);
            }
        }
        Ok(Self {
            schema_version: version,
            header,
            schema,
            tables,
            sequences: vec![],
            unpublished: true,
        })
    }
}
#[derive(Default)]
pub(crate) struct SnapshotCache {
    snapshot: RefCell<Option<Snapshot>>,
    changes: Arc<Mutex<Changes>>,
}
/// Note a row identity from the connection's shared preupdate hook.
pub(crate) fn record(changes: &Mutex<Changes>, database: &str, table: &str, case: &PreUpdateCase) {
    if database != "main" {
        return;
    }
    let mut changes = changes.lock().unwrap_or_else(|e| e.into_inner());
    let table = changes.entry(table.into()).or_default();
    match case {
        PreUpdateCase::Insert(row) => {
            table.ids.insert(row.get_new_row_id());
        }
        PreUpdateCase::Delete(row) => {
            table.ids.insert(row.get_old_row_id());
        }
        PreUpdateCase::Update {
            old_value_accessor,
            new_value_accessor,
        } => {
            table.ids.insert(old_value_accessor.get_old_row_id());
            table.ids.insert(new_value_accessor.get_new_row_id());
        }
        PreUpdateCase::Unknown => table.whole_table = true,
    }
}
impl SnapshotCache {
    pub fn tracker(&self) -> Arc<Mutex<Changes>> {
        self.changes.clone()
    }
    fn refresh(&self, db: &Connection, types: &schema::Types) -> Result<bool> {
        let result = self.refresh_inner(db, types);
        if result.is_err() {
            self.snapshot.replace(None);
        }
        result
    }
    fn refresh_inner(&self, db: &Connection, types: &schema::Types) -> Result<bool> {
        let version = db.query_row("PRAGMA main.schema_version", [], |r| r.get::<_, i64>(0))?;
        let changes = std::mem::take(&mut *self.changes.lock().unwrap_or_else(|e| e.into_inner()));
        let mut cache = self.snapshot.borrow_mut();
        if cache.as_ref().is_none_or(|s| s.schema_version != version) {
            *cache = Some(Snapshot::load(db, version, types)?);
        } else {
            let snapshot = cache.as_mut().unwrap();
            for (name, changes) in changes {
                if let Some(table) = snapshot.tables.get_mut(&name) {
                    snapshot.unpublished |= table.update(db, &name, changes, types)?;
                }
            }
        }
        let snapshot = cache.as_mut().unwrap();
        let mut header = vec![];
        write_document(
            &mut header,
            &Record::Database {
                version: 3,
                codec_version: libsqlite3_sys::wire::VERSION,
                application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
                user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
            },
        )?;
        if header != snapshot.header {
            snapshot.header = header;
            snapshot.unpublished = true;
        }
        // SQLite does not notify hooks for sqlite_sequence. Refresh the small
        // sequence section explicitly so deleted AUTOINCREMENT rows stay safe.
        let mut sequences = vec![];
        for record in sequence_records(db)? {
            write_document(&mut sequences, &record)?;
        }
        if sequences != snapshot.sequences {
            snapshot.sequences = sequences;
            snapshot.unpublished = true;
        }
        let size = snapshot.tables.values().fold(
            snapshot
                .header
                .len()
                .saturating_add(snapshot.schema.len())
                .saturating_add(snapshot.sequences.len()),
            |sum, table| sum.saturating_add(table.bytes),
        );
        if size > super::MAX_SNAPSHOT {
            return Err(Error::new("snapshot exceeds 256 MiB limit".into()));
        }
        Ok(snapshot.unpublished)
    }
    pub fn publish(
        &self,
        db: &Connection,
        types: &schema::Types,
        publish: impl FnOnce(&Snapshot) -> Result<()>,
    ) -> Result<()> {
        if !self.refresh(db, types)? {
            return Ok(());
        }
        {
            let cache = self.snapshot.borrow();
            publish(cache.as_ref().unwrap())?;
        }
        if let Some(snapshot) = self.snapshot.borrow_mut().as_mut() {
            snapshot.unpublished = false;
        }
        Ok(())
    }
}
