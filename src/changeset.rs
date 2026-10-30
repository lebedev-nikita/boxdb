//! SQLite's changeset wire format, encoded and decoded in Rust so recorded
//! changes keep BoxDB's tagged storage cells. The byte layout follows the
//! session extension's documented CHANGESET and PATCHSET formats.
use crate::{Error, Result, Value, schema};
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Insert,
    Update,
    Delete,
}
impl Operation {
    fn code(self) -> u8 {
        match self {
            Self::Insert => 0x12,
            Self::Update => 0x17,
            Self::Delete => 0x09,
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One row change with BoxDB values. `None` fields are undefined in the
/// changeset: unchanged columns of an UPDATE or omitted patchset fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub table: String,
    pub primary_key: Vec<bool>,
    pub operation: Operation,
    pub indirect: bool,
    pub old: Option<Vec<Option<Value>>>,
    pub new: Option<Vec<Option<Value>>>,
}

/// How `apply` treats a change whose target row no longer matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// Fail and roll back the whole apply (the default).
    #[default]
    Abort,
    /// Omit the conflicting change and report it.
    Skip,
    /// Write data and insert conflicts anyway; omit missing rows and
    /// constraint failures.
    Replace,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApplyOptions {
    /// Apply the inverse of the changeset, undoing it.
    pub invert: bool,
    pub on_conflict: ConflictPolicy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// The row exists but its recorded old values differ.
    Data,
    /// The row to update or delete is missing.
    NotFound,
    /// A row with the inserted primary key already exists.
    Conflict,
    /// SQLite rejected the write with a constraint failure.
    Constraint,
}
impl ConflictKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Constraint => "constraint",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub table: String,
    pub operation: Operation,
    pub kind: ConflictKind,
    /// Primary key values of the affected row as plain JSON.
    pub key: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied: usize,
    pub skipped: usize,
    pub conflicts: Vec<Conflict>,
}

pub(crate) type Record = Vec<Option<SqlValue>>;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawChange {
    pub table: String,
    pub primary_key: Vec<bool>,
    pub operation: Operation,
    pub indirect: bool,
    pub old: Option<Record>,
    pub new: Option<Record>,
}
impl RawChange {
    pub(crate) fn key(&self) -> Vec<u8> {
        let record = self.old.as_ref().or(self.new.as_ref()).unwrap();
        let mut bytes = vec![];
        for (value, pk) in record.iter().zip(&self.primary_key) {
            if *pk {
                write_value(&mut bytes, value.as_ref());
            }
        }
        bytes
    }
}

fn invalid(message: &str) -> Error {
    Error::new(format!("invalid changeset: {message}")).with_code("BOXDB_INVALID_VALUE")
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    if value <= 0x7f {
        out.push(value as u8);
        return;
    }
    if value > 0x00ff_ffff_ffff_ffff {
        let mut bytes = [0u8; 9];
        bytes[8] = value as u8;
        value >>= 8;
        for i in (0..8).rev() {
            bytes[i] = (value & 0x7f) as u8 | 0x80;
            value >>= 7;
        }
        out.extend(bytes);
        return;
    }
    let mut bytes = vec![];
    while value > 0 {
        bytes.push((value & 0x7f) as u8);
        value >>= 7;
    }
    bytes.reverse();
    let last = bytes.len() - 1;
    for (i, byte) in bytes.iter_mut().enumerate() {
        if i != last {
            *byte |= 0x80;
        }
    }
    out.extend(bytes);
}
fn read_varint(bytes: &[u8], position: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes
            .get(*position)
            .ok_or_else(|| invalid("truncated varint"))?;
        *position += 1;
        if i == 8 {
            return Ok((value << 8) | u64::from(byte));
        }
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(invalid("varint too long"))
}
fn write_value(out: &mut Vec<u8>, value: Option<&SqlValue>) {
    match value {
        None => out.push(0),
        Some(SqlValue::Integer(n)) => {
            out.push(1);
            out.extend(n.to_be_bytes());
        }
        Some(SqlValue::Real(n)) => {
            out.push(2);
            out.extend(n.to_bits().to_be_bytes());
        }
        Some(SqlValue::Text(text)) => {
            out.push(3);
            write_varint(out, text.len() as u64);
            out.extend(text.as_bytes());
        }
        Some(SqlValue::Blob(bytes)) => {
            out.push(4);
            write_varint(out, bytes.len() as u64);
            out.extend(bytes);
        }
        Some(SqlValue::Null) => out.push(5),
    }
}
fn read_value(bytes: &[u8], position: &mut usize) -> Result<Option<SqlValue>> {
    let kind = *bytes
        .get(*position)
        .ok_or_else(|| invalid("truncated record"))?;
    *position += 1;
    let take = |position: &mut usize, count: usize| -> Result<&[u8]> {
        let slice = bytes
            .get(*position..*position + count)
            .ok_or_else(|| invalid("truncated value"))?;
        *position += count;
        Ok(slice)
    };
    Ok(match kind {
        0 => None,
        1 => Some(SqlValue::Integer(i64::from_be_bytes(
            take(position, 8)?.try_into().unwrap(),
        ))),
        2 => Some(SqlValue::Real(f64::from_bits(u64::from_be_bytes(
            take(position, 8)?.try_into().unwrap(),
        )))),
        3 | 4 => {
            let length = usize::try_from(read_varint(bytes, position)?)
                .map_err(|_| invalid("value too large"))?;
            let data = take(position, length)?;
            if kind == 3 {
                Some(SqlValue::Text(
                    std::str::from_utf8(data)
                        .map_err(|_| invalid("text is not UTF-8"))?
                        .to_owned(),
                ))
            } else {
                Some(SqlValue::Blob(data.to_vec()))
            }
        }
        5 => Some(SqlValue::Null),
        _ => return Err(invalid("unknown value type")),
    })
}
fn read_record(bytes: &[u8], position: &mut usize, columns: usize) -> Result<Record> {
    (0..columns).map(|_| read_value(bytes, position)).collect()
}

pub(crate) fn write_key(out: &mut Vec<u8>, value: &SqlValue) {
    write_value(out, Some(value));
}
pub(crate) fn read_key(bytes: &[u8]) -> Result<Vec<SqlValue>> {
    let mut position = 0;
    let mut values = vec![];
    while position < bytes.len() {
        values.push(read_value(bytes, &mut position)?.ok_or_else(|| invalid("undefined key"))?);
    }
    Ok(values)
}

pub(crate) fn encode_raw(changes: &[RawChange], patchset: bool) -> Vec<u8> {
    let mut out = vec![];
    let mut header: Option<(&str, &[bool])> = None;
    for change in changes {
        if header != Some((change.table.as_str(), change.primary_key.as_slice())) {
            out.push(if patchset { 0x50 } else { 0x54 });
            write_varint(&mut out, change.primary_key.len() as u64);
            out.extend(change.primary_key.iter().map(|pk| u8::from(*pk)));
            out.extend(change.table.as_bytes());
            out.push(0);
            header = Some((change.table.as_str(), change.primary_key.as_slice()));
        }
        out.push(change.operation.code());
        out.push(u8::from(change.indirect));
        let records: Vec<&Record> = match change.operation {
            Operation::Insert => vec![change.new.as_ref().unwrap()],
            Operation::Delete => vec![change.old.as_ref().unwrap()],
            Operation::Update if patchset => vec![change.new.as_ref().unwrap()],
            Operation::Update => vec![change.old.as_ref().unwrap(), change.new.as_ref().unwrap()],
        };
        for record in records {
            for value in record {
                write_value(&mut out, value.as_ref());
            }
        }
    }
    out
}

pub(crate) struct Decoded {
    pub patchset: bool,
    pub changes: Vec<RawChange>,
}
pub(crate) fn decode_raw(bytes: &[u8]) -> Result<Decoded> {
    let mut position = 0;
    let mut changes = vec![];
    let mut patchset = false;
    let mut table: Option<(String, Vec<bool>)> = None;
    while position < bytes.len() {
        let marker = bytes[position];
        if marker == 0x54 || marker == 0x50 {
            if table.is_none() {
                patchset = marker == 0x50;
            } else if patchset != (marker == 0x50) {
                return Err(invalid("mixed changeset and patchset tables"));
            }
            position += 1;
            let columns = usize::try_from(read_varint(bytes, &mut position)?)
                .map_err(|_| invalid("column count"))?;
            let flags = bytes
                .get(position..position + columns)
                .ok_or_else(|| invalid("truncated table header"))?;
            if flags.iter().any(|flag| *flag > 1) {
                return Err(invalid("primary key flags"));
            }
            let primary_key: Vec<bool> = flags.iter().map(|flag| *flag == 1).collect();
            position += columns;
            let end = bytes[position..]
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| invalid("unterminated table name"))?;
            let name = std::str::from_utf8(&bytes[position..position + end])
                .map_err(|_| invalid("table name is not UTF-8"))?
                .to_owned();
            if name.is_empty() || columns == 0 {
                return Err(invalid("empty table header"));
            }
            position += end + 1;
            table = Some((name, primary_key));
            continue;
        }
        let Some((name, primary_key)) = &table else {
            return Err(invalid("change precedes its table header"));
        };
        let operation = match marker {
            0x12 => Operation::Insert,
            0x17 => Operation::Update,
            0x09 => Operation::Delete,
            _ => return Err(invalid("unknown operation")),
        };
        let indirect = match bytes.get(position + 1) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(invalid("indirect flag")),
        };
        position += 2;
        let columns = primary_key.len();
        let (old, new) = match operation {
            Operation::Insert => (None, Some(read_record(bytes, &mut position, columns)?)),
            Operation::Delete => (Some(read_record(bytes, &mut position, columns)?), None),
            Operation::Update if patchset => {
                (None, Some(read_record(bytes, &mut position, columns)?))
            }
            Operation::Update => {
                let old = read_record(bytes, &mut position, columns)?;
                (Some(old), Some(read_record(bytes, &mut position, columns)?))
            }
        };
        let defined = |record: &Record, all: bool| {
            record
                .iter()
                .zip(primary_key)
                .all(|(value, pk)| value.is_some() || (!all && !*pk))
        };
        let complete = match (operation, patchset) {
            (Operation::Insert, _) => defined(new.as_ref().unwrap(), true),
            (Operation::Delete, false) => defined(old.as_ref().unwrap(), true),
            (Operation::Delete, true) => defined(old.as_ref().unwrap(), false),
            (Operation::Update, false) => defined(old.as_ref().unwrap(), false),
            (Operation::Update, true) => defined(new.as_ref().unwrap(), false),
        };
        if !complete {
            return Err(invalid("undefined required field"));
        }
        changes.push(RawChange {
            table: name.clone(),
            primary_key: primary_key.clone(),
            operation,
            indirect,
            old,
            new,
        });
    }
    Ok(Decoded { patchset, changes })
}

