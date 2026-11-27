//! Opening a readable log: lines are parsed on several threads and applied in
//! order to an in-memory SQLite database. Triggers and foreign-key actions do
//! not run, because the log already holds their effects.
use super::*;
use rusqlite::{OptionalExtension, config::DbConfig};

/// The parsed steps of one log line.
pub(super) struct Entry<'a> {
    pub(super) line: usize,
    pub(super) steps: Vec<Step<'a>>,
}

/// A failure while reading or applying one line.
pub(super) fn at_line(line: usize, error: Error) -> Error {
    Error::new(format!("line {line}: {error}")).with_code(error.code())
}

fn conflict(message: String) -> Error {
    Error::new(message).with_code("BOXDB_CONFLICT")
}

/// Changes that do not match the database a branch was written against.
fn as_conflict(error: Error) -> Error {
    match error.code() {
        "BOXDB_INVALID_VALUE" | "BOXDB_FORMAT" | "BOXDB_YAML" | "BOXDB_CONFLICT" => error,
        _ => conflict(error.to_string()),
    }
}

/// Parse one line; blank and comment lines have no entry.
pub(super) fn parse(line: usize, text: &str) -> Result<Option<Entry<'_>>> {
    let node = flow::parse_line(text).map_err(|error| {
        Error::new(format!(
            "line {line}, column {}: {}",
            error.column, error.message
        ))
        .with_code("BOXDB_YAML")
    })?;
    let Some(node) = node else { return Ok(None) };
    let steps = entry::steps(node).map_err(|message| {
        Error::new(format!("line {line}: {message}")).with_code("BOXDB_FORMAT")
    })?;
    Ok(Some(Entry { line, steps }))
}

/// Lines of the committed content with their numbers.
pub(super) fn lines(text: &str) -> Vec<(usize, &str)> {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(i, line)| (i + 1, line.strip_suffix('\n').unwrap_or(line)))
        .map(|(i, line)| (i, line.strip_suffix('\r').unwrap_or(line)))
        .collect()
}

