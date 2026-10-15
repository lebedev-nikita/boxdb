//! A standalone, deterministic YAML stream of SQLite schemas and named rows.
use crate::{
    Error, Result,
    schema::{self, quote},
    storage::MAX_SNAPSHOT,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeMap,
};
use std::{collections::BTreeMap, fmt};
mod cache;
pub(crate) use cache::SnapshotCache;

/// Native SQLite cells have natural YAML scalars. BLOBs use an explicit hex
/// mapping so text, byte arrays and numbers remain distinguishable on import.
struct Cell(SqlValue);
impl Serialize for Cell {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match &self.0 {
            SqlValue::Null => serializer.serialize_none(),
            SqlValue::Integer(n) => serializer.serialize_i64(*n),
            SqlValue::Real(n) if n.is_finite() => serializer.serialize_f64(*n),
            SqlValue::Real(_) => Err(serde::ser::Error::custom(
                "readable storage cannot represent non-finite SQLite values",
            )),
            SqlValue::Text(s) => serializer.serialize_str(s),
            SqlValue::Blob(bytes) => {
                let mut map = serializer.serialize_map(Some(1))?;
                if let Some(text) = crate::decimal::decode(bytes) {
                    map.serialize_entry("decimal", text)?;
                } else if let Some((kind, text)) = libsqlite3_sys::native::decode(bytes) {
                    let name = match kind {
                        libsqlite3_sys::native::Kind::Uuid => "uuid",
                        libsqlite3_sys::native::Kind::Uint8 => "uint8",
                        libsqlite3_sys::native::Kind::Uint16 => "uint16",
                        libsqlite3_sys::native::Kind::Uint32 => "uint32",
                        libsqlite3_sys::native::Kind::Uint64 => "uint64",
                        libsqlite3_sys::native::Kind::Instant => "instant",
                        libsqlite3_sys::native::Kind::Date => "date",
                        libsqlite3_sys::native::Kind::DateTime => "datetime",
                        libsqlite3_sys::native::Kind::Interval => "interval",
                        _ => unreachable!(),
                    };
                    map.serialize_entry(name, &text)?;
                } else {
                    map.serialize_entry(
                        "blob",
                        &hex::encode(libsqlite3_sys::wire::blob_payload(bytes)),
                    )?;
                }
                map.end()
            }
        }
    }
}
impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CellVisitor;
        impl<'de> Visitor<'de> for CellVisitor {
            type Value = Cell;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a SQLite scalar or a {blob: hex} mapping")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Cell, E> {
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
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> std::result::Result<Cell, M::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "lowercase")]
                enum WireCell {
                    Blob(String),
                    Decimal(String),
                    Uuid(String),
                    Uint8(String),
                    Uint16(String),
                    Uint32(String),
                    Uint64(String),
                    Instant(String),
                    Date(String),
                    DateTime(String),
                    Interval(String),
                }
                let cell = WireCell::deserialize(MapAccessDeserializer::new(map))?;
                use libsqlite3_sys::native::Kind;
                let (kind, text) = match cell {
                    WireCell::Blob(blob) => {
                        return hex::decode(blob)
                            .map(|bytes| Cell(SqlValue::Blob(crate::decimal::encode_blob(&bytes))))
                            .map_err(de::Error::custom);
                    }
                    WireCell::Decimal(text) => {
                        crate::decimal::parse(&text).map_err(de::Error::custom)?;
                        return Ok(Cell(SqlValue::Blob(
                            [crate::decimal::PREFIX, text.as_bytes()].concat(),
                        )));
                    }
                    WireCell::Uuid(s) => (Kind::Uuid, s),
                    WireCell::Uint8(s) => (Kind::Uint8, s),
                    WireCell::Uint16(s) => (Kind::Uint16, s),
                    WireCell::Uint32(s) => (Kind::Uint32, s),
                    WireCell::Uint64(s) => (Kind::Uint64, s),
                    WireCell::Instant(s) => (Kind::Instant, s),
                    WireCell::Date(s) => (Kind::Date, s),
                    WireCell::DateTime(s) => (Kind::DateTime, s),
                    WireCell::Interval(s) => (Kind::Interval, s),
                };
                libsqlite3_sys::native::encode(kind, &text)
                    .map(|b| Cell(SqlValue::Blob(b)))
                    .map_err(|e| de::Error::custom(e.0))
            }
        }
        deserializer.deserialize_any(CellVisitor)
    }
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
    Row {
        table: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rowid: Option<i64>,
        values: BTreeMap<String, Cell>,
    },
    Sequence {
        table: String,
        value: i64,
    },
}
// Decode fields directly instead of Serde's internally tagged enum buffer,
// which otherwise allocates an intermediate tree for every row document.
impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Database,
            Schema,
            Row,
            Sequence,
        }
        fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
            d: D,
        ) -> std::result::Result<Option<T>, D::Error> {
            T::deserialize(d).map(Some)
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            kind: Kind,
            #[serde(default, deserialize_with = "present")]
            version: Option<u32>,
            #[serde(default, deserialize_with = "present")]
            codec_version: Option<i64>,
            #[serde(default, deserialize_with = "present")]
            application_id: Option<i64>,
            #[serde(default, deserialize_with = "present")]
            user_version: Option<i64>,
            #[serde(default, deserialize_with = "present")]
            object_type: Option<String>,
            #[serde(default, deserialize_with = "present")]
            name: Option<String>,
            #[serde(default, deserialize_with = "present")]
            table: Option<String>,
            #[serde(default, deserialize_with = "present")]
            sql: Option<String>,
            #[serde(default, deserialize_with = "present")]
            rowid: Option<Option<i64>>,
            #[serde(default, deserialize_with = "present")]
            values: Option<BTreeMap<String, Cell>>,
            #[serde(default, deserialize_with = "present")]
            value: Option<i64>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let allowed: &[&str] = match fields.kind {
            Kind::Database => &["version", "codec_version", "application_id", "user_version"],
            Kind::Schema => &["object_type", "name", "table", "sql"],
            Kind::Row => &["table", "rowid", "values"],
            Kind::Sequence => &["table", "value"],
        };
        for (name, exists) in [
            ("version", fields.version.is_some()),
            ("codec_version", fields.codec_version.is_some()),
            ("application_id", fields.application_id.is_some()),
            ("user_version", fields.user_version.is_some()),
            ("object_type", fields.object_type.is_some()),
            ("name", fields.name.is_some()),
            ("table", fields.table.is_some()),
            ("sql", fields.sql.is_some()),
            ("rowid", fields.rowid.is_some()),
            ("values", fields.values.is_some()),
            ("value", fields.value.is_some()),
        ] {
            if exists && !allowed.contains(&name) {
                return Err(de::Error::unknown_field(name, allowed));
            }
        }
        fn required<T, E: de::Error>(
            field: Option<T>,
            name: &'static str,
        ) -> std::result::Result<T, E> {
            field.ok_or_else(|| E::missing_field(name))
        }
        Ok(match fields.kind {
            Kind::Database => Self::Database {
                version: required(fields.version, "version")?,
                codec_version: required(fields.codec_version, "codec_version")?,
                application_id: required(fields.application_id, "application_id")?,
                user_version: required(fields.user_version, "user_version")?,
            },
            Kind::Schema => Self::Schema {
                object_type: required(fields.object_type, "object_type")?,
                name: required(fields.name, "name")?,
                table: required(fields.table, "table")?,
                sql: required(fields.sql, "sql")?,
            },
            Kind::Row => Self::Row {
                table: required(fields.table, "table")?,
                rowid: fields.rowid.flatten(),
                values: required(fields.values, "values")?,
            },
            Kind::Sequence => Self::Sequence {
                table: required(fields.table, "table")?,
                value: required(fields.value, "value")?,
            },
        })
    }
}
fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT name FROM pragma_table_xinfo(?1,'main') WHERE hidden=0 ORDER BY cid")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn rowid(db: &Connection, table: &str) -> Result<Option<&'static str>> {
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
    Ok(["_rowid_", "rowid", "oid"]
        .into_iter()
        .find(|name| !columns.iter().any(|c| c.eq_ignore_ascii_case(name))))
}
/// Cached table layout; scalar quoting for column names is done once per schema.
struct TablePlan {
    rowid: Option<&'static str>,
    select: String,
    lookup: Option<String>,
    prefix: Vec<u8>,
    keys: Vec<(usize, Vec<u8>)>,
}
impl TablePlan {
    fn new(db: &Connection, name: &str) -> Result<Self> {
        let columns = columns(db, name)?;
        let rowid = rowid(db, name)?;
        let selected = columns
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
        let mut prefix = b"---\n".to_vec();
        serde_saphyr::to_io_writer(
            &mut prefix,
            &Head {
                kind: "row",
                table: name,
            },
        )?;
        let mut ordered: Vec<usize> = (0..columns.len()).collect();
        ordered.sort_by(|a, b| columns[*a].cmp(&columns[*b]));
        let mut keys = vec![];
        for index in ordered {
            let yaml = serde_saphyr::to_string(&BTreeMap::from([(&columns[index], None::<()>)]))?;
            let key = yaml
                .strip_suffix(" null\n")
                .ok_or_else(|| Error::new("invalid YAML key serialization".into()))?;
            let mut indented = vec![];
            for line in key.split_inclusive('\n') {
                indented.extend_from_slice(b"  ");
                indented.extend_from_slice(line.as_bytes());
            }
            keys.push((index, indented));
        }
        Ok(Self {
            rowid,
            select,
            lookup,
            prefix,
            keys,
        })
    }
    fn write_row(&self, row: &rusqlite::Row<'_>, writer: &mut impl std::io::Write) -> Result<()> {
        use rusqlite::types::ValueRef;
        writer.write_all(&self.prefix)?;
        if self.rowid.is_some() {
            writeln!(writer, "rowid: {}", row.get::<_, i64>(0)?)?;
        }
        writer.write_all(b"values:\n")?;
        let offset = usize::from(self.rowid.is_some());
        for (index, key) in &self.keys {
            writer.write_all(key)?;
            match row.get_ref(index + offset)? {
                ValueRef::Null => writer.write_all(b" null\n")?,
                ValueRef::Integer(n) => writeln!(writer, " {n}")?,
                ValueRef::Real(n) if n.is_finite() => {
                    write_scalar(writer, &serde_saphyr::to_string(&n)?)?
                }
                ValueRef::Real(_) => {
                    return Err(Error::new(
                        "readable storage cannot represent non-finite SQLite values".into(),
                    ));
                }
                ValueRef::Text(bytes) => {
                    let text = std::str::from_utf8(bytes).map_err(Error::validation)?;
                    if plain_text(text) {
                        writer.write_all(b" ")?;
                        writer.write_all(bytes)?;
                        writer.write_all(b"\n")?;
                    } else {
                        write_scalar(writer, &serde_saphyr::to_string(&text)?)?;
                    }
                }
                ValueRef::Blob(bytes) => {
                    if crate::decimal::decode(bytes).is_some()
                        || libsqlite3_sys::native::decode(bytes).is_some()
                    {
                        let yaml = serde_saphyr::to_string(&Cell(SqlValue::Blob(bytes.to_vec())))?;
                        writer.write_all(b"\n")?;
                        for line in yaml.split_inclusive('\n') {
                            writer.write_all(b"    ")?;
                            writer.write_all(line.as_bytes())?;
                        }
                        continue;
                    }
                    writer.write_all(b"\n    blob: '")?;
                    let mut hex = [0u8; 8192];
                    for chunk in libsqlite3_sys::wire::blob_payload(bytes).chunks(4096) {
                        let output = &mut hex[..chunk.len() * 2];
                        hex::encode_to_slice(chunk, output)
                            .expect("hex buffer has twice the input length");
                        writer.write_all(output)?;
                    }
                    writer.write_all(b"'\n")?;
                }
            }
        }
        Ok(())
    }
}
// The common ASCII case is unambiguous in YAML 1.1 and 1.2. Everything else
// (including Unicode, dates, numeric text and blocks) uses the YAML serializer.
fn plain_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && bytes.len() <= 80
        && !bytes.ends_with(b" ")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b' '))
        && !["null", "true", "false", "yes", "no", "on", "off", "y", "n"]
            .iter()
            .any(|s| text.eq_ignore_ascii_case(s))
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
fn schema_records(db: &Connection) -> Result<Vec<Record>> {
    let mut records = vec![Record::Database {
        version: 3,
        codec_version: libsqlite3_sys::wire::VERSION,
        application_id: db.query_row("PRAGMA application_id", [], |r| r.get(0))?,
        user_version: db.query_row("PRAGMA user_version", [], |r| r.get(0))?,
    }];
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
pub(crate) fn decode(bytes: &[u8]) -> Result<Connection> {
    let records: Vec<Record> = serde_saphyr::from_slice_multiple_with_options(
        bytes,
        serde_saphyr::options! {
            emit_comments: false,
            strict_booleans: true,
            reject_unsupported_tags: true,
            // A database stream has one document per row, unlike a typical
            // configuration file. Tie structural budgets to the bounded input.
            budget: serde_saphyr::budget! {
                max_documents: bytes.len().saturating_add(1),
                max_nodes: bytes.len().saturating_add(16),
                max_events: bytes.len().saturating_mul(2).saturating_add(16),
                max_total_scalar_bytes: MAX_SNAPSHOT,
                max_total_comment_bytes: MAX_SNAPSHOT,
            },
        },
    )?;
    let Some(Record::Database {
        version: 3,
        codec_version,
        application_id,
        user_version,
    }) = records.first()
    else {
        return Err(Error::new("invalid readable format version".into()));
    };
    if *codec_version != libsqlite3_sys::wire::VERSION {
        return Err(Error::new("unsupported BoxDB BLOB codec version".into()));
    }
    if records
        .iter()
        .skip(1)
        .any(|r| matches!(r, Record::Database { .. }))
    {
        return Err(Error::new("duplicate readable header".into()));
    }
    let db = Connection::open_in_memory()?;
    crate::extensions::register(&db)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.execute_batch("BEGIN; PRAGMA defer_foreign_keys=ON")?;
    for record in &records {
        if let Record::Schema {
            object_type,
            name,
            sql,
            ..
        } = record
            && object_type == "table"
        {
            if !sql
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE TABLE")
            {
                return Err(Error::new("invalid table declaration".into()));
            }
            db.prepare(sql)?.execute([])?;
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [name],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error::new("mismatched schema record".into()));
            }
        }
    }
    struct InsertPlan {
        columns: Vec<String>,
        rowid: Option<&'static str>,
        sql: String,
    }
    let mut inserts = BTreeMap::<&str, InsertPlan>::new();
    for record in &records {
        if let Record::Row {
            table,
            rowid: id,
            values,
        } = record
        {
            if !inserts.contains_key(table.as_str()) {
                let columns = columns(&db, table)?;
                let alias = rowid(&db, table)?;
                let names = alias
                    .into_iter()
                    .chain(columns.iter().map(String::as_str))
                    .map(quote)
                    .collect::<Vec<_>>()
                    .join(",");
                let placeholders =
                    vec!["?"; columns.len() + usize::from(alias.is_some())].join(",");
                inserts.insert(
                    table,
                    InsertPlan {
                        columns,
                        rowid: alias,
                        sql: format!(
                            "INSERT INTO {}({names}) VALUES({placeholders})",
                            quote(table)
                        ),
                    },
                );
            }
            let plan = &inserts[table.as_str()];
            if plan.columns.len() != values.len()
                || plan.columns.iter().any(|name| !values.contains_key(name))
            {
                return Err(Error::new(format!(
                    "row columns do not match table: {table}"
                )));
            }
            if id.is_some() && plan.rowid.is_none() {
                return Err(Error::new("unexpected rowid".into()));
            }
            let rowid = id.map(SqlValue::Integer).unwrap_or(SqlValue::Null);
            let parameters = plan
                .rowid
                .map(|_| &rowid)
                .into_iter()
                .chain(plan.columns.iter().map(|name| &values[name].0))
                .cloned()
                .map(crate::decimal::WireValue::new);
            db.prepare_cached(&plan.sql)?
                .execute(rusqlite::params_from_iter(parameters))?;
        }
    }
    // Normalize imported values before recreating indexes and user triggers.
    // Import restores data, so it must not replay application update effects.
    let types = schema::load(&db)?;
    schema::install(&db, &types)?;
    schema::normalize_arrays(&db, &types)?;
    schema::uninstall(&db)?;
    for record in &records {
        match record {
            Record::Schema {
                object_type,
                sql,
                name,
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
            _ => (),
        }
    }
    db.pragma_update(None, "application_id", application_id)?;
    db.pragma_update(None, "user_version", user_version)?;
    db.execute_batch("COMMIT")?;
    let types = schema::load(&db)?;
    schema::validate_all(&db, &types)?;
    Ok(db)
}
