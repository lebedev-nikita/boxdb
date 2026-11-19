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
    sync::Arc,
};
mod decode;
mod document;
mod encode;
mod journal;
mod layout;
mod salvage;
mod values;
use self::decode::*;
pub(crate) use self::{
    decode::{decode, decode_file},
    encode::write_snapshot,
    journal::{Journal, Publication},
    layout::rowid,
    salvage::salvage,
};
use self::{document::*, encode::*, layout::*, values::*};

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