/// Lines of at least this many bytes are parsed on several threads.
const PARALLEL_BYTES: usize = 256 * 1024;
/// Lines are parsed and applied in chunks of about this many bytes.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Parse lines in order, on several threads when they are large enough.
pub(super) fn parse_all<'a>(lines: &[(usize, &'a str)]) -> Vec<Result<Option<Entry<'a>>>> {
    let bytes: usize = lines.iter().map(|(_, line)| line.len() + 1).sum();
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if threads < 2 || bytes < PARALLEL_BYTES {
        return lines.iter().map(|(n, line)| parse(*n, line)).collect();
    }
    // Contiguous shares of about equal size.
    let share = bytes.div_ceil(threads);
    let mut ranges = vec![];
    let mut start = 0;
    let mut size = 0;
    for (i, (_, line)) in lines.iter().enumerate() {
        size += line.len() + 1;
        if size >= share {
            ranges.push(start..i + 1);
            start = i + 1;
            size = 0;
        }
    }
    if start < lines.len() {
        ranges.push(start..lines.len());
    }
    std::thread::scope(|scope| {
        ranges
            .into_iter()
            .map(|range| {
                let lines = &lines[range];
                scope.spawn(move || {
                    lines
                        .iter()
                        .map(|(n, line)| parse(*n, line))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// Previous formats started with YAML document markers.
fn previous_format(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| line == "---" || line.starts_with("kind:"))
}
fn previous_format_error() -> Error {
    Error::new(
        "readable files of BoxDB 0.8 and earlier are no longer supported; convert them to binary with BoxDB 0.8, then back to readable with this version"
            .into(),
    )
    .with_code("BOXDB_FORMAT")
}

pub(super) fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|error| {
        Error::new(format!("readable files are UTF-8: {error}")).with_code("BOXDB_FORMAT")
    })
}

/// Restore the committed content of a readable file and report its extent.
pub(crate) fn decode_file(bytes: &[u8]) -> Result<(Connection, Extent)> {
    let extent = extent(bytes);
    let text = utf8(&bytes[..extent.committed])?;
    if previous_format(text) {
        return Err(previous_format_error());
    }
    let lines = lines(text);
    let db = Applier::connection()?;
    let mut applier = Applier::new(&db);
    let mut header = false;
    let mut chunk_start = 0;
    while chunk_start < lines.len() {
        let mut size = 0;
        let mut chunk_end = chunk_start;
        while chunk_end < lines.len() && size < CHUNK_BYTES {
            size += lines[chunk_end].1.len() + 1;
            chunk_end += 1;
        }
        for entry in parse_all(&lines[chunk_start..chunk_end]) {
            let Some(entry) = entry? else { continue };
            if !header {
                check_header(&entry)?;
                header = true;
                continue;
            }
            let line = entry.line;
            applier
                .entry(entry.steps)
                .map_err(|error| at_line(line, error))?;
        }
        chunk_start = chunk_end;
    }
    if !header {
        return Err(
            Error::new("a readable file starts with - {boxdb: 5}".into()).with_code("BOXDB_FORMAT"),
        );
    }
    applier.finish()?;
    Ok((db, extent))
}

pub(super) fn check_header(entry: &Entry<'_>) -> Result<()> {
    match entry.steps.as_slice() {
        [Step::Header(VERSION)] => Ok(()),
        [Step::Header(version)] => Err(Error::new(format!(
            "line {}: unsupported readable format version {version}",
            entry.line
        ))
        .with_code("BOXDB_FORMAT")),
        _ => Err(Error::new(format!(
            "line {}: a readable file starts with - {{boxdb: {VERSION}}}",
            entry.line
        ))
        .with_code("BOXDB_FORMAT")),
    }
}

/// A row decoded from the log: listed columns, and the rowid when stored.
struct Partial {
    values: Vec<Option<SqlValue>>,
    rowid: Option<i64>,
}

pub(super) struct Applier<'c> {
    db: &'c Connection,
    layouts: Layouts,
    /// INSERT statements by table, listed columns and stored rowid.
    inserts: HashMap<String, HashMap<(Vec<bool>, bool), String>>,
    /// Check logical types of each inserted row, for recovery.
    pub(super) validate: bool,
}

impl<'c> Applier<'c> {
    /// An in-memory database that applies the log without running triggers
    /// or foreign-key actions.
    pub(super) fn connection() -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        crate::extensions::register(&db)?;
        db.pragma_update(None, "foreign_keys", false)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
        db.execute_batch("BEGIN")?;
        Ok(db)
    }
    pub(super) fn new(db: &'c Connection) -> Self {
        Self {
            db,
            layouts: Layouts::new(),
            inserts: HashMap::new(),
            validate: false,
        }
    }
    /// Forget layouts after a rolled-back line.
    pub(super) fn forget(&mut self) {
        self.layouts.clear();
        self.inserts.clear();
    }
    /// Check the restored database and make it an ordinary connection.
    pub(super) fn finish(self) -> Result<()> {
        let db = self.db;
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
        let types = schema::load(db)?;
        schema::install(db, &types)?;
        schema::normalize_arrays(db, &types)?;
        schema::uninstall(db)?;
        db.execute_batch("COMMIT")?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, true)?;
        db.flush_prepared_statement_cache();
        db.pragma_update(None, "foreign_keys", true)?;
        // Native values were checked when their text was read and by the
        // engine's column types; other logical types use TEMP validators,
        // which do not run while lines are applied.
        let unchecked: schema::Types = types
            .into_iter()
            .filter(|(_, ty)| ty.native_kind().is_none())
            .collect();
        schema::validate_all(db, &unchecked)?;
        Ok(())
    }
    /// Apply the steps of one line.
    pub(super) fn entry(&mut self, steps: Vec<Step<'_>>) -> Result<()> {
        let mut segment = vec![];
        for step in steps {
            if step.table().is_some() {
                segment.push(step);
                continue;
            }
            self.segment(std::mem::take(&mut segment))?;
            match step {
                Step::Header(_) => {
                    return Err(
                        Error::new("duplicate readable header".into()).with_code("BOXDB_FORMAT")
                    );
                }
                Step::Schema(statements) => {
                    for sql in statements {
                        self.schema(&sql).map_err(as_conflict)?;
                    }
                    self.forget();
                }
                Step::Pragma { set, was } => {
                    for (name, expected) in was.iter().flatten() {
                        let current: i64 =
                            self.db
                                .query_row(&format!("PRAGMA main.{name}"), [], |r| r.get(0))?;
                        if current != *expected {
                            return Err(conflict(format!(
                                "PRAGMA {name} is {current}, not {expected}"
                            )));
                        }
                    }
                    for (name, value) in set {
                        self.db.pragma_update(Some("main"), &name, value)?;
                    }
                }
                Step::Sequences(values) => {
                    for (table, value) in values {
                        self.sequence(&table, value).map_err(as_conflict)?;
                    }
                }
                Step::Insert { .. } | Step::Update { .. } | Step::Delete { .. } => unreachable!(),
            }
        }
        self.segment(segment)
    }
    fn schema(&mut self, sql: &str) -> Result<()> {
        let words: Vec<String> = sql
            .split(|c: char| c.is_whitespace() || c == '(' || c == '"')
            .filter(|w| !w.is_empty())
            .take(3)
            .map(str::to_ascii_uppercase)
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let allowed = matches!(
            words.as_slice(),
            ["CREATE", "UNIQUE", "INDEX", ..]
                | [
                    "CREATE" | "DROP",
                    "TABLE" | "INDEX" | "VIEW" | "TRIGGER",
                    ..
                ]
                | ["ALTER", "TABLE", ..]
        );
        if !allowed {
            return Err(Error::new(format!("unsupported schema statement: {sql}"))
                .with_code("BOXDB_FORMAT"));
        }
        self.db.prepare(sql)?.raw_execute()?;
        Ok(())
    }
    fn sequence(&self, table: &str, value: Option<i64>) -> Result<()> {
        match value {
            None => {
                self.db
                    .execute("DELETE FROM main.sqlite_sequence WHERE name=?1", [table])?;
            }
            Some(value) => {
                if self.db.execute(
                    "UPDATE main.sqlite_sequence SET seq=?1 WHERE name=?2",
                    rusqlite::params![value, table],
                )? == 0
                {
                    self.db.execute(
                        "INSERT INTO main.sqlite_sequence(name,seq) VALUES(?1,?2)",
                        rusqlite::params![table, value],
                    )?;
                }
            }
        }
        Ok(())
    }
    /// BoxDB metadata rows come first, because typed values are read through
    /// them.
    fn segment(&mut self, steps: Vec<Step<'_>>) -> Result<()> {
        if steps.is_empty() {
            return Ok(());
        }
        let (metadata, rows): (Vec<_>, Vec<_>) = steps
            .into_iter()
            .partition(|step| step.table().is_some_and(|t| t.starts_with(METADATA_PREFIX)));
        if !metadata.is_empty() {
            self.rows(metadata)?;
            self.forget();
        }
        self.rows(rows)
    }
    fn layout(&mut self, table: &str) -> Result<Arc<Layout>> {
        self.layouts.get(self.db, table).map_err(as_conflict)
    }
    fn partial(&self, layout: &Layout, fields: &Fields<'_>) -> Result<Partial> {
        let mut partial = Partial {
            values: vec![None; layout.columns.len()],
            rowid: None,
        };
        for (name, node) in fields {
            match layout.column(&name.text).map_err(as_conflict)? {
                Some(index) => {
                    if partial.values[index].is_some() {
                        return Err(Error::new(format!("duplicate column: {}", name.text))
                            .with_code("BOXDB_FORMAT"));
                    }
                    partial.values[index] = Some(layout.decode(index, node)?);
                }
                None => {
                    let Node::Scalar(scalar) = node else {
                        return Err(
                            Error::new("rowid is an integer".into()).with_code("BOXDB_FORMAT")
                        );
                    };
                    let Ok(Resolved::Integer(rowid)) = scalar.resolve() else {
                        return Err(
                            Error::new("rowid is an integer".into()).with_code("BOXDB_FORMAT")
                        );
                    };
                    partial.rowid = Some(rowid);
                }
            }
        }
        Ok(partial)
    }
    fn identity(&self, layout: &Layout, partial: &Partial) -> Result<Vec<SqlValue>> {
        let missing = || {
            Error::new(format!("a {} row lacks its key", layout.table)).with_code("BOXDB_FORMAT")
        };
        let column = |i: usize| partial.values[i].clone().ok_or_else(missing);
        Ok(match &layout.identity {
            Identity::Alias(i) => vec![column(*i)?],
            Identity::Rowid(_) => vec![SqlValue::Integer(partial.rowid.ok_or_else(missing)?)],
            Identity::Key(key) => key.iter().map(|i| column(*i)).collect::<Result<_>>()?,
            Identity::Row => (0..layout.columns.len())
                .map(column)
                .collect::<Result<_>>()?,
        })
    }
    fn lookup(&self, layout: &Layout, identity: &[SqlValue]) -> Result<Option<Row>> {
        let rowid = layout.selects_rowid();
        Ok(self
            .db
            .prepare_cached(&layout.lookup)?
            .query_row(
                rusqlite::params_from_iter(
                    identity.iter().cloned().map(crate::decimal::WireValue::new),
                ),
                |row| {
                    let offset = usize::from(rowid);
                    Ok(Row {
                        rowid: if rowid { Some(row.get(0)?) } else { None },
                        values: (0..layout.columns.len())
                            .map(|i| row.get(i + offset))
                            .collect::<rusqlite::Result<_>>()?,
                    })
                },
            )
            .optional()?)
    }
    fn missing(layout: &Layout, identity: &[SqlValue]) -> Error {
        conflict(format!(
            "{} has no row {}",
            layout.table,
            describe(identity)
        ))
    }
    fn check(layout: &Layout, row: &Row, expected: &Partial, what: &str) -> Result<()> {
        for (i, value) in expected.values.iter().enumerate() {
            if let Some(value) = value
                && *value != row.values[i]
            {
                return Err(conflict(format!(
                    "{what} {}.{} expected {}, found {}",
                    layout.table,
                    layout.columns[i],
                    describe(std::slice::from_ref(value)),
                    describe(std::slice::from_ref(&row.values[i]))
                )));
            }
        }
        Ok(())
    }
    fn delete(&self, layout: &Layout, identity: &[SqlValue]) -> Result<usize> {
        Ok(self
            .db
            .prepare_cached(&layout.delete)?
            .execute(rusqlite::params_from_iter(
                identity.iter().cloned().map(crate::decimal::WireValue::new),
            ))?)
    }
    /// Apply deletions and updates first, then insert new and updated rows,
    /// so values swapped between rows do not conflict.
    fn rows(&mut self, steps: Vec<Step<'_>>) -> Result<()> {
        let mut reinserts: Vec<(Arc<Layout>, Row)> = vec![];
        let mut inserts: Vec<(Arc<Layout>, Partial)> = vec![];
        for step in steps {
            match step {
                Step::Delete { table, rows } => {
                    let layout = self.layout(&table)?;
                    for fields in rows {
                        let partial = self.partial(&layout, &fields)?;
                        let identity = self.identity(&layout, &partial)?;
                        if layout.identity == Identity::Row {
                            let matching = self.delete(&layout, &identity)?;
                            if matching == 0 {
                                return Err(Self::missing(&layout, &identity));
                            }
                            let row = Row {
                                rowid: None,
                                values: identity,
                            };
                            for _ in 1..matching {
                                reinserts.push((Arc::clone(&layout), row.clone()));
                            }
                            continue;
                        }
                        let Some(row) = self.lookup(&layout, &identity)? else {
                            return Err(Self::missing(&layout, &identity));
                        };
                        Self::check(&layout, &row, &partial, "deleted")?;
                        self.delete(&layout, &identity)?;
                    }
                }
                Step::Update {
                    table,
                    key,
                    set,
                    was,
                } => {
                    let layout = self.layout(&table)?;
                    if layout.identity == Identity::Row {
                        return Err(Error::new(format!(
                            "{} has no key to update rows by",
                            layout.table
                        ))
                        .with_code("BOXDB_FORMAT"));
                    }
                    let key = self.partial(&layout, &key)?;
                    let set = self.partial(&layout, &set)?;
                    let was = self.partial(&layout, &was)?;
                    let identity = self.identity(&layout, &key)?;
                    let Some(mut row) = self.lookup(&layout, &identity)? else {
                        return Err(Self::missing(&layout, &identity));
                    };
                    Self::check(&layout, &row, &was, "updated")?;
                    for (i, value) in set.values.into_iter().enumerate() {
                        if let Some(value) = value {
                            row.values[i] = value;
                        }
                    }
                    self.delete(&layout, &identity)?;
                    reinserts.push((layout, row));
                }
                Step::Insert { table, rows } => {
                    let layout = self.layout(&table)?;
                    for fields in rows {
                        let partial = self.partial(&layout, &fields)?;
                        if let Identity::Rowid(_) = layout.identity
                            && partial.rowid.is_none()
                        {
                            return Err(Error::new(format!(
                                "rows of {} store their rowid",
                                layout.table
                            ))
                            .with_code("BOXDB_FORMAT"));
                        }
                        inserts.push((Arc::clone(&layout), partial));
                    }
                }
                _ => unreachable!(),
            }
        }
        for (layout, row) in reinserts {
            let partial = Partial {
                values: row.values.into_iter().map(Some).collect(),
                rowid: row.rowid,
            };
            self.insert(&layout, partial)?;
        }
        for (layout, partial) in inserts {
            self.insert(&layout, partial)?;
        }
        Ok(())
    }
    fn insert(&mut self, layout: &Layout, partial: Partial) -> Result<()> {
        if self.validate {
            for (i, value) in partial.values.iter().enumerate() {
                if let (Some(ty), Some(value)) = (&layout.types[i], value) {
                    schema::decode(ty, value.clone())
                        .map_err(|error| error.with_code("BOXDB_INVALID_VALUE"))?;
                }
            }
        }
        let rowid = partial.rowid.is_some() && layout.rowid.is_some();
        let mask: Vec<bool> = partial.values.iter().map(Option::is_some).collect();
        let statements = match self.inserts.get_mut(layout.table.as_str()) {
            Some(statements) => statements,
            None => self.inserts.entry(layout.table.clone()).or_default(),
        };
        let sql = match statements.entry((mask, rowid)) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let names = layout
                    .rowid
                    .filter(|_| rowid)
                    .into_iter()
                    .chain(
                        layout
                            .columns
                            .iter()
                            .zip(&entry.key().0)
                            .filter(|(_, listed)| **listed)
                            .map(|(c, _)| c.as_str()),
                    )
                    .map(quote)
                    .collect::<Vec<_>>();
                entry.insert(if names.is_empty() {
                    format!("INSERT INTO main.{} DEFAULT VALUES", quote(&layout.table))
                } else {
                    format!(
                        "INSERT INTO main.{}({}) VALUES({})",
                        quote(&layout.table),
                        names.join(","),
                        vec!["?"; names.len()].join(",")
                    )
                })
            }
        };
        let values = partial
            .rowid
            .filter(|_| rowid)
            .map(SqlValue::Integer)
            .into_iter()
            .chain(partial.values.into_iter().flatten())
            .map(crate::decimal::WireValue::new);
        self.db
            .prepare_cached(sql)?
            .execute(rusqlite::params_from_iter(values))
            .map_err(|error| conflict(format!("insert into {}: {error}", layout.table)))?;
        Ok(())
    }
}

/// Short text of values for conflict messages.
fn describe(values: &[SqlValue]) -> String {
    let text = values
        .iter()
        .map(|value| match value {
            SqlValue::Null => "NULL".into(),
            SqlValue::Integer(n) => n.to_string(),
            SqlValue::Real(n) => n.to_string(),
            SqlValue::Text(t) if t.chars().count() > 40 => {
                format!("{:?}…", t.chars().take(40).collect::<String>())
            }
            SqlValue::Text(t) => format!("{t:?}"),
            SqlValue::Blob(b) => format!("<{} bytes>", b.len()),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() == 1 {
        text
    } else {
        format!("({text})")
    }
}
