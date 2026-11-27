//! A readable file is a YAML log: a header line followed by one line per
//! committed transaction. Opening a file replays its lines in order; VACUUM
//! and conversion write a canonical log of the current content.
use crate::{
    Error, Result, Type,
    schema::{self, quote},
    storage::MAX_SNAPSHOT,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use libsqlite3_sys::native::Kind;
use rusqlite::{
    Connection, OptionalExtension,
    types::{Value as SqlValue, ValueRef},
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
mod encode;
mod entry;
mod flow;
mod journal;
mod layout;
mod replay;
mod salvage;
mod values;
pub(crate) use self::{
    encode::write_log,
    journal::{Journal, Publication},
    layout::rowid,
    replay::decode_file,
    salvage::salvage,
};
use self::{
    entry::*,
    flow::{Node, Resolved, Scalar},
    layout::*,
    values::*,
};

const VERSION: i64 = 5;
const HEADER: &str = "- {boxdb: 5}\n";
const METADATA_PREFIX: &str = "__boxdb_";

/// The committed part of a readable file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Extent {
    pub committed: usize,
    /// An interrupted append left content after `committed`.
    pub torn: bool,
    /// The committed content ends with a line break, so lines can follow.
    pub terminated: bool,
}

/// Every line ends with a line break when it is appended. A final line without
/// one is an interrupted append unless it is a complete entry; a last line
/// holding NUL bytes comes from blocks that were never written.
pub(crate) fn extent(bytes: &[u8]) -> Extent {
    let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let tail = &bytes[end..];
    let complete = !tail.is_empty()
        && !tail.contains(&0)
        && std::str::from_utf8(tail).is_ok_and(|line| {
            matches!(
                flow::parse_line(line.strip_suffix('\r').unwrap_or(line)),
                Ok(Some(_))
            )
        });
    if complete {
        return Extent {
            committed: bytes.len(),
            torn: false,
            terminated: false,
        };
    }
    let mut extent = Extent {
        committed: end,
        torn: !tail.is_empty(),
        terminated: true,
    };
    if end > 0 {
        let start = bytes[..end - 1]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        if bytes[start..end].contains(&0) {
            extent.committed = start;
            extent.torn = true;
        }
    }
    extent
}

/// Report lines that are not valid log entries.
pub(crate) fn check_documents(bytes: &[u8], report: &mut crate::CheckReport) {
    let bytes = &bytes[..extent(bytes).committed];
    let text = String::from_utf8_lossy(bytes);
    for (index, (line, text)) in replay::lines(&text).into_iter().enumerate() {
        if let Err(error) = replay::parse(line, text) {
            report.issue("yaml", &error);
            let issue = report.issues.last_mut().unwrap();
            issue.document = Some(index + 1);
            issue.line = Some(line);
            if report.issues.len() >= 1000 {
                break;
            }
        }
    }
}

pub(crate) type Rejected = (crate::RejectedDocument, Vec<u8>);
