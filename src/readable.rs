//! A standalone YAML stream of SQLite schemas and named rows: a canonical
//! snapshot followed by appended commit documents. Each table's rows form one
//! document; typed columns use plain scalars decoded through the table schema.
use crate::{
    Error, Result, Type,
    schema::{self, quote},
    storage::MAX_SNAPSHOT,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use libsqlite3_sys::native::Kind;
use rusqlite::{
    Connection,
    types::{Value as SqlValue, ValueRef},
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    rc::Rc,
};
mod journal;
pub(crate) use journal::{Journal, Publication};

const VERSION: u32 = 4;
const METADATA_PREFIX: &str = "__boxdb_";
/// BoxDB starts every appended commit with these bytes.
const COMMIT: &[u8] = b"---\nkind: commit\n";

/// The committed part of a readable file and where its journal starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Extent {
    pub committed: usize,
    pub snapshot: usize,
    /// An interrupted append left content after `committed`.
    pub torn: bool,
    /// The committed content ends with a `...` line, so commits can follow.
    pub appendable: bool,
}

fn end_marker(line: &[u8]) -> bool {
    line.strip_prefix(b"...").is_some_and(|rest| {
        let rest = rest.trim_ascii();
        rest.is_empty() || rest.starts_with(b"#")
    })
}

/// Snapshots and commits end with `...`. Content after the last marker that
/// starts a commit or contains NUL bytes is an interrupted append, as is a
/// final commit with NUL bytes from blocks that were never written.
pub(crate) fn extent(bytes: &[u8]) -> Extent {
    let mut last_end = None;
    let mut offset = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        offset += line.len();
        if end_marker(line) {
            last_end = Some(offset);
        }
    }
    let mut committed = bytes.len();
    let mut torn = false;
    if let Some(end) = last_end {
        let tail = &bytes[end..];
        let start = tail.trim_ascii_start();
        if !blank(tail)
            && (tail.contains(&0) || start.starts_with(COMMIT) || COMMIT.starts_with(start))
        {
            committed = end;
            torn = true;
        }
    }
    let body = &bytes[..committed];
    let fragments = documents(body);
    let start_of = |fragment: &[u8]| fragment.as_ptr() as usize - body.as_ptr() as usize;
    if let Some((_, last)) = fragments.last()
        && last.starts_with(COMMIT)
        && last.contains(&0)
    {
        return Extent {
            torn: true,
            ..extent(&bytes[..start_of(last)])
        };
    }
    let snapshot = fragments
        .iter()
        .find(|(_, fragment)| fragment.starts_with(COMMIT))
        .map_or(committed, |(_, fragment)| start_of(fragment));
    let appendable = last_end.is_some_and(|end| end <= committed && blank(&body[end..]))
        && body.ends_with(b"\n");
    Extent {
        committed,
        snapshot,
        torn,
        appendable,
    }
}

/// Top-level YAML document markers cannot occur inside indented scalar content.
pub(crate) fn documents(bytes: &[u8]) -> Vec<(usize, &[u8])> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut start_line = 1;
    let mut offset = 0;
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if line.starts_with(b"---") && line.get(3).is_none_or(u8::is_ascii_whitespace) {
            if offset > start && !blank(&bytes[start..offset]) {
                result.push((start_line, &bytes[start..offset]));
            }
            start = offset;
            start_line = index + 1;
        }
        offset += line.len();
    }
    if start < bytes.len() {
        result.push((start_line, &bytes[start..]));
    }
    result
}

/// Whether a fragment has only whitespace, comments and document markers.
fn blank(bytes: &[u8]) -> bool {
    bytes.split(|b| *b == b'\n').all(|line| {
        line.iter().all(u8::is_ascii_whitespace) || line.starts_with(b"#") || line == b"---"
    })
}

pub(crate) fn check_documents(bytes: &[u8], report: &mut crate::CheckReport) {
    let bytes = &bytes[..extent(bytes).committed];
    for (index, (line, bytes)) in documents(bytes).iter().enumerate() {
        if let Err(error) = parse(bytes, &mut |_| Ok(None), &mut None) {
            let location = error
                .location()
                .map(|l| *line + l.line() as usize - 1)
                .unwrap_or(*line);
            report.issue(
                "yaml",
                &Error::new("invalid YAML document".into()).with_code("BOXDB_YAML"),
            );
            let issue = report.issues.last_mut().unwrap();
            issue.document = Some(index + 1);
            issue.line = Some(location);
            if report.issues.len() >= 1000 {
                break;
            }
        }
    }
}

pub(crate) type Rejected = (crate::RejectedDocument, Vec<u8>);

