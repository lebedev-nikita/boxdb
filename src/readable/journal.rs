//! Decide how a commit reaches a readable file: append a commit document or
//! rewrite the canonical snapshot. Hooks record changed row identities; rows
//! are serialized after the SQL statement ends.
use super::{TablePlan, header_record, plain_text, quoted, write_document};
use crate::{Result, schema, storage::MAX_SNAPSHOT};
use rusqlite::{Connection, hooks::PreUpdateCase};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct ChangedRows {
    ids: BTreeSet<i64>,
    whole_table: bool,
}
type Changes = BTreeMap<String, ChangedRows>;

/// Journals of small files may exceed half of the snapshot up to this size,
/// which parses in a few milliseconds.
const JOURNAL_FLOOR: usize = 64 * 1024;

/// What the file currently holds.
struct Published {
    schema_version: i64,
    header: Vec<u8>,
    sequences: BTreeMap<String, i64>,
    snapshot: usize,
    journal: usize,
    appendable: bool,
}

pub(crate) enum Publication {
    Unchanged,
    Append(Vec<u8>),
    Rewrite,
}

#[derive(Default)]
pub(crate) struct Journal {
    changes: Arc<Mutex<Changes>>,
    /// `None` until the file is known to match the database.
    published: RefCell<Option<Published>>,
}

fn schema_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA main.schema_version", [], |r| r.get(0))?)
}
fn header(db: &Connection) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    write_document(&mut bytes, &header_record(db)?)?;
    Ok(bytes)
}
fn sequences(db: &Connection) -> Result<BTreeMap<String, i64>> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='sqlite_sequence')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(BTreeMap::new());
    }
    Ok(db
        .prepare("SELECT name,seq FROM main.sqlite_sequence")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}
fn key(name: &str) -> String {
    if plain_text(name) {
        name.into()
    } else {
        quoted(name)
    }
}

impl Journal {
    pub fn watch(&self, db: &Connection) -> Result<()> {
        // A rolled-back transaction leaves nothing to publish.
        let rolled_back = self.changes.clone();
        db.rollback_hook(Some(move || {
            rolled_back
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }))?;
        let changes = self.changes.clone();
        db.preupdate_hook(Some(
            move |_, database: &str, table: &str, case: &PreUpdateCase| {
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
            },
        ))?;
        Ok(())
    }
    fn record(
        &self,
        db: &Connection,
        snapshot: usize,
        journal: usize,
        appendable: bool,
    ) -> Result<()> {
        self.published.replace(Some(Published {
            schema_version: schema_version(db)?,
            header: header(db)?,
            sequences: sequences(db)?,
            snapshot,
            journal,
            appendable,
        }));
        Ok(())
    }
    /// The file was just decoded into `db`.
    pub fn opened(&self, db: &Connection, extent: super::Extent) -> Result<()> {
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.record(
            db,
            extent.snapshot,
            extent.committed - extent.snapshot,
            extent.appendable,
        )
    }
    /// A failed statement was rolled back to a savepoint outside any
    /// transaction, so no recorded change remains.
    pub fn discard(&self) {
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    /// Changes outside the hooks, such as VACUUM, require a rewrite.
    pub fn reset(&self) {
        self.published.replace(None);
    }
    pub fn rewritten(&self, db: &Connection, snapshot: usize) -> Result<()> {
        self.record(db, snapshot, 0, true)
    }
    pub fn appended(&self, db: &Connection, bytes: usize) -> Result<()> {
        if let Some(published) = self.published.borrow_mut().as_mut() {
            published.journal += bytes;
            published.sequences = sequences(db)?;
        }
        Ok(())
    }
    /// Consume recorded changes and choose a publication. Appending stops when
    /// the journal would exceed half of the snapshot or `JOURNAL_FLOOR`,
    /// whichever is larger.
    pub fn plan(&self, db: &Connection, types: &schema::Types) -> Result<Publication> {
        let changes = std::mem::take(&mut *self.changes.lock().unwrap_or_else(|e| e.into_inner()));
        let published = self.published.borrow();
        let Some(published) = published.as_ref() else {
            return Ok(Publication::Rewrite);
        };
        if schema_version(db)? != published.schema_version || header(db)? != published.header {
            return Ok(Publication::Rewrite);
        }
        let current = sequences(db)?;
        if published
            .sequences
            .keys()
            .any(|name| !current.contains_key(name))
        {
            return Ok(Publication::Rewrite);
        }
        let limit = (published.snapshot / 2)
            .max(JOURNAL_FLOOR)
            .min(MAX_SNAPSHOT.saturating_sub(published.snapshot));
        let budget = limit.saturating_sub(published.journal);
        let mut tables = String::new();
        for (table, changed) in changes {
            if changed.whole_table {
                return Ok(Publication::Rewrite);
            }
            // Statements that failed or rolled back may have touched tables
            // that no longer exist; dropping a table changes the schema version.
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1)",
                [&table],
                |r| r.get::<_, bool>(0),
            )? {
                continue;
            }
            let plan = TablePlan::new(db, &table, types)?;
            let Some(lookup) = &plan.lookup else {
                return Ok(Publication::Rewrite);
            };
            let mut statement = db.prepare_cached(lookup)?;
            let mut put = vec![];
            let mut delete = vec![];
            for id in changed.ids {
                let mut rows = statement.query([id])?;
                match rows.next()? {
                    Some(row) => plan.write_row(row, &mut put)?,
                    None => delete.push(id.to_string()),
                }
                // Stop serializing a commit that will be replaced by a rewrite.
                if tables.len() + put.len() > budget {
                    return Ok(Publication::Rewrite);
                }
            }
            if put.is_empty() && delete.is_empty() {
                continue;
            }
            tables.push_str(&format!("  {}:\n", key(&table)));
            if !put.is_empty() {
                tables.push_str("    put:\n");
                for line in String::from_utf8(put)
                    .map_err(crate::Error::validation)?
                    .split_inclusive('\n')
                {
                    tables.push_str("    ");
                    tables.push_str(line);
                }
            }
            if !delete.is_empty() {
                tables.push_str(&format!("    delete: [{}]\n", delete.join(", ")));
            }
        }
        let changed_sequences: Vec<String> = current
            .iter()
            .filter(|(name, value)| published.sequences.get(*name) != Some(value))
            .map(|(name, value)| format!("{}: {value}", key(name)))
            .collect();
        if tables.is_empty() && changed_sequences.is_empty() {
            return Ok(Publication::Unchanged);
        }
        let mut commit = super::COMMIT.to_vec();
        if !tables.is_empty() {
            commit.extend_from_slice(b"tables:\n");
            commit.extend_from_slice(tables.as_bytes());
        }
        if !changed_sequences.is_empty() {
            writeln!(commit, "sequences: {{{}}}", changed_sequences.join(", "))?;
        }
        commit.extend_from_slice(b"...\n");
        if !published.appendable || commit.len() > budget {
            return Ok(Publication::Rewrite);
        }
        Ok(Publication::Append(commit))
    }
}
