//! Strict import of the committed content of a readable stream.
use super::*;

pub(super) fn out_of_order() -> Error {
    Error::new(
        "readable documents are out of order; expected the header, schema, BoxDB metadata rows, table rows, sequences and commits"
            .into(),
    )
    .with_code("BOXDB_FORMAT")
}

pub(super) fn check_header(record: Option<&Record>) -> Result<(i64, i64)> {
    match record {
        Some(Record::Database {
            version: VERSION,
            codec_version,
            application_id,
            user_version,
        }) => {
            if *codec_version != libsqlite3_sys::wire::VERSION {
                return Err(Error::new("unsupported BoxDB BLOB codec version".into())
                    .with_code("BOXDB_UNSUPPORTED_CODEC"));
            }
            Ok((*application_id, *user_version))
        }
        Some(Record::Database { version: 3, .. }) => Err(Error::new(
            "readable format version 3 is no longer supported; convert it to binary with BoxDB 0.7, then back to readable with this version"
                .into(),
        )
        .with_code("BOXDB_FORMAT")),
        _ => Err(Error::new("invalid readable format version".into()).with_code("BOXDB_FORMAT")),
    }
}

/// Indexes, views, triggers and sequences follow rows, so imports do not
/// replay application triggers.
pub(super) fn apply_deferred(db: &Connection, record: &Record) -> Result<()> {
    match record {
        Record::Schema {
            object_type,
            name,
            sql,
            ..
        } if object_type != "table" => {
            if !["index", "view", "trigger"].contains(&object_type.as_str())
                || !sql.trim_start().to_ascii_uppercase().starts_with("CREATE ")
            {
                return Err(Error::new("invalid schema record".into()));
            }
            db.prepare(sql)?.execute([])?;
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
                rusqlite::params![object_type, name],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error::new("mismatched schema record".into()));
            }
        }
        Record::Sequence { table, value } => {
            let updated = db.execute(
                "UPDATE sqlite_sequence SET seq=?1 WHERE name=?2",
                rusqlite::params![value, table],
            )?;
            if updated == 0 {
                db.execute(
                    "INSERT INTO sqlite_sequence(name,seq) VALUES(?1,?2)",
                    rusqlite::params![table, value],
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Remove every changed row before inserting final images, so values swapped
/// between rows do not conflict. Import runs without foreign-key actions.
pub(super) fn apply_commit(
    db: &Connection,
    tables: Vec<TableChanges>,
    sequences: &BTreeMap<String, i64>,
) -> Result<()> {
    let mut plans = Vec::with_capacity(tables.len());
    for changes in tables {
        let layout = changes
            .layout
            .ok_or_else(|| Error::internal("commit parsed without a schema"))?;
        let delete = layout.delete.clone().ok_or_else(|| {
            Error::new("commits require tables with an accessible rowid".into())
                .with_code("BOXDB_FORMAT")
        })?;
        if changes.put.iter().any(|row| row.rowid.is_none()) {
            return Err(Error::new("commit rows require a rowid".into()).with_code("BOXDB_FORMAT"));
        }
        let mut statement = db.prepare_cached(&delete)?;
        for id in changes
            .delete
            .iter()
            .copied()
            .chain(changes.put.iter().filter_map(|row| row.rowid))
        {
            statement.execute([id])?;
        }
        plans.push((layout, changes.put));
    }
    for (layout, rows) in plans {
        for row in rows {
            layout.insert(db, row)?;
        }
    }
    for (table, value) in sequences {
        apply_deferred(
            db,
            &Record::Sequence {
                table: table.clone(),
                value: *value,
            },
        )?;
    }
    Ok(())
}

/// Stream sections in order. Typed rows need the schema and BoxDB metadata
/// that precede them; commits follow the snapshot.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub(super) enum Section {
    Schema,
    Metadata,
    Rows,
    Sequences,
    Commits,
}
pub(super) struct Import<'a> {
    pub(super) section: Section,
    pub(super) layouts: Layouts<'a>,
}
impl Import<'_> {
    pub(super) fn enter(&mut self, section: Section) -> Result<()> {
        if section < self.section {
            return Err(out_of_order());
        }
        self.section = section;
        Ok(())
    }
    pub(super) fn rows_section(table: &str) -> Section {
        if table.starts_with(METADATA_PREFIX) {
            Section::Metadata
        } else {
            Section::Rows
        }
    }
    pub(super) fn layout(&mut self, table: &str) -> Result<Arc<Layout>> {
        // Commit tables resolve before the commit is entered; rows documents
        // are checked again once parsed.
        if self.section < Section::Sequences {
            self.enter(Self::rows_section(table))?;
        }
        self.layouts.get(table)
    }
}

/// Rows documents of at least this size are parsed on several threads.
const PARALLEL_BYTES: usize = 256 * 1024;