fn options(len: usize) -> serde_saphyr::Options {
    serde_saphyr::options! {
        emit_comments: false,
        strict_booleans: true,
        reject_unsupported_tags: true,
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_nodes: len.saturating_add(16),
            max_events: len.saturating_mul(2).saturating_add(16),
            max_total_scalar_bytes: MAX_SNAPSHOT,
            max_total_comment_bytes: MAX_SNAPSHOT,
        },
    }
}

/// Resolves a table's row layout while its document is parsed. `None` checks
/// document structure without a schema.
type Resolve<'a> = dyn FnMut(&str) -> Result<Option<Rc<Layout>>> + 'a;

/// Parse one document. Schema errors raised by `resolve` are kept in `failure`
/// so their codes survive the YAML deserializer.
fn parse(
    bytes: &[u8],
    resolve: &mut Resolve<'_>,
    failure: &mut Option<Error>,
) -> std::result::Result<Record, serde_saphyr::DeserializeError> {
    serde_saphyr::with_deserializer_from_slice_with_options(bytes, options(bytes.len()), |de| {
        DocumentSeed { resolve, failure }.deserialize(de)
    })
}

fn parse_document(bytes: &[u8], resolve: &mut Resolve<'_>) -> Result<Record> {
    let mut failure = None;
    parse(bytes, resolve, &mut failure).map_err(|error| failure.take().unwrap_or(error.into()))
}

fn out_of_order() -> Error {
    Error::new(
        "readable documents are out of order; expected the header, schema, BoxDB metadata rows, table rows, sequences and commits"
            .into(),
    )
    .with_code("BOXDB_FORMAT")
}

fn check_header(record: Option<&Record>) -> Result<(i64, i64)> {
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

/// Where a rows section starts within a document fragment.
enum RowsSection {
    Block { head: usize, items: usize },
    Inline,
}

fn rows_section(fragment: &[u8]) -> Option<RowsSection> {
    let mut offset = 0;
    for line in fragment.split_inclusive(|b| *b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"rows:") {
            return Some(
                if rest.iter().all(u8::is_ascii_whitespace)
                    || rest.trim_ascii_start().starts_with(b"#")
                {
                    RowsSection::Block {
                        head: offset,
                        items: offset + line.len(),
                    }
                } else {
                    RowsSection::Inline
                },
            );
        }
        offset += line.len();
    }
    None
}

/// Split a block rows section into items that start with `-` at column zero.
/// Item content is always indented, so a damaged row cannot hide its successor.
fn row_items(section: &[u8], first_line: usize) -> Vec<(usize, &[u8])> {
    let mut items: Vec<(usize, usize, usize)> = vec![];
    let mut offset = 0;
    for (index, line) in section.split_inclusive(|b| *b == b'\n').enumerate() {
        let starts = line.first() == Some(&b'-') && line.get(1).is_none_or(u8::is_ascii_whitespace);
        let content = !line.iter().all(u8::is_ascii_whitespace) && !line.starts_with(b"#");
        match items.last_mut() {
            Some(item) if !starts => item.2 = offset + line.len(),
            _ if starts || content => items.push((first_line + index, offset, offset + line.len())),
            _ => {}
        }
        offset += line.len();
    }
    items
        .into_iter()
        .map(|(line, start, end)| (line, &section[start..end]))
        .collect()
}

fn parse_items(item: &[u8], layout: &Layout) -> Result<Vec<Row>> {
    Ok(serde_saphyr::with_deserializer_from_slice_with_options(
        item,
        options(item.len()),
        |de| RowsSeed(Some(layout)).deserialize(de),
    )?)
}

