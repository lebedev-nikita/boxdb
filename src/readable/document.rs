//! Documents of a readable stream and their schema-aware deserialization.
use super::*;

pub(super) fn options(len: usize) -> serde_saphyr::Options {
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
pub(super) type Resolve<'a> = dyn FnMut(&str) -> Result<Option<Arc<Layout>>> + 'a;

/// Parse one document. Schema errors raised by `resolve` are kept in `failure`
/// so their codes survive the YAML deserializer.
pub(super) fn parse(
    bytes: &[u8],
    resolve: &mut Resolve<'_>,
    failure: &mut Option<Error>,
) -> std::result::Result<Record, serde_saphyr::DeserializeError> {
    serde_saphyr::with_deserializer_from_slice_with_options(bytes, options(bytes.len()), |de| {
        DocumentSeed { resolve, failure }.deserialize(de)
    })
}

pub(super) fn parse_document(bytes: &[u8], resolve: &mut Resolve<'_>) -> Result<Record> {
    let mut failure = None;
    parse(bytes, resolve, &mut failure).map_err(|error| failure.take().unwrap_or(error.into()))
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Record {
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
pub(super) struct TableChanges {
    /// `None` when parsed without a schema.
    pub(super) layout: Option<Arc<Layout>>,
    pub(super) put: Vec<Row>,
    pub(super) delete: Vec<i64>,
}

/// Cells in the layout's column order; empty when parsed without a schema.
pub(super) struct Row {
    pub(super) rowid: Option<i64>,
    pub(super) values: Vec<SqlValue>,
}

/// Decode fields directly; rows need the table's layout, so `table` must
/// precede `rows`.
pub(super) struct DocumentSeed<'a, 'b> {
    pub(super) resolve: &'a mut Resolve<'b>,
    pub(super) failure: &'a mut Option<Error>,
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
pub(super) struct TablesSeed<'a, 'b> {
    pub(super) resolve: &'a mut Resolve<'b>,
    pub(super) failure: &'a mut Option<Error>,
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

pub(super) struct ChangesSeed(pub(super) Option<Arc<Layout>>);
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

pub(super) struct RowsSeed<'a>(pub(super) Option<&'a Layout>);
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

pub(super) struct RowSeed<'a>(pub(super) Option<&'a Layout>);
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
pub(super) struct KeySeed<'a>(pub(super) &'a Layout);
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

/// Where a rows section starts within a document fragment.
pub(super) enum RowsSection {
    Block { head: usize, items: usize },
    Inline,
}

pub(super) fn rows_section(fragment: &[u8]) -> Option<RowsSection> {
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
pub(super) fn row_items(section: &[u8], first_line: usize) -> Vec<(usize, &[u8])> {
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

pub(super) fn parse_items(item: &[u8], layout: &Layout) -> Result<Vec<Row>> {
    Ok(serde_saphyr::with_deserializer_from_slice_with_options(
        item,
        options(item.len()),
        |de| RowsSeed(Some(layout)).deserialize(de),
    )?)
}

/// The table of a rows document from the lines before `rows:`.
pub(super) fn rows_head(head: &[u8]) -> Result<String> {
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
    Ok(serde_saphyr::from_slice_with_options::<Head>(head, options(head.len()))?.table)
}
