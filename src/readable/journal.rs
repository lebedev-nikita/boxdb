//! Turn committed transactions into log lines. Preupdate hooks record the
//! identity and prior image of each changed row; at commit, rows are compared
//! with their current state, so a line holds the net change of the
//! transaction. Schema statements split a transaction into segments: rows
//! changed before a statement are written before it.
use super::*;
use rusqlite::hooks::PreUpdateCase;
use std::{
    collections::{HashSet, hash_map::Entry as MapEntry},
    sync::Mutex,
};

/// A complete SQLite row image, indexed by column position.
type Image = Vec<SqlValue>;
/// The prior and new rowid and image of one change.
type Event = (Option<(i64, Image)>, Option<(i64, Image)>);
/// Schema objects with the schema version they were read at.
type Schema = Arc<(i64, Vec<(String, String, String)>)>;

/// Row changes of one table since the last segment.
#[derive(Clone)]
enum TableCapture {
    /// Rows found by rowid: the image before their first change.
    Positions {
        order: Vec<i64>,
        before: HashMap<i64, Option<Image>>,
    },
    /// Complete images of every change, for tables without a usable rowid.
    Events(Vec<Event>),
}

#[derive(Clone, Default)]
struct Capture {
    order: Vec<String>,
    tables: HashMap<String, TableCapture>,
}
impl Capture {
    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// Steps of the current transaction. A segment of row changes keeps its
/// capture, so rolling back to an earlier savepoint can recompute it.
struct Segment {
    steps: Vec<String>,
    capture: Option<Capture>,
    /// AUTOINCREMENT values and schema restored before this segment.
    sequences: BTreeMap<String, i64>,
    schema: Schema,
}

struct Mark {
    name: Option<String>,
    segments: usize,
}

/// What the file holds once the last line was written.
#[derive(Clone)]
struct Published {
    header: [(&'static str, i64); 2],
    sequences: BTreeMap<String, i64>,
    schema: Schema,
    terminated: bool,
}

#[derive(Default)]
struct Inner {
    capture: Capture,
    /// Tables whose rows the hooks track by rowid.
    by_rowid: HashSet<String>,
    segments: Vec<Segment>,
    marks: Vec<Mark>,
    /// The schema the written segments restore.
    schema: Option<Schema>,
    /// AUTOINCREMENT values the written segments restore.
    sequences: BTreeMap<String, i64>,
    /// `None` until the file is known to match the database.
    published: Option<Published>,
    /// The schema version `by_rowid` and `layouts` describe.
    inspected: Option<i64>,
    layouts: HashMap<String, Arc<Layout>>,
}

pub(crate) enum Publication {
    Unchanged,
    Append(Vec<u8>),
    Rewrite,
}

#[derive(Default)]
pub(crate) struct Journal {
    inner: Arc<Mutex<Inner>>,
}

fn schema_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA main.schema_version", [], |r| r.get(0))?)
}
fn current_schema(db: &Connection) -> Result<Schema> {
    Ok(Arc::new((schema_version(db)?, encode::objects(db)?)))
}
fn header(db: &Connection) -> Result<[(&'static str, i64); 2]> {
    Ok([
        (
            "application_id",
            db.query_row("PRAGMA main.application_id", [], |r| r.get(0))?,
        ),
        (
            "user_version",
            db.query_row("PRAGMA main.user_version", [], |r| r.get(0))?,
        ),
    ])
}
fn table_exists(db: &Connection, table: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
fn layout(db: &Connection, table: &str, types: &schema::Types) -> Result<Arc<Layout>> {
    Ok(Arc::new(if table.starts_with(METADATA_PREFIX) {
        Layout::load(db, table, &schema::Types::new())?
    } else {
        Layout::load(db, table, types)?
    }))
}

impl Journal {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn watch(&self, db: &Connection) -> Result<()> {
        // A rolled-back transaction leaves nothing to publish. Schema changes
        // may be undone, so rows are tracked by complete images until the
        // tables are inspected again.
        let rolled_back = self.inner.clone();
        db.rollback_hook(Some(move || {
            let mut guard = rolled_back.lock().unwrap_or_else(|e| e.into_inner());
            let inner = &mut *guard;
            inner.inspected = None;
            inner.capture = Capture::default();
            inner.segments.clear();
            inner.marks.clear();
            inner.by_rowid.clear();
            if let Some(published) = &inner.published {
                inner.sequences = published.sequences.clone();
                inner.schema = Some(Arc::clone(&published.schema));
            }
        }))?;
        let inner = self.inner.clone();
        db.preupdate_hook(Some(
            move |_, database: &str, table: &str, case: &PreUpdateCase| {
                // SQLite's own tables are derived: sequences are compared at commit.
                if database != "main" || table.starts_with("sqlite_") {
                    return;
                }
                let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
                let inner = &mut *guard;
                let capture = &mut inner.capture;
                let entry = match capture.tables.entry(table.to_owned()) {
                    MapEntry::Occupied(entry) => entry.into_mut(),
                    MapEntry::Vacant(entry) => {
                        capture.order.push(table.to_owned());
                        entry.insert(if inner.by_rowid.contains(table) {
                            TableCapture::Positions {
                                order: vec![],
                                before: HashMap::new(),
                            }
                        } else {
                            TableCapture::Events(vec![])
                        })
                    }
                };
                record_change(entry, case);
            },
        ))?;
        Ok(())
    }
    /// Tables whose rows can be found by rowid.
    fn inspect(inner: &mut Inner, db: &Connection) -> Result<()> {
        let version = schema_version(db)?;
        if inner.inspected == Some(version) {
            return Ok(());
        }
        inner.layouts.clear();
        inner.inspected = Some(version);
        let tables: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND wr=0 AND name NOT GLOB 'sqlite_*'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        inner.by_rowid.clear();
        for table in tables {
            let layout = Layout::load(db, &table, &schema::Types::new())?;
            if layout.rowid.is_some() || matches!(layout.identity, Identity::Alias(_)) {
                inner.by_rowid.insert(table);
            }
        }
        Ok(())
    }
    fn record(&self, db: &Connection, terminated: bool) -> Result<()> {
        let mut inner = self.lock();
        let published = Published {
            header: header(db)?,
            sequences: encode::sequences(db)?,
            schema: current_schema(db)?,
            terminated,
        };
        inner.sequences = published.sequences.clone();
        inner.schema = Some(Arc::clone(&published.schema));
        inner.published = Some(published);
        inner.capture = Capture::default();
        inner.segments.clear();
        inner.marks.clear();
        Self::inspect(&mut inner, db)
    }
    /// The layout of a table in the current schema.
    fn layout(
        inner: &mut Inner,
        db: &Connection,
        table: &str,
        types: &schema::Types,
    ) -> Result<Arc<Layout>> {
        let version = schema_version(db)?;
        if inner.inspected != Some(version) {
            inner.inspected = None;
            Self::inspect(inner, db)?;
        }
        if let Some(layout) = inner.layouts.get(table) {
            return Ok(Arc::clone(layout));
        }
        let layout = layout(db, table, types)?;
        inner.layouts.insert(table.to_owned(), Arc::clone(&layout));
        Ok(layout)
    }
    /// The file was just decoded into `db`.
    pub fn opened(&self, db: &Connection, extent: super::Extent) -> Result<()> {
        self.record(db, extent.terminated)
    }
    /// A failed statement was rolled back outside any transaction, so no
    /// recorded change remains.
    pub fn discard(&self) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        inner.capture = Capture::default();
        inner.segments.clear();
        inner.marks.clear();
        if let Some(published) = &inner.published {
            inner.sequences = published.sequences.clone();
            inner.schema = Some(Arc::clone(&published.schema));
        }
    }
    /// The next publication rewrites the canonical log.
    pub fn reset(&self) {
        self.lock().published = None;
    }
    pub fn rewritten(&self, db: &Connection) -> Result<()> {
        self.record(db, true)
    }
    pub fn appended(&self, db: &Connection) -> Result<()> {
        self.record(db, true)
    }

    /// Savepoints mark the steps written so far.
    pub fn savepoint(&self, name: Option<&str>) {
        let mut inner = self.lock();
        let segments = inner.segments.len();
        inner.marks.push(Mark {
            name: name.map(str::to_ascii_lowercase),
            segments,
        });
    }
    /// The innermost savepoint with this name, or the last unnamed one.
    fn find(inner: &Inner, name: Option<&str>) -> Option<usize> {
        let name = name.map(str::to_ascii_lowercase);
        inner.marks.iter().rposition(|mark| mark.name == name)
    }
    pub fn release(&self, name: Option<&str>) {
        let mut inner = self.lock();
        if let Some(index) = Self::find(&inner, name) {
            inner.marks.truncate(index);
        }
    }
    /// Steps after the savepoint are discarded. Rows changed since the last
    /// remaining segment are compared again when the transaction commits.
    pub fn rollback_to(&self, db: &Connection, name: Option<&str>) -> Result<()> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let Some(index) = Self::find(inner, name) else {
            return Ok(());
        };
        let segments = inner.marks[index].segments;
        inner.marks.truncate(index + 1);
        if inner.segments.len() > segments {
            let first = inner.segments.drain(segments..).next().unwrap();
            inner.capture = first.capture.unwrap_or_default();
            inner.sequences = first.sequences;
            inner.schema = Some(first.schema);
        }
        inner.inspected = None;
        Self::inspect(inner, db)
    }

    /// Write rows changed so far into a segment before a schema statement.
    pub fn before_schema(&self, db: &Connection, types: &schema::Types) -> Result<()> {
        let mut inner = self.lock();
        Self::flush(&mut inner, db, types)
    }
    /// Record the schema change of a successful statement. `alter` is the
    /// ALTER TABLE statement SQLite executed.
    pub fn after_schema(
        &self,
        db: &Connection,
        types: &schema::Types,
        alter: Option<&str>,
    ) -> Result<()> {
        let mut inner = self.lock();
        Self::schema_change(&mut inner, db, types, alter)?;
        Self::inspect(&mut inner, db)
    }
    /// Steps that turn the written schema into the current one. Changes
    /// made outside statements BoxDB recognizes as DDL are found here too.
    fn schema_change(
        inner: &mut Inner,
        db: &Connection,
        types: &schema::Types,
        alter: Option<&str>,
    ) -> Result<()> {
        let Some(before) = inner.schema.clone() else {
            return Err(Error::internal("readable schema is unknown"));
        };
        if schema_version(db)? == before.0 {
            return Ok(());
        }
        let after = current_schema(db)?;
        let (statements, created) = schema_statements(&before.1, &after.1, alter)?;
        let sequences = inner.sequences.clone();
        let mut writer = Writer::new();
        writer.schema(statements.iter().map(String::as_str));
        // Created tables, including CREATE TABLE AS SELECT, are written whole.
        for table in &created {
            inner.capture.tables.remove(table);
            inner.capture.order.retain(|t| t != table);
            let layout = Self::layout(inner, db, table, types)?;
            let mut inserts = vec![];
            encode::rows(db, &layout, |row| {
                inserts.push(row);
                Ok(())
            })?;
            writer.changes(&TableChanges {
                layout,
                inserts,
                updates: vec![],
                deletes: vec![],
            })?;
        }
        inner.segments.push(Segment {
            steps: writer.into_steps(),
            capture: None,
            sequences,
            schema: before,
        });
        // Rows of dropped tables need no steps.
        let tables: Vec<String> = inner.capture.order.clone();
        for table in tables {
            if !table_exists(db, &table)? {
                inner.capture.tables.remove(&table);
                inner.capture.order.retain(|t| *t != table);
            }
        }
        inner.sequences = encode::sequences(db)?;
        inner.schema = Some(after);
        Ok(())
    }

    /// Turn captured rows into a segment of steps.
    fn flush(inner: &mut Inner, db: &Connection, types: &schema::Types) -> Result<()> {
        Self::schema_change(inner, db, types, None)?;
        let capture = std::mem::take(&mut inner.capture);
        if capture.is_empty() {
            return Ok(());
        }
        let mut writer = Writer::new();
        let mut expected = inner.sequences.clone();
        for table in &capture.order {
            if !table_exists(db, table)? {
                continue;
            }
            let layout = Self::layout(inner, db, table, types)?;
            let changes = changes(db, &layout, &capture.tables[table])?;
            if layout.autoincrement
                && let Identity::Alias(i) = layout.identity
            {
                for row in changes
                    .inserts
                    .iter()
                    .chain(changes.updates.iter().map(|(_, after)| after))
                {
                    if let SqlValue::Integer(id) = row.values[i] {
                        let seq = expected.entry(table.clone()).or_insert(0);
                        *seq = id.max(*seq);
                    }
                }
            }
            writer.changes(&changes)?;
        }
        let actual = encode::sequences(db)?;
        writer.sequences(&sequence_changes(&expected, &actual));
        let sequences = std::mem::replace(&mut inner.sequences, actual);
        let schema = inner
            .schema
            .clone()
            .ok_or_else(|| Error::internal("readable schema is unknown"))?;
        inner.segments.push(Segment {
            steps: writer.into_steps(),
            capture: Some(capture),
            sequences,
            schema,
        });
        Ok(())
    }

    /// Consume recorded changes and choose a publication.
    pub fn plan(&self, db: &Connection, types: &schema::Types) -> Result<Publication> {
        let mut inner = self.lock();
        let Some(published) = inner.published.clone() else {
            return Ok(Publication::Rewrite);
        };
        Self::flush(&mut inner, db, types)?;
        let mut writer = Writer::new();
        for segment in &inner.segments {
            writer.extend(segment.steps.iter().cloned());
        }
        // Sequences also change without a captured row: SQLite records one for
        // a statement that inserted nothing, and sqlite_sequence can be edited.
        let sequences = encode::sequences(db)?;
        if sequences != inner.sequences {
            writer.sequences(&sequence_changes(&inner.sequences, &sequences));
            inner.sequences = sequences;
        }
        let current = header(db)?;
        let set: Vec<(&str, i64)> = current
            .iter()
            .zip(&published.header)
            .filter(|(now, before)| now.1 != before.1)
            .map(|(now, _)| *now)
            .collect();
        if !set.is_empty() {
            let was: Vec<(&str, i64)> = published
                .header
                .iter()
                .zip(&current)
                .filter(|(before, now)| now.1 != before.1)
                .map(|(before, _)| *before)
                .collect();
            writer.pragma(&set, Some(&was));
        }
        let Some(line) = writer.line() else {
            inner.segments.clear();
            inner.marks.clear();
            return Ok(Publication::Unchanged);
        };
        let mut bytes = Vec::with_capacity(line.len() + 1);
        if !published.terminated {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(line.as_bytes());
        if bytes.len() > MAX_SNAPSHOT {
            return Err(Error::new("a readable transaction exceeds 256 MiB".into()));
        }
        Ok(Publication::Append(bytes))
    }
}

/// Sequence values to write so that replaying `expected` yields `actual`.
fn sequence_changes(
    expected: &BTreeMap<String, i64>,
    actual: &BTreeMap<String, i64>,
) -> BTreeMap<String, Option<i64>> {
    let mut differing: BTreeMap<String, Option<i64>> = actual
        .iter()
        .filter(|(name, value)| expected.get(*name) != Some(value))
        .map(|(name, value)| (name.clone(), Some(*value)))
        .collect();
    for name in expected.keys() {
        if !actual.contains_key(name) {
            differing.insert(name.clone(), None);
        }
    }
    differing
}

fn owned(value: ValueRef<'_>) -> SqlValue {
    match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(n) => SqlValue::Integer(n),
        ValueRef::Real(n) => SqlValue::Real(n),
        ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

/// A hashable spelling of row identity values.
fn identity_key(values: &[SqlValue]) -> String {
    format!("{values:?}")
}

/// Remember the first image of a changed row, or every image for tables
/// without a usable rowid.
fn record_change(entry: &mut TableCapture, case: &PreUpdateCase) {
    fn old_image(old: &rusqlite::hooks::PreUpdateOldValueAccessor) -> Image {
        (0..old.get_column_count())
            .map(|i| {
                // Virtual generated columns have no stored value.
                old.get_old_column_value(i).map_or(SqlValue::Null, owned)
            })
            .collect()
    }
    fn new_image(new: &rusqlite::hooks::PreUpdateNewValueAccessor) -> Image {
        (0..new.get_column_count())
            .map(|i| new.get_new_column_value(i).map_or(SqlValue::Null, owned))
            .collect()
    }
    match entry {
        TableCapture::Positions { order, before } => {
            let mut touch = |rowid: i64, image: &dyn Fn() -> Option<Image>| {
                if let MapEntry::Vacant(slot) = before.entry(rowid) {
                    order.push(rowid);
                    slot.insert(image());
                }
            };
            match case {
                PreUpdateCase::Insert(new) => touch(new.get_new_row_id(), &|| None),
                PreUpdateCase::Delete(old) => touch(old.get_old_row_id(), &|| Some(old_image(old))),
                PreUpdateCase::Update {
                    old_value_accessor: old,
                    new_value_accessor: new,
                } => {
                    touch(old.get_old_row_id(), &|| Some(old_image(old)));
                    touch(new.get_new_row_id(), &|| None);
                }
                PreUpdateCase::Unknown => {}
            }
        }
        TableCapture::Events(events) => match case {
            PreUpdateCase::Insert(new) => {
                events.push((None, Some((new.get_new_row_id(), new_image(new)))))
            }
            PreUpdateCase::Delete(old) => {
                events.push((Some((old.get_old_row_id(), old_image(old))), None))
            }
            PreUpdateCase::Update {
                old_value_accessor: old,
                new_value_accessor: new,
            } => events.push((
                Some((old.get_old_row_id(), old_image(old))),
                Some((new.get_new_row_id(), new_image(new))),
            )),
            PreUpdateCase::Unknown => {}
        },
    }
}

/// The SQL that turns the schema `before` into `after`.
fn schema_statements(
    before: &[(String, String, String)],
    after: &[(String, String, String)],
    alter: Option<&str>,
) -> Result<(Vec<String>, Vec<String>)> {
    let find = |objects: &[(String, String, String)], kind: &str, name: &str| {
        objects
            .iter()
            .find(|(k, n, _)| k == kind && n.eq_ignore_ascii_case(name))
            .map(|(_, _, sql)| sql.clone())
    };
    let drop_sql = |kind: &str, name: &str| {
        format!(
            "DROP {} IF EXISTS {}",
            kind.to_ascii_uppercase(),
            quote(name)
        )
    };
    let mut statements = vec![];
    let mut created = vec![];
    // Objects that no longer exist; tables are dropped last.
    let mut drops = vec![];
    for (kind, name, sql) in before {
        let current = find(after, kind, name);
        let removed = current.is_none();
        let changed = current.as_ref().is_some_and(|now| now != sql);
        if kind == "table" {
            if alter.is_some() {
                // ALTER TABLE renames and rewrites tables itself.
                continue;
            }
            if changed {
                return Err(Error::new(format!(
                    "readable storage cannot record a changed definition of table {name}"
                )));
            }
        }
        if removed || (changed && kind != "table") {
            drops.push((kind != "table", drop_sql(kind, name)));
        }
    }
    drops.sort_by_key(|(object, _)| !*object);
    if let Some(alter) = alter {
        statements.push(alter.trim().trim_end_matches(';').trim_end().to_owned());
    }
    statements.extend(drops.into_iter().map(|(_, sql)| sql));
    for (kind, name, sql) in after {
        let previous = find(before, kind, name);
        if previous.as_deref() == Some(sql.as_str()) {
            continue;
        }
        if kind == "table" {
            if alter.is_some() && !name.starts_with(METADATA_PREFIX) {
                continue;
            }
            if previous.is_some() {
                continue;
            }
            created.push(name.clone());
        }
        statements.push(sql.clone());
    }
    Ok((statements, created))
}

/// Net row changes since the last segment.
fn changes(db: &Connection, layout: &Arc<Layout>, capture: &TableCapture) -> Result<TableChanges> {
    let mut result = TableChanges {
        layout: Arc::clone(layout),
        inserts: vec![],
        updates: vec![],
        deletes: vec![],
    };
    let row = |rowid: Option<i64>, image: &Image| Row {
        rowid,
        values: layout
            .cids
            .iter()
            .map(|cid| image.get(*cid).cloned().unwrap_or(SqlValue::Null))
            .collect(),
    };
    // Images before and after, at each position a row was changed.
    let mut pairs: Vec<(Option<Row>, Option<Row>)> = vec![];
    match capture {
        TableCapture::Positions { order, before } => {
            for rowid in order {
                let image = before[rowid].as_ref().map(|image| row(Some(*rowid), image));
                pairs.push((image, by_rowid(db, layout, *rowid)?));
            }
        }
        TableCapture::Events(events) if layout.identity == Identity::Row => {
            // Rows without identity are a multiset: cancel matching images.
            let mut removed: Vec<Row> = vec![];
            let mut added: Vec<Row> = vec![];
            for (old, new) in events {
                if let Some((_, image)) = old {
                    let old = row(None, image);
                    match added.iter().position(|row| *row == old) {
                        Some(i) => {
                            added.remove(i);
                        }
                        None => removed.push(old),
                    }
                }
                if let Some((_, image)) = new {
                    added.push(row(None, image));
                }
            }
            result.deletes = removed;
            result.inserts = added;
            return Ok(result);
        }
        TableCapture::Events(events) => {
            // Positions by rowid when SQL can address it, otherwise by key.
            let rowid = layout.rowid.is_some() || matches!(layout.identity, Identity::Alias(_));
            let mut order: Vec<(String, Vec<SqlValue>)> = vec![];
            let mut before: HashMap<String, Option<Row>> = HashMap::new();
            let position = |id: i64, image: &Image| {
                let row = row(rowid.then_some(id), image);
                if rowid {
                    vec![SqlValue::Integer(id)]
                } else {
                    layout.identity_of(row.rowid, &row.values)
                }
            };
            for (old, new) in events {
                if let Some((id, image)) = old {
                    let key = position(*id, image);
                    let text = identity_key(&key);
                    if let MapEntry::Vacant(slot) = before.entry(text.clone()) {
                        order.push((text, key));
                        slot.insert(Some(row(rowid.then_some(*id), image)));
                    }
                }
                if let Some((id, image)) = new {
                    let key = position(*id, image);
                    let text = identity_key(&key);
                    if let MapEntry::Vacant(slot) = before.entry(text.clone()) {
                        order.push((text, key));
                        slot.insert(None);
                    }
                }
            }
            for (text, key) in order {
                let after = if rowid {
                    let SqlValue::Integer(id) = key[0] else {
                        unreachable!()
                    };
                    by_rowid(db, layout, id)?
                } else {
                    lookup(db, layout, &key)?
                };
                pairs.push((before.remove(&text).flatten(), after));
            }
        }
    }
    match &layout.identity {
        Identity::Alias(_) | Identity::Rowid(_) => {
            for (before, after) in pairs {
                match (before, after) {
                    (None, Some(after)) => result.inserts.push(after),
                    (Some(before), None) => result.deletes.push(before),
                    (Some(before), Some(after)) if before.values != after.values => {
                        result.updates.push((before, after))
                    }
                    _ => {}
                }
            }
        }
        Identity::Key(_) | Identity::Row => {
            // Compare rows by key: a key may move between rowids.
            let mut keys: Vec<String> = vec![];
            let mut seen: HashSet<String> = HashSet::new();
            let mut befores: HashMap<String, Row> = HashMap::new();
            let mut afters: HashMap<String, Row> = HashMap::new();
            for (before, after) in pairs {
                for (row, map) in [(before, &mut befores), (after, &mut afters)] {
                    if let Some(row) = row {
                        let key = identity_key(&layout.identity_of(row.rowid, &row.values));
                        if seen.insert(key.clone()) {
                            keys.push(key.clone());
                        }
                        map.insert(key, row);
                    }
                }
            }
            for key in keys {
                match (befores.remove(&key), afters.remove(&key)) {
                    (None, Some(after)) => result.inserts.push(after),
                    (Some(before), None) => result.deletes.push(before),
                    (Some(before), Some(after)) if before.values != after.values => {
                        result.updates.push((before, after))
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(result)
}

fn read_row(layout: &Layout, row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let rowid = layout.selects_rowid();
    let offset = usize::from(rowid);
    Ok(Row {
        rowid: if rowid { Some(row.get(0)?) } else { None },
        values: (0..layout.columns.len())
            .map(|i| row.get(i + offset))
            .collect::<rusqlite::Result<_>>()?,
    })
}
fn by_rowid(db: &Connection, layout: &Layout, rowid: i64) -> Result<Option<Row>> {
    let column = match (&layout.identity, layout.rowid) {
        (Identity::Alias(i), _) => quote(&layout.columns[*i]),
        (_, Some(name)) => quote(name),
        _ => return Err(Error::internal("rows of this table have no rowid name")),
    };
    let mut row = db
        .prepare_cached(&format!("{} WHERE {column}=?1", layout.select))?
        .query_row([rowid], |r| read_row(layout, r))
        .optional()?;
    if let Some(row) = &mut row
        && row.rowid.is_none()
        && layout.rowid.is_some()
    {
        row.rowid = Some(rowid);
    }
    if let Some(row) = &mut row
        && matches!(layout.identity, Identity::Alias(_))
    {
        row.rowid = Some(rowid);
    }
    Ok(row)
}
fn lookup(db: &Connection, layout: &Layout, identity: &[SqlValue]) -> Result<Option<Row>> {
    Ok(db
        .prepare_cached(&layout.lookup)?
        .query_row(
            rusqlite::params_from_iter(
                identity.iter().cloned().map(crate::decimal::WireValue::new),
            ),
            |r| read_row(layout, r),
        )
        .optional()?)
}