/// Recover only independently delimited documents and rows. Metadata is never inferred.
pub(crate) fn salvage(bytes: &[u8]) -> Result<(Connection, Vec<Rejected>)> {
    if bytes.len() > MAX_SNAPSHOT {
        return Err(Error::new("readable database exceeds 256 MiB limit".into()));
    }
    enum Content<'a> {
        Record(Record),
        Rows {
            table: String,
            head: &'a [u8],
            items: Vec<(usize, &'a [u8])>,
        },
    }
    struct Part<'a> {
        document: usize,
        line: usize,
        fragment: &'a [u8],
        content: Content<'a>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Head {
        #[allow(dead_code)]
        kind: HeadKind,
        table: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum HeadKind {
        Rows,
    }
    let reject = |document: usize, line: usize, error: &Error, message: &str, fragment: &[u8]| {
        (
            crate::RejectedDocument {
                document,
                line,
                code: error.code().into(),
                message: message.into(),
            },
            fragment.to_vec(),
        )
    };
    let mut parts = vec![];
    let mut rejected = vec![];
    let extent = extent(bytes);
    let fragments = documents(&bytes[..extent.committed]);
    if extent.torn {
        rejected.push((
            crate::RejectedDocument {
                document: fragments.len() + 1,
                line: bytes[..extent.committed]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1,
                code: "BOXDB_INCOMPLETE_COMMIT".into(),
                message: "an interrupted write left an incomplete commit".into(),
            },
            bytes[extent.committed..].to_vec(),
        ));
    }
    for (index, (line, fragment)) in fragments.into_iter().enumerate() {
        let document = index + 1;
        let content = match rows_section(fragment) {
            Some(RowsSection::Block { head, items }) => {
                serde_saphyr::from_slice_with_options::<Head>(&fragment[..head], options(head))
                    .map_err(Error::from)
                    .map(|parsed| Content::Rows {
                        table: parsed.table,
                        head: &fragment[..items],
                        items: row_items(
                            &fragment[items..],
                            line + fragment[..items].iter().filter(|b| **b == b'\n').count(),
                        ),
                    })
            }
            Some(RowsSection::Inline) => {
                Err(Error::new("rows must use a block sequence".into()).with_code("BOXDB_YAML"))
            }
            None => parse_document(fragment, &mut |_| Ok(None)).and_then(|record| match record {
                Record::Rows { .. } => {
                    Err(Error::new("rows must use a block sequence".into()).with_code("BOXDB_YAML"))
                }
                record => Ok(Content::Record(record)),
            }),
        };
        match content {
            Ok(content) => parts.push(Part {
                document,
                line,
                fragment,
                content,
            }),
            Err(_) => rejected.push((
                crate::RejectedDocument {
                    document,
                    line,
                    code: "BOXDB_YAML".into(),
                    message: "invalid YAML document".into(),
                },
                fragment.to_vec(),
            )),
        }
    }
    let header = match parts.first() {
        Some(Part {
            document: 1,
            content: Content::Record(record),
            ..
        }) => Some(record),
        _ => None,
    };
    if !matches!(header, Some(Record::Database { .. })) {
        return Err(
            Error::new("recovery requires an intact readable header".into())
                .with_code("BOXDB_FORMAT"),
        );
    }
    check_header(header)?;
    if parts
        .iter()
        .skip(1)
        .any(|part| matches!(part.content, Content::Record(Record::Database { .. })))
    {
        return Err(Error::new("duplicate readable header".into()).with_code("BOXDB_FORMAT"));
    }
    let mut base = parts[0].fragment.to_vec();
    for part in &parts[1..] {
        if matches!(&part.content, Content::Record(Record::Schema { object_type, .. }) if object_type == "table")
        {
            base.extend_from_slice(part.fragment);
        }
    }
    for part in &parts {
        if matches!(&part.content, Content::Rows { table, .. } if table.starts_with(METADATA_PREFIX))
        {
            base.extend_from_slice(part.fragment);
        }
    }
    let db = decode(&base)
        .map_err(|error| error.with_context("recover readable schema and metadata"))?;
    let types = schema::load(&db)?;
    schema::install(&db, &types)?;
    db.pragma_update(None, "foreign_keys", false)?;
    db.execute_batch("BEGIN")?;
    let mut layouts = HashMap::new();
    for part in &parts {
        let Content::Rows { table, head, items } = &part.content else {
            continue;
        };
        if table.starts_with(METADATA_PREFIX) {
            continue;
        }
        let layout = match layouts.get(table) {
            Some(layout) => Rc::clone(layout),
            None => match Layout::load(&db, table, &types) {
                Ok(layout) => {
                    let layout = Rc::new(layout);
                    layouts.insert(table.clone(), Rc::clone(&layout));
                    layout
                }
                Err(error) => {
                    rejected.push(reject(
                        part.document,
                        part.line,
                        &error,
                        "row document could not be restored",
                        part.fragment,
                    ));
                    continue;
                }
            },
        };
        for (line, item) in items {
            let result = (|| -> Result<()> {
                for row in parse_items(item, &layout)? {
                    check_values(&types, table, &layout, &row)?;
                    layout.insert(&db, row)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                rejected.push(reject(
                    part.document,
                    *line,
                    &error,
                    "row could not be restored",
                    &[*head, *item].concat(),
                ));
            }
        }
    }
    let mut rejected_part = |part: &Part, error: &Error, message: &str| {
        rejected.push(reject(
            part.document,
            part.line,
            error,
            message,
            part.fragment,
        ))
    };
    for part in &parts {
        if let Content::Record(record @ Record::Sequence { .. }) = &part.content
            && let Err(error) = apply_deferred(&db, record)
        {
            rejected_part(
                part,
                &error,
                "schema or sequence document could not be restored",
            );
        }
    }
    // Each commit is atomic: a damaged commit is rejected as a whole.
    let mut resolve = |table: &str| -> Result<Option<Rc<Layout>>> {
        if let Some(layout) = layouts.get(table) {
            return Ok(Some(Rc::clone(layout)));
        }
        let empty = schema::Types::new();
        let metadata = table.starts_with(METADATA_PREFIX);
        let layout = Rc::new(Layout::load(
            &db,
            table,
            if metadata { &empty } else { &types },
        )?);
        layouts.insert(table.into(), Rc::clone(&layout));
        Ok(Some(layout))
    };
    for part in &parts {
        if !matches!(part.content, Content::Record(Record::Commit { .. })) {
            continue;
        }
        db.execute_batch("SAVEPOINT __boxdb_commit")?;
        let result = (|| -> Result<()> {
            let Record::Commit { tables, sequences } = parse_document(part.fragment, &mut resolve)?
            else {
                unreachable!()
            };
            for changes in &tables {
                for row in &changes.put {
                    let layout = changes.layout.as_ref().unwrap();
                    let table = &layout.table;
                    check_values(&types, table, layout, row)?;
                }
            }
            apply_commit(&db, tables, &sequences)
        })();
        match result {
            Ok(()) => db.execute_batch("RELEASE __boxdb_commit")?,
            Err(error) => {
                db.execute_batch("ROLLBACK TO __boxdb_commit; RELEASE __boxdb_commit")?;
                rejected_part(part, &error, "commit could not be restored");
            }
        }
    }
    // Application triggers are installed after rows, so recovery does not replay writes.
    for part in &parts {
        if let Content::Record(record @ Record::Schema { .. }) = &part.content
            && let Err(error) = apply_deferred(&db, record)
        {
            rejected_part(
                part,
                &error,
                "schema or sequence document could not be restored",
            );
        }
    }
    db.execute_batch("COMMIT")?;
    rejected.sort_by_key(|(document, _)| (document.document, document.line));
    Ok((db, rejected))
}

/// Salvage rejects a row whose typed value the column type does not accept.
fn check_values(types: &schema::Types, table: &str, layout: &Layout, row: &Row) -> Result<()> {
    for (index, value) in row.values.iter().enumerate() {
        if let Some(ty) = types.get(&(table.to_owned(), layout.columns[index].clone())) {
            schema::decode(ty, value.clone())
                .map_err(|error| error.with_code("BOXDB_INVALID_VALUE"))?;
        }
    }
    Ok(())
}

/// Indexes, views, triggers and sequences follow rows, so imports do not
/// replay application triggers.
fn apply_deferred(db: &Connection, record: &Record) -> Result<()> {
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

/// How a column's cells are written and read.
#[derive(Clone, Copy, PartialEq)]
enum Class {
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
    fn of(ty: Option<&Type>) -> Self {
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

/// A table's named columns, rowid alias and column classes.
struct Layout {
    table: String,
    rowid: Option<&'static str>,
    columns: Vec<String>,
    classes: Vec<Class>,
    insert: String,
    delete: Option<String>,
}
impl Layout {
    fn load(db: &Connection, table: &str, types: &schema::Types) -> Result<Self> {
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
        let classes = columns
            .iter()
            .map(|column| Class::of(types.get(&(table.to_owned(), column.clone()))))
            .collect();
        let names = rowid
            .into_iter()
            .chain(columns.iter().map(String::as_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = vec!["?"; columns.len() + usize::from(rowid.is_some())].join(",");
        Ok(Self {
            table: table.into(),
            rowid,
            insert: format!(
                "INSERT INTO {}({names}) VALUES({placeholders})",
                quote(table)
            ),
            delete: rowid
                .map(|alias| format!("DELETE FROM {} WHERE {}=?1", quote(table), quote(alias))),
            columns,
            classes,
        })
    }
    fn insert(&self, db: &Connection, row: Row) -> Result<()> {
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

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Database {
        version: u32,
        codec_version: i64,
        application_id: i64,
        user_version: i64,
    },
    Schema {
        object_type: String,
        name: String,
        table: String,
        sql: String,
    },
    /// Rows are written by `TablePlan`.
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    Rows {
        table: String,
        rows: Vec<Row>,
    },
    Sequence {
        table: String,
        value: i64,
    },
    /// Commits are written by `Journal`.
    #[serde(skip_serializing)]
    Commit {
        tables: Vec<TableChanges>,
        sequences: BTreeMap<String, i64>,
    },
}

/// Final images of changed rows and deleted rowids of one table.
struct TableChanges {
    /// `None` when parsed without a schema.
    layout: Option<Rc<Layout>>,
    put: Vec<Row>,
    delete: Vec<i64>,
}

/// Remove every changed row before inserting final images, so values swapped
/// between rows do not conflict. Import runs without foreign-key actions.
fn apply_commit(
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

/// Cells in the layout's column order; empty when parsed without a schema.
struct Row {
    rowid: Option<i64>,
    values: Vec<SqlValue>,
}

/// Decode fields directly; rows need the table's layout, so `table` must
/// precede `rows`.
struct DocumentSeed<'a, 'b> {
    resolve: &'a mut Resolve<'b>,
    failure: &'a mut Option<Error>,
}
impl<'de> DeserializeSeed<'de> for DocumentSeed<'_, '_> {
    type Value = Record;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Record, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for DocumentSeed<'_, '_> {
    type Value = Record;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a BoxDB readable document")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Record, M::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Kind,
            Version,
            CodecVersion,
            ApplicationId,
            UserVersion,
            ObjectType,
            Name,
            Table,
            Sql,
            Rows,
            Value,
            Tables,
            Sequences,
        }
        #[derive(Deserialize, Clone, Copy, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum DocumentKind {
            Database,
            Schema,
            Rows,
            Sequence,
            Commit,
        }
        let mut kind = None;
        let mut version = None;
        let mut codec_version = None;
        let mut application_id = None;
        let mut user_version = None;
        let mut object_type = None;
        let mut name = None;
        let mut table: Option<String> = None;
        let mut sql = None;
        let mut rows = None;
        let mut value = None;
        let mut tables = None;
        let mut sequences = None;
        macro_rules! set {
            ($slot:ident) => {{
                if $slot.is_some() {
                    return Err(de::Error::duplicate_field(stringify!($slot)));
                }
                $slot = Some(map.next_value()?);
            }};
        }
        while let Some(field) = map.next_key::<Field>()? {
            match field {
                Field::Kind => set!(kind),
                Field::Version => set!(version),
                Field::CodecVersion => set!(codec_version),
                Field::ApplicationId => set!(application_id),
                Field::UserVersion => set!(user_version),
                Field::ObjectType => set!(object_type),
                Field::Name => set!(name),
                Field::Table => set!(table),
                Field::Sql => set!(sql),
                Field::Value => set!(value),
                Field::Sequences => set!(sequences),
                Field::Tables => {
                    if tables.is_some() {
                        return Err(de::Error::duplicate_field("tables"));
                    }
                    if kind != Some(DocumentKind::Commit) {
                        return Err(de::Error::custom("tables must follow kind: commit"));
                    }
                    tables = Some(map.next_value_seed(TablesSeed {
                        resolve: &mut *self.resolve,
                        failure: &mut *self.failure,
                    })?);
                }
                Field::Rows => {
                    if rows.is_some() {
                        return Err(de::Error::duplicate_field("rows"));
                    }
                    let (Some(DocumentKind::Rows), Some(table)) = (kind, &table) else {
                        return Err(de::Error::custom("rows must follow kind: rows and table"));
                    };
                    let layout = match (self.resolve)(table) {
                        Ok(layout) => layout,
                        Err(error) => {
                            let message = error.to_string();
                            *self.failure = Some(error);
                            return Err(de::Error::custom(message));
                        }
                    };
                    rows = Some(map.next_value_seed(RowsSeed(layout.as_deref()))?);
                }
            }
        }
        let kind = kind.ok_or_else(|| de::Error::missing_field("kind"))?;
        let allowed: &[&str] = match kind {
            DocumentKind::Database => {
                &["version", "codec_version", "application_id", "user_version"]
            }
            DocumentKind::Schema => &["object_type", "name", "table", "sql"],
            DocumentKind::Rows => &["table", "rows"],
            DocumentKind::Sequence => &["table", "value"],
            DocumentKind::Commit => &["tables", "sequences"],
        };
        for (field, exists) in [
            ("version", version.is_some()),
            ("codec_version", codec_version.is_some()),
            ("application_id", application_id.is_some()),
            ("user_version", user_version.is_some()),
            ("object_type", object_type.is_some()),
            ("name", name.is_some()),
            ("table", table.is_some()),
            ("sql", sql.is_some()),
            ("rows", rows.is_some()),
            ("value", value.is_some()),
            ("tables", tables.is_some()),
            ("sequences", sequences.is_some()),
        ] {
            if exists && !allowed.contains(&field) {
                return Err(de::Error::unknown_field(field, allowed));
            }
        }
        fn required<T, E: de::Error>(
            field: Option<T>,
            name: &'static str,
        ) -> std::result::Result<T, E> {
            field.ok_or_else(|| E::missing_field(name))
        }
        Ok(match kind {
            DocumentKind::Database => Record::Database {
                version: required(version, "version")?,
                codec_version: required(codec_version, "codec_version")?,
                application_id: required(application_id, "application_id")?,
                user_version: required(user_version, "user_version")?,
            },
            DocumentKind::Schema => Record::Schema {
                object_type: required(object_type, "object_type")?,
                name: required(name, "name")?,
                table: required(table, "table")?,
                sql: required(sql, "sql")?,
            },
            DocumentKind::Rows => Record::Rows {
                table: required(table, "table")?,
                rows: required(rows, "rows")?,
            },
            DocumentKind::Sequence => Record::Sequence {
                table: required(table, "table")?,
                value: required(value, "value")?,
            },
            DocumentKind::Commit => Record::Commit {
                tables: tables.unwrap_or_default(),
                sequences: sequences.unwrap_or_default(),
            },
        })
    }
}

/// A commit's `tables` mapping; each table's layout is resolved by name.
struct TablesSeed<'a, 'b> {
    resolve: &'a mut Resolve<'b>,
    failure: &'a mut Option<Error>,
}
impl<'de> DeserializeSeed<'de> for TablesSeed<'_, '_> {
    type Value = Vec<TableChanges>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Vec<TableChanges>, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for TablesSeed<'_, '_> {
    type Value = Vec<TableChanges>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a mapping of table names to changes")
    }
    fn visit_map<M: MapAccess<'de>>(
        self,
        mut map: M,
    ) -> std::result::Result<Vec<TableChanges>, M::Error> {
        let mut seen = std::collections::HashSet::new();
        let mut tables = vec![];
        while let Some(table) = map.next_key::<String>()? {
            if !seen.insert(table.clone()) {
                return Err(de::Error::custom(format!("duplicate table: {table}")));
            }
            let layout = match (self.resolve)(&table) {
                Ok(layout) => layout,
                Err(error) => {
                    let message = error.to_string();
                    *self.failure = Some(error);
                    return Err(de::Error::custom(message));
                }
            };
            tables.push(map.next_value_seed(ChangesSeed(layout))?);
        }
        Ok(tables)
    }
}

struct ChangesSeed(Option<Rc<Layout>>);
impl<'de> DeserializeSeed<'de> for ChangesSeed {
    type Value = TableChanges;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<TableChanges, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for ChangesSeed {
    type Value = TableChanges;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("put rows and deleted rowids")
    }
    fn visit_map<M: MapAccess<'de>>(
        self,
        mut map: M,
    ) -> std::result::Result<TableChanges, M::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Put,
            Delete,
        }
        let mut put = None;
        let mut delete = None;
        while let Some(field) = map.next_key::<Field>()? {
            match field {
                Field::Put if put.is_none() => {
                    put = Some(map.next_value_seed(RowsSeed(self.0.as_deref()))?)
                }
                Field::Delete if delete.is_none() => delete = Some(map.next_value()?),
                Field::Put => return Err(de::Error::duplicate_field("put")),
                Field::Delete => return Err(de::Error::duplicate_field("delete")),
            }
        }
        Ok(TableChanges {
            layout: self.0,
            put: put.unwrap_or_default(),
            delete: delete.unwrap_or_default(),
        })
    }
}

struct RowsSeed<'a>(Option<&'a Layout>);
impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
    type Value = Vec<Row>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Vec<Row>, D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for RowsSeed<'_> {
    type Value = Vec<Row>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a sequence of rows")
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> std::result::Result<Vec<Row>, S::Error> {
        let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(row) = seq.next_element_seed(RowSeed(self.0))? {
            rows.push(row);
        }
        Ok(rows)
    }
}

struct RowSeed<'a>(Option<&'a Layout>);
impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = Row;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Row, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for RowSeed<'_> {
    type Value = Row;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a mapping of column names to values")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Row, M::Error> {
        let Some(layout) = self.0 else {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(Row {
                rowid: None,
                values: vec![],
            });
        };
        let mut values = vec![SqlValue::Null; layout.columns.len()];
        let mut seen = vec![false; layout.columns.len()];
        let mut rowid = None;
        let mut rowid_seen = false;
        while let Some(key) = map.next_key_seed(KeySeed(layout))? {
            let Some(index) = key else {
                if std::mem::replace(&mut rowid_seen, true) {
                    return Err(de::Error::duplicate_field("rowid"));
                }
                rowid = map.next_value::<Option<i64>>()?;
                continue;
            };
            if std::mem::replace(&mut seen[index], true) {
                return Err(de::Error::custom(format!(
                    "duplicate column: {}",
                    layout.columns[index]
                )));
            }
            values[index] = match layout.classes[index] {
                Class::Plain | Class::Boolean => map.next_value::<Cell>()?.0,
                Class::Native(kind) => map.next_value_seed(NativeSeed(kind))?,
                Class::Bytea => map.next_value_seed(ByteaSeed)?,
            };
        }
        Ok(Row { rowid, values })
    }
}