fn typed(record: &Record) -> Vec<Option<Value>> {
    record
        .iter()
        .map(|value| value.clone().map(schema::native))
        .collect()
}
/// Decode a changeset or patchset into typed changes.
pub fn decode(bytes: &[u8]) -> Result<Vec<Change>> {
    Ok(decode_raw(bytes)?
        .changes
        .into_iter()
        .map(|change| Change {
            table: change.table,
            primary_key: change.primary_key,
            operation: change.operation,
            indirect: change.indirect,
            old: change.old.as_ref().map(typed),
            new: change.new.as_ref().map(typed),
        })
        .collect())
}
/// Validate changeset bytes without materializing typed values.
pub fn validate(bytes: &[u8]) -> Result<()> {
    decode_raw(bytes).map(|_| ())
}

pub(crate) fn invert_raw(change: &RawChange) -> Result<RawChange> {
    let mut inverted = change.clone();
    match change.operation {
        Operation::Insert => {
            inverted.operation = Operation::Delete;
            inverted.old = change.new.clone();
            inverted.new = None;
        }
        Operation::Delete => {
            inverted.operation = Operation::Insert;
            inverted.new = change.old.clone();
            inverted.old = None;
        }
        Operation::Update => {
            let (Some(old), Some(new)) = (&change.old, &change.new) else {
                return Err(invalid("patchsets cannot be inverted"));
            };
            inverted.old = Some(
                change
                    .primary_key
                    .iter()
                    .enumerate()
                    .map(|(i, pk)| if *pk { old[i].clone() } else { new[i].clone() })
                    .collect(),
            );
            inverted.new = Some(
                change
                    .primary_key
                    .iter()
                    .enumerate()
                    .map(|(i, pk)| if *pk { None } else { old[i].clone() })
                    .collect(),
            );
        }
    }
    Ok(inverted)
}
/// Produce the changeset that undoes `bytes`. Patchsets cannot be inverted.
pub fn invert(bytes: &[u8]) -> Result<Vec<u8>> {
    let decoded = decode_raw(bytes)?;
    if decoded.patchset {
        return Err(invalid("patchsets cannot be inverted"));
    }
    let changes = decoded
        .changes
        .iter()
        .map(invert_raw)
        .collect::<Result<Vec<_>>>()?;
    Ok(encode_raw(&changes, false))
}

