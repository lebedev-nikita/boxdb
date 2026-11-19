//! Canonical snapshots and the row lines shared with commits.
use super::*;

/// Cached table layout; key quoting is done once per schema.
pub(super) struct TablePlan {
    pub(super) table: String,
    pub(super) layout: Layout,
    pub(super) select: String,
    pub(super) lookup: Option<String>,
    pub(super) head: Vec<u8>,
    /// Column index and YAML key, ordered by column name.
    pub(super) keys: Vec<(usize, String)>,
}
impl TablePlan {
    pub(super) fn new(db: &Connection, name: &str, types: &schema::Types) -> Result<Self> {
        let layout = Layout::load(db, name, types)?;
        let rowid = layout.rowid;
        let selected = layout
            .columns
            .iter()
            .map(|n| quote(n))
            .collect::<Vec<_>>()
            .join(",");
        let prefix = rowid.map(|r| format!("{},", quote(r))).unwrap_or_default();
        let ordering = rowid.map(quote).unwrap_or_else(|| selected.clone());
        let source = format!("SELECT {prefix}{selected} FROM main.{}", quote(name));
        let select = format!("{source} ORDER BY {ordering}");
        let lookup = rowid.map(|alias| format!("{source} WHERE {}=?1", quote(alias)));
        #[derive(Serialize)]
        struct Head<'a> {
            kind: &'static str,
            table: &'a str,
        }
        let mut head = b"---\n".to_vec();
        serde_saphyr::to_io_writer(
            &mut head,
            &Head {
                kind: "rows",
                table: name,
            },
        )?;
        head.extend_from_slice(b"rows:\n");
        let mut keys = layout
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let key = if plain_text(column) {
                    column.clone()
                } else {
                    quoted(column)
                };
                // YAML limits implicit keys to 1024 characters.
                if key.chars().count() > 1024 {
                    return Err(Error::new(
                        "readable storage supports column names up to 1000 characters".into(),
                    ));
                }
                Ok((index, key))
            })
            .collect::<Result<Vec<_>>>()?;
        keys.sort_by(|a, b| layout.columns[a.0].cmp(&layout.columns[b.0]));
        Ok(Self {
            table: name.into(),
            layout,
            select,
            lookup,
            head,
            keys,
        })
    }
    pub(super) fn mismatch(&self, index: usize) -> Error {
        Error::new(format!(
            "readable export found a value of another type in typed column {}.{}",
            self.table, self.layout.columns[index]
        ))
        .with_code("BOXDB_INVALID_VALUE")
    }
    /// Inline text of a cell; multi-line text returns `None`.
    pub(super) fn inline(&self, index: usize, value: ValueRef<'_>) -> Result<Option<String>> {
        let class = self.layout.classes[index];
        Ok(Some(match (class, value) {
            (_, ValueRef::Null) => unreachable!(),
            (Class::Boolean, ValueRef::Integer(n @ (0 | 1))) => (n == 1).to_string(),
            (Class::Plain | Class::Boolean, ValueRef::Integer(n)) => n.to_string(),
            (Class::Plain | Class::Boolean, ValueRef::Real(n)) if n.is_finite() => {
                serde_saphyr::to_string(&n)?.trim_end().to_owned()
            }
            (Class::Plain | Class::Boolean, ValueRef::Real(_)) => {
                return Err(Error::new(
                    "readable storage cannot represent non-finite SQLite values".into(),
                ));
            }
            (Class::Plain | Class::Boolean, ValueRef::Text(bytes)) => {
                let text = std::str::from_utf8(bytes).map_err(Error::validation)?;
                if plain_text(text) {
                    text.into()
                } else if text.contains('\n') {
                    return Ok(None);
                } else {
                    quoted(text)
                }
            }
            (Class::Plain | Class::Boolean, ValueRef::Blob(bytes)) => match native_text(bytes) {
                Some((kind, text)) => {
                    format!("{{{}: {}}}", native_name(kind), bare_or_quoted(&text))
                }
                None => format!(
                    "{{blob: {}}}",
                    bare_or_quoted(&BASE64.encode(libsqlite3_sys::wire::blob_payload(bytes)))
                ),
            },
            (Class::Bytea, ValueRef::Blob(bytes)) if native_text(bytes).is_none() => {
                bare_or_quoted(&BASE64.encode(libsqlite3_sys::wire::blob_payload(bytes)))
            }
            (Class::Native(expected), ValueRef::Blob(bytes)) => match native_text(bytes) {
                Some((kind, text)) if kind == expected => bare_or_quoted(&text),
                _ => return Err(self.mismatch(index)),
            },
            _ => return Err(self.mismatch(index)),
        }))
    }
    /// Rows are flow mappings on one line unless a text value spans lines.
    pub(super) fn write_row(
        &self,
        row: &rusqlite::Row<'_>,
        writer: &mut impl std::io::Write,
    ) -> Result<()> {
        let offset = usize::from(self.layout.rowid.is_some());
        let mut entries: Vec<(&str, Option<String>, usize)> =
            Vec::with_capacity(self.keys.len() + 1);
        if let Some(alias) = self.layout.rowid {
            entries.push((alias, Some(row.get::<_, i64>(0)?.to_string()), 0));
        }
        let mut block = false;
        for (index, key) in &self.keys {
            let value = row.get_ref(index + offset)?;
            if matches!(value, ValueRef::Null) {
                continue;
            }
            let text = self.inline(*index, value)?;
            block |= text.is_none();
            entries.push((key, text, index + offset));
        }
        if !block {
            writer.write_all(b"- {")?;
            for (position, (key, text, _)) in entries.iter().enumerate() {
                if position > 0 {
                    writer.write_all(b", ")?;
                }
                writer.write_all(key.as_bytes())?;
                writer.write_all(b": ")?;
                writer.write_all(text.as_deref().unwrap().as_bytes())?;
            }
            writer.write_all(b"}\n")?;
            return Ok(());
        }
        for (position, (key, text, column)) in entries.iter().enumerate() {
            writer.write_all(if position == 0 { b"- " } else { b"  " })?;
            writer.write_all(key.as_bytes())?;
            writer.write_all(b":")?;
            match text {
                Some(text) => {
                    writer.write_all(b" ")?;
                    writer.write_all(text.as_bytes())?;
                    writer.write_all(b"\n")?;
                }
                None => {
                    let ValueRef::Text(bytes) = row.get_ref(*column)? else {
                        unreachable!()
                    };
                    let text = std::str::from_utf8(bytes).map_err(Error::validation)?;
                    write_scalar(writer, &serde_saphyr::to_string(&text)?)?;
                }
            }
        }
        Ok(())
    }
}
pub(super) fn write_scalar(writer: &mut impl std::io::Write, yaml: &str) -> Result<()> {
    writer.write_all(b" ")?;
    for (i, line) in yaml.split_inclusive('\n').enumerate() {
        if i > 0 {
            writer.write_all(b"  ")?;
        }
        writer.write_all(line.as_bytes())?;
    }
    Ok(())
}
pub(super) fn write_document(writer: &mut impl std::io::Write, record: &Record) -> Result<()> {
    writer.write_all(b"---\n")?;
    serde_saphyr::to_io_writer(writer, record)?;
    Ok(())
}
pub(super) fn header_record(db: &Connection) -> Result<Record> {
    Ok(Record::Database {
        version: VERSION,
        codec_version: libsqlite3_sys::wire::VERSION,
        application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
        user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
    })
}
pub(super) fn schema_records(db: &Connection) -> Result<Vec<Record>> {
    let mut records = vec![header_record(db)?];
    let objects = db.prepare("SELECT type,name,tbl_name,sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*' ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END,name")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (object_type, name, table, sql) in objects {
        if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE") {
            return Err(Error::new(
                "readable export of virtual tables is unsupported".into(),
            ));
        }
        records.push(Record::Schema {
            object_type,
            name,
            table,
            sql,
        });
    }
    Ok(records)
}
pub(super) fn sequence_records(db: &Connection) -> Result<Vec<Record>> {
    let mut records = vec![];
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='sqlite_sequence')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        for row in db
            .prepare("SELECT name,seq FROM main.sqlite_sequence ORDER BY name")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        {
            let (table, value) = row?;
            records.push(Record::Sequence { table, value });
        }
    }
    Ok(records)
}
/// Limit both exports and streamed publication before the destination is replaced.
pub(super) struct LimitedWriter<W> {
    pub(super) inner: W,
    pub(super) bytes: usize,
}
impl<W: std::io::Write> std::io::Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SNAPSHOT.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("snapshot exceeds 256 MiB limit"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Write the canonical snapshot: header, schema, BoxDB metadata rows, other
/// rows and sequences, ending with `...` so commits can follow.
pub(crate) fn write_snapshot(
    db: &Connection,
    types: &schema::Types,
    writer: &mut impl std::io::Write,
) -> Result<usize> {
    let mut writer = LimitedWriter {
        inner: writer,
        bytes: 0,
    };
    let records = schema_records(db)?;
    let mut tables = vec![];
    for record in &records {
        write_document(&mut writer, record)?;
        if let Record::Schema {
            object_type, name, ..
        } = record
            && object_type == "table"
        {
            tables.push(name.as_str());
        }
    }
    tables.sort_by_key(|name| !name.starts_with(METADATA_PREFIX));
    for name in tables {
        let plan = TablePlan::new(db, name, types)?;
        let mut statement = db.prepare(&plan.select)?;
        let mut rows = statement.query([])?;
        let mut first = true;
        while let Some(row) = rows.next()? {
            if std::mem::take(&mut first) {
                std::io::Write::write_all(&mut writer, &plan.head)?;
            }
            plan.write_row(row, &mut writer)?;
        }
    }
    for record in sequence_records(db)? {
        write_document(&mut writer, &record)?;
    }
    std::io::Write::write_all(&mut writer, b"...\n")?;
    Ok(writer.bytes)
}