/// A row key: `None` for the rowid alias, otherwise a column index.
struct KeySeed<'a>(&'a Layout);
impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Option<usize>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Option<usize>, D::Error> {
        deserializer.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = Option<usize>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a column name")
    }
    fn visit_str<E: de::Error>(self, key: &str) -> std::result::Result<Option<usize>, E> {
        if self.0.rowid == Some(key) {
            return Ok(None);
        }
        self.0
            .columns
            .iter()
            .position(|column| column == key)
            .map(Some)
            .ok_or_else(|| E::custom(format!("unknown column: {key}")))
    }
}

/// Typed text is read verbatim, so `12.50` is not rounded through a float.
struct NativeSeed(Kind);
impl<'de> DeserializeSeed<'de> for NativeSeed {
    type Value = SqlValue;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<SqlValue, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(SqlValue::Null);
        };
        libsqlite3_sys::native::encode(self.0, &text)
            .map(SqlValue::Blob)
            .map_err(|error| de::Error::custom(format!("{}: {text}", error.0)))
    }
}

struct ByteaSeed;
impl<'de> DeserializeSeed<'de> for ByteaSeed {
    type Value = SqlValue;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<SqlValue, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(SqlValue::Null);
        };
        BASE64
            .decode(text)
            .map(|bytes| SqlValue::Blob(crate::decimal::encode_blob(&bytes)))
            .map_err(de::Error::custom)
    }
}

