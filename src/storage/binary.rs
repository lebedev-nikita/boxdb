//! Independent v3 snapshot and commit-log codec: little-endian scalars, length-prefixed UTF-8
//! and byte strings, schema-typed cells, and a SHA-256 payload checksum.
//! No readable records, JSONL serialization, or compression are involved.
use super::{MAX_BYTES, declare_index, declare_table, error};
use crate::{Error, Result, Type, Value, engine::State};
use sha2::{Digest, Sha256};

pub(super) const MAGIC: &[u8] = b"BOXDB\0\x03";
const FRAME_HEADER: usize = 41;
const FOOTER: &[u8; 8] = b"COMMIT\0\0";
const HEADER: usize = MAGIC.len() + FRAME_HEADER + FOOTER.len();
const MAX_DEPTH: usize = 128;

struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_BYTES as usize - HEADER - self.0.len() {
            return Err(Error("database exceeds 256 MiB limit".into()));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<()> {
        let count = u32::try_from(count).map_err(error)?;
        self.bytes(&count.to_le_bytes())
    }
    fn blob(&mut self, bytes: &[u8]) -> Result<()> {
        self.count(bytes.len())?;
        self.bytes(bytes)
    }
    fn string(&mut self, text: &str) -> Result<()> {
        self.blob(text.as_bytes())
    }
    fn value(&mut self, ty: &Type, value: &Value) -> Result<()> {
        if value == &Value::Null {
            return self.bytes(&[0]);
        }
        self.bytes(&[1])?;
        match (ty, value) {
            (Type::SmallInt, Value::Integer(n)) => self.bytes(&(*n as i16).to_le_bytes()),
            (Type::Integer, Value::Integer(n)) => self.bytes(&(*n as i32).to_le_bytes()),
            (Type::BigInt, Value::Integer(n)) => self.bytes(&n.to_le_bytes()),
            (Type::Real, Value::Real(n)) => self.bytes(&(*n as f32).to_le_bytes()),
            (Type::Double, Value::Real(n)) => self.bytes(&n.to_le_bytes()),
            (Type::Boolean, Value::Boolean(b)) => self.bytes(&[u8::from(*b)]),
            (Type::Bytea, Value::Bytea(bytes)) => self.blob(bytes),
            (Type::Uuid, Value::Uuid(text)) => {
                self.bytes(uuid::Uuid::parse_str(text).map_err(error)?.as_bytes())
            }
            (Type::Numeric(_), Value::Numeric(text))
            | (Type::Text | Type::Varchar(_), Value::Text(text))
            | (Type::Date, Value::Date(text))
            | (Type::Time, Value::Time(text))
            | (Type::Timestamp, Value::Timestamp(text))
            | (Type::Timestamptz, Value::Timestamptz(text)) => self.string(text),
            (Type::Array(inner), Value::Array(values)) => {
                self.count(values.len())?;
                for value in values {
                    self.value(inner, value)?;
                }
                Ok(())
            }
            (Type::Json, Value::Json(value)) => self.json(value, 0),
            _ => Err(Error("invalid typed binary cell".into())),
        }
    }
    fn json(&mut self, value: &serde_json::Value, depth: usize) -> Result<()> {
        use serde_json::Value as Json;
        if depth > MAX_DEPTH {
            return Err(Error("binary JSON nesting exceeds limit".into()));
        }
        match value {
            Json::Null => self.bytes(&[0]),
            Json::Bool(false) => self.bytes(&[1]),
            Json::Bool(true) => self.bytes(&[2]),
            Json::Number(n) => {
                if let Some(n) = n.as_i64() {
                    self.bytes(&[3])?;
                    self.bytes(&n.to_le_bytes())
                } else if let Some(n) = n.as_u64() {
                    self.bytes(&[4])?;
                    self.bytes(&n.to_le_bytes())
                } else {
                    self.bytes(&[5])?;
                    self.bytes(&n.as_f64().unwrap().to_le_bytes())
                }
            }
            Json::String(text) => {
                self.bytes(&[6])?;
                self.string(text)
            }
            Json::Array(values) => {
                self.bytes(&[7])?;
                self.count(values.len())?;
                for value in values {
                    self.json(value, depth + 1)?;
                }
                Ok(())
            }
            Json::Object(values) => {
                self.bytes(&[8])?;
                self.count(values.len())?;
                for (key, value) in values {
                    self.string(key)?;
                    self.json(value, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        if length > self.bytes.len() {
            return Err(Error("truncated binary payload".into()));
        }
        let (value, rest) = self.bytes.split_at(length);
        self.bytes = rest;
        Ok(value)
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.fixed::<1>()?[0])
    }
    fn count(&mut self, minimum_bytes: usize) -> Result<usize> {
        let count = u32::from_le_bytes(self.fixed()?) as usize;
        // Bound lengths before allocating. No declared count can require more
        // cells/records than the remaining input can physically contain.
        if count > self.bytes.len() / minimum_bytes {
            return Err(Error("invalid binary length".into()));
        }
        Ok(count)
    }
    fn blob(&mut self) -> Result<&'a [u8]> {
        let count = self.count(1)?;
        self.take(count)
    }
    fn string(&mut self) -> Result<String> {
        Ok(std::str::from_utf8(self.blob()?).map_err(error)?.into())
    }
    fn value(&mut self, ty: &Type) -> Result<Value> {
        match self.byte()? {
            0 => return Ok(Value::Null),
            1 => (),
            _ => return Err(Error("invalid binary null marker".into())),
        }
        Ok(match ty {
            Type::SmallInt => Value::Integer(i16::from_le_bytes(self.fixed()?).into()),
            Type::Integer => Value::Integer(i32::from_le_bytes(self.fixed()?).into()),
            Type::BigInt => Value::Integer(i64::from_le_bytes(self.fixed()?)),
            Type::Real => Value::Real(f32::from_le_bytes(self.fixed()?).into()),
            Type::Double => Value::Real(f64::from_le_bytes(self.fixed()?)),
            Type::Boolean => Value::Boolean(match self.byte()? {
                0 => false,
                1 => true,
                _ => return Err(Error("invalid binary boolean".into())),
            }),
            Type::Bytea => Value::Bytea(self.blob()?.into()),
            Type::Uuid => Value::Uuid(uuid::Uuid::from_bytes(self.fixed()?).to_string()),
            Type::Numeric(_) => Value::Numeric(self.string()?),
            Type::Text | Type::Varchar(_) => Value::Text(self.string()?),
            Type::Date => Value::Date(self.string()?),
            Type::Time => Value::Time(self.string()?),
            Type::Timestamp => Value::Timestamp(self.string()?),
            Type::Timestamptz => Value::Timestamptz(self.string()?),
            Type::Array(inner) => {
                let count = self.count(1)?;
                let mut values = Vec::new();
                for _ in 0..count {
                    values.push(self.value(inner)?);
                }
                Value::Array(values)
            }
            Type::Json => Value::Json(self.json(0)?),
        })
    }
    fn json(&mut self, depth: usize) -> Result<serde_json::Value> {
        use serde_json::Value as Json;
        if depth > MAX_DEPTH {
            return Err(Error("binary JSON nesting exceeds limit".into()));
        }
        Ok(match self.byte()? {
            0 => Json::Null,
            1 => Json::Bool(false),
            2 => Json::Bool(true),
            3 => Json::Number(i64::from_le_bytes(self.fixed()?).into()),
            4 => Json::Number(u64::from_le_bytes(self.fixed()?).into()),
            5 => Json::Number(
                serde_json::Number::from_f64(f64::from_le_bytes(self.fixed()?))
                    .ok_or_else(|| Error("invalid binary JSON number".into()))?,
            ),
            6 => Json::String(self.string()?),
            7 => {
                let count = self.count(1)?;
                let mut values = Vec::new();
                for _ in 0..count {
                    values.push(self.json(depth + 1)?);
                }
                Json::Array(values)
            }
            8 => {
                let count = self.count(5)?;
                let mut values = serde_json::Map::new();
                for _ in 0..count {
                    let key = self.string()?;
                    if values.insert(key, self.json(depth + 1)?).is_some() {
                        return Err(Error("duplicate binary JSON object key".into()));
                    }
                }
                Json::Object(values)
            }
            _ => return Err(Error("invalid binary JSON tag".into())),
        })
    }
}

fn normalize(ty: &Type, value: Value) -> Result<Value> {
    if value == Value::Null {
        return Ok(value);
    }
    match ty {
        Type::Numeric(_) | Type::Date | Type::Time | Type::Timestamp | Type::Timestamptz => {
            ty.validate(&value)
        }
        Type::Array(inner) => {
            let Value::Array(values) = value else {
                unreachable!()
            };
            Ok(Value::Array(
                values
                    .into_iter()
                    .map(|v| normalize(inner, v))
                    .collect::<Result<_>>()?,
            ))
        }
        _ => Ok(value),
    }
}
fn encode_payload(state: &State) -> Result<Vec<u8>> {
    let mut payload = Writer(Vec::new());
    payload.count(state.tables.len())?;
    for (name, table) in &state.tables {
        payload.string(name)?;
        payload.string(&table.sql)?;
        payload.bytes(&table.next_rowid.to_le_bytes())?;
        payload.count(table.rows.len())?;
        for (id, row) in table.rows.records() {
            payload.bytes(&id.to_le_bytes())?;
            for (column, value) in table.columns.iter().zip(row) {
                payload.value(&column.ty, value)?;
            }
        }
    }
    payload.count(state.indexes.len())?;
    for (name, index) in &state.indexes {
        payload.string(name)?;
        payload.string(&index.sql)?;
    }
    Ok(payload.0)
}

fn decode_payload(payload: &[u8]) -> Result<State> {
    let mut reader = Reader { bytes: payload };
    let tables = reader.count(20)?;
    let mut state = State::default();
    for _ in 0..tables {
        let name = reader.string()?;
        let sql = reader.string()?;
        let next_rowid = u64::from_le_bytes(reader.fixed()?);
        declare_table(&mut state, &name, &sql, next_rowid)?;
        let table = state.tables.get_mut(&name).unwrap();
        let rows = reader.count(8 + table.columns.len())?;
        for _ in 0..rows {
            let id = u64::from_le_bytes(reader.fixed()?);
            let mut row = Vec::with_capacity(table.columns.len());
            for column in &table.columns {
                // Normalize rich values before checking cross-row constraints.
                row.push(normalize(&column.ty, reader.value(&column.ty)?)?);
            }
            table.rows.push(id, row)?;
        }
    }
    let indexes = reader.count(8)?;
    for _ in 0..indexes {
        let name = reader.string()?;
        let sql = reader.string()?;
        declare_index(&mut state, &name, &sql)?;
    }
    if !reader.bytes.is_empty() {
        return Err(Error("trailing binary payload".into()));
    }
    Ok(state)
}

fn frame(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() + HEADER > MAX_BYTES as usize {
        return Err(Error("database exceeds 256 MiB limit".into()));
    }
    let length = u32::try_from(payload.len()).map_err(error)?;
    let mut hash = Sha256::new();
    hash.update([kind]);
    hash.update(payload);
    let mut bytes = Vec::with_capacity(FRAME_HEADER + payload.len() + FOOTER.len());
    bytes.push(kind);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&(!length).to_le_bytes());
    bytes.extend_from_slice(&hash.finalize());
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(FOOTER);
    Ok(bytes)
}
fn stored_equal(a: &Value, b: &Value) -> bool {
    fn json_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
        use serde_json::Value as Json;
        match (a, b) {
            (Json::Number(a), Json::Number(b)) if a.is_f64() && b.is_f64() => {
                a.as_f64().unwrap().to_bits() == b.as_f64().unwrap().to_bits()
            }
            (Json::Array(a), Json::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_equal(a, b))
            }
            (Json::Object(a), Json::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ak, a), (bk, b))| ak == bk && json_equal(a, b))
            }
            _ => a == b,
        }
    }
    match (a, b) {
        (Value::Real(a), Value::Real(b)) => a.to_bits() == b.to_bits(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| stored_equal(a, b))
        }
        (Value::Json(a), Value::Json(b)) => json_equal(a, b),
        _ => a == b,
    }
}
pub(super) fn encode(state: &State) -> Result<Vec<u8>> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(frame(0, &encode_payload(state)?)?);
    Ok(bytes)
}
pub(super) fn commit(state: &State, previous: &State) -> Result<Option<Vec<u8>>> {
    let same_schema = state.tables.len() == previous.tables.len()
        && state.indexes.len() == previous.indexes.len()
        && state.tables.iter().all(|(name, table)| {
            previous
                .tables
                .get(name)
                .is_some_and(|old| table.sql == old.sql && table.rows.shares_history(&old.rows))
        })
        && state.indexes.iter().all(|(name, index)| {
            previous
                .indexes
                .get(name)
                .is_some_and(|old| old.sql == index.sql)
        });
    if !same_schema {
        return frame(0, &encode_payload(state)?).map(Some);
    }
    let mut patches = Vec::new();
    for (name, table) in &state.tables {
        let old = &previous.tables[name];
        let changes = table
            .rows
            .changes(&old.rows)
            .into_iter()
            .filter(|change| match (change.old, change.new) {
                (Some(a), Some(b)) => !a.iter().zip(b).all(|(a, b)| stored_equal(a, b)),
                _ => true,
            })
            .collect::<Vec<_>>();
        if !changes.is_empty() || old.next_rowid != table.next_rowid {
            patches.push((name, table, changes));
        }
    }
    if patches.is_empty() {
        return Ok(None);
    }
    let mut payload = Writer(Vec::new());
    payload.count(patches.len())?;
    for (name, table, changes) in patches {
        payload.string(name)?;
        payload.bytes(&table.next_rowid.to_le_bytes())?;
        payload.count(changes.len())?;
        for change in changes {
            let kind = match (change.old, change.new) {
                (_, None) => 0,
                (None, Some(_)) => 1,
                (Some(_), Some(_)) => 2,
            };
            payload.bytes(&[kind])?;
            payload.bytes(&change.id.to_le_bytes())?;
            if kind == 2 {
                let (old, row) = (change.old.unwrap(), change.new.unwrap());
                let cells = old
                    .iter()
                    .zip(row)
                    .enumerate()
                    .filter(|(_, (a, b))| !stored_equal(a, b))
                    .collect::<Vec<_>>();
                payload.count(cells.len())?;
                for (column, (_, value)) in cells {
                    payload.count(column)?;
                    payload.value(&table.columns[column].ty, value)?;
                }
            } else if let Some(row) = change.new {
                for (column, value) in table.columns.iter().zip(row) {
                    payload.value(&column.ty, value)?;
                }
            }
        }
    }
    frame(1, &payload.0).map(Some)
}
fn apply_commit(state: &mut State, payload: &[u8]) -> Result<()> {
    let mut reader = Reader { bytes: payload };
    let count = reader.count(16)?;
    for _ in 0..count {
        let name = reader.string()?;
        let table = state
            .tables
            .get_mut(&name)
            .ok_or_else(|| Error("commit references unknown table".into()))?;
        let next_rowid = u64::from_le_bytes(reader.fixed()?);
        if next_rowid < table.next_rowid {
            return Err(Error("invalid commit row ID sequence".into()));
        }
        let count = reader.count(9)?;
        for _ in 0..count {
            let kind = reader.byte()?;
            let id = u64::from_le_bytes(reader.fixed()?);
            if id == 0 || id >= next_rowid {
                return Err(Error("invalid commit row ID".into()));
            }
            match kind {
                0 => {
                    let position = table
                        .rows
                        .find(id)
                        .ok_or_else(|| Error("commit deletes a missing row".into()))?;
                    table.rows.remove(position);
                }
                1 => {
                    let mut row = Vec::with_capacity(table.columns.len());
                    for column in &table.columns {
                        row.push(normalize(&column.ty, reader.value(&column.ty)?)?);
                    }
                    if let Some(position) = table.rows.find(id) {
                        table.rows[position] = row;
                    } else {
                        table.rows.push(id, row)?;
                    }
                }
                2 => {
                    let position = table
                        .rows
                        .find(id)
                        .ok_or_else(|| Error("commit updates a missing row".into()))?;
                    let count = reader.count(5)?;
                    let mut previous = None;
                    for _ in 0..count {
                        let column = u32::from_le_bytes(reader.fixed()?) as usize;
                        if column >= table.columns.len() || previous.is_some_and(|p| p >= column) {
                            return Err(Error("invalid commit column sequence".into()));
                        }
                        previous = Some(column);
                        let value = normalize(
                            &table.columns[column].ty,
                            reader.value(&table.columns[column].ty)?,
                        )?;
                        table.rows[position][column] = value;
                    }
                }
                _ => return Err(Error("invalid commit row tag".into())),
            }
        }
        table.next_rowid = next_rowid;
    }
    if !reader.bytes.is_empty() {
        return Err(Error("trailing commit payload".into()));
    }
    Ok(())
}
// A complete frame is a committed transaction. Recovery ignores only an
// incomplete final frame; complete corrupt frames always fail validation.
pub(super) fn decode(bytes: &[u8], recover: bool) -> Result<(State, super::Journal)> {
    let mut position = MAGIC.len();
    let mut state = None;
    let mut journal = super::Journal::default();
    while position < bytes.len() {
        let remaining = &bytes[position..];
        if remaining.len() < FRAME_HEADER {
            break;
        }
        let kind = remaining[0];
        let length = u32::from_le_bytes(remaining[1..5].try_into().unwrap());
        let inverse = u32::from_le_bytes(remaining[5..9].try_into().unwrap());
        if length != !inverse || kind > 1 || length as u64 > MAX_BYTES {
            return Err(Error("invalid binary commit header".into()));
        }
        let total = FRAME_HEADER + length as usize + FOOTER.len();
        if remaining.len() < total {
            break;
        }
        let payload = &remaining[FRAME_HEADER..FRAME_HEADER + length as usize];
        let mut hash = Sha256::new();
        hash.update([kind]);
        hash.update(payload);
        if hash.finalize().as_slice() != &remaining[9..FRAME_HEADER] {
            return Err(Error("database checksum mismatch".into()));
        }
        if &remaining[total - FOOTER.len()..total] != FOOTER {
            return Err(Error("invalid binary commit marker".into()));
        }
        if kind == 0 {
            state = Some(decode_payload(payload)?);
            journal.base_length = (MAGIC.len() + total) as u64;
            journal.commits = 0;
        } else {
            apply_commit(
                state
                    .as_mut()
                    .ok_or_else(|| Error("missing binary base snapshot".into()))?,
                payload,
            )?;
            journal.commits += 1;
        }
        position += total;
    }
    let state = state.ok_or_else(|| Error("truncated binary base snapshot".into()))?;
    if !recover && position != bytes.len() {
        return Err(Error("incomplete binary commit".into()));
    }
    journal.length = position as u64;
    Ok((state, journal))
}
