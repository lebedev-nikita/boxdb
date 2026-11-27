//! The canonical log of a database: its schema, header values and one line
//! per row, in a deterministic order.
use super::*;
use std::io::Write;

/// Limit both exports and streamed publication before the destination is replaced.
pub(super) struct LimitedWriter<W> {
    pub(super) inner: W,
    pub(super) bytes: usize,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SNAPSHOT.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("readable file exceeds 256 MiB limit"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Schema objects of the main database in creation order.
pub(super) fn objects(db: &Connection) -> Result<Vec<(String, String, String)>> {
    let objects = db
        .prepare("SELECT type,name,sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*' ORDER BY rowid")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    if objects
        .iter()
        .any(|(_, _, sql)| sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
    {
        return Err(Error::new(
            "virtual tables require binary storage; readable storage does not support them".into(),
        ));
    }
    Ok(objects)
}

/// Every row of a table in identity order.
pub(super) fn rows(
    db: &Connection,
    layout: &Layout,
    mut each: impl FnMut(Row) -> Result<()>,
) -> Result<()> {
    let order = match &layout.identity {
        Identity::Alias(i) => quote(&layout.columns[*i]),
        Identity::Rowid(name) => quote(name),
        Identity::Key(key) => key
            .iter()
            .map(|i| quote(&layout.columns[*i]))
            .collect::<Vec<_>>()
            .join(","),
        Identity::Row => (1..=layout.columns.len())
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(","),
    };
    let rowid = layout.selects_rowid();
    let mut statement = db.prepare(&format!("{} ORDER BY {order}", layout.select))?;
    let mut cursor = statement.query([])?;
    while let Some(row) = cursor.next()? {
        let offset = usize::from(rowid);
        each(Row {
            rowid: if rowid { Some(row.get(0)?) } else { None },
            values: (0..layout.columns.len())
                .map(|i| row.get(i + offset))
                .collect::<rusqlite::Result<_>>()?,
        })?;
    }
    Ok(())
}

/// AUTOINCREMENT values that inserting the table's rows would not restore.
pub(super) fn sequences(db: &Connection) -> Result<BTreeMap<String, i64>> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='sqlite_sequence')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(BTreeMap::new());
    }
    Ok(db
        .prepare("SELECT name,seq FROM main.sqlite_sequence ORDER BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Write the canonical log: tables, header values, BoxDB metadata rows, other
/// rows, then indexes, views, triggers and sequences.
pub(crate) fn write_log(
    db: &Connection,
    types: &schema::Types,
    writer: &mut impl Write,
) -> Result<usize> {
    let mut writer = LimitedWriter {
        inner: writer,
        bytes: 0,
    };
    writer.write_all(HEADER.as_bytes())?;
    let objects = objects(db)?;
    let mut tables = vec![];
    for (kind, name, sql) in &objects {
        if kind == "table" {
            let mut step = Writer::new();
            step.schema([sql.as_str()]);
            write_lines(&mut writer, &step)?;
            tables.push(name.as_str());
        }
    }
    let header: Vec<(&str, i64)> = [
        (
            "application_id",
            db.query_row("PRAGMA main.application_id", [], |r| r.get(0))?,
        ),
        (
            "user_version",
            db.query_row("PRAGMA main.user_version", [], |r| r.get(0))?,
        ),
    ]
    .into_iter()
    .filter(|(_, value)| *value != 0)
    .collect();
    if !header.is_empty() {
        let mut step = Writer::new();
        step.pragma(&header, None);
        write_lines(&mut writer, &step)?;
    }
    tables.sort_by_key(|name| !name.starts_with(METADATA_PREFIX));
    let mut restored = BTreeMap::new();
    for table in tables {
        let layout = Arc::new(if table.starts_with(METADATA_PREFIX) {
            Layout::load(db, table, &schema::Types::new())?
        } else {
            Layout::load(db, table, types)?
        });
        let mut line = String::new();
        let head = format!("- {{insert: {}, rows: [", key(table));
        rows(db, &layout, |row| {
            if layout.autoincrement
                && let Identity::Alias(i) = layout.identity
                && let SqlValue::Integer(id) = row.values[i]
            {
                let seq = restored.entry(table.to_owned()).or_insert(0);
                *seq = id.max(*seq);
            }
            line.clear();
            line.push_str(&head);
            inserted(&mut line, &layout, &row)?;
            line.push_str("]}\n");
            writer.write_all(line.as_bytes())?;
            Ok(())
        })?;
    }
    for (kind, _, sql) in &objects {
        if kind != "table" {
            let mut step = Writer::new();
            step.schema([sql.as_str()]);
            write_lines(&mut writer, &step)?;
        }
    }
    let current = sequences(db)?;
    let mut differing: BTreeMap<String, Option<i64>> = current
        .iter()
        .filter(|(name, value)| restored.get(*name) != Some(value))
        .map(|(name, value)| (name.clone(), Some(*value)))
        .collect();
    for name in restored.keys() {
        if !current.contains_key(name) {
            differing.insert(name.clone(), None);
        }
    }
    let mut step = Writer::new();
    step.sequences(&differing);
    write_lines(&mut writer, &step)?;
    Ok(writer.bytes)
}

fn write_lines(writer: &mut impl Write, step: &Writer) -> Result<()> {
    for line in step.lines() {
        writer.write_all(line.as_bytes())?;
    }
    Ok(())
}