/// Combine sequential changes to one row the way SQLite's session extension does.
pub(crate) fn merge(first: RawChange, second: RawChange) -> Result<Option<RawChange>> {
    use Operation::*;
    let columns = first.primary_key.len();
    let merged = match (first.operation, second.operation) {
        (Insert, Insert) | (Update, Insert) | (Delete, Update) | (Delete, Delete) => {
            return Err(invalid("changes to one row cannot be combined"));
        }
        (Insert, Update) => {
            let mut new = first.new.unwrap();
            for (i, value) in second.new.unwrap().into_iter().enumerate() {
                if let Some(value) = value {
                    new[i] = Some(value);
                }
            }
            RawChange {
                operation: Insert,
                old: None,
                new: Some(new),
                ..first
            }
        }
        (Insert, Delete) => return Ok(None),
        (Update, Update) => {
            let first_old = first.old.as_ref().unwrap();
            let first_new = first.new.as_ref().unwrap();
            let second_old = second.old.unwrap();
            let second_new = second.new.unwrap();
            let mut unchanged = true;
            let mut masked_old = vec![None; columns];
            let mut masked_new = vec![None; columns];
            for i in 0..columns {
                // A column the first update left alone kept its original value
                // until the second update read it as old.
                let original = first_old[i].clone().or_else(|| second_old[i].clone());
                let current = second_new[i].clone().or_else(|| first_new[i].clone());
                if first.primary_key[i] {
                    masked_old[i] = original;
                } else if let (Some(before), Some(after)) = (original, current)
                    && before != after
                {
                    masked_old[i] = Some(before);
                    masked_new[i] = Some(after);
                    unchanged = false;
                }
            }
            if unchanged {
                return Ok(None);
            }
            RawChange {
                operation: Update,
                old: Some(masked_old),
                new: Some(masked_new),
                ..first
            }
        }
        (Update, Delete) => {
            let old = first.old.unwrap();
            let deleted = second.old.unwrap();
            let record = (0..columns)
                .map(|i| old[i].clone().or_else(|| deleted[i].clone()))
                .collect();
            RawChange {
                operation: Delete,
                old: Some(record),
                new: None,
                ..first
            }
        }
        (Delete, Insert) => {
            let old = first.old.unwrap();
            let new = second.new.unwrap();
            let mut masked_old = vec![None; columns];
            let mut masked_new = vec![None; columns];
            let mut unchanged = true;
            for i in 0..columns {
                if first.primary_key[i] {
                    masked_old[i] = old[i].clone();
                } else if old[i] != new[i] {
                    masked_old[i] = old[i].clone();
                    masked_new[i] = new[i].clone();
                    unchanged = false;
                }
            }
            if unchanged {
                return Ok(None);
            }
            RawChange {
                operation: Update,
                old: Some(masked_old),
                new: Some(masked_new),
                ..first
            }
        }
    };
    Ok(Some(merged))
}
pub(crate) fn combine(changes: Vec<RawChange>) -> Result<Vec<RawChange>> {
    let mut order = vec![];
    let mut rows: BTreeMap<(String, Vec<u8>), Option<RawChange>> = BTreeMap::new();
    for change in changes {
        let key = (change.table.to_lowercase(), change.key());
        match rows.get_mut(&key) {
            Some(slot) => {
                let merged = match slot.take() {
                    Some(previous) => merge(previous, change)?,
                    None => Some(change),
                };
                *slot = merged;
            }
            None => {
                order.push(key.clone());
                rows.insert(key, Some(change));
            }
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|key| rows.remove(&key).flatten())
        .collect())
}
/// Append `second` to `first`, merging changes that affect the same row.
pub fn concat(first: &[u8], second: &[u8]) -> Result<Vec<u8>> {
    let a = decode_raw(first)?;
    let b = decode_raw(second)?;
    if a.patchset != b.patchset && !a.changes.is_empty() && !b.changes.is_empty() {
        return Err(invalid("cannot combine a changeset with a patchset"));
    }
    let patchset = a.patchset || b.patchset;
    if patchset {
        let mut out = first.to_vec();
        out.extend(second);
        return Ok(out);
    }
    let mut changes = a.changes;
    changes.extend(b.changes);
    Ok(encode_raw(&combine(changes)?, false))
}