/// Untyped SQLite cells have natural YAML scalars. BLOBs and native values use
/// explicit mappings so text, byte arrays and numbers remain distinguishable.
struct Cell(SqlValue);
impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CellVisitor;
        impl<'de> Visitor<'de> for CellVisitor {
            type Value = Cell;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a SQLite scalar or a {blob: base64} mapping")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Null))
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Null))
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(n)))
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> std::result::Result<Cell, E> {
                i64::try_from(n)
                    .map(|n| Cell(SqlValue::Integer(n)))
                    .map_err(E::custom)
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> std::result::Result<Cell, E> {
                if n.is_finite() {
                    Ok(Cell(SqlValue::Real(n)))
                } else {
                    Err(E::custom("non-finite SQLite value"))
                }
            }
            fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(i64::from(b))))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s.into())))
            }
            fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s)))
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Cell, M::Error> {
                let Some(name) = map.next_key::<String>()? else {
                    return Err(de::Error::invalid_length(0, &self));
                };
                let text = map.next_value::<String>()?;
                if map.next_key::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(2, &self));
                }
                let kind = match name.as_str() {
                    "blob" => {
                        return BASE64
                            .decode(text)
                            .map(|bytes| Cell(SqlValue::Blob(crate::decimal::encode_blob(&bytes))))
                            .map_err(de::Error::custom);
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
                    _ => return Err(de::Error::unknown_field(&name, NATIVE_NAMES)),
                };
                libsqlite3_sys::native::encode(kind, &text)
                    .map(|b| Cell(SqlValue::Blob(b)))
                    .map_err(|e| de::Error::custom(e.0))
            }
        }
        deserializer.deserialize_any(CellVisitor)
    }
}