/// Byte offsets where a block rows section can be split: starts of items at
/// column zero, near equal shares of the section.
fn chunk_starts(section: &[u8], chunks: usize) -> Vec<usize> {
    let mut starts = vec![0];
    for share in 1..chunks {
        let target = (section.len() * share / chunks).max(starts[starts.len() - 1] + 1);
        let found = section[target.min(section.len())..]
            .windows(3)
            .position(|w| w[0] == b'\n' && w[1] == b'-' && w[2].is_ascii_whitespace())
            .map(|position| target + position + 1);
        match found {
            Some(start) if start > starts[starts.len() - 1] => starts.push(start),
            _ => break,
        }
    }
    starts
}

/// Parse a large rows document in chunks of whole rows on several threads.
/// `None` leaves the document to the sequential parser, which also reports
/// errors and resolves aliases between rows.
fn parse_rows_in_parallel(
    fragment: &[u8],
    import: &mut Import,
) -> Result<Option<(String, Vec<Row>)>> {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if threads < 2 || fragment.len() < PARALLEL_BYTES {
        return Ok(None);
    }
    let Some(RowsSection::Block { head, items }) = rows_section(fragment) else {
        return Ok(None);
    };
    let section = &fragment[items..];
    if !section.starts_with(b"- ") {
        return Ok(None);
    }
    let Ok(table) = rows_head(&fragment[..head]) else {
        return Ok(None);
    };
    let layout = import.layout(&table)?;
    let chunks = threads.min(section.len() / (PARALLEL_BYTES / 2)).max(2);
    let mut starts = chunk_starts(section, chunks);
    starts.push(section.len());
    let parsed: Vec<Result<Vec<Row>>> = std::thread::scope(|scope| {
        starts
            .windows(2)
            .map(|range| {
                let chunk = &section[range[0]..range[1]];
                let layout = &layout;
                scope.spawn(move || parse_items(chunk, layout))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let mut rows = Vec::new();
    for chunk in parsed {
        match chunk {
            Ok(chunk) => rows.extend(chunk),
            Err(_) => return Ok(None),
        }
    }
    Ok(Some((table, rows)))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Connection> {
    decode_file(bytes).map(|(db, _)| db)
}

/// Restore the committed content of a readable file and report its extent.
pub(crate) fn decode_file(bytes: &[u8]) -> Result<(Connection, Extent)> {
    let extent = extent(bytes);
    let mut documents = documents(&bytes[..extent.committed]).into_iter();
    let header = documents
        .next()
        .map(|(_, bytes)| parse_document(bytes, &mut |_| Ok(None)))
        .transpose()?;
    let (application_id, user_version) = check_header(header.as_ref())?;
    let db = Connection::open_in_memory()?;
    crate::extensions::register(&db)?;
    // Commit replay deletes rows; foreign keys are checked once at the end.
    db.pragma_update(None, "foreign_keys", false)?;
    db.execute_batch("BEGIN")?;
    let mut import = Import {
        section: Section::Schema,
        layouts: Layouts::new(&db, None),
    };
    let mut deferred = vec![];
    for (_, fragment) in documents {
        if let Some((table, rows)) = parse_rows_in_parallel(fragment, &mut import)? {
            import.enter(Import::rows_section(&table))?;
            let layout = import.layouts.get(&table)?;
            for row in rows {
                layout.insert(&db, row)?;
            }
            continue;
        }
        let record = parse_document(fragment, &mut |table| import.layout(table).map(Some))?;
        match record {
            Record::Database { .. } => {
                return Err(
                    Error::new("duplicate readable header".into()).with_code("BOXDB_FORMAT")
                );
            }
            Record::Schema {
                object_type,
                name,
                sql,
                ..
            } if object_type == "table" => {
                import.enter(Section::Schema)?;
                if !sql
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE")
                {
                    return Err(Error::new("invalid table declaration".into()));
                }
                db.prepare(&sql)?.execute([])?;
                if !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                    [&name],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(Error::new("mismatched schema record".into()));
                }
            }
            record @ Record::Schema { .. } => {
                import.enter(Section::Schema)?;
                deferred.push(record);
            }
            Record::Rows { table, rows } => {
                import.enter(Import::rows_section(&table))?;
                let layout = import.layouts.get(&table)?;
                for row in rows {
                    layout.insert(&db, row)?;
                }
            }
            record @ Record::Sequence { .. } => {
                import.enter(Section::Sequences)?;
                apply_deferred(&db, &record)?;
            }
            Record::Commit { tables, sequences } => {
                import.enter(Section::Commits)?;
                apply_commit(&db, tables, &sequences)?;
            }
        }
    }
    let types = import.layouts.into_types()?;
    // Normalize imported values before recreating indexes and user triggers.
    // Import restores data, so it must not replay application update effects.
    schema::install(&db, &types)?;
    schema::normalize_arrays(&db, &types)?;
    schema::uninstall(&db)?;
    for record in &deferred {
        apply_deferred(&db, record)?;
    }
    if db
        .prepare("PRAGMA main.foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(Error::sqlite(
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            "FOREIGN KEY constraint failed".into(),
        ));
    }
    db.pragma_update(None, "application_id", application_id)?;
    db.pragma_update(None, "user_version", user_version)?;
    db.execute_batch("COMMIT")?;
    db.pragma_update(None, "foreign_keys", true)?;
    let types = schema::load(&db)?;
    schema::validate_all(&db, &types)?;
    Ok((db, extent))
}