const NATIVE_NAMES: &[&str] = &[
    "blob", "decimal", "uuid", "uint8", "uint16", "uint32", "uint64", "instant", "date", "time",
    "datetime", "interval",
];

fn native_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Decimal => "decimal",
        Kind::Uuid => "uuid",
        Kind::Uint8 => "uint8",
        Kind::Uint16 => "uint16",
        Kind::Uint32 => "uint32",
        Kind::Uint64 => "uint64",
        Kind::Instant => "instant",
        Kind::Date => "date",
        Kind::Time => "time",
        Kind::DateTime => "datetime",
        Kind::Interval => "interval",
    }
}

/// The native type and canonical text of a tagged BLOB.
fn native_text(bytes: &[u8]) -> Option<(Kind, std::borrow::Cow<'_, str>)> {
    if let Some(text) = crate::decimal::decode(bytes) {
        return Some((Kind::Decimal, text.into()));
    }
    libsqlite3_sys::native::decode(bytes).map(|(kind, text)| (kind, text.into()))
}

fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
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
/// Cached table layout; key quoting is done once per schema.
struct TablePlan {
    table: String,
    layout: Layout,
    select: String,
    lookup: Option<String>,
    head: Vec<u8>,
    /// Column index and YAML key, ordered by column name.
    keys: Vec<(usize, String)>,
}
impl TablePlan {
    fn new(db: &Connection, name: &str, types: &schema::Types) -> Result<Self> {
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
    fn mismatch(&self, index: usize) -> Error {
        Error::new(format!(
            "readable export found a value of another type in typed column {}.{}",
            self.table, self.layout.columns[index]
        ))
        .with_code("BOXDB_INVALID_VALUE")
    }
    /// Inline text of a cell; multi-line text returns `None`.
    fn inline(&self, index: usize, value: ValueRef<'_>) -> Result<Option<String>> {
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
    fn write_row(&self, row: &rusqlite::Row<'_>, writer: &mut impl std::io::Write) -> Result<()> {
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
// A leading letter rules out YAML numbers, dates and special floats; the
// remaining characters cannot start a comment, flow item or mapping value.
// Other text is double-quoted or, when it spans lines, a block scalar.
fn plain_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && bytes.len() <= 80
        && !bytes.ends_with(b" ")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_- .@/+".contains(b))
        && !["null", "true", "false", "yes", "no", "on", "off", "y", "n"]
            .iter()
            .any(|s| text.eq_ignore_ascii_case(s))
}
/// Typed text is read as a string, so numbers and dates may stay unquoted.
/// Only structure and null spellings need quotes.
fn bare_or_quoted(text: &str) -> String {
    let bytes = text.as_bytes();
    let bare = !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"+-./:=_ ".contains(b))
        && !matches!(bytes[0], b' ' | b':')
        && !matches!(bytes[bytes.len() - 1], b' ' | b':')
        && !text.starts_with("- ")
        && text != "-"
        && !text.contains(": ")
        && !text.eq_ignore_ascii_case("null");
    if bare { text.into() } else { quoted(text) }
}
/// A single-line YAML double-quoted scalar. Escapes cover YAML's
/// non-printable characters and line breaks.
fn quoted(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if c < ' '
                || ('\u{7f}'..='\u{9f}').contains(&c)
                || matches!(
                    c,
                    '\u{2028}' | '\u{2029}' | '\u{feff}' | '\u{fffe}' | '\u{ffff}'
                ) =>
            {
                write!(out, "\\u{:04X}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn write_scalar(writer: &mut impl std::io::Write, yaml: &str) -> Result<()> {
    writer.write_all(b" ")?;
    for (i, line) in yaml.split_inclusive('\n').enumerate() {
        if i > 0 {
            writer.write_all(b"  ")?;
        }
        writer.write_all(line.as_bytes())?;
    }
    Ok(())
}
fn write_document(writer: &mut impl std::io::Write, record: &Record) -> Result<()> {
    writer.write_all(b"---\n")?;
    serde_saphyr::to_io_writer(writer, record)?;
    Ok(())
}
fn header_record(db: &Connection) -> Result<Record> {
    Ok(Record::Database {
        version: VERSION,
        codec_version: libsqlite3_sys::wire::VERSION,
        application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
        user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
    })
}
fn schema_records(db: &Connection) -> Result<Vec<Record>> {
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
fn sequence_records(db: &Connection) -> Result<Vec<Record>> {
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
struct LimitedWriter<W> {
    inner: W,
    bytes: usize,
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

/// Stream sections in order. Typed rows need the schema and BoxDB metadata
/// that precede them; commits follow the snapshot.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Section {
    Schema,
    Metadata,
    Rows,
    Sequences,
    Commits,
}
struct Import<'a> {
    db: &'a Connection,
    section: Section,
    types: Option<schema::Types>,
    layouts: HashMap<String, Rc<Layout>>,
}
impl Import<'_> {
    fn enter(&mut self, section: Section) -> Result<()> {
        if section < self.section {
            return Err(out_of_order());
        }
        self.section = section;
        Ok(())
    }
    fn rows_section(table: &str) -> Section {
        if table.starts_with(METADATA_PREFIX) {
            Section::Metadata
        } else {
            Section::Rows
        }
    }
    fn layout(&mut self, table: &str) -> Result<Rc<Layout>> {
        let metadata = table.starts_with(METADATA_PREFIX);
        // Commit tables resolve before the commit is entered; rows documents
        // are checked again once parsed.
        if self.section < Section::Sequences {
            self.enter(Self::rows_section(table))?;
        }
        if !metadata && self.types.is_none() {
            self.types = Some(schema::load(self.db)?);
        }
        if let Some(layout) = self.layouts.get(table) {
            return Ok(Rc::clone(layout));
        }
        let empty = schema::Types::new();
        let types = if metadata {
            &empty
        } else {
            self.types.as_ref().unwrap()
        };
        let layout = Rc::new(Layout::load(self.db, table, types)?);
        self.layouts.insert(table.into(), Rc::clone(&layout));
        Ok(layout)
    }
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
        db: &db,
        section: Section::Schema,
        types: None,
        layouts: HashMap::new(),
    };
    let mut deferred = vec![];
    for (_, fragment) in documents {
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
                let layout = Rc::clone(&import.layouts[&table]);
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
    let types = match import.types.take() {
        Some(types) => types,
        None => schema::load(&db)?,
    };
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
